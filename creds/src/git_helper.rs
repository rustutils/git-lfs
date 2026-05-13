//! `git credential fill/approve/reject` bridge.
//!
//! The wire protocol is documented at <https://git-scm.com/docs/git-credential>:
//!
//! * `git credential fill` reads `key=value` lines on stdin terminated by a
//!   blank line, and writes the same on stdout (with `username` /
//!   `password` filled in).
//! * `git credential approve` and `reject` take the same key/value input
//!   and produce no useful stdout.
//!
//! For now we only emit `protocol`, `host`, and (optionally) `path` —
//! upstream LFS also passes `wwwauth[]` and `state[]` for multi-stage
//! authentication, which is on the deferred list.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::helper::{Credentials, Helper, HelperError};
use crate::query::Query;

/// Shells out to the `git` binary's credential subsystem.
///
/// `git_program` defaults to `"git"`. Override for tests via
/// [`Self::with_program`] — point at a fake `git` that records its input
/// or scripts a response.
///
/// `protect_protocol` mirrors `credential.protectProtocol` (default
/// `true`). When set, every value written to `git credential` stdin is
/// rejected if it contains a carriage return — preventing newline-based
/// protocol smuggling. Newlines and null bytes are rejected
/// unconditionally regardless of this flag.
#[derive(Debug, Clone)]
pub struct GitCredentialHelper {
    git_program: String,
    protect_protocol: bool,
}

impl Default for GitCredentialHelper {
    fn default() -> Self {
        Self {
            git_program: "git".to_owned(),
            protect_protocol: true,
        }
    }
}

impl GitCredentialHelper {
    /// Build a helper that shells out to the system `git` binary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the helper around a custom `git`-compatible binary.
    ///
    /// Used by the integration tests, which point at a shell script
    /// that fakes the protocol.
    pub fn with_program(git_program: impl Into<String>) -> Self {
        Self {
            git_program: git_program.into(),
            protect_protocol: true,
        }
    }

    /// Toggle `credential.protectProtocol` for this helper.
    ///
    /// Default is `true`. Pass `false` only when the user has
    /// explicitly opted out; carriage returns in URLs are otherwise
    /// a known smuggling vector.
    pub fn with_protect_protocol(mut self, protect: bool) -> Self {
        self.protect_protocol = protect;
        self
    }

    fn run(&self, subcommand: &str, query: &Query) -> Result<String, HelperError> {
        // Upstream's `creds: git credential <sub> (%q, %q, %q)` trace
        // at `creds/creds.go:328`. Go's %q quotes the string with the
        // double-quote form Rust's {:?} produces, so a hex-clean
        // protocol/host/path round-trips byte-for-byte.
        // `t-credentials-no-prompt.sh::askpass: push with bad askpass`
        // greps for `creds: git credential fill`.
        {
            let mut e = std::io::stderr().lock();
            let _ = writeln!(
                e,
                "creds: git credential {subcommand} ({:?}, {:?}, {:?})",
                query.protocol, query.host, query.path,
            );
        }
        let mut child = Command::new(&self.git_program)
            .args(["credential", subcommand])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        {
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| HelperError::Failed("git stdin unavailable".into()))?;
            write_input(stdin, query, None, self.protect_protocol)?;
        }

        let out = child.wait_with_output()?;
        if !out.status.success() {
            // `git credential fill` exits 128 when no helper is configured
            // to provide credentials. Treat that as "I don't know" rather
            // than a hard failure so the chain can fall through.
            if subcommand == "fill" && out.status.code() == Some(128) {
                return Ok(String::new());
            }
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            return Err(HelperError::Failed(format!(
                "git credential {subcommand} exited {}: {stderr}",
                out.status,
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

impl Helper for GitCredentialHelper {
    fn fill(&self, query: &Query) -> Result<Option<Credentials>, HelperError> {
        let stdout = self.run("fill", query)?;
        Ok(parse_response(&stdout))
    }

    fn approve(&self, query: &Query, creds: &Credentials) -> Result<(), HelperError> {
        let mut child = spawn(&self.git_program, "approve")?;
        if let Some(stdin) = child.stdin.as_mut() {
            write_input(stdin, query, Some(creds), self.protect_protocol)?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(HelperError::Failed(format!(
                "git credential approve exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim(),
            )));
        }
        Ok(())
    }

    fn reject(&self, query: &Query, creds: &Credentials) -> Result<(), HelperError> {
        let mut child = spawn(&self.git_program, "reject")?;
        if let Some(stdin) = child.stdin.as_mut() {
            write_input(stdin, query, Some(creds), self.protect_protocol)?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(HelperError::Failed(format!(
                "git credential reject exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim(),
            )));
        }
        Ok(())
    }
}

fn spawn(program: &str, subcommand: &str) -> Result<std::process::Child, HelperError> {
    Command::new(program)
        .args(["credential", subcommand])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(HelperError::Io)
}

fn write_input(
    sink: &mut impl Write,
    query: &Query,
    creds: Option<&Credentials>,
    protect_protocol: bool,
) -> Result<(), HelperError> {
    write_field(sink, "protocol", &query.protocol, protect_protocol)?;
    write_field(sink, "host", &query.host, protect_protocol)?;
    write_field(sink, "path", &query.path, protect_protocol)?;
    if let Some(c) = creds {
        write_field(sink, "username", &c.username, protect_protocol)?;
        // password is required for approve/reject to be meaningful, even
        // if empty — let git decide. Empty values still get a key= line.
        validate_value("password", &c.password, protect_protocol)?;
        writeln!(sink, "password={}", c.password)?;
    }
    // Trailing blank line tells git "end of input".
    writeln!(sink)?;
    Ok(())
}

/// Emit a `key=value` line to the credential helper's stdin if `value`
/// is non-empty. Empty fields are skipped to match git-credential's own
/// "absent = no constraint" semantics. Returns an error if `value`
/// contains bytes that would break the line-based protocol.
fn write_field(
    sink: &mut impl Write,
    key: &str,
    value: &str,
    protect_protocol: bool,
) -> Result<(), HelperError> {
    if value.is_empty() {
        return Ok(());
    }
    validate_value(key, value, protect_protocol)?;
    writeln!(sink, "{key}={value}")?;
    Ok(())
}

/// Reject any byte in `value` that would let an attacker inject extra
/// `key=value` lines into the credential-helper stream. Mirrors
/// upstream's `Creds.buffer` checks (`creds/creds.go`):
///
/// - `\n` (newline) and `\0` (null) are rejected unconditionally.
/// - `\r` (carriage return) is rejected when `protect_protocol` is set
///   (the default); disabling it is the documented escape hatch for
///   pre-existing setups whose URLs contain CRs.
///
/// Error wording matches upstream verbatim so `t-credentials-protect`
/// can grep for it.
fn validate_value(key: &str, value: &str, protect_protocol: bool) -> Result<(), HelperError> {
    if value.contains('\n') {
        return Err(HelperError::Failed(format!(
            "credential value for {key} contains newline: {value:?}"
        )));
    }
    if value.contains('\0') {
        return Err(HelperError::Failed(format!(
            "credential value for {key} contains null byte: {value:?}"
        )));
    }
    if protect_protocol && value.contains('\r') {
        return Err(HelperError::Failed(format!(
            "credential value for {key} contains carriage return: {value:?}\n\
             If this is intended, set `credential.protectProtocol=false`"
        )));
    }
    Ok(())
}

/// Parse the `key=value\n…\n` response from `git credential fill`.
///
/// Returns `None` if no password was provided — git can return a partial
/// response (e.g. just a username) when a helper bails halfway through;
/// a usable credential needs at least the password.
fn parse_response(stdout: &str) -> Option<Credentials> {
    let mut username = String::new();
    let mut password: Option<String> = None;
    for line in stdout.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k {
            "username" => username = v.to_owned(),
            "password" => password = Some(v.to_owned()),
            _ => {}
        }
    }
    password.map(|p| Credentials::new(username, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_extracts_credentials() {
        let out = "protocol=https\nhost=git.example.com\nusername=alice\npassword=hunter2\n";
        assert_eq!(
            parse_response(out),
            Some(Credentials::new("alice", "hunter2")),
        );
    }

    #[test]
    fn parse_response_returns_none_without_password() {
        // `git credential fill` can spit out everything but a password if
        // every helper bailed. A username-only response isn't usable.
        let out = "protocol=https\nhost=git.example.com\nusername=alice\n";
        assert_eq!(parse_response(out), None);
    }

    #[test]
    fn parse_response_allows_empty_username_with_token_password() {
        let out = "password=ghp_token\n";
        assert_eq!(parse_response(out), Some(Credentials::new("", "ghp_token")));
    }

    #[test]
    fn write_input_rejects_newline_in_path() {
        let q = Query {
            protocol: "https".into(),
            host: "h.example".into(),
            path: "evil\nrepo".into(),
        };
        let mut buf = Vec::new();
        let err = write_input(&mut buf, &q, None, true).unwrap_err();
        assert!(
            matches!(&err, HelperError::Failed(m) if m.contains("contains newline")),
            "got {err:?}"
        );
    }

    #[test]
    fn write_input_rejects_null_byte_even_when_protection_off() {
        let q = Query {
            protocol: "https".into(),
            host: "h.example".into(),
            path: "evil\0repo".into(),
        };
        let mut buf = Vec::new();
        let err = write_input(&mut buf, &q, None, false).unwrap_err();
        assert!(
            matches!(&err, HelperError::Failed(m) if m.contains("contains null byte")),
            "got {err:?}"
        );
    }

    #[test]
    fn write_input_rejects_carriage_return_by_default() {
        let q = Query {
            protocol: "https".into(),
            host: "h.example".into(),
            path: "evil\rrepo".into(),
        };
        let mut buf = Vec::new();
        let err = write_input(&mut buf, &q, None, true).unwrap_err();
        assert!(
            matches!(&err, HelperError::Failed(m) if m.contains("contains carriage return")),
            "got {err:?}"
        );
    }

    #[test]
    fn write_input_allows_carriage_return_when_protection_off() {
        let q = Query {
            protocol: "https".into(),
            host: "h.example".into(),
            path: "evil\rrepo".into(),
        };
        let mut buf = Vec::new();
        write_input(&mut buf, &q, None, false).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("path=evil\rrepo\n"));
    }

    #[test]
    fn write_input_skips_empty_fields() {
        let q = Query {
            protocol: "https".into(),
            host: "h.example".into(),
            path: String::new(),
        };
        let mut buf = Vec::new();
        write_input(&mut buf, &q, None, true).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.contains("path="));
        assert!(s.contains("protocol=https\n"));
        assert!(s.contains("host=h.example\n"));
    }
}
