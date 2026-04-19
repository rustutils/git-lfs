//! Clean and smudge filters for git-lfs.
//!
//! See `docs/spec.md` § "Intercepting Git" for the protocol contract.

use std::io::{self, Read};

use git_lfs_pointer::{MAX_POINTER_SIZE, Pointer};

mod clean;
mod filter_process;
mod smudge;

pub use clean::{CleanOutcome, clean};
pub use filter_process::{FilterProcessError, filter_process};
pub use smudge::{SmudgeError, SmudgeOutcome, smudge, smudge_with_fetch};

/// Boxed error returned by the on-demand fetch closure passed to
/// [`smudge_with_fetch`] / [`filter_process`].
///
/// Kept as a boxed trait object so callers can plug in any error type
/// (HTTP failures, missing config, custom-transfer breakage, …) without
/// the filter crate needing to know about it. The typed
/// [`git_lfs_transfer::TransferError`] is the most common payload — it
/// converts via `Into` since it implements `std::error::Error + Send + Sync`.
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
