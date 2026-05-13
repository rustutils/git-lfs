//! Clean and smudge filters and the filter-process protocol for Git LFS.
//!
//! Git invokes content filters whenever a file moves between the
//! working tree and a git blob: a *clean* filter runs on the way
//! in (`git add`) and a *smudge* filter runs on the way out
//! (`git checkout`). LFS hooks into both ends. Clean hashes the
//! working-tree bytes, hands them to the local LFS store, and
//! emits a small pointer file (which is what git ends up storing);
//! smudge takes the pointer back from git, looks up the real
//! bytes (fetching from the server if they're not local), and
//! writes the content into the working tree.
//!
//! This crate implements both filters plus the long-running
//! [filter-process protocol][filter-process], which modern git
//! uses by default: one subprocess handles many files in a single
//! session over a pkt-line-framed connection.
//!
//! Three entry points: [`clean`] runs the clean side
//! ([`CleanOutcome`] reports whether the input was an
//! already-canonical pointer that passed through verbatim or
//! content that was hashed and stored), [`smudge`] runs the
//! smudge side and errors with [`SmudgeError::ObjectMissing`]
//! when the local store doesn't have the object, and
//! [`filter_process`] is the long-running variant, multiplexing
//! many `clean` and `smudge` requests in one pkt-line session.
//!
//! Pointer extensions chain external programs between the raw
//! bytes and the stored object. [`CleanExtension`] and
//! [`SmudgeExtension`] describe one chain stage each (name,
//! priority, command); a clean run pipes the file through each
//! registered extension in priority order and records the
//! per-stage OID in the resulting pointer, and smudge undoes the
//! chain in reverse. [`build_pointer_with_extensions`] runs the
//! chain on a preview path that doesn't insert into the store,
//! used by `git lfs pointer --file=X` to show what `clean` would
//! emit.
//!
//! Two convenience variants on the smudge side:
//! [`smudge_object_to`] streams an already-parsed pointer's
//! object content to a writer (used by pull and checkout, which
//! have the pointer in hand from an index walk and don't need
//! the pointer-detection front end); [`smudge_with_fetch`]
//! fetches missing objects via a caller-supplied closure rather
//! than erroring out.
//!
//! See [`docs/spec.md`] § "Intercepting Git" for the protocol
//! contract.
//!
//! [filter-process]: https://git-scm.com/docs/gitattributes#_long_running_filter_process
//! [`docs/spec.md`]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/spec.md

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

/// Boxed error returned by the on-demand fetch closure passed to
/// [`smudge_with_fetch`] or [`filter_process`].
///
/// Kept as a boxed trait object so callers can plug in any error
/// type (HTTP failures, missing config, custom-transfer breakage, …)
/// without the filter crate needing to know about it. The typed
/// [`git_lfs_transfer::TransferError`] is the most common payload;
/// it converts via `Into` since it implements
/// `std::error::Error + Send + Sync`.
pub type FetchError = Box<dyn std::error::Error + Send + Sync>;

/// Read up to [`MAX_POINTER_SIZE`] bytes from `input` and try to parse them
/// as a pointer.
///
/// The returned `Vec` is the buffered head: callers that fall through to a
/// content path need to prepend it to whatever's still in the stream. The
/// `Option<Pointer>` is `Some` iff the head fit entirely in the buffer (i.e.
/// total input was strictly less than [`MAX_POINTER_SIZE`]) **and** parsed
/// as a valid pointer.
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
