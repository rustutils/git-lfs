//! The clean filter: stdin → store + pointer-on-stdout.

use std::io::{Read, Write};

use git_lfs_pointer::{MAX_POINTER_SIZE, Pointer};
use git_lfs_store::{Store, StoreError};

/// Result of running the [`clean`] filter on a piece of input.
#[derive(Debug)]
pub enum CleanOutcome {
    /// Input was already a valid pointer; the original bytes were emitted
    /// to the output stream verbatim and nothing was inserted into the store.
    /// This is what makes `git lfs clean` idempotent on already-cleaned blobs.
    Passthrough(Pointer),
    /// Input was content; it was hashed and inserted into the store, and the
    /// canonical encoding of the resulting [`Pointer`] was written to the
    /// output stream.
    Stored(Pointer),
}

impl CleanOutcome {
    /// The pointer associated with this outcome (the parsed pass-through one,
    /// or the freshly-stored one).
    pub fn pointer(&self) -> &Pointer {
        match self {
            Self::Passthrough(p) | Self::Stored(p) => p,
        }
    }

    /// `true` if the input was recognized as an existing pointer.
    pub fn was_passthrough(&self) -> bool {
        matches!(self, Self::Passthrough(_))
    }
}

/// Apply the clean filter to `input`, writing the resulting pointer (or the
/// pass-through bytes) to `output`.
///
/// Algorithm (matches upstream `gitfilter_clean.go`):
/// 1. Read up to `MAX_POINTER_SIZE` bytes.
/// 2. If we hit EOF before filling the buffer **and** the bytes parse as a
///    valid pointer, emit the original bytes verbatim ([`CleanOutcome::Passthrough`]).
/// 3. Otherwise stream the buffered head + the rest of `input` into the
///    [`Store`], computing SHA-256 as we go, and emit the canonical encoding
///    of the resulting pointer ([`CleanOutcome::Stored`]).
pub fn clean<R: Read, W: Write>(
    store: &Store,
    input: &mut R,
    output: &mut W,
) -> Result<CleanOutcome, StoreError> {
    let mut head = vec![0u8; MAX_POINTER_SIZE];
    let mut filled = 0;
    while filled < head.len() {
        match input.read(&mut head[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    head.truncate(filled);

    // Buffer didn't fill ⇒ entire input is in `head` and is short enough to
    // possibly be a pointer. (At exactly MAX_POINTER_SIZE bytes the spec says
    // it can't be a pointer, so we skip parsing in that case.)
    if filled < MAX_POINTER_SIZE
        && let Ok(pointer) = Pointer::parse(&head)
    {
        output.write_all(&head)?;
        return Ok(CleanOutcome::Passthrough(pointer));
    }

    // Content path: hash `head ++ remaining input` and store it.
    let mut combined = head.as_slice().chain(input);
    let (oid, size) = store.insert(&mut combined)?;
    let pointer = Pointer::new(oid, size);
    output.write_all(pointer.encode().as_bytes())?;
    Ok(CleanOutcome::Stored(pointer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_lfs_pointer::{Oid, VERSION_LATEST};
    use tempfile::TempDir;

    fn fixture() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let store = Store::new(tmp.path().join("lfs"));
        (tmp, store)
    }

    /// Run clean and return (outcome, output_bytes).
    fn run(store: &Store, input: &[u8]) -> (CleanOutcome, Vec<u8>) {
        let mut out = Vec::new();
        let outcome = clean(store, &mut { input }, &mut out).unwrap();
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
        // Pointer-shaped header but with content after the size line. Parser
        // returns ExtraLine, so clean must hash this as content.
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
        // Starts looking like a pointer, but >= 1024 bytes total ⇒ content.
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
        assert_eq!(out, pointer_text.as_bytes(), "output must be input verbatim");
        // Critically: nothing was added to the store.
        assert!(!store.root().join("objects").exists());
    }

    #[test]
    fn non_canonical_pointer_passes_through_verbatim() {
        // CRLF pointer: parses, marked non-canonical. Pass-through must keep
        // the CRLFs, *not* re-emit the canonical (LF) encoding — otherwise the
        // git blob hash would change underneath the user.
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
        // Cleaning the output of a previous clean must yield the same bytes.
        let (_t, store) = fixture();
        let (_, first) = run(&store, b"some content here");
        // `first` is now a freshly-emitted pointer.
        let (outcome2, second) = run(&store, &first);
        assert!(matches!(outcome2, CleanOutcome::Passthrough(_)));
        assert_eq!(first, second);
    }
}
