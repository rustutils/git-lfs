//! The smudge filter, which replaces a pointer with its object's content.

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

/// The smudge side of one pointer extension.
///
/// It's matched to a pointer's `ext-N-<name>` line by name.
#[derive(Debug, Clone)]
pub struct SmudgeExtension {
    /// Extension name, as configured under `lfs.extension.<name>`.
    pub name: String,
    /// Position in the chain, from 0 to 9.
    ///
    /// Smudge doesn't read it. The order comes from the pointer.
    pub priority: u8,
    /// Command from `lfs.extension.<name>.smudge`.
    ///
    /// It's split on whitespace into a program and arguments, with no shell
    /// involved. `%f` is replaced with the working-tree path.
    pub command: String,
}

/// Result of running the [`smudge`] filter on a piece of input.
#[derive(Debug)]
pub enum SmudgeOutcome {
    /// The input wasn't a pointer and was written out unchanged.
    Passthrough,
    /// The input was a pointer and its content was written out.
    ///
    /// The empty pointer writes nothing.
    Resolved(Pointer),
}

/// Things that can go wrong while running [`smudge`].
#[derive(Debug, thiserror::Error)]
pub enum SmudgeError {
    /// Reading the input, writing the output, or reading the store failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The store doesn't have the object, or has it with a different size.
    #[error("object {} (size {}) is not present in the local store", .0.oid, .0.size)]
    ObjectMissing(Pointer),
    /// The fetch closure passed to [`smudge_with_fetch`] failed.
    ///
    /// Displays with upstream's wording, which the shell tests match.
    #[error("Error downloading {path} ({oid}): {source}")]
    FetchFailed {
        /// The path being smudged.
        path: String,
        oid: Oid,
        #[source]
        source: FetchError,
    },
    /// The pointer names an extension that isn't in the configured list.
    #[error("extension {name:?} is not configured")]
    ExtensionNotConfigured { name: String },
    /// An extension has an empty command.
    #[error("extension {name:?} has no smudge command configured")]
    ExtensionMissingCommand { name: String },
    /// An extension's program couldn't be started.
    #[error("failed to spawn extension {name:?}: {source}")]
    ExtensionSpawnFailed {
        name: String,
        #[source]
        source: io::Error,
    },
    /// An extension exited with a non-zero status.
    #[error("extension {name:?} exited with status {status:?}")]
    ExtensionFailed { name: String, status: Option<i32> },
    /// A stage's output didn't hash to the OID recorded in the pointer.
    ///
    /// Only checked for pointers with extensions. The stored object is
    /// checked against the pointer's OID, and each extension's output
    /// against the OID that stage had on the clean side. Output from the
    /// last stage has already been written when this is returned.
    #[error("OID mismatch for {stage}: expected {expected}, got {actual}")]
    OidMismatch {
        stage: String,
        expected: Oid,
        actual: Oid,
    },
}

impl SmudgeError {
    /// The line upstream prints when a smudge download fails.
    ///
    /// `None` for errors other than [`FetchFailed`](Self::FetchFailed).
    pub fn download_error_line(&self) -> Option<String> {
        let SmudgeError::FetchFailed { path, oid, .. } = self else {
            return None;
        };
        let short = &oid.to_string()[..7];
        Some(format!(
            "Error downloading object: {path} ({short}): Smudge error: {self}"
        ))
    }
}

/// Run the smudge filter on `input` and write the content to `output`.
///
/// If `input` is a pointer, its object is copied from the store, through
/// the pointer's extensions in reverse priority order if it has any. Other
/// input is written out unchanged. Fails with
/// [`SmudgeError::ObjectMissing`], before writing anything, if the store
/// doesn't have the object at the pointer's size.
///
/// `path` is the working-tree path, substituted for `%f` in extension
/// commands. `extensions` can be in any order.
pub fn smudge<R: Read, W: Write>(
    store: &Store,
    input: &mut R,
    output: &mut W,
    path: &str,
    extensions: &[SmudgeExtension],
) -> Result<SmudgeOutcome, SmudgeError> {
    let (head, maybe_pointer) = detect_pointer(input)?;

    let Some(pointer) = maybe_pointer else {
        output.write_all(&head)?;
        io::copy(input, output)?;
        return Ok(SmudgeOutcome::Passthrough);
    };

    if pointer.is_empty() {
        return Ok(SmudgeOutcome::Resolved(pointer));
    }

    // A size mismatch means a corrupt or partial local copy, which a
    // download fixes just like a real miss.
    if !store.contains_with_size(pointer.oid, pointer.size) {
        return Err(SmudgeError::ObjectMissing(pointer));
    }

    smudge_object_to(store, &pointer, output, path, extensions, None)?;
    Ok(SmudgeOutcome::Resolved(pointer))
}

/// Like [`smudge`], but calls `fetch` to download a missing object.
///
/// `fetch` gets the pointer and must put that object into `store`. Its
/// error is returned as [`SmudgeError::FetchFailed`]. If it returns `Ok`
/// and the store still doesn't have the object at the right size, this
/// fails with [`SmudgeError::ObjectMissing`].
pub fn smudge_with_fetch<R, W, F>(
    store: &Store,
    input: &mut R,
    output: &mut W,
    path: &str,
    extensions: &[SmudgeExtension],
    fetch: F,
) -> Result<SmudgeOutcome, SmudgeError>
where
    R: Read,
    W: Write,
    F: FnMut(&Pointer) -> Result<(), FetchError>,
{
    let outcome = smudge_or_fetch(store, input, output, path, extensions, fetch)?;
    // Upstream logs each smudged file as a `download`, whether or not it
    // needed downloading.
    if let SmudgeOutcome::Resolved(pointer) = &outcome
        && pointer.size > 0
    {
        crate::progress::log_file("download", path, pointer.size);
    }
    Ok(outcome)
}

fn smudge_or_fetch<R, W, F>(
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
            fetch(&pointer).map_err(|source| SmudgeError::FetchFailed {
                path: path.to_owned(),
                oid: pointer.oid,
                source,
            })?;
            if !store.contains_with_size(pointer.oid, pointer.size) {
                return Err(SmudgeError::ObjectMissing(pointer));
            }
            smudge_object_to(store, &pointer, output, path, extensions, None)?;
            Ok(SmudgeOutcome::Resolved(pointer))
        }
        other => other,
    }
}

/// Write the content for an already-parsed `pointer` to `output`.
///
/// The caller must check [`Store::contains_with_size`] first. This doesn't
/// fetch, and a missing object fails as [`SmudgeError::Io`].
///
/// `spawn_cwd` is the directory extensions run in, or `None` for the
/// current one. Commands run from a subdirectory pass the work-tree root, so
/// extensions see the same directory as when git runs the filter.
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
    // `pointer.extensions` is sorted by priority. Clean ran them in that
    // order, so smudge runs them in reverse.
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

    // Check the stored object too, so a corrupt object isn't reported as a
    // mismatch in some extension's output.
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
        // Longer than MAX_POINTER_SIZE, so both the head and the rest are
        // copied.
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
        // Replace "size 3" with "size 99". It parses fine, but won't match the
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
        let pointer_text = clean_into(&store, content);
        // Remove the object so the fetch closure has to put it back.
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
            e @ SmudgeError::FetchFailed { .. } => {
                assert!(e.to_string().ends_with("): server is on fire"), "{e}");
            }
            other => panic!("expected FetchFailed, got {other:?}"),
        }
        assert!(out.is_empty());
    }

    #[test]
    fn fetch_returning_ok_but_not_inserting_still_errors() {
        // The closure claims success but doesn't populate the store.
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

    /// Round-trip clean and smudge through `tr`, checking the subprocess
    /// chain and the OID bookkeeping.
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
        // The store doesn't have the object, so ObjectMissing comes first.
        // ExtensionNotConfigured would only surface once it's present.
        assert!(matches!(err, SmudgeError::ObjectMissing(_)));
    }
}
