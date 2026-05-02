//! `git lfs install` / `uninstall`: register or remove the LFS filter
//! configuration and git hooks.

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

/// `--skip-smudge` variant: tell git to invoke smudge/process with `--skip`
/// so pointer text passes through untouched. Use with a follow-up
/// `git lfs pull` to download content on demand.
const FILTER_KEYS_SKIP_SMUDGE: &[(&str, &str)] = &[
    ("filter.lfs.clean", "git-lfs clean -- %f"),
    ("filter.lfs.smudge", "git-lfs smudge --skip -- %f"),
    ("filter.lfs.process", "git-lfs filter-process --skip"),
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
    /// Configure smudge/process with `--skip` so pointer text passes
    /// through. `git lfs pull` is the recovery path.
    pub skip_smudge: bool,
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
    let keys = if opts.skip_smudge {
        FILTER_KEYS_SKIP_SMUDGE
    } else {
        FILTER_KEYS
    };
    for (key, wanted) in keys {
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

pub(crate) fn install_all_hooks(cwd: &Path, opts: &InstallOptions) -> Result<(), InstallError> {
    let hooks_dir = effective_hooks_dir(cwd)?;
    fs::create_dir_all(&hooks_dir)?;
    for hook in HOOKS {
        install_one_hook(&hooks_dir, hook, opts)?;
    }
    Ok(())
}

/// Resolve the hooks directory git would actually invoke. Honors
/// `core.hookspath` (relative paths resolve against the working tree
/// root, or the git dir for bare repos), falling back to
/// `<git-dir>/hooks` when unset.
pub fn effective_hooks_dir(cwd: &Path) -> Result<std::path::PathBuf, InstallError> {
    let git_dir = git_dir(cwd)?;
    if let Ok(Some(hookspath)) = config::get(cwd, ConfigScope::Local, "core.hookspath")
        && !hookspath.is_empty()
    {
        let hp = Path::new(&hookspath);
        if hp.is_absolute() {
            return Ok(hp.to_path_buf());
        }
        // Relative paths anchor on the working-tree root for non-bare
        // repos (where git_dir is `<work>/.git`), or on the git dir
        // itself for bare repos.
        let base = git_dir.parent().unwrap_or(&git_dir);
        return Ok(base.join(hp));
    }
    Ok(git_dir.join("hooks"))
}

/// Best-effort hook installer used by `git lfs track`'s auto-install
/// pathway: writes any of our four hooks that don't already exist (or
/// already match our template), silently *skips* hooks that exist with
/// user-edited contents. Never errors on conflict — track shouldn't
/// fail because someone has a custom pre-push hook.
pub fn try_install_hooks(cwd: &Path) -> Result<(), InstallError> {
    let hooks_dir = git_dir(cwd)?.join("hooks");
    fs::create_dir_all(&hooks_dir)?;
    for hook in HOOKS {
        let path = hooks_dir.join(hook);
        let wanted = HOOK_TEMPLATE.replace("{{Command}}", hook);
        match classify_hook(&path, hook)? {
            HookStatus::Missing | HookStatus::Current | HookStatus::Legacy => {
                write_hook(&path, &wanted)?;
            }
            HookStatus::Conflict { .. } => {
                // User-edited hook — leave it alone.
            }
        }
    }
    Ok(())
}

/// Outcome of inspecting an existing hook file relative to our current
/// template. Drives both the install/update writer and the
/// `git lfs update`-side conflict UI.
#[derive(Debug)]
pub enum HookStatus {
    /// File doesn't exist or is empty — safe to write.
    Missing,
    /// File matches our current template — already installed.
    Current,
    /// File matches a previously-shipped template (or a leading-
    /// whitespace variant of one) — replace with the current version.
    Legacy,
    /// File has user-edited content. Carries the existing text so the
    /// caller can render it in a conflict message.
    Conflict { existing: String },
}

/// Inspect `<hooks_dir>/<hook>` and classify it.
pub fn classify_hook(hook_path: &Path, hook: &str) -> io::Result<HookStatus> {
    let existing = match fs::read_to_string(hook_path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HookStatus::Missing),
        Err(e) => return Err(e),
    };
    if existing.trim().is_empty() {
        return Ok(HookStatus::Missing);
    }
    let wanted = HOOK_TEMPLATE.replace("{{Command}}", hook);
    if existing.trim() == wanted.trim() {
        return Ok(HookStatus::Current);
    }
    let normalized = strip_leading_indent(&existing);
    if legacy_templates(hook)
        .iter()
        .any(|t| normalized.trim() == t.trim())
    {
        return Ok(HookStatus::Legacy);
    }
    Ok(HookStatus::Conflict { existing })
}

/// Strip leading tabs/spaces from each line. Lets us recognize the
/// pre-2.6 hook format that indents the body with one TAB (test
/// "update with leading spaces" exercises this).
fn strip_leading_indent(s: &str) -> String {
    s.lines()
        .map(|l| l.trim_start_matches(['\t', ' ']))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Previously-shipped hook templates we recognize as ours and silently
/// upgrade to the current [`HOOK_TEMPLATE`]. Each returned string is
/// compared verbatim against the existing hook (after leading-whitespace
/// normalization). The first three are pre-push-only — older versions
/// shipped a `git lfs push --stdin` invocation specific to that hook.
fn legacy_templates(hook: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if hook == "pre-push" {
        out.push("#!/bin/sh\ngit lfs push --stdin $*".into());
        out.push("#!/bin/sh\ngit lfs push --stdin \"$@\"".into());
    }
    out.push(format!("#!/bin/sh\ngit lfs {hook} \"$@\""));
    out.push(format!(
        "#!/bin/sh\n\
         command -v git-lfs >/dev/null 2>&1 || \
         {{ echo >&2 \"\\nThis repository has been set up with Git LFS but Git LFS is not installed.\\n\"; exit 0; }}\n\
         git lfs {hook} \"$@\""
    ));
    out.push(format!(
        "#!/bin/sh\n\
         command -v git-lfs >/dev/null 2>&1 || \
         {{ echo >&2 \"\\nThis repository has been set up with Git LFS but Git LFS is not installed.\\n\"; exit 2; }}\n\
         git lfs {hook} \"$@\""
    ));
    out.push(format!(
        "#!/bin/sh\n\
         command -v git-lfs >/dev/null 2>&1 || \
         {{ echo >&2 \"\\nThis repository is configured for Git LFS but 'git-lfs' was not found on your path. \
         If you no longer wish to use Git LFS, remove this hook by deleting '.git/hooks/{hook}'.\\n\"; exit 2; }}\n\
         git lfs {hook} \"$@\""
    ));
    out.push(format!(
        "#!/bin/sh\n\
         command -v git-lfs >/dev/null 2>&1 || \
         {{ echo >&2 \"\\nThis repository is configured for Git LFS but 'git-lfs' was not found on your path. \
         If you no longer wish to use Git LFS, remove this hook by deleting the '{hook}' file in the hooks directory \
         (set by 'core.hookspath'; usually '.git/hooks').\\n\"; exit 2; }}\n\
         git lfs {hook} \"$@\""
    ));
    out
}

/// The current template for `hook`, used both as the on-disk content
/// and as the body printed by `git lfs update --manual`.
pub fn current_template(hook: &str) -> String {
    HOOK_TEMPLATE.replace("{{Command}}", hook)
}

fn install_one_hook(
    hooks_dir: &Path,
    hook: &str,
    opts: &InstallOptions,
) -> Result<(), InstallError> {
    let path = hooks_dir.join(hook);
    let wanted = HOOK_TEMPLATE.replace("{{Command}}", hook);
    match classify_hook(&path, hook)? {
        HookStatus::Current => Ok(()),
        HookStatus::Missing | HookStatus::Legacy => {
            write_hook(&path, &wanted)?;
            Ok(())
        }
        HookStatus::Conflict { .. } if opts.force => {
            write_hook(&path, &wanted)?;
            Ok(())
        }
        HookStatus::Conflict { .. } => Err(InstallError::HookConflict { hook: hook.into() }),
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

#[derive(Debug, Clone)]
pub struct UninstallOptions {
    pub scope: ConfigScope,
    /// Skip removing hooks; only unset the config.
    pub skip_repo: bool,
}

/// Run the uninstall. Mirrors [`install`]: clears the four `filter.lfs.*`
/// config keys and (unless `skip_repo`) removes the LFS hooks. Hooks are
/// only deleted if their contents match what we'd write — a user-edited
/// hook is left in place so we don't blow away local customizations.
pub fn uninstall(cwd: &Path, opts: &UninstallOptions) -> Result<(), InstallError> {
    unset_filter_config(cwd, opts)?;

    let in_repo = git_dir(cwd).is_ok();
    let touch_hooks = !opts.skip_repo && (opts.scope == ConfigScope::Local || in_repo);
    if touch_hooks {
        uninstall_all_hooks(cwd)?;
    }
    Ok(())
}

fn unset_filter_config(cwd: &Path, opts: &UninstallOptions) -> Result<(), InstallError> {
    for (key, _) in FILTER_KEYS {
        config::unset(cwd, opts.scope, key)?;
    }
    Ok(())
}

fn uninstall_all_hooks(cwd: &Path) -> Result<(), InstallError> {
    let hooks_dir = git_dir(cwd)?.join("hooks");
    for hook in HOOKS {
        uninstall_one_hook(&hooks_dir, hook)?;
    }
    Ok(())
}

fn uninstall_one_hook(hooks_dir: &Path, hook: &str) -> Result<(), InstallError> {
    let path = hooks_dir.join(hook);
    let wanted = HOOK_TEMPLATE.replace("{{Command}}", hook);
    match fs::read_to_string(&path) {
        Ok(existing) if existing.trim() == wanted.trim() => {
            fs::remove_file(&path)?;
            Ok(())
        }
        Ok(_) => {
            // Hook exists but isn't ours — leave it alone.
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(InstallError::Io(e)),
    }
}
