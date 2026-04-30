//! `git lfs ext` — list configured pointer extensions.
//!
//! Pointer extensions chain external programs around each LFS object's
//! clean/smudge cycle. They're configured via three keys per extension:
//! `lfs.extension.<name>.{clean,smudge,priority}`. The clean side runs
//! these (see `git_lfs_filter::clean_with_extensions`); smudge support
//! is still pending — see NOTES.md.
//!
//! Output format mirrors upstream byte-for-byte:
//! ```text
//! Extension: env-test
//!     clean = env-test-clean
//!     smudge = env-test-smudge
//!     priority = 0
//! ```

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ExtError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn run(cwd: &Path) -> Result<(), ExtError> {
    for ext in git_lfs_git::list_extensions(cwd) {
        println!("Extension: {}", ext.name);
        println!("    clean = {}", ext.clean);
        println!("    smudge = {}", ext.smudge);
        println!("    priority = {}", ext.priority);
    }
    Ok(())
}
