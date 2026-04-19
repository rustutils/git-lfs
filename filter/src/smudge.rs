//! The smudge filter: pointer-on-stdin → content-on-stdout.

use std::io::{self, Read, Write};

use git_lfs_pointer::{Oid, Pointer};
use git_lfs_store::Store;

use crate::detect_pointer;

/// Result of running the [`smudge`] filter on a piece of input.
#[derive(Debug)]
pub enum SmudgeOutcome {
    /// Input wasn't a pointer (or was malformed) and was emitted to the
    /// output stream verbatim. This matches upstream's "smudge with invalid
    /// pointer" behavior — git wraps everything through the filter, and
    /// non-LFS content has to come out unchanged.
    Passthrough,
    /// Input was a pointer; its content was streamed from the store to the
    /// output (or it was the empty pointer, which writes nothing).
    Resolved(Pointer),
}

#[derive(Debug, thiserror::Error)]
pub enum SmudgeError {
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The pointer references an object that isn't in the local store.
    /// Once `git-lfs-transfer` lands, this is the trigger to download.
    #[error("object {oid} (size {size}) is not present in the local store")]
    ObjectMissing { oid: Oid, size: u64 },
    /// Pointer extensions aren't supported yet.
    #[error("pointer extensions are not yet supported")]
    ExtensionsUnsupported,
}

/// Apply the smudge filter to `input`, writing the working-tree content
/// (or pass-through bytes) to `output`.
///
/// 1. If `input` parses as a pointer, look the OID up in the store and
///    stream the bytes out. Empty pointer → write nothing.
/// 2. If `input` doesn't parse as a pointer, pass it through verbatim
///    (head buffer + remaining stream).
pub fn smudge<R: Read, W: Write>(
    store: &Store,
    input: &mut R,
    output: &mut W,
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

    if !pointer.extensions.is_empty() {
        return Err(SmudgeError::ExtensionsUnsupported);
    }

    // Treat any size mismatch as "missing": same OID + different size means
    // a corrupt or partial local copy, and the recovery path (once transfer
    // lands) is the same as a real miss — re-download.
    if !store.contains_with_size(pointer.oid, pointer.size) {
        return Err(SmudgeError::ObjectMissing {
            oid: pointer.oid,
            size: pointer.size,
        });
    }

    let mut file = store.open(pointer.oid)?;
    io::copy(&mut file, output)?;
    Ok(SmudgeOutcome::Resolved(pointer))
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
        let outcome = smudge(store, &mut { input }, &mut out);
        (outcome, out)
    }

    /// Insert content via the clean filter and return the resulting pointer text.
    fn clean_into(store: &Store, content: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        clean(store, &mut { content }, &mut out).unwrap();
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
            smudge(&store, &mut { &pointer_text[..] }, &mut out).unwrap();
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
            SmudgeError::ObjectMissing { oid, size } => {
                assert_eq!(oid.to_string(), unknown_oid);
                assert_eq!(size, 5);
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
        assert!(matches!(
            outcome.unwrap_err(),
            SmudgeError::ObjectMissing { size: 99, .. }
        ));
    }

    #[test]
    fn extensions_are_not_yet_supported() {
        let (_t, store) = fixture();
        let oid_hex = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";
        let ext_oid = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        let pointer_text = format!(
            "version {VERSION_LATEST}\n\
             ext-0-foo sha256:{ext_oid}\n\
             oid sha256:{oid_hex}\n\
             size 12345\n",
        );
        let (outcome, _) = run(&store, pointer_text.as_bytes());
        assert!(matches!(
            outcome.unwrap_err(),
            SmudgeError::ExtensionsUnsupported
        ));
    }
}
