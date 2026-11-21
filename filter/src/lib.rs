//! Clean and smudge filters and the filter-process protocol for Git LFS.
//!
//! Git runs a file's content through the clean filter when it's staged and
//! through the smudge filter when it's checked out. [`clean`] stores the
//! content in a [`Store`](git_lfs_store::Store) and writes a pointer in its
//! place. Input that already parses as a pointer is written back unchanged.
//! [`smudge`] does the reverse: it reads a pointer and writes the object's
//! content from the store. Input that isn't a pointer passes through. The
//! [spec] describes how Git LFS hooks into git.
//!
//! [`smudge`] fails with [`SmudgeError::ObjectMissing`] when the store
//! doesn't have the object. [`smudge_with_fetch`] instead calls a closure the
//! caller supplies, which downloads the object into the store and returns a
//! [`FetchError`] if it can't. [`smudge_object_to`] writes the content for a
//! pointer the caller has already parsed, for commands such as pull and
//! checkout.
//!
//! [`filter_process`] serves git's [long-running filter
//! protocol][filter-process], handling many clean and smudge requests in one
//! process. Besides the fetch closure, it takes the switch for
//! `GIT_LFS_SKIP_SMUDGE` and a path filter (such as `lfs.fetchinclude` and
//! `lfs.fetchexclude`). Skipped and filtered-out paths get the pointer back
//! unchanged.
//!
//! Pointer extensions pipe content through external programs. Clean runs each
//! [`CleanExtension`] in the order given (lowest priority first) and records
//! each stage's input OID in the pointer. Smudge runs the matching
//! [`SmudgeExtension`]s in reverse and checks each stage's output against
//! the recorded OID. [`build_pointer_with_extensions`] computes the pointer
//! without storing anything.
//!
//! [filter-process]: https://git-scm.com/docs/gitattributes#_long_running_filter_process
//! [spec]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/spec.md

use std::io::{self, Read};

use git_lfs_pointer::{MAX_POINTER_SIZE, Pointer};

mod clean;
mod filter_process;
mod smudge;

pub use clean::{CleanError, CleanExtension, CleanOutcome, build_pointer_with_extensions, clean};
pub use filter_process::{FilterProcessError, filter_process};
pub use smudge::{
    SmudgeError, SmudgeExtension, SmudgeOutcome, smudge, smudge_object_to, smudge_with_fetch,
};

/// Error returned by the fetch closure passed to [`smudge_with_fetch`] or
/// [`filter_process`].
///
/// Boxed so the caller's fetcher can return any error type that is `Send`
/// and `Sync`. Such errors convert with `?` or `Into`.
pub type FetchError = Box<dyn std::error::Error + Send + Sync>;

/// Read up to [`MAX_POINTER_SIZE`] bytes from `input` and try to parse them
/// as a pointer.
///
/// Returns the bytes read, which callers that treat the input as content
/// must prepend to the rest of the stream. The pointer is `Some` only if the
/// whole input was shorter than [`MAX_POINTER_SIZE`] and parsed.
fn detect_pointer<R: Read>(input: &mut R) -> io::Result<(Vec<u8>, Option<Pointer>)> {
    let mut head = vec![0u8; MAX_POINTER_SIZE];
    let mut filled = 0;
    while filled < head.len() {
        match input.read(&mut head[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    head.truncate(filled);

    let pointer = if filled < MAX_POINTER_SIZE {
        Pointer::parse(&head).ok()
    } else {
        None
    };
    Ok((head, pointer))
}
