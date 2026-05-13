//! `git lfs ext` — list configured pointer extensions.
//!
//! Pointer extensions chain external programs around each LFS object's
//! clean/smudge cycle. They're configured via three keys per extension:
//! `lfs.extension.<name>.{clean,smudge,priority}`.
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

/// Print all configured extensions (bare `git lfs ext` and `git lfs ext list`
/// with no name args).
pub fn run(cwd: &Path) -> Result<(), ExtError> {
    for ext in git_lfs_git::extension::list_extensions(cwd) {
        print_ext(&ext);
    }
    Ok(())
}

/// Print only the named extensions, in argument order. An unknown name
/// emits a header with empty fields, matching upstream's behavior of
/// indexing into the extension map and printing the zero value.
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
