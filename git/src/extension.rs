//! Pointer extension config (`lfs.extension.<name>.{clean,smudge,priority}`).
//!
//! Extensions chain external programs around each LFS object's clean/smudge
//! cycle (`docs/extensions.md`). They're declared via three keys per
//! extension; `priority` is the only one that can come from `.lfsconfig`.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// One configured extension. Missing knobs come back as empty/0 to mirror
/// upstream's zero-value Extension struct — callers that *run* extensions
/// must reject empty `clean`/`smudge` themselves.
#[derive(Debug, Clone)]
pub struct ExtensionConfig {
    pub name: String,
    pub clean: String,
    pub smudge: String,
    pub priority: i64,
}

/// Discover and resolve every configured extension. Sorted ascending by
/// priority, with name as the tiebreaker so duplicate priorities (which
/// upstream errors on, but which we currently surface) at least come out
/// deterministically.
pub fn list_extensions(cwd: &Path) -> Vec<ExtensionConfig> {
    let mut extensions: Vec<ExtensionConfig> = list_extension_names(cwd)
        .into_iter()
        .map(|name| read_extension(cwd, &name))
        .collect();
    extensions.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.name.cmp(&b.name)));
    extensions
}

/// Discover extension names from any source — local/global/system git
/// config plus `.lfsconfig`. We deliberately enumerate from raw config
/// (rather than `get_effective`) because we want to find names declared
/// only in `.lfsconfig` too; the per-key resolution still goes through
/// `get_effective` so the safe-key filter is honored.
pub fn list_extension_names(cwd: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();

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

fn read_extension(cwd: &Path, name: &str) -> ExtensionConfig {
    let lookup = |suffix: &str| -> String {
        crate::config::get_effective(cwd, &format!("lfs.extension.{name}.{suffix}"))
            .ok()
            .flatten()
            .unwrap_or_default()
    };
    let clean = lookup("clean");
    let smudge = lookup("smudge");
    let priority = lookup("priority").parse::<i64>().unwrap_or(0);
    ExtensionConfig {
        name: name.to_owned(),
        clean,
        smudge,
        priority,
    }
}
