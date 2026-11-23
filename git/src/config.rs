//! Read and write git config, including the repository's `.lfsconfig`.
//!
//! [`get`], [`set`] and [`unset`] act on one [`ConfigScope`].
//! [`get_effective`] is what most callers want: git's merged config, then
//! `.lfsconfig` as a fallback. Only a fixed set of keys is read from
//! `.lfsconfig`, because it comes from the repository and can't be trusted
//! with settings that run commands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::Error;

/// Which config file `git config` operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    /// `~/.gitconfig` (or `~/.config/git/config`). The default for upstream
    /// `git lfs install`.
    Global,
    /// The current repository's `.git/config`.
    Local,
    /// `/etc/gitconfig`. Usually requires root.
    System,
}

impl ConfigScope {
    fn flag(self) -> &'static str {
        match self {
            Self::Global => "--global",
            Self::Local => "--local",
            Self::System => "--system",
        }
    }
}

/// Read a single config value from the given scope.
///
/// Returns `Ok(None)` if the key isn't set, and also if the scope can't be
/// read: `git config --local` exits 128 outside a repository, any scope
/// exits 128 when a variable like `GIT_WORK_TREE` points at a missing path,
/// and git exits 129 when `GIT_CONFIG` is set alongside a scope flag. This
/// matches upstream, which doesn't tell an unset key from an unreadable
/// scope either.
pub fn get(cwd: &Path, scope: ConfigScope, key: &str) -> Result<Option<String>, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--includes", scope.flag(), "--get", key])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
        // 1 is an unset key, 128 and 129 an unreadable scope.
        Some(1) | Some(128) | Some(129) => Ok(None),
        _ => Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
}

/// Read a single config value from git's merged view of all scopes.
///
/// Git applies its own precedence and `include.path` handling. Returns
/// `Ok(None)` if the key isn't set or the config can't be read (exit 128).
fn get_any_scope(cwd: &Path, key: &str) -> Result<Option<String>, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--includes", "--get", key])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
        Some(1) | Some(128) => Ok(None),
        _ => Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
}

/// Read a single config value from `file`, relative to `cwd`.
///
/// Returns `Ok(None)` if the file doesn't exist or the key isn't set. No
/// key filtering is applied, unlike [`get_from_lfsconfig`].
pub fn get_from_file(cwd: &Path, file: &Path, key: &str) -> Result<Option<String>, Error> {
    if !cwd.join(file).is_file() {
        // `git config --file` fails on a missing file.
        return Ok(None);
    }
    let file_arg = format!("--file={}", file.display());
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--includes", &file_arg, "--get", key])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
        Some(1) => Ok(None),
        _ => Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
}

/// Look up `key` in git's merged config, falling back to `.lfsconfig`.
///
/// Anything set in git config wins over `.lfsconfig`, as in upstream.
/// See [`get_from_lfsconfig`] for which keys `.lfsconfig` may set.
pub fn get_effective(cwd: &Path, key: &str) -> Result<Option<String>, Error> {
    if let Some(v) = get_any_scope(cwd, key)? {
        return Ok(Some(v));
    }
    get_from_lfsconfig(cwd, key)
}

/// Read a boolean setting or environment variable, like upstream's
/// `config.Bool`.
///
/// Unset or empty gives `default`. `true`, `1`, `on`, `yes`, and `t` are
/// true, and `false`, `0`, `off`, `no`, and `f` are false, in any case.
/// Anything else is false, even when `default` is true.
pub fn parse_bool(value: Option<&str>, default: bool) -> bool {
    match value.map(str::to_ascii_lowercase).as_deref() {
        None | Some("") => default,
        Some("true" | "1" | "on" | "yes" | "t") => true,
        _ => false,
    }
}

/// The boolean setting `key`, from [`get_effective`], read with
/// [`parse_bool`].
pub fn get_bool(cwd: &Path, key: &str, default: bool) -> bool {
    parse_bool(get_effective(cwd, key).ok().flatten().as_deref(), default)
}

/// The boolean environment variable `name`, read with [`parse_bool`].
pub fn env_bool(name: &str, default: bool) -> bool {
    let value = std::env::var_os(name);
    parse_bool(
        value.as_ref().map(|v| v.to_string_lossy()).as_deref(),
        default,
    )
}

/// Look up `key` in the repository's `.lfsconfig`.
///
/// The file is read from the working tree, then the index, then `HEAD`,
/// and the first one found is used. A bare repository reads only `HEAD`.
/// Only URL and access settings and a few other safe `lfs.*` keys are
/// kept. The rest are dropped with a warning on stderr.
///
/// The file is parsed once per repository per process. Later lookups, and
/// the warning, come from that first parse.
pub fn get_from_lfsconfig(cwd: &Path, key: &str) -> Result<Option<String>, Error> {
    let entries = load_lfsconfig(cwd)?;
    Ok(entries
        .get(&fold_key(key))
        .and_then(|vs| vs.last().cloned()))
}

/// Set `key = value` in the given scope.
pub fn set(cwd: &Path, scope: ConfigScope, key: &str, value: &str) -> Result<(), Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", scope.flag(), key, value])
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ))
    }
}

/// Unset `key` in the given scope.
///
/// Returns `Ok(())` if the key wasn't set.
pub fn unset(cwd: &Path, scope: ConfigScope, key: &str) -> Result<(), Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", scope.flag(), "--unset", key])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(()),
        // git config --unset exits 5 when the key isn't set.
        Some(5) => Ok(()),
        _ => Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
}

/// Keys allowed in `.lfsconfig` besides those [`is_safe_key`] matches by
/// pattern, as in `safeKeys` in upstream's `config/git_fetcher.go`.
const SAFE_KEYS: &[&str] = &[
    "lfs.allowincompletepush",
    "lfs.fetchexclude",
    "lfs.fetchinclude",
    "lfs.gitprotocol",
    "lfs.locksverify",
    "lfs.pushurl",
    "lfs.skipdownloaderrors",
    "lfs.url",
];

/// Whether `key` may be read from `.lfsconfig`.
///
/// `key` must be in the canonical form `git config --list` prints, with
/// the section and last component lowercased.
fn is_safe_key(key: &str) -> bool {
    let parts: Vec<&str> = key.split('.').collect();

    // An extension's `clean` and `smudge` run commands, so only its
    // `priority` is allowed.
    if parts.len() == 4 && parts[0] == "lfs" && parts[1] == "extension" && parts[3] == "priority" {
        return true;
    }

    // `remote.<name>.lfsurl` is the only safe key under `remote.*`.
    if parts.len() >= 3 && parts[0] == "remote" && *parts.last().unwrap() == "lfsurl" {
        return true;
    }

    // `lfs.<url>.access`, which sets the auth scheme for a URL. As in
    // upstream, only under `lfs.`.
    if parts.len() >= 3 && parts[0] == "lfs" && *parts.last().unwrap() == "access" {
        return true;
    }

    SAFE_KEYS.contains(&key)
}

/// Canonicalize a config key the way git does.
///
/// The first and last components are lowercased. The middle, such as a URL
/// or remote name, is case-sensitive and kept as is.
fn fold_key(key: &str) -> String {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.len() < 3 {
        return key.to_lowercase();
    }
    let last = parts.len() - 1;
    let middle = parts[1..last].join(".");
    format!(
        "{}.{}.{}",
        parts[0].to_lowercase(),
        middle,
        parts[last].to_lowercase(),
    )
}

type LfsConfigEntries = HashMap<String, Vec<String>>;

/// Config entries as `(key, value)`, in the order git lists them.
type ConfigList = Vec<(String, String)>;

/// Filtered `.lfsconfig` entries, keyed by the canonicalized work tree
/// root.
///
/// Caching avoids running `git config` on every lookup and makes the
/// unsafe-key warning print only once.
static LFSCONFIG_CACHE: OnceLock<Mutex<HashMap<PathBuf, LfsConfigEntries>>> = OnceLock::new();

fn lfsconfig_cache() -> &'static Mutex<HashMap<PathBuf, LfsConfigEntries>> {
    LFSCONFIG_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn load_lfsconfig(cwd: &Path) -> Result<LfsConfigEntries, Error> {
    // `.lfsconfig` lives at the top of the work tree. Bare repositories
    // and non-repository paths have no top level, so use `cwd`.
    let root = repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let canon = root.canonicalize().unwrap_or_else(|_| root.clone());
    if let Some(cached) = lfsconfig_cache().lock().unwrap().get(&canon) {
        return Ok(cached.clone());
    }

    let entries = group(read_lfsconfig(cwd, &root)?.unwrap_or_default());
    let (safe, ignored) = filter_safe(entries);

    if !ignored.is_empty() {
        // Upstream's exact wording, which t-config 9 matches.
        eprintln!("warning: These unsafe '.lfsconfig' keys were ignored:");
        eprintln!();
        for key in &ignored {
            eprintln!("  {key}");
        }
    }

    lfsconfig_cache()
        .lock()
        .unwrap()
        .insert(canon, safe.clone());
    Ok(safe)
}

/// An `lfs.*` key set more than once in one config source, with a value
/// that differs from the one before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clash {
    pub key: String,
    /// The values read for `key` before this one, from `.lfsconfig` and
    /// then git's config.
    pub earlier: Vec<String>,
    pub value: String,
}

/// Find the `lfs.*` values that clash, as upstream's `git lfs env` warns.
///
/// `.lfsconfig` and git's merged config are read in that order. Within
/// each, a repeated key clashes when its value differs from the last value
/// read for it. Unsafe `.lfsconfig` keys are skipped.
pub fn clashes(cwd: &Path) -> Result<Vec<Clash>, Error> {
    let root = repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let lfsconfig = read_lfsconfig(cwd, &root)?
        .unwrap_or_default()
        .into_iter()
        .filter(|(k, _)| is_safe_key(k))
        .collect();
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--includes", "--list", "--null"])
        .output()?;
    let gitconfig = if out.status.success() {
        parse_list_entries(&out.stdout)
    } else {
        ConfigList::new()
    };
    Ok(find_clashes([lfsconfig, gitconfig]))
}

fn find_clashes(sources: impl IntoIterator<Item = ConfigList>) -> Vec<Clash> {
    let mut values = LfsConfigEntries::new();
    let mut clashes = Vec::new();
    for source in sources {
        let mut seen = std::collections::HashSet::new();
        for (key, value) in source {
            let earlier = values.entry(key.clone()).or_default();
            if !seen.insert(key.clone())
                && key.starts_with("lfs.")
                && earlier.last().is_some_and(|last| *last != value)
            {
                clashes.push(Clash {
                    key: key.clone(),
                    earlier: earlier.clone(),
                    value: value.clone(),
                });
            }
            earlier.push(value);
        }
    }
    clashes
}

/// Read every entry of `.lfsconfig`, in order, with unsafe keys included.
///
/// Same order as upstream's `git/config.go::Sources`: working tree,
/// index, then `HEAD`, or only `HEAD` in a bare repository. The first one
/// found wins. t-config 2 covers the index case, staging the file with
/// `git read-tree` without writing it to the working tree.
fn read_lfsconfig(cwd: &Path, root: &Path) -> Result<Option<ConfigList>, Error> {
    let bare = is_bare(cwd);
    if !bare && root.join(".lfsconfig").is_file() {
        return read_lfsconfig_file(root).map(Some);
    }
    if !bare && let Some(entries) = read_lfsconfig_blob(cwd, ":.lfsconfig")? {
        return Ok(Some(entries));
    }
    read_lfsconfig_blob(cwd, "HEAD:.lfsconfig")
}

/// Read `.lfsconfig` from the working tree at `root`.
fn read_lfsconfig_file(root: &Path) -> Result<ConfigList, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "config",
            "--includes",
            "--file=.lfsconfig",
            "--list",
            "--null",
        ])
        .output()?;
    if !out.status.success() {
        return Err(Error::Failed(format!(
            "git config --file=.lfsconfig --list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(parse_list_entries(&out.stdout))
}

/// Read `.lfsconfig` from a blob, such as `:.lfsconfig` for the index or
/// `HEAD:.lfsconfig`.
///
/// Returns `None` whenever `git config` fails, which includes a missing
/// blob or ref.
fn read_lfsconfig_blob(cwd: &Path, revision: &str) -> Result<Option<ConfigList>, Error> {
    let blob_arg = format!("--blob={revision}");
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--includes", &blob_arg, "--list", "--null"])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(Some(parse_list_entries(&out.stdout))),
        _ => Ok(None),
    }
}

/// Whether `cwd` is inside a bare repository.
///
/// Returns `false` when git can't tell, so the working tree is still
/// checked.
fn is_bare(cwd: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--is-bare-repository"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
}

/// Find the top of the work tree, where `.lfsconfig` lives.
///
/// Returns `None` in a bare repository or outside any repository.
fn repo_root(cwd: &Path) -> Option<PathBuf> {
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
        return None;
    }
    Some(PathBuf::from(s))
}

/// Parse the output of `git config --list --null` into its entries, in
/// order.
///
/// Each entry ends with a NUL, and its key and value are separated by the
/// first newline, so a value containing a newline can't start another
/// entry, as in upstream's `ParseConfigEntries`. A key without a value is
/// skipped. Git already prints keys in canonical case.
fn parse_list_entries(bytes: &[u8]) -> ConfigList {
    String::from_utf8_lossy(bytes)
        .split('\0')
        .filter_map(|entry| entry.split_once('\n'))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// Group entries by key. Repeated keys keep every value, in order.
fn group(entries: ConfigList) -> LfsConfigEntries {
    let mut grouped = LfsConfigEntries::new();
    for (k, v) in entries {
        grouped.entry(k).or_default().push(v);
    }
    grouped
}

/// Split `.lfsconfig` entries into the safe entries and the sorted names
/// of the ignored keys.
fn filter_safe(entries: LfsConfigEntries) -> (LfsConfigEntries, Vec<String>) {
    let mut safe = LfsConfigEntries::new();
    let mut ignored = Vec::new();
    let mut keys: Vec<String> = entries.keys().cloned().collect();
    keys.sort();
    for k in keys {
        let values = entries.get(&k).cloned().unwrap_or_default();
        if is_safe_key(&k) {
            safe.insert(k, values);
        } else {
            ignored.push(k);
        }
    }
    (safe, ignored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn booleans_parse_like_upstream() {
        for (value, default, want) in [
            (None, true, true),
            (None, false, false),
            (Some(""), true, true),
            (Some(""), false, false),
            (Some("true"), false, true),
            (Some("TRUE"), false, true),
            (Some("1"), false, true),
            (Some("on"), false, true),
            (Some("yes"), false, true),
            (Some("t"), false, true),
            (Some("false"), true, false),
            (Some("False"), true, false),
            (Some("0"), true, false),
            (Some("off"), true, false),
            (Some("no"), true, false),
            (Some("f"), true, false),
            // Unrecognized is false, whatever the default.
            (Some("maybe"), true, false),
            (Some("2"), false, false),
        ] {
            assert_eq!(parse_bool(value, default), want, "{value:?}, {default}");
        }
    }

    fn init_repo() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .arg(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        tmp
    }

    #[test]
    fn get_unset_key_returns_none() {
        let tmp = init_repo();
        let v = get(tmp.path(), ConfigScope::Local, "filter.lfs.clean").unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn set_then_get_round_trips() {
        let tmp = init_repo();
        set(
            tmp.path(),
            ConfigScope::Local,
            "filter.lfs.clean",
            "git-lfs clean -- %f",
        )
        .unwrap();
        let v = get(tmp.path(), ConfigScope::Local, "filter.lfs.clean").unwrap();
        assert_eq!(v.as_deref(), Some("git-lfs clean -- %f"));
    }

    #[test]
    fn unset_removes_key() {
        let tmp = init_repo();
        set(
            tmp.path(),
            ConfigScope::Local,
            "filter.lfs.required",
            "true",
        )
        .unwrap();
        unset(tmp.path(), ConfigScope::Local, "filter.lfs.required").unwrap();
        let v = get(tmp.path(), ConfigScope::Local, "filter.lfs.required").unwrap();
        assert_eq!(v, None);
    }

    #[test]
    fn unset_missing_key_is_ok() {
        let tmp = init_repo();
        unset(tmp.path(), ConfigScope::Local, "never.was.set").unwrap();
    }

    #[test]
    fn safe_key_classification() {
        // From the hardcoded list.
        assert!(is_safe_key("lfs.url"));
        assert!(is_safe_key("lfs.fetchinclude"));
        assert!(is_safe_key("lfs.locksverify"));

        // Per-URL access setting, where the middle component is a URL.
        assert!(is_safe_key("lfs.http://example.com/repo.git.access"));
        assert!(is_safe_key("lfs.https://host.access"));
        // `access` keys are only safe under `lfs.`.
        assert!(!is_safe_key("other.example.access"));
        assert!(!is_safe_key("http.https://host.access"));

        // Only `lfsurl` is safe under `remote.*`.
        assert!(is_safe_key("remote.origin.lfsurl"));
        assert!(!is_safe_key("remote.origin.url"));
        assert!(!is_safe_key("remote.origin.pushurl"));

        // Extension priority is safe, but clean and smudge run commands.
        assert!(is_safe_key("lfs.extension.foo.priority"));
        assert!(!is_safe_key("lfs.extension.foo.clean"));
        assert!(!is_safe_key("lfs.extension.foo.smudge"));

        // Credential and core settings are never safe.
        assert!(!is_safe_key("core.askpass"));
        assert!(!is_safe_key("credential.helper"));
        assert!(!is_safe_key("lfs.concurrenttransfers"));
    }

    #[test]
    fn fold_key_lowercases_first_and_last_only() {
        assert_eq!(fold_key("LFS.URL"), "lfs.url");
        assert_eq!(
            fold_key("LFS.http://Example.com.ACCESS"),
            "lfs.http://Example.com.access"
        );
        assert_eq!(fold_key("Section.Key"), "section.key");
    }

    #[test]
    fn parse_list_handles_values_with_equals() {
        let raw = b"lfs.url\nhttp://example.com/path?x=1\0remote.origin.lfsurl\nhttp://a\0";
        let parsed = group(parse_list_entries(raw));
        assert_eq!(
            parsed["lfs.url"],
            vec!["http://example.com/path?x=1".to_owned()]
        );
        assert_eq!(parsed["remote.origin.lfsurl"], vec!["http://a".to_owned()]);
    }

    #[test]
    fn clashes_are_repeated_lfs_keys_with_new_values() {
        let list = |entries: &[(&str, &str)]| -> ConfigList {
            entries
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect()
        };
        let lfsconfig = list(&[("lfs.url", "a")]);
        let gitconfig = list(&[
            // The first value in a source never clashes.
            ("lfs.url", "b"),
            ("lfs.url", "b"),
            ("lfs.url", "c"),
            ("remote.origin.url", "x"),
            ("remote.origin.url", "y"),
        ]);
        assert_eq!(
            find_clashes([lfsconfig, gitconfig]),
            vec![Clash {
                key: "lfs.url".into(),
                earlier: vec!["a".into(), "b".into(), "b".into()],
                value: "c".into(),
            }]
        );
    }

    #[test]
    fn parse_list_collects_repeated_keys_in_order() {
        let raw = b"lfs.fetchinclude\na\0lfs.fetchinclude\nb\0";
        let parsed = group(parse_list_entries(raw));
        assert_eq!(
            parsed["lfs.fetchinclude"],
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    #[test]
    fn lfsconfig_falls_back_to_head_blob_when_no_working_tree_file() {
        // Mirrors t-config 2's "config reads from repository" scenario:
        // no working-tree `.lfsconfig`, but HEAD has one committed.
        let tmp = init_repo();
        let path = tmp.path();
        // Configure a local identity so commits work without HOME.
        let _ = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["config", "user.name", "test"])
            .status();
        let _ = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["config", "user.email", "test@example.com"])
            .status();
        std::fs::write(
            path.join(".lfsconfig"),
            "[lfs]\n\turl = http://from-head/\n",
        )
        .unwrap();
        let _ = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["add", ".lfsconfig"])
            .status();
        let _ = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["-c", "commit.gpgsign=false", "commit", "-m", "init"])
            .status();
        // Remove the working-tree copy so only HEAD has it.
        std::fs::remove_file(path.join(".lfsconfig")).unwrap();

        let entries = read_lfsconfig_blob(path, "HEAD:.lfsconfig")
            .unwrap()
            .unwrap();
        assert_eq!(
            entries,
            vec![("lfs.url".to_owned(), "http://from-head/".to_owned())]
        );
    }

    #[test]
    fn read_lfsconfig_blob_missing_returns_none() {
        let tmp = init_repo();
        // Empty repo: no `:.lfsconfig`, no `HEAD:.lfsconfig`.
        assert!(
            read_lfsconfig_blob(tmp.path(), ":.lfsconfig")
                .unwrap()
                .is_none()
        );
        assert!(
            read_lfsconfig_blob(tmp.path(), "HEAD:.lfsconfig")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn filter_safe_partitions_keys() {
        let mut entries = LfsConfigEntries::new();
        entries.insert("lfs.url".into(), vec!["http://x".into()]);
        entries.insert("core.askpass".into(), vec!["unsafe".into()]);
        entries.insert("lfs.extension.e.priority".into(), vec!["1".into()]);
        entries.insert("lfs.extension.e.clean".into(), vec!["bad".into()]);

        let (safe, ignored) = filter_safe(entries);
        assert!(safe.contains_key("lfs.url"));
        assert!(safe.contains_key("lfs.extension.e.priority"));
        assert!(!safe.contains_key("core.askpass"));
        assert!(!safe.contains_key("lfs.extension.e.clean"));
        // Sorted for deterministic warning output.
        assert_eq!(ignored, vec!["core.askpass", "lfs.extension.e.clean"]);
    }

    #[test]
    fn lfsconfig_value_with_newline_does_not_inject_a_key() {
        let repo = init_repo();
        let set = |key: &str, value: &str| {
            let status = Command::new("git")
                .arg("-C")
                .arg(repo.path())
                .args(["config", "--file=.lfsconfig", key, value])
                .status()
                .unwrap();
            assert!(status.success());
        };
        set("lfs.url", "http://real-endpoint");
        set("lfs.fetchinclude", "x\nlfs.url=http://injected-endpoint");

        let url = get_from_lfsconfig(repo.path(), "lfs.url").unwrap();
        assert_eq!(url.as_deref(), Some("http://real-endpoint"));
    }
}
