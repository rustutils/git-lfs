//! URL rewriting with `url.<base>.insteadOf` and `pushInsteadOf`.
//!
//! With `git config url."https://github.com/".insteadOf gh:`, any URL that
//! starts with `gh:` is rewritten to start with `https://github.com/`
//! instead. LFS applies the same rewrite, so a setting like
//! `lfs.url = gh:org/repo` resolves the way `git fetch` would.
//!
//! The longest matching alias wins. When two entries give the same alias
//! different bases, the first one wins and a warning is printed, as in
//! upstream.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::Error;

/// Base URL for each alias, from `url.<base>.insteadOf <alias>` entries.
pub type Aliases = HashMap<String, String>;

/// Load the `insteadOf` aliases from git config.
///
/// The result is cached per `cwd` for the life of the process, so the
/// conflict warning prints once.
pub fn load_aliases(cwd: &Path) -> Result<Aliases, Error> {
    let canon = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    if let Some(cached) = aliases_cache().lock().unwrap().get(&canon) {
        return Ok(cached.clone());
    }

    let entries = list_aliases_entries(cwd, "insteadof")?;
    let aliases = build_aliases(&entries);

    aliases_cache()
        .lock()
        .unwrap()
        .insert(canon, aliases.clone());
    Ok(aliases)
}

/// Load the `pushInsteadOf` aliases from git config.
///
/// Cached like [`load_aliases`]. When no alias here matches a push URL,
/// callers should fall back to [`load_aliases`], as git does.
pub fn load_push_aliases(cwd: &Path) -> Result<Aliases, Error> {
    let canon = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    if let Some(cached) = push_aliases_cache().lock().unwrap().get(&canon) {
        return Ok(cached.clone());
    }

    let entries = list_aliases_entries(cwd, "pushinsteadof")?;
    let aliases = build_aliases(&entries);

    push_aliases_cache()
        .lock()
        .unwrap()
        .insert(canon, aliases.clone());
    Ok(aliases)
}

/// Rewrite `url` with the `insteadOf` aliases for `cwd`.
pub fn rewrite(cwd: &Path, url: &str) -> Result<String, Error> {
    let aliases = load_aliases(cwd)?;
    Ok(apply(&aliases, url))
}

/// Rewrite a push `url` for `cwd`, as git does.
///
/// The `pushInsteadOf` aliases are tried first. If none matches, the
/// `insteadOf` aliases are used.
pub fn rewrite_for_push(cwd: &Path, url: &str) -> Result<String, Error> {
    match try_apply(&load_push_aliases(cwd)?, url) {
        Some(rewritten) => Ok(rewritten),
        None => rewrite(cwd, url),
    }
}

/// Rewrite `url` with an alias map you already hold.
///
/// The longest alias that prefixes `url` is replaced with its base. With
/// no match, `url` is returned unchanged.
pub fn apply(aliases: &Aliases, url: &str) -> String {
    try_apply(aliases, url).unwrap_or_else(|| url.to_owned())
}

/// Like [`apply`], but `None` when no alias matches.
fn try_apply(aliases: &Aliases, url: &str) -> Option<String> {
    let mut best: Option<&str> = None;
    for alias in aliases.keys() {
        if !url.starts_with(alias.as_str()) {
            continue;
        }
        if best.is_none_or(|b| alias.len() > b.len()) {
            best = Some(alias);
        }
    }
    best.map(|alias| format!("{}{}", aliases[alias], &url[alias.len()..]))
}

/// One `url.<base>.insteadOf <alias>` or `pushInsteadOf` entry.
struct InsteadOf {
    base: String,
    alias: String,
}

/// Read every `url.<base>.<suffix>` entry from git's merged config.
///
/// `suffix` is `insteadof` or `pushinsteadof`, lowercase because git
/// prints keys that way. Entries keep config order and duplicates, for
/// [`build_aliases`].
fn list_aliases_entries(cwd: &Path, suffix: &str) -> Result<Vec<InsteadOf>, Error> {
    let regex = format!(r"^url\..*\.{suffix}$");
    // `--null` keeps a value containing a newline in one record.
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--includes", "--null", "--get-regexp", &regex])
        .output()?;
    // Exit 1 means no entries matched.
    match out.status.code() {
        Some(0) => {}
        Some(1) => return Ok(Vec::new()),
        _ => {
            return Err(Error::Failed(format!(
                "git config --get-regexp {suffix} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
    }

    let dot_suffix = format!(".{suffix}");
    let mut entries = Vec::new();
    for record in out.stdout.split(|&b| b == 0) {
        if record.is_empty() {
            continue;
        }
        // With `--null`, each record is the key, a newline, then the
        // value.
        let s = std::str::from_utf8(record)
            .map_err(|e| Error::Failed(format!("non-utf8 {suffix} entry: {e}")))?;
        let (key, value) = match s.split_once('\n') {
            Some(kv) => kv,
            None => continue,
        };
        // The base URL is everything between `url.` and `.<suffix>`,
        // dots included.
        let trimmed = match key.strip_prefix("url.") {
            Some(s) => s,
            None => continue,
        };
        let base = match trimmed.strip_suffix(dot_suffix.as_str()) {
            Some(s) => s,
            None => continue,
        };
        entries.push(InsteadOf {
            base: base.to_owned(),
            alias: value.to_owned(),
        });
    }
    Ok(entries)
}

/// Build the alias map from entries in config order.
///
/// When an alias appears again with a different base, the first base is
/// kept and a warning is printed once for that alias, as in upstream.
fn build_aliases(entries: &[InsteadOf]) -> Aliases {
    let mut map = Aliases::new();
    let mut warned: std::collections::HashSet<String> = Default::default();
    for entry in entries {
        if let Some(existing) = map.get(&entry.alias) {
            if existing != &entry.base && warned.insert(entry.alias.clone()) {
                eprintln!(
                    "warning: Multiple 'url.*.insteadof' keys with the same alias: {:?}",
                    entry.alias
                );
            }
            continue;
        }
        map.insert(entry.alias.clone(), entry.base.clone());
    }
    map
}

static ALIASES_CACHE: OnceLock<Mutex<HashMap<PathBuf, Aliases>>> = OnceLock::new();
static PUSH_ALIASES_CACHE: OnceLock<Mutex<HashMap<PathBuf, Aliases>>> = OnceLock::new();

fn aliases_cache() -> &'static Mutex<HashMap<PathBuf, Aliases>> {
    ALIASES_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn push_aliases_cache() -> &'static Mutex<HashMap<PathBuf, Aliases>> {
    PUSH_ALIASES_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_returns_input_when_no_alias_matches() {
        let aliases = Aliases::new();
        assert_eq!(
            apply(&aliases, "https://github.com/foo"),
            "https://github.com/foo"
        );
    }

    #[test]
    fn apply_rewrites_simple_prefix() {
        let mut aliases = Aliases::new();
        aliases.insert("alias:".into(), "http://actual-url/".into());
        assert_eq!(apply(&aliases, "alias:rest"), "http://actual-url/rest");
    }

    #[test]
    fn apply_picks_longest_match() {
        let mut aliases = Aliases::new();
        // `alias` and `alias:` both prefix `alias:rest`, but the
        // longer match wins.
        aliases.insert("alias".into(), "http://wrong-url/".into());
        aliases.insert("alias:".into(), "http://actual-url/".into());
        assert_eq!(apply(&aliases, "alias:rest"), "http://actual-url/rest");
    }

    #[test]
    fn apply_does_not_rewrite_non_prefix() {
        let mut aliases = Aliases::new();
        aliases.insert("alias:".into(), "http://actual-url/".into());
        // Doesn't start with the alias, so left alone.
        assert_eq!(apply(&aliases, "badalias:rest"), "badalias:rest");
    }

    #[test]
    fn build_aliases_does_not_warn_on_duplicate_same_value() {
        // Two entries with the same alias and the same base are not a
        // conflict. (We can't capture stderr here, but we can at least
        // exercise the path and check the resulting map.)
        let entries = vec![
            InsteadOf {
                base: "https://host.example/domain/".into(),
                alias: "git@host.example:domain/".into(),
            },
            InsteadOf {
                base: "https://host.example/domain/".into(),
                alias: "git@host.example:domain/".into(),
            },
        ];
        let map = build_aliases(&entries);
        assert_eq!(map.len(), 1);
        assert_eq!(
            map["git@host.example:domain/"],
            "https://host.example/domain/"
        );
    }

    #[test]
    fn build_aliases_keeps_first_base_on_conflict() {
        let entries = vec![
            InsteadOf {
                base: "http://actual-url/".into(),
                alias: "alias:".into(),
            },
            InsteadOf {
                base: "http://dupe-url".into(),
                alias: "alias:".into(),
            },
        ];
        let map = build_aliases(&entries);
        assert_eq!(map["alias:"], "http://actual-url/");
    }

    #[test]
    fn build_aliases_handles_multiple_distinct_aliases() {
        let entries = vec![
            InsteadOf {
                base: "http://actual-url/".into(),
                alias: "alias:".into(),
            },
            InsteadOf {
                base: "http://actual-url/".into(),
                alias: "alias2:".into(),
            },
        ];
        let map = build_aliases(&entries);
        assert_eq!(map["alias:"], "http://actual-url/");
        assert_eq!(map["alias2:"], "http://actual-url/");
    }
}
