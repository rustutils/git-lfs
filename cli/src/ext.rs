//! `git lfs ext`: list the configured pointer extensions.
//!
//! Each extension is set by `lfs.extension.<name>.clean`,
//! `lfs.extension.<name>.smudge`, and `lfs.extension.<name>.priority`. The
//! output matches upstream's:
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

/// Print every configured extension, in priority order.
pub fn run(cwd: &Path) -> Result<(), ExtError> {
    for ext in git_lfs_git::extension::list_extensions(cwd) {
        print_ext(&ext);
    }
    Ok(())
}

/// Print the named extensions in argument order, or all of them if none
/// are named.
///
/// An unknown name prints a header with that name and empty fields.
/// Upstream prints an empty name in that header.
pub fn run_list(cwd: &Path, names: &[String]) -> Result<(), ExtError> {
    if names.is_empty() {
        return run(cwd);
    }
    let configured = git_lfs_git::extension::list_extensions(cwd);
    for name in names {
        match configured.iter().find(|e| e.name == *name) {
            Some(ext) => print_ext(ext),
            None => print_ext(&git_lfs_git::extension::ExtensionConfig {
                name: name.clone(),
                clean: String::new(),
                smudge: String::new(),
                priority: 0,
            }),
        }
    }
    Ok(())
}

fn print_ext(ext: &git_lfs_git::extension::ExtensionConfig) {
    println!("Extension: {}", ext.name);
    println!("    clean = {}", ext.clean);
    println!("    smudge = {}", ext.smudge);
    println!("    priority = {}", ext.priority);
}
