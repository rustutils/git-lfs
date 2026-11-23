//! `git lfs install` and `git lfs uninstall`: set or remove the LFS filter
//! config and Git hooks.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use git_lfs_git::{config, git_common_dir, git_dir};

/// The config file install and uninstall write to, passed to `git config`
/// as `--global`, `--system`, `--local`, `--worktree`, or `--file`.
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

    /// Whether this is the current repository's config.
    ///
    /// Hooks are always installed for these scopes, and for others only
    /// when run inside a repository.
    pub fn is_repo_scope(&self) -> bool {
        matches!(self, Self::Local | Self::Worktree)
    }

    /// What uninstall says it removed, as upstream does.
    ///
    /// Upstream says "Global" for `--file` too, which t-uninstall.sh
    /// checks, and nothing for a repository scope.
    pub fn removed_message(&self) -> Option<&'static str> {
        match self {
            Self::System => Some("System Git LFS configuration has been removed."),
            Self::Global | Self::File(_) => Some("Global Git LFS configuration has been removed."),
            Self::Local | Self::Worktree => None,
        }
    }
}

/// The filter settings install writes.
///
/// Git uses `process` when it can and falls back to `clean` and `smudge`.
/// `required` makes a failing filter an error instead of passing the
/// content through.
const FILTER_KEYS: &[(&str, &str)] = &[
    ("filter.lfs.clean", "git-lfs clean -- %f"),
    ("filter.lfs.smudge", "git-lfs smudge -- %f"),
    ("filter.lfs.process", "git-lfs filter-process"),
    ("filter.lfs.required", "true"),
];

/// The filter settings for `--skip-smudge`, which check out pointers
/// instead of content.
const FILTER_KEYS_SKIP_SMUDGE: &[(&str, &str)] = &[
    ("filter.lfs.clean", "git-lfs clean -- %f"),
    ("filter.lfs.smudge", "git-lfs smudge --skip -- %f"),
    ("filter.lfs.process", "git-lfs filter-process --skip"),
    ("filter.lfs.required", "true"),
];

/// Older values of filter setting `key` that install replaces without
/// `--force`, like upstream's `Upgradeables` in `lfs/attribute.go`.
///
/// They include the other mode's current value, so switching to or from
/// `--skip-smudge` works without `--force`.
fn upgradeables_for(key: &str, skip_smudge: bool) -> &'static [&'static str] {
    match (key, skip_smudge) {
        ("filter.lfs.clean", false) => &["git-lfs clean %f"],
        ("filter.lfs.clean", true) => &["git-lfs clean %f"],
        ("filter.lfs.smudge", false) => &[
            "git-lfs smudge %f",
            "git-lfs smudge --skip %f",
            "git-lfs smudge --skip -- %f",
        ],
        ("filter.lfs.smudge", true) => &[
            "git-lfs smudge %f",
            "git-lfs smudge --skip %f",
            "git-lfs smudge -- %f",
        ],
        ("filter.lfs.process", false) => &[
            "git-lfs filter",
            "git-lfs filter --skip",
            "git-lfs filter-process --skip",
        ],
        ("filter.lfs.process", true) => &[
            "git-lfs filter",
            "git-lfs filter --skip",
            "git-lfs filter-process",
        ],
        _ => &[],
    }
}

const HOOKS: &[&str] = &["pre-push", "post-checkout", "post-commit", "post-merge"];

/// The hook script, with `{{Command}}` standing for the hook name.
///
/// Matches upstream's exactly, so each recognizes the other's hooks.
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
    /// Don't write hooks.
    pub skip_repo: bool,
    /// Write the [`FILTER_KEYS_SKIP_SMUDGE`] settings.
    pub skip_smudge: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    /// A filter setting has a value install doesn't recognize.
    ///
    /// The message matches upstream's, which t-install.sh greps for.
    #[error("the {key:?} attribute should be {wanted:?} but is {existing:?}")]
    FilterAttribute {
        key: String,
        existing: String,
        wanted: String,
    },
    /// A hook exists with content install doesn't recognize.
    ///
    /// [`print_hook_conflict`] prints it.
    #[error("hook {hook:?} already exists with different contents")]
    HookConflict { hook: String, existing: String },
    /// A `git config` write failed.
    ///
    /// For example, `--worktree` without `extensions.worktreeConfig`, or
    /// `--local` with a read-only `.git`. Tests grep stdout for this
    /// message.
    #[error("error running 'git {}': {stderr}", args.join(" "))]
    ConfigCommandFailed { args: Vec<String>, stderr: String },
}

/// Run `git lfs install`.
///
/// Writes the filter settings, then the hooks unless `skip_repo` is set
/// or there's no repository.
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
        let current = scoped_get(cwd, &opts.scope, key)?;
        let needs_set = match current.as_deref() {
            None | Some("") => true,
            Some(v) if v == *wanted => false,
            Some(v) if opts.force => {
                let _ = v;
                true
            }
            Some(v) => {
                let upgradeables = upgradeables_for(key, opts.skip_smudge);
                if upgradeables.contains(&v) {
                    true
                } else {
                    return Err(InstallError::FilterAttribute {
                        key: (*key).into(),
                        existing: v.to_owned(),
                        wanted: (*wanted).into(),
                    });
                }
            }
        };
        if needs_set {
            scoped_set(cwd, &opts.scope, key, wanted)?;
        }
    }
    Ok(())
}

/// Read `key` from the config file `scope` names.
///
/// Returns `Ok(None)` if the key isn't set or the file can't be read.
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
        // 1 is an unset or invalid key, 2 a key without a section, and
        // 128 or 129 an unreadable scope.
        Some(1) | Some(2) | Some(128) | Some(129) => Ok(None),
        _ => Err(git_lfs_git::Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
}

/// Set `key` in the config file `scope` names.
///
/// `--replace-all` turns a key with several values into one, as
/// t-install.sh checks for `--force`.
fn scoped_set(
    cwd: &Path,
    scope: &InstallScope,
    key: &str,
    value: &str,
) -> Result<(), InstallError> {
    let scope_arg = scope.config_arg();
    let args = vec![
        "config".into(),
        scope_arg.clone(),
        "--replace-all".into(),
        key.into(),
        value.into(),
    ];
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("config")
        .arg(&scope_arg)
        .args(["--replace-all", key, value])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(InstallError::ConfigCommandFailed {
            args,
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        })
    }
}

fn scoped_unset(cwd: &Path, scope: &InstallScope, key: &str) -> Result<(), InstallError> {
    let scope_arg = scope.config_arg();
    let args = vec![
        "config".into(),
        scope_arg.clone(),
        "--unset".into(),
        key.into(),
    ];
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .arg("config")
        .arg(&scope_arg)
        .args(["--unset", key])
        .output()?;
    match out.status.code() {
        // 5 means the key wasn't set.
        Some(0) | Some(5) => Ok(()),
        _ => Err(InstallError::ConfigCommandFailed {
            args,
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        }),
    }
}

pub(crate) fn install_all_hooks(cwd: &Path, opts: &InstallOptions) -> Result<(), InstallError> {
    let hooks_dir = effective_hooks_dir(cwd)?;
    fs::create_dir_all(&hooks_dir)?;
    // Check every hook before writing any, so a conflict leaves them all
    // as they were. Upstream writes the hooks before the conflicting one.
    if !opts.force {
        for hook in HOOKS {
            let path = hooks_dir.join(hook);
            if let HookStatus::Conflict { existing } = classify_hook(&path, hook)? {
                return Err(InstallError::HookConflict {
                    hook: (*hook).into(),
                    existing,
                });
            }
        }
    }
    for hook in HOOKS {
        install_one_hook(&hooks_dir, hook, opts)?;
    }
    Ok(())
}

/// Print a hook conflict to stderr in upstream's format.
///
/// t-install.sh and t-update.sh compare it exactly.
pub fn print_hook_conflict(hook: &str, existing: &str) {
    eprintln!("Hook already exists: {hook}");
    eprintln!();
    for line in existing.lines() {
        eprintln!("\t{line}");
    }
    eprintln!();
    eprintln!("To resolve this, either:");
    eprintln!("  1: run `git lfs update --manual` for instructions on how to merge hooks.");
    eprintln!("  2: run `git lfs update --force` to overwrite your hook.");
}

/// The directory Git runs hooks from, and so where to write them.
///
/// That's `core.hooksPath` from any config scope, as in upstream's
/// `HookDir`, or else `hooks` in the common Git directory. A relative
/// `core.hooksPath` is resolved the way Git does: against the working tree
/// root, or against the Git directory in a bare repository.
pub fn effective_hooks_dir(cwd: &Path) -> Result<std::path::PathBuf, InstallError> {
    if let Ok(Some(hookspath)) = config::get_effective(cwd, "core.hookspath")
        && !hookspath.is_empty()
    {
        let hp = git_lfs_git::expand_tilde(&hookspath);
        if hp.is_absolute() {
            return Ok(hp);
        }
        let base = match git_lfs_git::work_tree_root(cwd) {
            Ok(root) => root,
            Err(_) => git_dir(cwd)?,
        };
        return Ok(base.join(hp));
    }
    Ok(git_common_dir(cwd)?.join("hooks"))
}

/// Install the hooks that are missing or ours, leaving any other hook as
/// it is.
///
/// Commands such as `clean`, `smudge`, and `track` call this on every
/// run and ignore its errors, as upstream does.
pub fn try_install_hooks(cwd: &Path) -> Result<(), InstallError> {
    // Without `core.hooksPath`, hooks from a linked worktree go in the
    // main `.git/hooks`, as t-worktree.sh checks.
    let hooks_dir = effective_hooks_dir(cwd)?;
    fs::create_dir_all(&hooks_dir)?;
    for hook in HOOKS {
        let path = hooks_dir.join(hook);
        let wanted = HOOK_TEMPLATE.replace("{{Command}}", hook);
        match classify_hook(&path, hook)? {
            HookStatus::Missing | HookStatus::Current | HookStatus::Legacy => {
                write_hook(&path, &wanted)?;
            }
            HookStatus::Conflict { .. } => {}
        }
    }
    Ok(())
}

/// What a hook file holds, compared with [`HOOK_TEMPLATE`].
#[derive(Debug)]
pub enum HookStatus {
    /// The file is missing or blank.
    Missing,
    /// The file is the current template.
    Current,
    /// The file is an older template, possibly indented.
    Legacy,
    /// The file has other content.
    Conflict { existing: String },
}

/// Classify the file at `hook_path`, the hook named `hook`.
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

/// Strip leading tabs and spaces from each line, so indented older hooks
/// still match.
fn strip_leading_indent(s: &str) -> String {
    s.lines()
        .map(|l| l.trim_start_matches(['\t', ' ']))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Older hook templates that install replaces without `--force`.
///
/// The first two are only for `pre-push`, which used to run
/// `git lfs push --stdin`.
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

/// The current hook script for `hook`.
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
        // Without `force`, `install_all_hooks` has already rejected this.
        HookStatus::Conflict { existing } if opts.force => {
            let _ = existing;
            write_hook(&path, &wanted)?;
            Ok(())
        }
        HookStatus::Conflict { existing } => Err(InstallError::HookConflict {
            hook: hook.into(),
            existing,
        }),
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
    /// Don't remove hooks.
    pub skip_repo: bool,
    /// Remove only the hooks, for `git lfs uninstall hooks`.
    pub hooks_only: bool,
}

/// Run `git lfs uninstall`.
///
/// Unsets the filter settings, then removes the hooks unless `skip_repo`
/// is set or there's no repository. Only hooks that are exactly the
/// current template are removed. Upstream also removes older templates
/// and blank hooks.
///
/// A setting that can't be unset prints a warning, and the hooks are
/// still removed, as in upstream. Returns whether the hooks were removed.
pub fn uninstall(cwd: &Path, opts: &UninstallOptions) -> Result<bool, InstallError> {
    // A config that can't be changed is a warning, and the hooks are
    // still removed, as in upstream.
    if !opts.hooks_only {
        match unset_filter_config(cwd, opts) {
            Ok(()) => {}
            Err(e @ InstallError::ConfigCommandFailed { .. }) => println!("warning: {e}"),
            Err(e) => return Err(e),
        }
    }

    let in_repo = git_dir(cwd).is_ok();
    let touch_hooks =
        opts.hooks_only || (!opts.skip_repo && (opts.scope.is_repo_scope() || in_repo));
    if touch_hooks {
        uninstall_all_hooks(cwd)?;
    }
    Ok(touch_hooks)
}

/// Unset each filter key, going on past a failure, and return the first
/// error.
fn unset_filter_config(cwd: &Path, opts: &UninstallOptions) -> Result<(), InstallError> {
    let mut first_error = None;
    for (key, _) in FILTER_KEYS {
        if let Err(e) = scoped_unset(cwd, &opts.scope, key) {
            first_error.get_or_insert(e);
        }
    }
    first_error.map_or(Ok(()), Err)
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
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(InstallError::Io(e)),
    }
}
