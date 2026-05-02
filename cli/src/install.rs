//! `git lfs install` / `uninstall`: register or remove the LFS filter
//! configuration and git hooks.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use git_lfs_git::{ConfigScope, config, git_dir};

/// Which config file the install/uninstall write goes to. Maps to the
/// scope flag git config takes; `File(path)` becomes `--file=<path>`,
/// the rest are the obvious mappings.
///
/// `Global` and `System` and `File(_)` are "scoped outside the repo" —
/// the success message says "Global Git LFS configuration has been
/// removed." (per t-uninstall test 10's `--file` assertion). `Local`
/// and `Worktree` are per-repo and stay quieter.
#[derive(Debug, Clone)]
pub enum InstallScope {
    Global,
    System,
    Local,
    Worktree,
    File(PathBuf),
}

impl InstallScope {
    fn config_arg(&self) -> String {
        match self {
            Self::Global => "--global".into(),
            Self::System => "--system".into(),
            Self::Local => "--local".into(),
            Self::Worktree => "--worktree".into(),
            Self::File(p) => format!("--file={}", p.display()),
        }
    }

    /// `Local` / `Worktree` are per-repo; everything else operates on
    /// a config file outside the repo and shouldn't try to touch hooks.
    pub fn is_repo_scope(&self) -> bool {
        matches!(self, Self::Local | Self::Worktree)
    }

    /// Whether the success message should say "Global ..." (vs the
    /// quieter "Local ..." used for per-repo scopes). Mirrors
    /// upstream's distinction — `--file` is treated as global-like
    /// since it usually points at `$XDG_CONFIG_HOME/git/config`.
    pub fn announces_global(&self) -> bool {
        matches!(self, Self::Global | Self::System | Self::File(_))
    }
}

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
    pub scope: InstallScope,
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

/// Run the install. Writes the four LFS git hooks when in a repo and
/// `skip_repo` is false; the scope governs which config file gets the
/// `filter.lfs.*` writes.
pub fn install(cwd: &Path, opts: &InstallOptions) -> Result<(), InstallError> {
    set_filter_config(cwd, opts)?;

    let in_repo = git_dir(cwd).is_ok();
    let install_hooks_too = !opts.skip_repo && (opts.scope.is_repo_scope() || in_repo);
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
        match scoped_get(cwd, &opts.scope, key)? {
            Some(v) if v == *wanted => continue,
            Some(v) if !opts.force => {
                return Err(InstallError::ConfigConflict {
                    key: (*key).into(),
                    existing: v,
                    wanted: (*wanted).into(),
                });
            }
            _ => scoped_set(cwd, &opts.scope, key, wanted)?,
        }
    }
    Ok(())
}

/// `git config <scope> --get <key>` for one of the install scopes,
/// including `--file=<path>`. Returns `Ok(None)` when the key isn't
/// set or when the scope is unreachable (matches `git_lfs_git::config`'s
/// permissive interpretation of `git config` exit codes).
fn scoped_get(
    cwd: &Path,
    scope: &InstallScope,
    key: &str,
) -> Result<Option<String>, git_lfs_git::Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--includes"])
        .arg(scope.config_arg())
        .args(["--get", key])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
        Some(1) | Some(128) | Some(129) => Ok(None),
        _ => Err(git_lfs_git::Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
}

fn scoped_set(
    cwd: &Path,
    scope: &InstallScope,
    key: &str,
    value: &str,
) -> Result<(), git_lfs_git::Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("config")
        .arg(scope.config_arg())
        .args([key, value])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(git_lfs_git::Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ))
    }
}

fn scoped_unset(cwd: &Path, scope: &InstallScope, key: &str) -> Result<(), git_lfs_git::Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("config")
        .arg(scope.config_arg())
        .args(["--unset", key])
        .output()?;
    match out.status.code() {
        // 0 = unset; 5 = key wasn't there; 128 = scope unreachable
        // (no repo for --local outside one). The latter is the caller's
        // problem to detect upstream — we just need to be idempotent
        // here so a redundant unset is harmless.
        Some(0) | Some(5) | Some(128) => Ok(()),
        _ => Err(git_lfs_git::Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
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
    pub scope: InstallScope,
    /// Skip removing hooks; only unset the config.
    pub skip_repo: bool,
    /// Inverse of `skip_repo`: remove only the hooks and leave the
    /// `filter.lfs.*` config alone. Set by `git lfs uninstall hooks`.
    pub hooks_only: bool,
}

/// Run the uninstall. Mirrors [`install`]: clears the four `filter.lfs.*`
/// config keys and (unless `skip_repo`) removes the LFS hooks. Hooks are
/// only deleted if their contents match what we'd write — a user-edited
/// hook is left in place so we don't blow away local customizations.
pub fn uninstall(cwd: &Path, opts: &UninstallOptions) -> Result<(), InstallError> {
    if !opts.hooks_only {
        unset_filter_config(cwd, opts)?;
    }

    let in_repo = git_dir(cwd).is_ok();
    let touch_hooks =
        opts.hooks_only || (!opts.skip_repo && (opts.scope.is_repo_scope() || in_repo));
    if touch_hooks {
        uninstall_all_hooks(cwd)?;
    }
    Ok(())
}

fn unset_filter_config(cwd: &Path, opts: &UninstallOptions) -> Result<(), InstallError> {
    for (key, _) in FILTER_KEYS {
        scoped_unset(cwd, &opts.scope, key)?;
    }
    Ok(())
}

fn uninstall_all_hooks(cwd: &Path) -> Result<(), InstallError> {
    let hooks_dir = effective_hooks_dir(cwd)?;
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
