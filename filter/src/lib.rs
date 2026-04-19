//! Clean and smudge filters for git-lfs.
//!
//! See `docs/spec.md` § "Intercepting Git" for the protocol contract.

use std::io::{self, Read};

use git_lfs_pointer::{MAX_POINTER_SIZE, Pointer};

mod clean;
mod smudge;

pub use clean::{CleanOutcome, clean};
pub use smudge::{SmudgeError, SmudgeOutcome, smudge};

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
