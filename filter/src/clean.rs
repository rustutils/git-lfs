//! The clean filter, which stores content and writes a pointer in its place.

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use git_lfs_pointer::{Extension, Oid, Pointer};
use git_lfs_store::{Store, StoreError};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::detect_pointer;

const COPY_BUFFER: usize = 64 * 1024;

/// Result of running the [`clean`] filter on a piece of input.
#[derive(Debug)]
pub enum CleanOutcome {
    /// The input already parsed as a pointer and was written out unchanged.
    ///
    /// Nothing was stored. This makes cleaning a pointer a no-op, and keeps
    /// a non-canonical pointer's exact bytes so its blob hash doesn't change.
    Passthrough(Pointer),
    /// The input was content.
    ///
    /// It was inserted into the store and the canonical encoding of the new
    /// pointer was written out.
    Stored(Pointer),
}

impl CleanOutcome {
    /// The pointer that was passed through or created.
    pub fn pointer(&self) -> &Pointer {
        match self {
            Self::Passthrough(p) | Self::Stored(p) => p,
        }
    }

    /// Whether the input was already a pointer.
    pub fn was_passthrough(&self) -> bool {
        matches!(self, Self::Passthrough(_))
    }
}

/// The clean side of one pointer extension.
#[derive(Debug, Clone)]
pub struct CleanExtension {
    /// Extension name, as configured under `lfs.extension.<name>`.
    pub name: String,
    /// Position in the chain, from 0 to 9.
    ///
    /// It's written into the pointer as the `N` in `ext-N-<name>`.
    pub priority: u8,
    /// Command from `lfs.extension.<name>.clean`.
    ///
    /// It's split on whitespace into a program and arguments, with no shell
    /// involved. `%f` is replaced with the working-tree path.
    pub command: String,
}

/// Things that can go wrong while running [`clean`].
#[derive(Debug, thiserror::Error)]
pub enum CleanError {
    /// Reading the input, writing the output, or a temporary file failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The store couldn't insert the object.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// An extension has an empty command.
    #[error("extension {name:?} has no clean command configured")]
    ExtensionMissingCommand { name: String },
    /// An extension's program couldn't be started, usually because it isn't
    /// on `PATH`.
    #[error("failed to spawn extension {name:?}: {source}")]
    ExtensionSpawnFailed {
        name: String,
        #[source]
        source: io::Error,
    },
    /// An extension exited with a non-zero status.
    #[error("extension {name:?} exited with status {status:?}")]
    ExtensionFailed { name: String, status: Option<i32> },
}

/// Run the clean filter on `input` and write the pointer to `output`.
///
/// Input shorter than [`MAX_POINTER_SIZE`](git_lfs_pointer::MAX_POINTER_SIZE)
/// that parses as a pointer is written out unchanged. Anything else is
/// streamed through `extensions` in the order given, which should be by
/// priority, and the last stage's output is inserted into the store. Each
/// stage's input OID goes into the pointer as an `ext-N-<name>` line.
///
/// `path` is the working-tree path, substituted for `%f` in extension
/// commands. It may be empty when git didn't pass one.
pub fn clean<R: Read, W: Write>(
    store: &Store,
    input: &mut R,
    output: &mut W,
    path: &str,
    extensions: &[CleanExtension],
) -> Result<CleanOutcome, CleanError> {
    let outcome = clean_content(store, input, output, path, extensions)?;
    if let CleanOutcome::Stored(pointer) = &outcome {
        crate::progress::log_file("clean", path, pointer.size);
    }
    Ok(outcome)
}

fn clean_content<R: Read, W: Write>(
    store: &Store,
    input: &mut R,
    output: &mut W,
    path: &str,
    extensions: &[CleanExtension],
) -> Result<CleanOutcome, CleanError> {
    let (head, maybe_pointer) = detect_pointer(input)?;

    if let Some(pointer) = maybe_pointer {
        output.write_all(&head)?;
        return Ok(CleanOutcome::Passthrough(pointer));
    }

    if extensions.is_empty() {
        let mut combined = head.as_slice().chain(input);
        let (oid, size) = store.insert(&mut combined)?;
        let pointer = Pointer::new(oid, size);
        output.write_all(pointer.encode().as_bytes())?;
        return Ok(CleanOutcome::Stored(pointer));
    }

    for ext in extensions {
        if ext.command.trim().is_empty() {
            return Err(CleanError::ExtensionMissingCommand {
                name: ext.name.clone(),
            });
        }
    }

    let tmp_dir = store.tmp_dir();
    std::fs::create_dir_all(&tmp_dir)?;

    // Buffer each stage's input to a temporary file while hashing it.
    let mut combined = head.as_slice().chain(input);
    let mut current_tmp = NamedTempFile::new_in(&tmp_dir)?;
    let orig_oid = hash_and_write(&mut combined, current_tmp.as_file_mut())?;
    let mut input_oids: Vec<Oid> = Vec::with_capacity(extensions.len());
    input_oids.push(orig_oid);

    // The last extension's output goes straight into the store.
    for (i, ext) in extensions.iter().enumerate() {
        let cmd_str = ext.command.replace("%f", path);
        let mut parts = cmd_str.split_whitespace();
        let prog = parts
            .next()
            .ok_or_else(|| CleanError::ExtensionMissingCommand {
                name: ext.name.clone(),
            })?;
        let args: Vec<&str> = parts.collect();

        let stdin_file = std::fs::File::open(current_tmp.path())?;
        let mut child = Command::new(prog)
            .args(&args)
            .stdin(stdin_file)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| CleanError::ExtensionSpawnFailed {
                name: ext.name.clone(),
                source: e,
            })?;
        let mut stdout = child.stdout.take().expect("piped stdout");

        let is_last = i + 1 == extensions.len();
        if is_last {
            let (oid, size) = store.insert(&mut stdout)?;
            let status = child.wait()?;
            if !status.success() {
                return Err(CleanError::ExtensionFailed {
                    name: ext.name.clone(),
                    status: status.code(),
                });
            }

            let pointer_extensions = build_pointer_extensions(extensions, &input_oids);
            let pointer = Pointer {
                oid,
                size,
                extensions: pointer_extensions,
                canonical: true,
            };
            output.write_all(pointer.encode().as_bytes())?;
            return Ok(CleanOutcome::Stored(pointer));
        }

        let mut next_tmp = NamedTempFile::new_in(&tmp_dir)?;
        let next_oid = hash_and_write(&mut stdout, next_tmp.as_file_mut())?;
        let status = child.wait()?;
        if !status.success() {
            return Err(CleanError::ExtensionFailed {
                name: ext.name.clone(),
                status: status.code(),
            });
        }

        current_tmp = next_tmp;
        input_oids.push(next_oid);
    }

    unreachable!("clean loop exited without storing")
}

/// Compute the pointer [`clean`] would produce for `input`, without storing
/// anything.
///
/// Runs the same extension chain and hashes the last stage's output.
/// Unlike [`clean`], it doesn't check whether the input is already a
/// pointer. `tmp_dir` holds each stage's intermediate output, for example
/// the store's [`tmp_dir`](Store::tmp_dir) or [`std::env::temp_dir`].
pub fn build_pointer_with_extensions<R: Read>(
    input: &mut R,
    path: &str,
    extensions: &[CleanExtension],
    tmp_dir: &Path,
) -> Result<Pointer, CleanError> {
    if extensions.is_empty() {
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; COPY_BUFFER];
        let mut size: u64 = 0;
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            size += n as u64;
        }
        let bytes: [u8; 32] = hasher.finalize().into();
        return Ok(Pointer::new(Oid::from_bytes(bytes), size));
    }

    for ext in extensions {
        if ext.command.trim().is_empty() {
            return Err(CleanError::ExtensionMissingCommand {
                name: ext.name.clone(),
            });
        }
    }

    std::fs::create_dir_all(tmp_dir)?;

    let mut current_tmp = NamedTempFile::new_in(tmp_dir)?;
    let orig_oid = hash_and_write(input, current_tmp.as_file_mut())?;
    let mut input_oids: Vec<Oid> = Vec::with_capacity(extensions.len());
    input_oids.push(orig_oid);

    for (i, ext) in extensions.iter().enumerate() {
        let cmd_str = ext.command.replace("%f", path);
        let mut parts = cmd_str.split_whitespace();
        let prog = parts
            .next()
            .ok_or_else(|| CleanError::ExtensionMissingCommand {
                name: ext.name.clone(),
            })?;
        let args: Vec<&str> = parts.collect();

        let stdin_file = std::fs::File::open(current_tmp.path())?;
        let mut child = Command::new(prog)
            .args(&args)
            .stdin(stdin_file)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| CleanError::ExtensionSpawnFailed {
                name: ext.name.clone(),
                source: e,
            })?;
        let mut stdout = child.stdout.take().expect("piped stdout");

        let is_last = i + 1 == extensions.len();
        if is_last {
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; COPY_BUFFER];
            let mut size: u64 = 0;
            loop {
                let n = stdout.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                size += n as u64;
            }
            let status = child.wait()?;
            if !status.success() {
                return Err(CleanError::ExtensionFailed {
                    name: ext.name.clone(),
                    status: status.code(),
                });
            }
            let bytes: [u8; 32] = hasher.finalize().into();
            return Ok(Pointer {
                oid: Oid::from_bytes(bytes),
                size,
                extensions: build_pointer_extensions(extensions, &input_oids),
                canonical: true,
            });
        }

        let mut next_tmp = NamedTempFile::new_in(tmp_dir)?;
        let next_oid = hash_and_write(&mut stdout, next_tmp.as_file_mut())?;
        let status = child.wait()?;
        if !status.success() {
            return Err(CleanError::ExtensionFailed {
                name: ext.name.clone(),
                status: status.code(),
            });
        }
        current_tmp = next_tmp;
        input_oids.push(next_oid);
    }

    unreachable!("extension chain exited without producing a pointer")
}

fn build_pointer_extensions(extensions: &[CleanExtension], input_oids: &[Oid]) -> Vec<Extension> {
    extensions
        .iter()
        .enumerate()
        .map(|(i, ext)| Extension {
            name: ext.name.clone(),
            priority: ext.priority,
            oid: input_oids[i],
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use git_lfs_pointer::VERSION_LATEST;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::new(tmp.path().join("lfs"));
        (tmp, store)
    }

    fn run(store: &Store, input: &[u8]) -> (CleanOutcome, Vec<u8>) {
        let mut out = Vec::new();
        let outcome = clean(store, &mut { input }, &mut out, "", &[]).unwrap();
        (outcome, out)
    }

    // ---------- Stored path ----------

    #[test]
    fn small_content_is_hashed_and_stored() {
        let (_t, store) = fixture();
        let (outcome, out) = run(&store, b"hello world!");
        let p = match outcome {
            CleanOutcome::Stored(p) => p,
            o => panic!("expected Stored, got {o:?}"),
        };
        assert_eq!(p.size, 12);
        assert!(store.contains(p.oid));
        assert_eq!(out, p.encode().as_bytes());
    }

    #[test]
    fn known_sha256_for_abc() {
        let (_t, store) = fixture();
        let (outcome, _) = run(&store, b"abc");
        let expected: Oid = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            .parse()
            .unwrap();
        assert_eq!(outcome.pointer().oid, expected);
    }

    #[test]
    fn pseudo_pointer_with_extra_text_is_hashed() {
        let input = b"version https://git-lfs.github.com/spec/v1\n\
                      oid sha256:7cd8be1d2cd0dd22cd9d229bb6b5785009a05e8b39d405615d882caac56562b5\n\
                      size 1024\n\
                      \n\
                      This is my test pointer.\n";
        let (_t, store) = fixture();
        let (outcome, out) = run(&store, input);
        let p = match outcome {
            CleanOutcome::Stored(p) => p,
            o => panic!("expected Stored, got {o:?}"),
        };
        assert_eq!(p.size, input.len() as u64);
        assert!(store.contains(p.oid));
        assert_eq!(out, p.encode().as_bytes());
    }

    #[test]
    fn oversized_pointer_shaped_input_is_hashed() {
        let mut input = Vec::from(
            &b"version https://git-lfs.github.com/spec/v1\n\
               oid sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\n\
               size 5\n"[..],
        );
        input.extend(std::iter::repeat_n(b'x', 2000));
        let (_t, store) = fixture();
        let (outcome, _) = run(&store, &input);
        let p = match outcome {
            CleanOutcome::Stored(p) => p,
            o => panic!("expected Stored, got {o:?}"),
        };
        assert_eq!(p.size, input.len() as u64);
        assert!(store.contains(p.oid));
    }

    #[test]
    fn streaming_megabyte_input_works() {
        let (_t, store) = fixture();
        let content: Vec<u8> = (0..1_048_576u32).map(|i| (i ^ (i >> 5)) as u8).collect();
        let (outcome, _) = run(&store, &content);
        assert_eq!(outcome.pointer().size, content.len() as u64);
        assert!(store.contains(outcome.pointer().oid));
    }

    // ---------- Passthrough path ----------

    #[test]
    fn canonical_pointer_passes_through_verbatim() {
        let (_t, store) = fixture();
        let oid_hex = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";
        let pointer_text = format!("version {VERSION_LATEST}\noid sha256:{oid_hex}\nsize 12345\n");
        let (outcome, out) = run(&store, pointer_text.as_bytes());
        match &outcome {
            CleanOutcome::Passthrough(p) => assert!(p.canonical),
            o => panic!("expected Passthrough, got {o:?}"),
        }
        assert_eq!(
            out,
            pointer_text.as_bytes(),
            "output must be input verbatim"
        );
        assert!(!store.root().join("objects").exists());
    }

    #[test]
    fn non_canonical_pointer_passes_through_verbatim() {
        // A CRLF pointer parses as non-canonical. Pass-through must keep the
        // CRLFs rather than re-encode it, or the blob hash would change.
        let (_t, store) = fixture();
        let oid_hex = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";
        let crlf = format!("version {VERSION_LATEST}\r\noid sha256:{oid_hex}\r\nsize 12345\r\n");
        let (outcome, out) = run(&store, crlf.as_bytes());
        match &outcome {
            CleanOutcome::Passthrough(p) => assert!(!p.canonical),
            o => panic!("expected Passthrough, got {o:?}"),
        }
        assert_eq!(out, crlf.as_bytes());
    }

    #[test]
    fn empty_input_is_passthrough_empty_pointer() {
        let (_t, store) = fixture();
        let (outcome, out) = run(&store, b"");
        match &outcome {
            CleanOutcome::Passthrough(p) => {
                assert_eq!(p, &Pointer::empty());
            }
            o => panic!("expected Passthrough, got {o:?}"),
        }
        assert!(out.is_empty(), "empty pointer encodes to empty bytes");
    }

    #[test]
    fn passthrough_is_idempotent() {
        let (_t, store) = fixture();
        let (_, first) = run(&store, b"some content here");
        let (outcome2, second) = run(&store, &first);
        assert!(matches!(outcome2, CleanOutcome::Passthrough(_)));
        assert_eq!(first, second);
    }

    // ---------- Extensions ----------

    /// Use `tr a-z A-Z` as the extension, checking the subprocess chain and
    /// the recorded input OID.
    #[test]
    fn single_extension_records_input_oid() {
        let (_t, store) = fixture();
        let exts = vec![CleanExtension {
            name: "upper".into(),
            priority: 0,
            command: "tr a-z A-Z".into(),
        }];

        let mut out = Vec::new();
        let outcome = clean(&store, &mut &b"abc"[..], &mut out, "foo.txt", &exts).unwrap();

        let pointer = match outcome {
            CleanOutcome::Stored(p) => p,
            o => panic!("expected Stored, got {o:?}"),
        };

        // Input was "abc" → SHA-256 well-known.
        let abc_oid: Oid = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            .parse()
            .unwrap();
        // Output was "ABC" → distinct OID.
        let upper_oid: Oid = "b5d4045c3f466fa91fe2cc6abe79232a1a57cdf104f7a26e716e0a1e2789df78"
            .parse()
            .unwrap();

        assert_eq!(pointer.extensions.len(), 1);
        assert_eq!(pointer.extensions[0].name, "upper");
        assert_eq!(pointer.extensions[0].priority, 0);
        assert_eq!(pointer.extensions[0].oid, abc_oid);
        assert_eq!(pointer.oid, upper_oid);
        assert_eq!(pointer.size, 3);
        assert!(store.contains(upper_oid));
        // Stored bytes are "ABC".
        let mut f = store.open(upper_oid).unwrap();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut bytes).unwrap();
        assert_eq!(bytes, b"ABC");
    }

    #[test]
    fn extensions_skipped_for_passthrough_pointer() {
        // If the input is already a pointer, extensions are never invoked.
        let (_t, store) = fixture();
        let oid_hex = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";
        let pointer_text = format!("version {VERSION_LATEST}\noid sha256:{oid_hex}\nsize 12345\n");
        let exts = vec![CleanExtension {
            name: "fail".into(),
            priority: 0,
            // /bin/false would be invoked if we got past the passthrough check.
            command: "false".into(),
        }];
        let mut out = Vec::new();
        let outcome = clean(&store, &mut pointer_text.as_bytes(), &mut out, "x", &exts).unwrap();
        assert!(matches!(outcome, CleanOutcome::Passthrough(_)));
        assert_eq!(out, pointer_text.as_bytes());
    }

    #[test]
    fn extension_failure_is_propagated() {
        let (_t, store) = fixture();
        let exts = vec![CleanExtension {
            name: "fail".into(),
            priority: 0,
            command: "false".into(),
        }];
        let mut out = Vec::new();
        let err = clean(&store, &mut &b"hello"[..], &mut out, "x", &exts).unwrap_err();
        assert!(
            matches!(err, CleanError::ExtensionFailed { .. }),
            "got {err:?}"
        );
    }
}
