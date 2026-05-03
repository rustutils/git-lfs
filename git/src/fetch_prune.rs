//! Typed view of the `lfs.fetchrecent*` and `lfs.prune*` config knobs
//! that govern fetch-recent and prune retention. Mirrors upstream's
//! `lfs/config.go::FetchPruneConfig` field-for-field so the same
//! defaults apply.

use std::path::Path;

use crate::config;

/// Configuration for fetch-recent and prune retention. Built once per
/// command via [`FetchPruneConfig::from_repo`]; pass by reference into
/// the scanners + retention logic that consumes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchPruneConfig {
    /// Days prior to current date for which (local) refs other than
    /// HEAD will be fetched with `--recent` (default 7, 0 = HEAD only).
    pub fetch_recent_refs_days: i64,
    /// Apply [`Self::fetch_recent_refs_days`] to remote-tracking refs
    /// from the fetch source as well (default true).
    pub fetch_recent_refs_include_remotes: bool,
    /// Days prior to the latest commit on each kept ref to also fetch
    /// previous LFS pre-images (default 0 = at-ref only).
    pub fetch_recent_commits_days: i64,
    /// If true, fetch acts as if `--recent` were always passed.
    pub fetch_recent_always: bool,
    /// Days added to the fetch-recent windows when computing prune
    /// retention. Data outside the combined window can be pruned
    /// (default 3).
    pub prune_offset_days: i64,
    /// Always verify with the remote before pruning reachable objects.
    pub prune_verify_remote_always: bool,
    /// When verifying, also verify unreachable objects (default false).
    pub prune_verify_unreachable_always: bool,
    /// Remote name used for unpushed checks and verify queries.
    /// Defaults to `origin` if `lfs.pruneremotetocheck` isn't set.
    pub prune_remote_name: String,
}

impl FetchPruneConfig {
    /// Read every knob from git config under `cwd`, applying upstream's
    /// defaults where unset. Reads via the effective git config (so
    /// `.lfsconfig` overlays apply).
    pub fn from_repo(cwd: &Path) -> Self {
        Self {
            fetch_recent_refs_days: get_int(cwd, "lfs.fetchrecentrefsdays", 7),
            fetch_recent_refs_include_remotes: get_bool(cwd, "lfs.fetchrecentremoterefs", true),
            fetch_recent_commits_days: get_int(cwd, "lfs.fetchrecentcommitsdays", 0),
            fetch_recent_always: get_bool(cwd, "lfs.fetchrecentalways", false),
            prune_offset_days: get_int(cwd, "lfs.pruneoffsetdays", 3),
            prune_verify_remote_always: get_bool(cwd, "lfs.pruneverifyremotealways", false),
            prune_verify_unreachable_always: get_bool(
                cwd,
                "lfs.pruneverifyunreachablealways",
                false,
            ),
            prune_remote_name: get_string(cwd, "lfs.pruneremotetocheck", "origin"),
        }
    }
}

fn get_int(cwd: &Path, key: &str, default: i64) -> i64 {
    config::get_effective(cwd, key)
        .ok()
        .flatten()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(default)
}

/// Git-style boolean parsing: `true/yes/on/1` and `false/no/off/0` are
/// recognized case-insensitively. Anything else falls back to `default`
/// — upstream errors on garbage, we silently ignore (mirrors
/// `lfs.skipdownloaderrors`-style read paths elsewhere).
fn get_bool(cwd: &Path, key: &str, default: bool) -> bool {
    let Ok(Some(raw)) = config::get_effective(cwd, key) else {
        return default;
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => true,
        "false" | "no" | "off" | "0" | "" => false,
        _ => default,
    }
}

fn get_string(cwd: &Path, key: &str, default: &str) -> String {
    let raw = config::get_effective(cwd, key).ok().flatten();
    match raw {
        Some(s) if !s.trim().is_empty() => s.trim().to_owned(),
        _ => default.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::commit_helper;

    fn set(cwd: &Path, key: &str, value: &str) {
        std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["config", key, value])
            .status()
            .unwrap();
    }

    #[test]
    fn defaults_match_upstream() {
        let tmp = commit_helper::init_repo();
        let cfg = FetchPruneConfig::from_repo(tmp.path());
        assert_eq!(cfg.fetch_recent_refs_days, 7);
        assert!(cfg.fetch_recent_refs_include_remotes);
        assert_eq!(cfg.fetch_recent_commits_days, 0);
        assert!(!cfg.fetch_recent_always);
        assert_eq!(cfg.prune_offset_days, 3);
        assert!(!cfg.prune_verify_remote_always);
        assert!(!cfg.prune_verify_unreachable_always);
        assert_eq!(cfg.prune_remote_name, "origin");
    }

    #[test]
    fn reads_overrides() {
        let tmp = commit_helper::init_repo();
        set(tmp.path(), "lfs.fetchrecentrefsdays", "14");
        set(tmp.path(), "lfs.fetchrecentcommitsdays", "30");
        set(tmp.path(), "lfs.fetchrecentremoterefs", "false");
        set(tmp.path(), "lfs.fetchrecentalways", "yes");
        set(tmp.path(), "lfs.pruneoffsetdays", "0");
        set(tmp.path(), "lfs.pruneremotetocheck", "upstream");
        let cfg = FetchPruneConfig::from_repo(tmp.path());
        assert_eq!(cfg.fetch_recent_refs_days, 14);
        assert_eq!(cfg.fetch_recent_commits_days, 30);
        assert!(!cfg.fetch_recent_refs_include_remotes);
        assert!(cfg.fetch_recent_always);
        assert_eq!(cfg.prune_offset_days, 0);
        assert_eq!(cfg.prune_remote_name, "upstream");
    }

    #[test]
    fn bool_accepts_git_styles() {
        let tmp = commit_helper::init_repo();
        for (raw, expected) in [
            ("true", true),
            ("TRUE", true),
            ("yes", true),
            ("On", true),
            ("1", true),
            ("false", false),
            ("no", false),
            ("OFF", false),
            ("0", false),
        ] {
            set(tmp.path(), "lfs.fetchrecentalways", raw);
            let cfg = FetchPruneConfig::from_repo(tmp.path());
            assert_eq!(cfg.fetch_recent_always, expected, "raw={raw:?}");
        }
    }
}
