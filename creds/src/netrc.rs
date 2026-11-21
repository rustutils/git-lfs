//! Credential lookup in `~/.netrc` (or `_netrc` on Windows).
//!
//! Mirrors upstream's `netrcCredentialHelper` in `creds/netrc.go`.
//!
//! # Behavior
//!
//! - `fill` looks up the request host, without its port. It returns the
//!   `login` and `password` of the matching `machine` entry, or of the
//!   `default` entry if no machine matches. Hosts that were rejected are
//!   skipped until they are approved again.
//! - `approve` and `reject` act only when the credentials equal the
//!   netrc entry for the host, so credentials from another helper are
//!   left to the rest of the chain. `reject` makes later fills skip the
//!   host, since the file would keep returning the same wrong password.
//!   `approve` undoes that. The file itself is never written.
//!
//! All three print upstream's `netrc: git credential <command> (…)` trace
//! lines, which `t-credentials.sh` checks for.
//!
//! # Parser
//!
//! Tokens are separated by whitespace, and quoting isn't supported. The
//! keywords are `machine`, `default`, `login`, `password`, `account` and
//! `macdef`, in any case. Other tokens are skipped one at a time, so an
//! unknown keyword and its value are ignored. A `macdef` skips only the
//! macro name. The words of the macro body are then skipped as unknown
//! tokens, unless one of them is a keyword.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::helper::{Credentials, Helper, HelperError, HelperOutcome};
use crate::query::Query;
use crate::trace::trace_enabled;

/// One `machine` or `default` block from a netrc file.
///
/// A `default` block has the machine name `*`.
#[derive(Debug, Clone)]
struct NetrcEntry {
    machine: String,
    login: String,
    password: String,
}

/// Credential helper that reads a netrc file.
///
/// The file is read once, when the helper is built.
#[derive(Debug)]
pub struct NetrcCredentialHelper {
    entries: Vec<NetrcEntry>,
    skip: Mutex<HashSet<String>>,
}

impl NetrcCredentialHelper {
    /// Build from the text of a netrc file.
    pub fn from_contents(content: &str) -> Self {
        Self {
            entries: parse_netrc(content),
            skip: Mutex::new(HashSet::new()),
        }
    }

    /// Read the user's netrc file.
    ///
    /// Reads `$HOME/.netrc`, or on Windows `$HOME/_netrc` if `.netrc`
    /// doesn't exist. Returns `None` if `HOME` isn't set or the file is
    /// missing or can't be read as UTF-8.
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

    /// Read the netrc file at `path`.
    ///
    /// Returns `None` if the file is missing or can't be read as UTF-8.
    /// Parsing itself never fails.
    pub fn from_path(path: &Path) -> Option<Self> {
        let content = std::fs::read_to_string(path).ok()?;
        Some(Self::from_contents(&content))
    }

    /// Find the entry for `host`.
    ///
    /// Returns the first `machine` whose name equals `host` ignoring ASCII
    /// case, or else the first `default` block.
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

    fn approve(&self, query: &Query, creds: &Credentials) -> Result<HelperOutcome, HelperError> {
        let host = strip_port(&query.host);
        let Some(entry) = self.find_machine(host) else {
            return Ok(HelperOutcome::Continue);
        };
        if entry.login != creds.username || entry.password != creds.password {
            // These came from another helper.
            return Ok(HelperOutcome::Continue);
        }
        trace_netrc_simple("approve", &query.protocol, &query.host, &query.path);
        self.skip.lock().unwrap().remove(host);
        Ok(HelperOutcome::Handled)
    }

    fn reject(&self, query: &Query, creds: &Credentials) -> Result<HelperOutcome, HelperError> {
        let host = strip_port(&query.host);
        let Some(entry) = self.find_machine(host) else {
            return Ok(HelperOutcome::Continue);
        };
        if entry.login != creds.username || entry.password != creds.password {
            return Ok(HelperOutcome::Continue);
        }
        trace_netrc_simple("reject", &query.protocol, &query.host, &query.path);
        self.skip.lock().unwrap().insert(host.to_owned());
        Ok(HelperOutcome::Handled)
    }
}

/// Remove a `:port` suffix, so `machine localhost` matches `localhost:8080`.
///
/// Cuts at the last colon. Unlike upstream's `net.SplitHostPort`, this
/// keeps the brackets of an IPv6 address: `[::1]:8080` gives `[::1]`.
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

/// Quote `s` like Go's `%q` verb, for strings without control characters.
///
/// Escapes only `"` and `\`. That covers the URL parts and logins in the
/// trace lines.
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

/// Parse the text of a netrc file into entries.
///
/// See the module docs for the rules.
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
                // Skip the value, so it isn't read as a keyword.
                tokens.next();
            }
            "macdef" => {
                // The body ends at a blank line, which the tokenizer
                // can't see. Skip the name and let the body's words fall
                // through as unknown tokens.
                tokens.next();
            }
            _ => {
                // Unknown token. Its value, if any, is skipped on the
                // next iteration. Upstream's "credentials from netrc
                // with unknown keyword" test relies on this.
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
