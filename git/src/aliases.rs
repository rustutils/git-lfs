//! `url.<base>.insteadOf <alias>` rewrite handling.
//!
//! Git lets users define URL prefix aliases in config:
//! `git config url."https://github.com/".insteadOf gh:` makes any URL
//! starting with `gh:` rewrite to `https://github.com/...`. The git
//! tooling applies this rewrite universally; LFS has to do the same
//! so settings like `lfs.url = gh:org/repo` resolve the same way the
//! user's `git fetch` already does.
//!
//! The rewrite logic itself is dead simple — pick the longest alias
//! that's a prefix of the input URL and swap it for the configured
//! base. The only subtlety is duplicate detection: when two
//! `url.<base>.insteadOf` entries share the *same* alias value but
//! disagree on the base, we emit
//! `warning: Multiple 'url.*.insteadof' keys with the same alias: ...`
//! once per process, mirroring upstream.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::Error;

/// Maps `<alias>` → `<base>` for every `url.<base>.insteadOf <alias>`
/// entry in the effective git config.
pub type Aliases = HashMap<String, String>;

/// Load the alias map for `cwd`, warning once-per-process about
/// conflicts. Cached so repeated calls within one process don't fire
/// `git config` again.
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

/// Load the push-direction alias map (`url.<base>.pushInsteadOf`) for
/// `cwd`. Cached separately from `load_aliases`. Falls back to an empty
/// map when no `pushInsteadOf` entries are configured — callers should
/// then use [`load_aliases`] for the upload path too, so plain
/// `insteadOf` still applies.
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

/// Apply `url.*.insteadOf` rewriting to `url`, returning the longest-
/// prefix-match rewrite or the original string if nothing matches.
pub fn rewrite(cwd: &Path, url: &str) -> Result<String, Error> {
    let aliases = load_aliases(cwd)?;
    Ok(apply(&aliases, url))
}

/// Pure function: given a built alias map and a URL, do the longest-
/// prefix-match rewrite. Split out so unit tests don't need a temp
/// repo, and exposed so callers that already hold an [`Aliases`] map
/// (e.g. the transfer queue, which captures the map once at startup
/// instead of re-locking the per-call cache) can apply it directly.
pub fn apply(aliases: &Aliases, url: &str) -> String {
    let mut best: Option<&str> = None;
    for alias in aliases.keys() {
        if !url.starts_with(alias.as_str()) {
            continue;
        }
        if best.is_none_or(|b| alias.len() > b.len()) {
            best = Some(alias);
        }
    }
    match best {
        Some(alias) => format!("{}{}", aliases[alias], &url[alias.len()..]),
        None => url.to_owned(),
    }
}

/// One parsed `url.<base>.insteadOf <alias>` entry.
struct InsteadOf {
    base: String,
    alias: String,
}

/// Read every `url.*.<suffix>` from the effective config (all scopes),
/// preserving ordering and duplicates so callers can apply upstream's
/// "first-seen wins / warn on conflict" rule. `suffix` is `"insteadof"`
/// for the download/general path or `"pushinsteadof"` for the upload
/// path; git's config parsing is case-insensitive on the key, so the
/// lowercased form covers `pushInsteadOf` etc.
fn list_aliases_entries(cwd: &Path, suffix: &str) -> Result<Vec<InsteadOf>, Error> {
    let regex = format!(r"^url\..*\.{suffix}$");
    // `--null` so newlines in URLs (which never happen in practice but
    // *would* break the default key/value separator) and special chars
    // in alias values come out unambiguously.
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--includes", "--null", "--get-regexp", &regex])
        .output()?;
    // Exit 1 just means "no matches" — common case.
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
        // Each record is `<key>\n<value>` — `--null` separates entries
        // but uses the literal newline between key and value.
        let s = std::str::from_utf8(record)
            .map_err(|e| Error::Failed(format!("non-utf8 {suffix} entry: {e}")))?;
        let (key, value) = match s.split_once('\n') {
            Some(kv) => kv,
            None => continue,
        };
        // Strip `url.` prefix and `.<suffix>` suffix to recover the
        // base URL, which can itself contain dots.
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

/// Convert the raw entry list into a map, emitting the conflict
/// warning when an alias maps to two different bases. First-seen base
/// wins, matching upstream — but iteration order from `git config
/// --get-regexp` is config-file order, so this is deterministic.
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
            // First-seen base wins (matches upstream's `if v != url`
            // path: it warns but doesn't overwrite).
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
        // Two entries with the same alias *and* the same base → no
        // conflict, no warning. (We can't capture stderr here, but
        // we can at least exercise the path and check the resulting
        // map.)
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
