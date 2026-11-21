//! The `lfs.fetchrecent*` and `lfs.prune*` settings.
//!
//! Reads the same keys with the same defaults as upstream's
//! `NewFetchPruneConfig` in `lfs/config.go`.

use std::path::Path;

use crate::config;

/// Settings for `git lfs fetch --recent` and `git lfs prune`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchPruneConfig {
    /// `lfs.fetchrecentrefsdays`: fetch local refs other than `HEAD` with
    /// commits this many days old or newer. Default 7, and 0 means only
    /// `HEAD`.
    pub fetch_recent_refs_days: i64,
    /// `lfs.fetchrecentremoterefs`: apply
    /// [`Self::fetch_recent_refs_days`] to the remote's refs too. Default
    /// true.
    pub fetch_recent_refs_include_remotes: bool,
    /// `lfs.fetchrecentcommitsdays`: also fetch objects from commits this
    /// many days before each ref's latest commit. Default 0, which fetches
    /// only the ref itself.
    pub fetch_recent_commits_days: i64,
    /// `lfs.fetchrecentalways`: act as if `--recent` were always passed.
    /// Default false.
    pub fetch_recent_always: bool,
    /// `lfs.pruneoffsetdays`: days added to the fetch-recent windows when
    /// pruning. Objects outside the combined window can be pruned. Default
    /// 3.
    pub prune_offset_days: i64,
    /// `lfs.pruneverifyremotealways`: check that reachable objects exist
    /// on the remote before pruning them. Default false.
    pub prune_verify_remote_always: bool,
    /// `lfs.pruneverifyunreachablealways`: when verifying, check
    /// unreachable objects too. Default false.
    pub prune_verify_unreachable_always: bool,
    /// `lfs.pruneremotetocheck`: the remote checked for unpushed objects
    /// and verified against. Default `origin`.
    pub prune_remote_name: String,
}

impl FetchPruneConfig {
    /// Read the settings with [`config::get_effective`], using the
    /// defaults for unset keys.
    ///
    /// None of these keys are allowed in `.lfsconfig`, so in practice only
    /// git config is read. An unparsable value is read as the default.
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

/// Read a boolean setting.
///
/// `true`, `yes`, `on` and `1` are true, and `false`, `no`, `off`, `0` and
/// the empty string are false, in any case. Anything else gives `default`.
/// Upstream's `config.Bool` differs: it also accepts `t` and `f`, reads an
/// empty value as `default`, and reads anything else as false.
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
