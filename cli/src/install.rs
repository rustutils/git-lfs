//! `git lfs install`: register the LFS filter with git and install the hooks.

use std::fs;
use std::io;
use std::path::Path;

use git_lfs_git::{ConfigScope, config, git_dir};

/// `filter.lfs.<key>` settings written by install. Order is intentional —
/// `process` is what git actually uses when filter-process is available;
/// `clean`/`smudge` are the per-invocation fallback; `required = true`
/// means git refuses to commit if the filter fails (the safe default).
const FILTER_KEYS: &[(&str, &str)] = &[
    ("filter.lfs.clean", "git-lfs clean -- %f"),
    ("filter.lfs.smudge", "git-lfs smudge -- %f"),
    ("filter.lfs.process", "git-lfs filter-process"),
    ("filter.lfs.required", "true"),
];

const HOOKS: &[&str] = &["pre-push", "post-checkout", "post-commit", "post-merge"];

/// Hook script template. `{{Command}}` is replaced with the hook type at
/// install time. The PATH check matches the upstream wording verbatim so
/// users see the same error if `git-lfs` later disappears from PATH.
const HOOK_TEMPLATE: &str = "\
#!/bin/sh
command -v git-lfs >/dev/null 2>&1 || { printf >&2 \"\\n%s\\n\\n\" \
\"This repository is configured for Git LFS but 'git-lfs' was not found on your path. \
If you no longer wish to use Git LFS, remove this hook by deleting the '{{Command}}' file \
in the hooks directory (set by 'core.hookspath'; usually '.git/hooks').\"; exit 2; }
git lfs {{Command}} \"$@\"
";

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub scope: ConfigScope,
    pub force: bool,
    /// Skip writing hooks; only set the config.
    pub skip_repo: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(
        "config {key:?} is set to {existing:?} (would set {wanted:?}); \
         rerun with --force to overwrite"
    )]
    ConfigConflict {
        key: String,
        existing: String,
        wanted: String,
    },
    #[error(
        "hook {hook:?} already exists with different contents; \
         rerun with --force to overwrite"
    )]
    HookConflict { hook: String },
}

/// Run the install. With `scope = Local` (or when `cwd` is inside a repo and
/// `skip_repo` is false), this also writes the four LFS git hooks.
pub fn install(cwd: &Path, opts: &InstallOptions) -> Result<(), InstallError> {
    set_filter_config(cwd, opts)?;

    let in_repo = git_dir(cwd).is_ok();
    let install_hooks_too = !opts.skip_repo && (opts.scope == ConfigScope::Local || in_repo);
    if install_hooks_too {
        install_all_hooks(cwd, opts)?;
    }
    Ok(())
}

fn set_filter_config(cwd: &Path, opts: &InstallOptions) -> Result<(), InstallError> {
    for (key, wanted) in FILTER_KEYS {
        match config::get(cwd, opts.scope, key)? {
            Some(v) if v == *wanted => continue,
            Some(v) if !opts.force => {
                return Err(InstallError::ConfigConflict {
                    key: (*key).into(),
                    existing: v,
                    wanted: (*wanted).into(),
                });
            }
            _ => config::set(cwd, opts.scope, key, wanted)?,
        }
    }
    Ok(())
}

fn install_all_hooks(cwd: &Path, opts: &InstallOptions) -> Result<(), InstallError> {
    let hooks_dir = git_dir(cwd)?.join("hooks");
    fs::create_dir_all(&hooks_dir)?;
    for hook in HOOKS {
        install_one_hook(&hooks_dir, hook, opts)?;
    }
    Ok(())
}

fn install_one_hook(hooks_dir: &Path, hook: &str, opts: &InstallOptions) -> Result<(), InstallError> {
    let path = hooks_dir.join(hook);
    let wanted = HOOK_TEMPLATE.replace("{{Command}}", hook);

    match fs::read_to_string(&path) {
        Ok(existing) => {
            if existing.trim() == wanted.trim() {
                // Already installed at the current version.
                return Ok(());
            }
            if existing.trim().is_empty() || opts.force {
                write_hook(&path, &wanted)?;
                return Ok(());
            }
            Err(InstallError::HookConflict { hook: hook.into() })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            write_hook(&path, &wanted)?;
            Ok(())
        }
        Err(e) => Err(InstallError::Io(e)),
    }
}

fn write_hook(path: &Path, content: &str) -> io::Result<()> {
    fs::write(path, content)?;
    set_executable(path)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}
