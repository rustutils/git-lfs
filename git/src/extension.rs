//! Pointer extension config.
//!
//! Extensions run external programs on each object during clean and
//! smudge, as described in [`docs/extensions.md`][ext]. Each is configured
//! with `lfs.extension.<name>.clean`, `.smudge` and `.priority`. Only
//! `priority` can come from `.lfsconfig`.
//!
//! [ext]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/extensions.md

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// One configured extension.
///
/// Unset keys come back empty, and an unset or unparsable priority as 0,
/// as in upstream. Callers that run extensions must reject an empty
/// `clean` or `smudge` themselves.
#[derive(Debug, Clone)]
pub struct ExtensionConfig {
    pub name: String,
    /// Command for the clean filter.
    pub clean: String,
    /// Command for the smudge filter.
    pub smudge: String,
    /// Position in the chain. Lower runs first on clean.
    pub priority: i64,
}

/// Read every configured extension, sorted by priority.
///
/// Extensions with the same priority are sorted by name. Upstream rejects
/// duplicate priorities, but this function returns them.
pub fn list_extensions(cwd: &Path) -> Vec<ExtensionConfig> {
    let mut extensions: Vec<ExtensionConfig> = list_extension_names(cwd)
        .into_iter()
        .map(|name| read_extension(cwd, &name))
        .collect();
    extensions.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.name.cmp(&b.name)));
    extensions
}

/// Find the names of all configured extensions.
///
/// Looks in git's merged config and in the working tree's `.lfsconfig`,
/// but not in a `.lfsconfig` that is only in the index or `HEAD`. A name
/// found only in `.lfsconfig` is still listed, though [`list_extensions`]
/// then reads only its priority from there.
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

/// Get `<name>` from `lfs.extension.<name>.<key>`.
///
/// The name may contain dots, so this strips the known suffixes rather
/// than splitting on `.`.
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
