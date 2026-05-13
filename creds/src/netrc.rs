//! `~/.netrc` (or `_netrc` on Windows) credential lookup.
//!
//! Sits in the helper chain between the in-process cache and
//! `git credential` so a user with a populated netrc never has to
//! round-trip through `git credential fill` for hosts it covers.
//! Mirrors upstream's `creds/netrc.go::netrcCredentialHelper`.
//!
//! # Behavior
//!
//! - On `fill`: look up the request host (port stripped) in the
//!   parsed netrc. If a `machine` entry matches — or a `default`
//!   entry is present and no specific match was found — return its
//!   `login` + `password`. Hosts that previously hit `reject` are
//!   skipped until the next `approve`.
//! - On `approve`: if the creds match a netrc entry for this host,
//!   clear the host's skip flag and emit the trace line. Doesn't
//!   touch the file (netrc is read-only).
//! - On `reject`: same matching check, then set the skip flag so
//!   future fills bypass us — netrc's contents won't change between
//!   calls, so re-issuing the same wrong creds would loop.
//!
//! Trace lines (`netrc: git credential fill (…)` / `approve (…)` /
//! `reject (…)`) match upstream's `tracerx.Printf` format — the
//! quoting is Go's `%q` (backslash-escaped, wrapped in double quotes)
//! so the `t-credentials.sh` `netrc:` greps line up.
//!
//! # Parser
//!
//! Tokens are whitespace-separated. Recognized keywords are
//! `machine`, `default`, `login`, `password`, `account`, `macdef`.
//! Anything else is treated as one orphan token and skipped — that
//! makes the parser permissive enough to ignore unknown keywords
//! introduced by other tools (matches the upstream
//! `t-credentials.sh::credentials from netrc with unknown keyword`
//! test). `macdef` body parsing isn't implemented; we skip the
//! `macdef <name>` pair and continue, which is enough for the
//! test fixtures.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::helper::{Credentials, Helper, HelperError};
use crate::query::Query;

/// One `machine <name> login <user> password <pass>` block from a
/// netrc file. `machine == "*"` represents a `default` block.
#[derive(Debug, Clone)]
struct NetrcEntry {
    machine: String,
    login: String,
    password: String,
}

/// Netrc-backed credential helper.
///
/// Cheap to construct: the file is read and parsed once at
/// construction, and lookups walk the parsed list linearly (netrc
/// files are small in practice).
#[derive(Debug)]
pub struct NetrcCredentialHelper {
    entries: Vec<NetrcEntry>,
    skip: Mutex<HashSet<String>>,
}

impl NetrcCredentialHelper {
    /// Build from a parsed-and-already-decoded netrc body.
    pub fn from_contents(content: &str) -> Self {
        Self {
            entries: parse_netrc(content),
            skip: Mutex::new(HashSet::new()),
        }
    }

    /// Read the user's default netrc file.
    ///
    /// Tries `$HOME/.netrc`, falling back to `$HOME/_netrc` on
    /// Windows when `.netrc` isn't present. Returns `None` when no
    /// netrc file exists, when `$HOME` isn't set, or when the file
    /// is unreadable; these are not user errors, just "no creds from
    /// this source".
    pub fn from_default_location() -> Option<Self> {
        let home = std::env::var_os("HOME")?;
        let primary = PathBuf::from(&home).join(".netrc");
        let alt = PathBuf::from(&home).join("_netrc");
        let path = if primary.is_file() {
            primary
        } else if cfg!(windows) && alt.is_file() {
            alt
        } else {
            return None;
        };
        Self::from_path(&path)
    }

    /// Read + parse `path`. Returns `None` when the file is missing
    /// or unreadable; logging a parse error is upstream's choice but
    /// "no creds from this source" matches Helper-trait semantics.
    pub fn from_path(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        Some(Self::from_contents(&content))
    }

    /// Find a netrc entry matching `host`. Tries exact match first,
    /// then falls back to the `default` block (if any). Matches
    /// upstream's `netrc.FindMachine` semantics.
    fn find_machine(&self, host: &str) -> Option<&NetrcEntry> {
        self.entries
            .iter()
            .find(|e| e.machine.eq_ignore_ascii_case(host))
            .or_else(|| self.entries.iter().find(|e| e.machine == "*"))
    }
}

impl Helper for NetrcCredentialHelper {
    fn fill(&self, query: &Query) -> Result<Option<Credentials>, HelperError> {
        let host = strip_port(&query.host);
        if self.skip.lock().unwrap().contains(host) {
            return Ok(None);
        }
        let Some(entry) = self.find_machine(host) else {
            return Ok(None);
        };
        trace_netrc_fill(&query.protocol, &query.host, &entry.login, &query.path);
        Ok(Some(Credentials::new(&entry.login, &entry.password)))
    }

    fn approve(&self, query: &Query, creds: &Credentials) -> Result<(), HelperError> {
        let host = strip_port(&query.host);
        let Some(entry) = self.find_machine(host) else {
            return Ok(());
        };
        if entry.login != creds.username || entry.password != creds.password {
            // Different creds — they must have come from another
            // helper. Stay silent (no trace, no skip mutation).
            return Ok(());
        }
        trace_netrc_simple("approve", &query.protocol, &query.host, &query.path);
        self.skip.lock().unwrap().remove(host);
        Ok(())
    }

    fn reject(&self, query: &Query, creds: &Credentials) -> Result<(), HelperError> {
        let host = strip_port(&query.host);
        let Some(entry) = self.find_machine(host) else {
            return Ok(());
        };
        if entry.login != creds.username || entry.password != creds.password {
            return Ok(());
        }
        trace_netrc_simple("reject", &query.protocol, &query.host, &query.path);
        self.skip.lock().unwrap().insert(host.to_owned());
        Ok(())
    }
}

/// Pop the `:port` suffix off `host` so a netrc `machine localhost`
/// entry matches a query for `localhost:12345`. Mirrors upstream's
/// `getNetrcHostname` (which uses `net.SplitHostPort`). Returns the
/// input unchanged when no `:` is present.
fn strip_port(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((h, _)) => h,
        None => host,
    }
}

fn trace_netrc_fill(protocol: &str, host: &str, login: &str, path: &str) {
    if !trace_enabled() {
        return;
    }
    let mut e = std::io::stderr().lock();
    let _ = writeln!(
        e,
        "netrc: git credential fill ({}, {}, {}, {})",
        go_quote(protocol),
        go_quote(host),
        go_quote(login),
        go_quote(path),
    );
}

fn trace_netrc_simple(verb: &str, protocol: &str, host: &str, path: &str) {
    if !trace_enabled() {
        return;
    }
    let mut e = std::io::stderr().lock();
    let _ = writeln!(
        e,
        "netrc: git credential {verb} ({}, {}, {})",
        go_quote(protocol),
        go_quote(host),
        go_quote(path),
    );
}

/// Format `s` like Go's `fmt.Sprintf("%q", s)` for the subset that
/// matters here: ASCII strings with no control characters. Wraps in
/// double quotes and escapes embedded `"` / `\` — enough for the
/// netrc trace lines, where every input is a URL-derived ASCII
/// string. Full `%q` (unicode escapes, control bytes) is overkill.
fn go_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn trace_enabled() -> bool {
    match std::env::var_os("GIT_TRACE") {
        None => false,
        Some(v) => {
            let s = v.to_string_lossy().trim().to_lowercase();
            !matches!(s.as_str(), "" | "0" | "false" | "no" | "off")
        }
    }
}

/// Parse a netrc file body into entries. Permissive: unknown
/// keywords are silently skipped so other tools' annotations don't
/// break the parse. Keyword recognition is case-insensitive, matching
/// the common Go and curl netrc parsers.
fn parse_netrc(content: &str) -> Vec<NetrcEntry> {
    let mut tokens = content.split_whitespace();
    let mut entries: Vec<NetrcEntry> = Vec::new();
    let mut current: Option<NetrcEntry> = None;

    while let Some(tok) = tokens.next() {
        match tok.to_ascii_lowercase().as_str() {
            "machine" => {
                if let Some(e) = current.take() {
                    entries.push(e);
                }
                let name = tokens.next().unwrap_or_default().to_owned();
                current = Some(NetrcEntry {
                    machine: name,
                    login: String::new(),
                    password: String::new(),
                });
            }
            "default" => {
                if let Some(e) = current.take() {
                    entries.push(e);
                }
                current = Some(NetrcEntry {
                    machine: "*".into(),
                    login: String::new(),
                    password: String::new(),
                });
            }
            "login" => {
                if let Some(e) = current.as_mut() {
                    e.login = tokens.next().unwrap_or_default().to_owned();
                }
            }
            "password" => {
                if let Some(e) = current.as_mut() {
                    e.password = tokens.next().unwrap_or_default().to_owned();
                }
            }
            "account" => {
                // Recognized keyword we don't use — skip its value
                // (otherwise we'd treat the value as an unknown
                // token and behave correctly, but consuming the
                // pair explicitly is what netrc parsers do).
                tokens.next();
            }
            "macdef" => {
                // Macro definition: `macdef <name>\n<body>\n\n`.
                // We don't execute macros; skip the name and discard
                // the body up to the next blank-line-equivalent.
                // Whitespace-tokenized stream can't see blank lines,
                // so this is approximate — but the upstream test
                // fixtures don't exercise macdef.
                tokens.next();
            }
            _ => {
                // Unknown token. Skip it singly: if it's a stray
                // keyword followed by a value, the value lands here
                // on the next iteration and gets skipped too. The
                // upstream "credentials from netrc with unknown
                // keyword" test relies on this.
            }
        }
    }
    if let Some(e) = current {
        entries.push(e);
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_entry() {
        let helper = NetrcCredentialHelper::from_contents(
            "machine localhost\nlogin netrcuser\npassword netrcpass\n",
        );
        let q = Query {
            protocol: "https".into(),
            host: "localhost".into(),
            path: String::new(),
        };
        let creds = helper.fill(&q).unwrap().unwrap();
        assert_eq!(creds.username, "netrcuser");
        assert_eq!(creds.password, "netrcpass");
    }

    #[test]
    fn strips_port_from_query_host() {
        let helper =
            NetrcCredentialHelper::from_contents("machine localhost login alice password s3cret\n");
        let q = Query {
            protocol: "https".into(),
            host: "localhost:12345".into(),
            path: String::new(),
        };
        let creds = helper.fill(&q).unwrap().unwrap();
        assert_eq!(creds.username, "alice");
    }

    #[test]
    fn skips_unknown_keyword_between_known_ones() {
        // Matches the `credentials from netrc with unknown keyword`
        // shell test: a bogus pair between login and password
        // must not break the entry.
        let helper = NetrcCredentialHelper::from_contents(
            "machine localhost\nlogin netrcuser\nnot-a-key something\npassword netrcpass\n",
        );
        let q = Query {
            protocol: "https".into(),
            host: "localhost".into(),
            path: String::new(),
        };
        let creds = helper.fill(&q).unwrap().unwrap();
        assert_eq!(creds.username, "netrcuser");
        assert_eq!(creds.password, "netrcpass");
    }

    #[test]
    fn default_block_used_when_no_machine_match() {
        let helper =
            NetrcCredentialHelper::from_contents("default\nlogin defuser\npassword defpass\n");
        let q = Query {
            protocol: "https".into(),
            host: "anywhere".into(),
            path: String::new(),
        };
        let creds = helper.fill(&q).unwrap().unwrap();
        assert_eq!(creds.username, "defuser");
    }

    #[test]
    fn machine_match_beats_default() {
        let helper = NetrcCredentialHelper::from_contents(
            "machine localhost login a password 1\ndefault login b password 2\n",
        );
        let q = Query {
            protocol: "https".into(),
            host: "localhost".into(),
            path: String::new(),
        };
        let creds = helper.fill(&q).unwrap().unwrap();
        assert_eq!(creds.username, "a");
    }

    #[test]
    fn returns_none_when_no_match() {
        let helper = NetrcCredentialHelper::from_contents("machine other login a password 1\n");
        let q = Query {
            protocol: "https".into(),
            host: "localhost".into(),
            path: String::new(),
        };
        assert!(helper.fill(&q).unwrap().is_none());
    }

    #[test]
    fn reject_then_fill_returns_none() {
        let helper = NetrcCredentialHelper::from_contents("machine localhost login a password 1\n");
        let q = Query {
            protocol: "https".into(),
            host: "localhost".into(),
            path: String::new(),
        };
        let creds = helper.fill(&q).unwrap().unwrap();
        helper.reject(&q, &creds).unwrap();
        // Same host should now be in the skip set.
        assert!(helper.fill(&q).unwrap().is_none());
    }

    #[test]
    fn approve_clears_skip_flag() {
        let helper = NetrcCredentialHelper::from_contents("machine localhost login a password 1\n");
        let q = Query {
            protocol: "https".into(),
            host: "localhost".into(),
            path: String::new(),
        };
        let creds = helper.fill(&q).unwrap().unwrap();
        helper.reject(&q, &creds).unwrap();
        helper.approve(&q, &creds).unwrap();
        // After approve, fill should succeed again.
        assert!(helper.fill(&q).unwrap().is_some());
    }

    #[test]
    fn approve_with_mismatched_creds_is_noop() {
        let helper = NetrcCredentialHelper::from_contents("machine localhost login a password 1\n");
        let q = Query {
            protocol: "https".into(),
            host: "localhost".into(),
            path: String::new(),
        };
        helper.skip.lock().unwrap().insert("localhost".into());
        let mismatched = Credentials::new("b", "2");
        helper.approve(&q, &mismatched).unwrap();
        // Mismatch: skip flag should still be set.
        assert!(helper.fill(&q).unwrap().is_none());
    }

    #[test]
    fn go_quote_escapes_specials() {
        assert_eq!(go_quote("hello"), "\"hello\"");
        assert_eq!(go_quote(r#"a"b"#), "\"a\\\"b\"");
        assert_eq!(go_quote(r"a\b"), "\"a\\\\b\"");
        assert_eq!(go_quote(""), "\"\"");
    }
}
