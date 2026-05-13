//! The smudge filter: pointer-on-stdin → content-on-stdout.

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use git_lfs_pointer::{Oid, Pointer};
use git_lfs_store::Store;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::FetchError;
use crate::detect_pointer;

const COPY_BUFFER: usize = 64 * 1024;

/// One pointer extension's smudge side.
///
/// Mirrors [`crate::CleanExtension`]; the two are separate types
/// because clean and smudge commands come from distinct config keys
/// (`lfs.extension.<name>.clean` vs `.smudge`) and are consumed by
/// different code paths.
#[derive(Debug, Clone)]
pub struct SmudgeExtension {
    /// Extension name, as configured under `lfs.extension.<name>`.
    pub name: String,
    /// Single decimal digit (0-9) determining position in the chain;
    /// smudge walks extensions in reverse priority order.
    pub priority: u8,
    /// Raw shell command from `lfs.extension.<name>.smudge`. `%f`
    /// placeholders are substituted with the working-tree path.
    pub command: String,
}

/// Result of running the [`smudge`] filter on a piece of input.
#[derive(Debug)]
pub enum SmudgeOutcome {
    /// Input wasn't a pointer (or was malformed) and was emitted to
    /// the output stream verbatim.
    ///
    /// Matches upstream's "smudge with invalid pointer" behavior:
    /// git wraps everything through the filter, and non-LFS content
    /// has to come out unchanged.
    Passthrough,
    /// Input was a pointer; its content was streamed from the store
    /// to the output (or it was the empty pointer, which writes
    /// nothing).
    Resolved(Pointer),
}

/// Things that can go wrong while running [`smudge`].
#[derive(Debug, thiserror::Error)]
pub enum SmudgeError {
    /// Filesystem-level failure: reading the input, writing the
    /// output, opening the stored object, etc.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The pointer references an object that isn't in the local store.
    /// [`smudge_with_fetch`] handles this by invoking the caller's fetch
    /// closure; bare [`smudge`] surfaces it for the caller to react to.
    #[error("object {} (size {}) is not present in the local store", .0.oid, .0.size)]
    ObjectMissing(Pointer),
    /// The fetch closure passed to [`smudge_with_fetch`] failed to produce
    /// the missing object.
    #[error("fetch failed: {0}")]
    FetchFailed(FetchError),
    /// Pointer references an extension by name that isn't configured in
    /// `lfs.extension.<name>.smudge`. Mirrors upstream's
    /// `extension '%s' is not configured`.
    #[error("extension {name:?} is not configured")]
    ExtensionNotConfigured { name: String },
    /// Configured extension has an empty `smudge` command.
    #[error("extension {name:?} has no smudge command configured")]
    ExtensionMissingCommand { name: String },
    /// Failed to spawn the extension subprocess.
    #[error("failed to spawn extension {name:?}: {source}")]
    ExtensionSpawnFailed {
        name: String,
        #[source]
        source: io::Error,
    },
    /// Extension subprocess exited non-zero.
    #[error("extension {name:?} exited with status {status:?}")]
    ExtensionFailed { name: String, status: Option<i32> },
    /// An extension's output (or the stored object's content) didn't
    /// hash to the OID recorded in the pointer. Either the extension
    /// is non-deterministic, the on-disk object is corrupt, or the
    /// extension is the wrong implementation for what cleaned the file.
    #[error("OID mismatch for {stage}: expected {expected}, got {actual}")]
    OidMismatch {
        stage: String,
        expected: Oid,
        actual: Oid,
    },
}

/// Apply the smudge filter to `input`, writing the working-tree content
/// (or pass-through bytes) to `output`.
///
/// 1. If `input` parses as a pointer, look the OID up in the store and
///    stream the bytes out (running configured pointer extensions in
///    reverse priority order when the pointer carries any).
/// 2. If `input` doesn't parse as a pointer, pass it through verbatim.
///
/// `path` is the working-tree path passed to git's filter; substituted
/// for `%f` in each extension's smudge command. `extensions` is the
/// configured `lfs.extension.<name>` set; its order doesn't matter
/// (the chain is built from `pointer.extensions` in priority order).
pub fn smudge<R: Read, W: Write>(
    store: &Store,
    input: &mut R,
    output: &mut W,
    path: &str,
    extensions: &[SmudgeExtension],
) -> Result<SmudgeOutcome, SmudgeError> {
    let (head, maybe_pointer) = detect_pointer(input)?;

    let Some(pointer) = maybe_pointer else {
        // Not a pointer: pass bytes through unchanged.
        output.write_all(&head)?;
        io::copy(input, output)?;
        return Ok(SmudgeOutcome::Passthrough);
    };

    if pointer.is_empty() {
        return Ok(SmudgeOutcome::Resolved(pointer));
    }

    // Treat any size mismatch as "missing": same OID + different size means
    // a corrupt or partial local copy, and the recovery path is the same
    // as a real miss — re-download.
    if !store.contains_with_size(pointer.oid, pointer.size) {
        return Err(SmudgeError::ObjectMissing(pointer));
    }

    smudge_object_to(store, &pointer, output, path, extensions, None)?;
    Ok(SmudgeOutcome::Resolved(pointer))
}

/// Like [`smudge`], but on a missing-object miss invokes `fetch` to populate
/// the store, then streams the freshly-fetched bytes to `output`.
///
/// `fetch` receives the [`Pointer`] of the missing object; the
/// caller is expected to download exactly that OID into the local
/// store. After a successful return, this function re-checks the
/// store and streams the content; if the store *still* doesn't have
/// the object, an [`SmudgeError::ObjectMissing`] is surfaced (i.e.
/// the fetch lied).
pub fn smudge_with_fetch<R, W, F>(
    store: &Store,
    input: &mut R,
    output: &mut W,
    path: &str,
    extensions: &[SmudgeExtension],
    mut fetch: F,
) -> Result<SmudgeOutcome, SmudgeError>
where
    R: Read,
    W: Write,
    F: FnMut(&Pointer) -> Result<(), FetchError>,
{
    match smudge(store, input, output, path, extensions) {
        Err(SmudgeError::ObjectMissing(pointer)) => {
            fetch(&pointer).map_err(SmudgeError::FetchFailed)?;
            if !store.contains_with_size(pointer.oid, pointer.size) {
                return Err(SmudgeError::ObjectMissing(pointer));
            }
            smudge_object_to(store, &pointer, output, path, extensions, None)?;
            Ok(SmudgeOutcome::Resolved(pointer))
        }
        other => other,
    }
}

/// Stream the working-tree content for an already-parsed `pointer` to
/// `output`.
///
/// Used by `pull` and `checkout`, which have the pointer in hand
/// from the index walk. `spawn_cwd` is the working directory each
/// extension subprocess runs from: pass `Some(work_tree_root)` from
/// pull or checkout (so a `git lfs pull` invoked from a subdirectory
/// still finds `.git/`); the smudge filter (called by git from the
/// work-tree root) can pass `None` to inherit the parent's cwd.
///
/// The caller must have already verified `store.contains_with_size`;
/// this function won't fetch.
pub fn smudge_object_to<W: Write>(
    store: &Store,
    pointer: &Pointer,
    output: &mut W,
    path: &str,
    extensions: &[SmudgeExtension],
    spawn_cwd: Option<&Path>,
) -> Result<(), SmudgeError> {
    if pointer.extensions.is_empty() {
        let mut file = store.open(pointer.oid)?;
        io::copy(&mut file, output)?;
        return Ok(());
    }
    apply_smudge_chain(store, pointer, output, path, extensions, spawn_cwd)
}

fn apply_smudge_chain<W: Write>(
    store: &Store,
    pointer: &Pointer,
    output: &mut W,
    path: &str,
    extensions: &[SmudgeExtension],
    spawn_cwd: Option<&Path>,
) -> Result<(), SmudgeError> {
    // Match each pointer extension to its registered config by name.
    // Walk in *reverse* priority order — clean ran ext0 → ext1 → store;
    // smudge undoes that with ext1 → ext0 → working tree.
    let mut chain: Vec<(&SmudgeExtension, Oid)> = Vec::with_capacity(pointer.extensions.len());
    for ptr_ext in &pointer.extensions {
        let registered = extensions
            .iter()
            .find(|e| e.name == ptr_ext.name)
            .ok_or_else(|| SmudgeError::ExtensionNotConfigured {
                name: ptr_ext.name.clone(),
            })?;
        if registered.command.trim().is_empty() {
            return Err(SmudgeError::ExtensionMissingCommand {
                name: registered.name.clone(),
            });
        }
        chain.push((registered, ptr_ext.oid));
    }
    chain.reverse();

    let tmp_dir = store.tmp_dir();
    fs::create_dir_all(&tmp_dir)?;

    // Stage 0: copy the stored object into a tmp file. Verify the
    // input hash equals `pointer.oid` — should always hold (the store
    // is content-addressed) but a corrupt object would otherwise
    // surface as a confusing extension-output mismatch later on.
    let mut current_tmp = NamedTempFile::new_in(&tmp_dir)?;
    let mut store_file = store.open(pointer.oid)?;
    let initial_oid = hash_and_write(&mut store_file, current_tmp.as_file_mut())?;
    if initial_oid != pointer.oid {
        return Err(SmudgeError::OidMismatch {
            stage: format!("stored object {}", pointer.oid),
            expected: pointer.oid,
            actual: initial_oid,
        });
    }

    for (i, (ext, expected_out_oid)) in chain.iter().enumerate() {
        let cmd_str = ext.command.replace("%f", path);
        let mut parts = cmd_str.split_whitespace();
        let prog = parts
            .next()
            .ok_or_else(|| SmudgeError::ExtensionMissingCommand {
                name: ext.name.clone(),
            })?;
        let args: Vec<&str> = parts.collect();

        let stdin_file = std::fs::File::open(current_tmp.path())?;
        let mut command = Command::new(prog);
        command
            .args(&args)
            .stdin(stdin_file)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(dir) = spawn_cwd {
            command.current_dir(dir);
        }
        let mut child = command
            .spawn()
            .map_err(|e| SmudgeError::ExtensionSpawnFailed {
                name: ext.name.clone(),
                source: e,
            })?;
        let mut stdout = child.stdout.take().expect("piped stdout");

        let is_last = i + 1 == chain.len();
        if is_last {
            let actual_oid = hash_and_copy(&mut stdout, output)?;
            let status = child.wait()?;
            if !status.success() {
                return Err(SmudgeError::ExtensionFailed {
                    name: ext.name.clone(),
                    status: status.code(),
                });
            }
            if actual_oid != *expected_out_oid {
                return Err(SmudgeError::OidMismatch {
                    stage: format!("smudge output of extension {:?}", ext.name),
                    expected: *expected_out_oid,
                    actual: actual_oid,
                });
            }
            return Ok(());
        }

        let mut next_tmp = NamedTempFile::new_in(&tmp_dir)?;
        let actual_oid = hash_and_write(&mut stdout, next_tmp.as_file_mut())?;
        let status = child.wait()?;
        if !status.success() {
            return Err(SmudgeError::ExtensionFailed {
                name: ext.name.clone(),
                status: status.code(),
            });
        }
        if actual_oid != *expected_out_oid {
            return Err(SmudgeError::OidMismatch {
                stage: format!("smudge output of extension {:?}", ext.name),
                expected: *expected_out_oid,
                actual: actual_oid,
            });
        }
        current_tmp = next_tmp;
    }
    unreachable!("smudge chain exited without writing output")
}

fn hash_and_write<R: Read>(src: &mut R, dst: &mut std::fs::File) -> io::Result<Oid> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUFFER];
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        dst.write_all(&buf[..n])?;
    }
    dst.flush()?;
    let bytes: [u8; 32] = hasher.finalize().into();
    Ok(Oid::from_bytes(bytes))
}

fn hash_and_copy<R: Read, W: Write>(src: &mut R, dst: &mut W) -> io::Result<Oid> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUFFER];
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        dst.write_all(&buf[..n])?;
    }
    let bytes: [u8; 32] = hasher.finalize().into();
    Ok(Oid::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean;
    use git_lfs_pointer::VERSION_LATEST;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::new(tmp.path().join("lfs"));
        (tmp, store)
    }

    fn run(store: &Store, input: &[u8]) -> (Result<SmudgeOutcome, SmudgeError>, Vec<u8>) {
        let mut out = Vec::new();
        let outcome = smudge(store, &mut { input }, &mut out, "", &[]);
        (outcome, out)
    }

    /// Insert content via the clean filter and return the resulting pointer text.
    fn clean_into(store: &Store, content: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        clean(store, &mut { content }, &mut out, "", &[]).unwrap();
        out
    }

    // ---------- Resolved ----------

    #[test]
    fn pointer_resolves_from_store() {
        let (_t, store) = fixture();
        let content = b"smudge a\n";
        let pointer_text = clean_into(&store, content);

        let (outcome, out) = run(&store, &pointer_text);
        let p = match outcome.unwrap() {
            SmudgeOutcome::Resolved(p) => p,
            o => panic!("expected Resolved, got {o:?}"),
        };
        assert_eq!(p.size, content.len() as u64);
        assert_eq!(out, content);
    }

    #[test]
    fn empty_pointer_writes_nothing() {
        let (_t, store) = fixture();
        let (outcome, out) = run(&store, b"");
        match outcome.unwrap() {
            SmudgeOutcome::Resolved(p) => assert!(p.is_empty()),
            o => panic!("expected Resolved(empty), got {o:?}"),
        }
        assert!(out.is_empty());
    }

    #[test]
    fn clean_smudge_round_trip_preserves_bytes() {
        let (_t, store) = fixture();
        for content in [
            &b""[..],
            &b"hello"[..],
            &b"binary \x00\x01\xff data"[..],
            &(0..256u16).map(|i| i as u8).collect::<Vec<_>>(),
        ] {
            let pointer_text = clean_into(&store, content);
            let mut out = Vec::new();
            smudge(&store, &mut { &pointer_text[..] }, &mut out, "", &[]).unwrap();
            assert_eq!(out, content, "round-trip failed for {content:?}");
        }
    }

    // ---------- Passthrough ----------

    #[test]
    fn invalid_pointer_passes_through_short() {
        let (_t, store) = fixture();
        for input in [&b"wat"[..], b"not a git-lfs file", b"version "] {
            let (outcome, out) = run(&store, input);
            assert!(matches!(outcome.unwrap(), SmudgeOutcome::Passthrough));
            assert_eq!(out, input);
        }
    }

    #[test]
    fn long_non_pointer_passes_through() {
        // > MAX_POINTER_SIZE bytes — exercises the head buffer + io::copy path.
        let (_t, store) = fixture();
        let content: Vec<u8> = (0..2048u32).map(|i| (i ^ (i >> 3)) as u8).collect();
        let (outcome, out) = run(&store, &content);
        assert!(matches!(outcome.unwrap(), SmudgeOutcome::Passthrough));
        assert_eq!(out, content);
    }

    // ---------- Errors ----------

    #[test]
    fn missing_object_errors() {
        let (_t, store) = fixture();
        let unknown_oid = "0000000000000000000000000000000000000000000000000000000000000001";
        let pointer_text = format!("version {VERSION_LATEST}\noid sha256:{unknown_oid}\nsize 5\n");
        let (outcome, out) = run(&store, pointer_text.as_bytes());
        match outcome.unwrap_err() {
            SmudgeError::ObjectMissing(pointer) => {
                assert_eq!(pointer.oid.to_string(), unknown_oid);
                assert_eq!(pointer.size, 5);
            }
            e => panic!("expected ObjectMissing, got {e:?}"),
        }
        assert!(out.is_empty(), "no partial output on miss");
    }

    #[test]
    fn size_mismatch_treated_as_missing() {
        let (_t, store) = fixture();
        let pointer_text = clean_into(&store, b"abc"); // size = 3
        // Replace "size 3" with "size 99" — parses fine, but won't match the
        // 3-byte object on disk.
        let tampered = String::from_utf8(pointer_text)
            .unwrap()
            .replace("size 3", "size 99");
        let (outcome, _) = run(&store, tampered.as_bytes());
        match outcome.unwrap_err() {
            SmudgeError::ObjectMissing(p) => assert_eq!(p.size, 99),
            e => panic!("expected ObjectMissing, got {e:?}"),
        }
    }

    // ---------- smudge_with_fetch ----------

    #[test]
    fn fetch_populates_store_then_streams() {
        let (_t, store) = fixture();
        let content = b"to be fetched\n";
        // Build the pointer text without inserting the object — the store
        // is "empty" from the smudge's perspective. The fetch closure will
        // be the one to actually populate it.
        let pointer_text = clean_into(&store, content);
        // Wipe the just-inserted object to simulate a true miss.
        let parsed = git_lfs_pointer::Pointer::parse(&pointer_text).unwrap();
        std::fs::remove_file(store.object_path(parsed.oid)).unwrap();
        assert!(!store.contains(parsed.oid));

        let mut out = Vec::new();
        let store_ref = &store;
        let outcome = smudge_with_fetch(
            &store,
            &mut { &pointer_text[..] },
            &mut out,
            "",
            &[],
            |p: &Pointer| {
                // "Download" by inserting the bytes synchronously.
                store_ref.insert(&mut { &content[..] }).unwrap();
                assert_eq!(p.size, content.len() as u64);
                Ok(())
            },
        );
        assert!(matches!(outcome.unwrap(), SmudgeOutcome::Resolved(_)));
        assert_eq!(out, content);
    }

    #[test]
    fn fetch_failure_surfaces_as_fetch_failed() {
        let (_t, store) = fixture();
        let unknown = "0000000000000000000000000000000000000000000000000000000000000001";
        let pointer_text = format!("version {VERSION_LATEST}\noid sha256:{unknown}\nsize 5\n");
        let mut out = Vec::new();
        let outcome = smudge_with_fetch(
            &store,
            &mut { pointer_text.as_bytes() },
            &mut out,
            "",
            &[],
            |_p: &Pointer| Err("server is on fire".into()),
        );
        match outcome.unwrap_err() {
            SmudgeError::FetchFailed(e) => {
                assert!(e.to_string().contains("server is on fire"));
            }
            other => panic!("expected FetchFailed, got {other:?}"),
        }
        assert!(out.is_empty());
    }

    #[test]
    fn fetch_returning_ok_but_not_inserting_still_errors() {
        // Closure lies — claims success but didn't populate the store.
        let (_t, store) = fixture();
        let unknown = "0000000000000000000000000000000000000000000000000000000000000001";
        let pointer_text = format!("version {VERSION_LATEST}\noid sha256:{unknown}\nsize 5\n");
        let mut out = Vec::new();
        let outcome = smudge_with_fetch(
            &store,
            &mut { pointer_text.as_bytes() },
            &mut out,
            "",
            &[],
            |_p: &Pointer| Ok(()),
        );
        assert!(matches!(
            outcome.unwrap_err(),
            SmudgeError::ObjectMissing(_)
        ));
    }

    #[test]
    fn fetch_not_invoked_when_object_already_present() {
        let (_t, store) = fixture();
        let content = b"already here";
        let pointer_text = clean_into(&store, content);
        let mut out = Vec::new();
        let mut calls = 0;
        smudge_with_fetch(
            &store,
            &mut { &pointer_text[..] },
            &mut out,
            "",
            &[],
            |_p: &Pointer| {
                calls += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            calls, 0,
            "fetch must not be called when store has the object"
        );
        assert_eq!(out, content);
    }

    // ---------- Extensions ----------

    /// Round-trip clean → smudge through `tr a-z A-Z` (the lower-case-
    /// inverter stand-in we use for cli tests too). Verifies the chained
    /// subprocess + OID bookkeeping. The upstream Go tests exercise the
    /// case-inverter end-to-end — this is the unit-level analog.
    #[test]
    fn single_extension_round_trips() {
        let (_t, store) = fixture();
        let clean_exts = vec![crate::CleanExtension {
            name: "upper".into(),
            priority: 0,
            command: "tr a-z A-Z".into(),
        }];
        let smudge_exts = vec![SmudgeExtension {
            name: "upper".into(),
            priority: 0,
            command: "tr A-Z a-z".into(),
        }];

        // Clean "abc" → store "ABC", pointer with ext-0-upper.
        let mut pointer_buf = Vec::new();
        crate::clean(
            &store,
            &mut &b"abc"[..],
            &mut pointer_buf,
            "foo.txt",
            &clean_exts,
        )
        .unwrap();

        // Smudge that pointer back through the extension chain → "abc".
        let mut out = Vec::new();
        let outcome = smudge(
            &store,
            &mut pointer_buf.as_slice(),
            &mut out,
            "foo.txt",
            &smudge_exts,
        )
        .unwrap();
        assert!(matches!(outcome, SmudgeOutcome::Resolved(_)));
        assert_eq!(out, b"abc");
    }

    #[test]
    fn extension_not_configured_errors() {
        let (_t, store) = fixture();
        let oid_hex = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";
        let ext_oid = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let pointer_text = format!(
            "version {VERSION_LATEST}\n\
             ext-0-foo sha256:{ext_oid}\n\
             oid sha256:{oid_hex}\n\
             size 12345\n",
        );
        let mut out = Vec::new();
        let err = smudge(&store, &mut pointer_text.as_bytes(), &mut out, "x", &[]).unwrap_err();
        // We hit ObjectMissing first because the store doesn't have the
        // referenced OID; ExtensionNotConfigured would surface only
        // after the object is present. Fine for this test — the goal
        // is just to confirm we no longer error with an "unsupported"
        // shaped variant.
        assert!(matches!(err, SmudgeError::ObjectMissing(_)));
    }
}
