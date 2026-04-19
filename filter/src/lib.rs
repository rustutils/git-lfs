//! Clean and smudge filters for git-lfs.
//!
//! See `docs/spec.md` § "Intercepting Git" for the protocol contract.

mod clean;

pub use clean::{CleanOutcome, clean};
