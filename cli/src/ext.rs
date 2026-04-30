//! `git lfs ext` — list configured pointer extensions.
//!
//! Pointer extensions chain external programs around each LFS object's
//! clean/smudge cycle. They're configured via three keys per extension:
//! `lfs.extension.<name>.{clean,smudge,priority}`. We don't run them
//! yet (NOTES.md / `filter::ExtensionsUnsupported`), but the test suite
//! and tooling around `git lfs ext` exists independently of execution.
//!
//! Output format mirrors upstream byte-for-byte:
//! ```text
//! Extension: env-test
//!     clean = env-test-clean
//!     smudge = env-test-smudge
//!     priority = 0
//! ```
//! Extensions sort ascending by priority. Per the safe-key filter, a
//! `.lfsconfig` may contribute `priority` but never `clean`/`smudge`
//! (those are command-execution surfaces and are stripped from
//! `.lfsconfig` reads — see `git_lfs_git::config`).

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum ExtError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
struct Extension {
    name: String,
    clean: String,
    smudge: String,
    priority: i64,
}

pub fn run(cwd: &Path) -> Result<(), ExtError> {
    let mut extensions: Vec<Extension> = list_extension_names(cwd)
        .into_iter()
        .map(|name| read_extension(cwd, &name))
        .collect();
    // Stable ascending sort by priority, with name as tiebreaker so
    // duplicate priorities (which upstream errors on, but which we
    // currently surface) at least come out deterministically.
    extensions.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.name.cmp(&b.name)));

    for ext in &extensions {
        println!("Extension: {}", ext.name);
        println!("    clean = {}", ext.clean);
        println!("    smudge = {}", ext.smudge);
        println!("    priority = {}", ext.priority);
    }
    Ok(())
}

/// Discover extension names from any source — local/global/system git
/// config plus `.lfsconfig`. We deliberately enumerate from raw config
/// (rather than `get_effective`) because we want to find names declared
/// only in `.lfsconfig` too; the per-key resolution still goes through
/// `get_effective` so the safe-key filter is honored.
fn list_extension_names(cwd: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();

    // From the live git config (all standard scopes).
    if let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "config",
            "--name-only",
            "--get-regexp",
            r"^lfs\.extension\..*\.(clean|smudge|priority)$",
        ])
        .output()
        && out.status.success()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(name) = extension_name_from_key(line) {
                names.insert(name);
            }
        }
    }

    // From `.lfsconfig` directly. We walk the file rather than going
    // through `get_effective` so a `.lfsconfig`-only extension (just
    // `priority`, no clean/smudge) still shows up — matches upstream's
    // "list every name that contributed any key" behavior.
    if let Some(root) = repo_root(cwd) {
        let lfsconfig = root.join(".lfsconfig");
        if lfsconfig.is_file()
            && let Ok(out) = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args([
                    "config",
                    "--file=.lfsconfig",
                    "--name-only",
                    "--get-regexp",
                    r"^lfs\.extension\..*\.(clean|smudge|priority)$",
                ])
                .output()
            && out.status.success()
        {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(name) = extension_name_from_key(line) {
                    names.insert(name);
                }
            }
        }
    }

    names
}

/// Resolve the work-tree root (where `.lfsconfig` lives), via `git
/// rev-parse --show-toplevel`. Returns `None` for bare repos and
/// non-repos.
fn repo_root(cwd: &Path) -> Option<std::path::PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(s))
    }
}

/// Pull the `<name>` out of `lfs.extension.<name>.<prop>`. The middle
/// component can contain dots (rare for extensions, but defensible),
/// so split on the suffix not on `.`.
fn extension_name_from_key(key: &str) -> Option<String> {
    let rest = key.strip_prefix("lfs.extension.")?;
    for suffix in [".clean", ".smudge", ".priority"] {
        if let Some(name) = rest.strip_suffix(suffix) {
            return Some(name.to_owned());
        }
    }
    None
}

/// Resolve one extension's three knobs through the safe-key-aware
/// effective config. Missing values come back as empty / 0 to match
/// upstream's zero-value Extension struct.
fn read_extension(cwd: &Path, name: &str) -> Extension {
    let lookup = |suffix: &str| -> String {
        git_lfs_git::config::get_effective(cwd, &format!("lfs.extension.{name}.{suffix}"))
            .ok()
            .flatten()
            .unwrap_or_default()
    };
    let clean = lookup("clean");
    let smudge = lookup("smudge");
    let priority = lookup("priority").parse::<i64>().unwrap_or(0);
    Extension {
        name: name.to_owned(),
        clean,
        smudge,
        priority,
    }
}
