//! Git config get/set/unset, scoped to one of git's config files.

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

/// Read a single config value from the given scope. Returns `Ok(None)` if
/// the key isn't set, *or* if the scope itself isn't readable here:
/// `git config --local` exits 128 outside any repo, exits 129 ("only
/// one config file at a time") when `GIT_CONFIG` is also set, and any
/// scope exits 128 when env-vars like `GIT_WORK_TREE` point at a
/// missing path. Treating all of those as "no value" matches upstream's
/// `cfg.Git.Get(key)` semantics — `git lfs env` distinguishes a
/// configured value from an unconfigured one, but not between "key not
/// set" and "scope unreachable."
pub fn get(cwd: &Path, scope: ConfigScope, key: &str) -> Result<Option<String>, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--includes", scope.flag(), "--get", key])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
        // exit 1 = key not set; 128 = scope unreachable (no repo / bad
        // work tree); 129 = "only one config file at a time" when both
        // `GIT_CONFIG` and a scope flag are in effect. All three are
        // "no value here" from our perspective.
        Some(1) | Some(128) | Some(129) => Ok(None),
        _ => Err(Error::Failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )),
    }
}

/// Read a single config value from the merged (local → global → system,
/// plus `GIT_CONFIG` and `include.path` directives) view. Mirrors
/// upstream's `cfg.Git.Get` — they always read scope-less so git's own
/// priority + include resolution applies in one shot. Returns `Ok(None)`
/// for missing keys *or* unreadable config (no repo, bad work tree).
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

/// Read a single config value from a specific file (e.g. `.lfsconfig`).
/// Returns `Ok(None)` if the file doesn't exist or the key isn't set.
pub fn get_from_file(cwd: &Path, file: &Path, key: &str) -> Result<Option<String>, Error> {
    if !cwd.join(file).is_file() {
        // `git config --file` errors loudly on a missing file. The common
        // case for `.lfsconfig` is "no file" — treat that as "no value".
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

/// Look up `key` across `.lfsconfig` (committed; lowest priority) and
/// the standard git config scopes (local → global → system). Returns the
/// first match.
///
/// Mirrors upstream's effective config: settings written to `.lfsconfig`
/// at the repo root are visible without overriding anything explicitly
/// set in the user's git config. `.lfsconfig` reads are filtered through
/// the safe-key allowlist (see [`is_safe_key`]) — settings that aren't
/// URL/access related are ignored, with a one-shot warning to stderr.
pub fn get_effective(cwd: &Path, key: &str) -> Result<Option<String>, Error> {
    if let Some(v) = get_any_scope(cwd, key)? {
        return Ok(Some(v));
    }
    get_from_lfsconfig(cwd, key)
}

/// Look up `key` in `.lfsconfig`, applying the safe-key allowlist.
///
/// The first call per `cwd` per process loads + filters `.lfsconfig`,
/// caches the result, and emits the `warning: These unsafe '.lfsconfig'
/// keys were ignored:` line that t-config 9 / upstream's config loader
/// produces. Subsequent lookups hit the cache.
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

/// Unset `key` in the given scope. Idempotent: if the key isn't there,
/// returns `Ok(())` rather than erroring.
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

/// Hardcoded list of `.lfsconfig` keys upstream considers safe outside
/// of the URL/access pattern rules. Mirrors `safeKeys` in upstream's
/// `config/git_fetcher.go`. Anything not on this list (and not matching
/// the remote/extension/access rules) gets stripped from `.lfsconfig`
/// reads with a warning.
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

/// Whether `key` is allowed to come from `.lfsconfig`. Compared against
/// the lowercase canonical form (sections + final subkey lowercased,
/// middle preserved — as git itself emits via `--list`).
fn is_safe_key(key: &str) -> bool {
    let parts: Vec<&str> = key.split('.').collect();

    // `lfs.extension.<name>.priority` is the only extension knob safe
    // from `.lfsconfig`; `clean`/`smudge` are intentionally excluded
    // upstream because they're command-execution surfaces.
    if parts.len() == 4
        && parts[0] == "lfs"
        && parts[1] == "extension"
        && parts[3] == "priority"
    {
        return true;
    }

    // `remote.<name>.lfsurl` is the only safe key under `remote.*`.
    if parts.len() >= 3 && parts[0] == "remote" && *parts.last().unwrap() == "lfsurl" {
        return true;
    }

    // Any 3+ part key ending in `.access` — e.g. `lfs.<url>.access` —
    // is allowed; this is what attaches an auth scheme to a per-URL
    // override.
    if parts.len() >= 3 && *parts.last().unwrap() == "access" {
        return true;
    }

    SAFE_KEYS.iter().any(|s| *s == key)
}

/// Canonicalize a config key the way git does: lowercase the first and
/// last components, preserve the middle (which may be a URL or branch
/// name and is case-sensitive). Used so a stored `lfs.URL` and a lookup
/// for `lfs.url` resolve to the same entry.
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

/// Process-wide cache of parsed `.lfsconfig` files, keyed by the
/// canonicalized cwd. We load each `.lfsconfig` at most once per process
/// — both to avoid the `git config --list` subprocess on every
/// `get_effective` call, and so the unsafe-key warning fires once.
static LFSCONFIG_CACHE: OnceLock<Mutex<HashMap<PathBuf, LfsConfigEntries>>> = OnceLock::new();

fn lfsconfig_cache() -> &'static Mutex<HashMap<PathBuf, LfsConfigEntries>> {
    LFSCONFIG_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn load_lfsconfig(cwd: &Path) -> Result<LfsConfigEntries, Error> {
    // Resolve the repo root so subdirectory invocations still find the
    // `.lfsconfig` at the top of the work tree. Falls back to `cwd` when
    // we can't determine a top-level (e.g. bare repos, or callers that
    // explicitly pass a non-repo path for testing).
    let root = repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let canon = root.canonicalize().unwrap_or_else(|_| root.clone());
    if let Some(cached) = lfsconfig_cache().lock().unwrap().get(&canon) {
        return Ok(cached.clone());
    }

    // Upstream's lookup chain (`git/config.go::Sources`):
    //   - non-bare: working tree → `:.lfsconfig` (index) → `HEAD:.lfsconfig`
    //   - bare:     `HEAD:.lfsconfig` only
    // First hit wins; failures fall through silently. The index step is
    // what t-config 2 exercises after `git read-tree` populates the
    // staged copy without ever touching the working tree.
    let bare = is_bare(cwd);
    let mut entries = None;
    if !bare && root.join(".lfsconfig").is_file() {
        entries = Some(read_lfsconfig_file(&root)?);
    }
    if entries.is_none() && !bare {
        entries = read_lfsconfig_blob(cwd, ":.lfsconfig")?;
    }
    if entries.is_none() {
        entries = read_lfsconfig_blob(cwd, "HEAD:.lfsconfig")?;
    }
    let entries = entries.unwrap_or_default();

    let (safe, ignored) = filter_safe(entries);

    if !ignored.is_empty() {
        // Match upstream's wording verbatim — t-config 9 greps for the
        // exact prefix "warning: These unsafe '.lfsconfig' keys were
        // ignored:" and for each indented key on its own line.
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

/// Read an existing working-tree `.lfsconfig` via `git config --file`.
fn read_lfsconfig_file(root: &Path) -> Result<LfsConfigEntries, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--includes", "--file=.lfsconfig", "--list"])
        .output()?;
    if !out.status.success() {
        return Err(Error::Failed(format!(
            "git config --file=.lfsconfig --list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(parse_list_output(&out.stdout))
}

/// Read `.lfsconfig` from a git revision (`:.lfsconfig` for index,
/// `HEAD:.lfsconfig` for HEAD's tree). Returns `None` when the blob
/// doesn't exist — git emits exit 128 with an "ambiguous argument" or
/// "does not exist" error in that case.
fn read_lfsconfig_blob(cwd: &Path, revision: &str) -> Result<Option<LfsConfigEntries>, Error> {
    let blob_arg = format!("--blob={revision}");
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--includes", &blob_arg, "--list"])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(Some(parse_list_output(&out.stdout))),
        // Missing blob ⇒ silent fallback. Both "exit 1" (key not found
        // when the blob is empty) and "exit 128" (ambiguous arg / no
        // such ref) land here.
        _ => Ok(None),
    }
}

/// Whether `cwd` is inside a bare repository. Returns `false` for non-
/// repos too — anything we can't classify is treated as non-bare so
/// the working-tree lookup still runs.
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

/// Locate the work-tree root (where `.lfsconfig` lives). Returns `None`
/// for bare repos (no work tree) or when `cwd` isn't inside a repo at
/// all. We can't reuse [`crate::path::git_dir`] because that yields the
/// `.git` directory; `.lfsconfig` lives one level up.
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

/// Parse `git config --list` style `key=value` lines. Values can contain
/// `=` (URLs commonly do), so split on the *first* one only. Keys come
/// out already case-folded to git's canonical form.
fn parse_list_output(bytes: &[u8]) -> LfsConfigEntries {
    let s = String::from_utf8_lossy(bytes);
    let mut entries: LfsConfigEntries = HashMap::new();
    for line in s.lines() {
        if let Some((k, v)) = line.split_once('=') {
            entries.entry(k.to_owned()).or_default().push(v.to_owned());
        }
    }
    entries
}

/// Split parsed `.lfsconfig` entries into a (safe-keys map, ignored-key
/// list). The ignored list is sorted for deterministic warning output.
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

        // Per-URL access knob — middle component is a URL.
        assert!(is_safe_key("lfs.http://example.com/repo.git.access"));
        assert!(is_safe_key("lfs.https://host.access"));

        // Remote LFS URL override — but only `lfsurl`, no other remote.* keys.
        assert!(is_safe_key("remote.origin.lfsurl"));
        assert!(!is_safe_key("remote.origin.url"));
        assert!(!is_safe_key("remote.origin.pushurl"));

        // Extension priority is safe; clean/smudge are not (they execute commands).
        assert!(is_safe_key("lfs.extension.foo.priority"));
        assert!(!is_safe_key("lfs.extension.foo.clean"));
        assert!(!is_safe_key("lfs.extension.foo.smudge"));

        // Generic credential / core knobs from .lfsconfig — never safe.
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
        let raw = b"lfs.url=http://example.com/path?x=1\nremote.origin.lfsurl=http://a\n";
        let parsed = parse_list_output(raw);
        assert_eq!(
            parsed["lfs.url"],
            vec!["http://example.com/path?x=1".to_owned()]
        );
        assert_eq!(
            parsed["remote.origin.lfsurl"],
            vec!["http://a".to_owned()]
        );
    }

    #[test]
    fn parse_list_collects_repeated_keys_in_order() {
        let raw = b"url.http://a/.insteadof=alias\nurl.http://b/.insteadof=alias\n";
        let parsed = parse_list_output(raw);
        assert_eq!(
            parsed["url.http://a/.insteadof"],
            vec!["alias".to_owned()]
        );
        assert_eq!(
            parsed["url.http://b/.insteadof"],
            vec!["alias".to_owned()]
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
        std::fs::write(path.join(".lfsconfig"), "[lfs]\n\turl = http://from-head/\n").unwrap();
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

        let entries = read_lfsconfig_blob(path, "HEAD:.lfsconfig").unwrap().unwrap();
        assert_eq!(
            entries.get("lfs.url").map(|v| v.last().cloned()).flatten(),
            Some("http://from-head/".to_owned())
        );
    }

    #[test]
    fn read_lfsconfig_blob_missing_returns_none() {
        let tmp = init_repo();
        // Empty repo: no `:.lfsconfig`, no `HEAD:.lfsconfig`.
        assert!(read_lfsconfig_blob(tmp.path(), ":.lfsconfig").unwrap().is_none());
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
}
