//! Bridge to `git credential fill`, `approve` and `reject`.
//!
//! The protocol is documented at <https://git-scm.com/docs/git-credential>.
//! Each command reads `key=value` lines on stdin, ended by a blank line.
//! `fill` writes the same format on stdout with the credentials filled in.
//! `approve` and `reject` also take the credentials as input.
//!
//! A fill can also carry [`FillContext`](crate::FillContext) lines
//! (`capability[]`, `wwwauth[]`, `state[]`). The API client fills them
//! from the last 401's `Lfs-Authenticate` and `WWW-Authenticate` headers
//! and from the `state[]` the helper returned last time. Multistage
//! credential helpers need them to pick a scheme and resume between fills.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::helper::{Credentials, FillContext, Helper, HelperError, HelperOutcome};
use crate::query::Query;
use crate::trace::trace_enabled;

/// Runs `git credential` to fill, approve and reject credentials.
///
/// Values containing a newline or null byte are never sent, because they
/// could inject extra lines into the protocol. Carriage returns are also
/// refused unless [`with_protect_protocol`](Self::with_protect_protocol)
/// turns that off.
///
/// [`approve`](Helper::approve) and [`reject`](Helper::reject) always
/// return [`HelperOutcome::Handled`], so a
/// [`HelperChain`](crate::HelperChain) stops here.
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

    /// Set whether values containing a carriage return are refused.
    ///
    /// Mirrors `credential.protectProtocol`, which defaults to `true`.
    /// Pass `false` only when the user has set it to false.
    pub fn with_protect_protocol(mut self, protect: bool) -> Self {
        self.protect_protocol = protect;
        self
    }

    fn run(
        &self,
        subcommand: &str,
        query: &Query,
        ctx: Option<&FillContext>,
    ) -> Result<String, HelperError> {
        trace_call(subcommand, query);
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
            write_input(stdin, query, None, ctx, self.protect_protocol)?;
        }

        let out = child.wait_with_output()?;
        if !out.status.success() {
            // `git credential fill` exits 128 when it can't get
            // credentials. Treat that as "none" so the chain moves on.
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
        let stdout = self.run("fill", query, None)?;
        Ok(parse_response(&stdout))
    }

    fn fill_with_context(
        &self,
        query: &Query,
        ctx: &FillContext,
    ) -> Result<Option<Credentials>, HelperError> {
        let stdout = self.run("fill", query, Some(ctx))?;
        Ok(parse_response(&stdout))
    }

    fn approve(&self, query: &Query, creds: &Credentials) -> Result<HelperOutcome, HelperError> {
        trace_call("approve", query);
        let mut child = spawn(&self.git_program, "approve")?;
        if let Some(stdin) = child.stdin.as_mut() {
            write_input(stdin, query, Some(creds), None, self.protect_protocol)?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(HelperError::Failed(format!(
                "git credential approve exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim(),
            )));
        }
        Ok(HelperOutcome::Handled)
    }

    fn reject(&self, query: &Query, creds: &Credentials) -> Result<HelperOutcome, HelperError> {
        trace_call("reject", query);
        let mut child = spawn(&self.git_program, "reject")?;
        if let Some(stdin) = child.stdin.as_mut() {
            write_input(stdin, query, Some(creds), None, self.protect_protocol)?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(HelperError::Failed(format!(
                "git credential reject exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim(),
            )));
        }
        Ok(HelperOutcome::Handled)
    }
}

/// Print upstream's `creds: git credential <sub> (%q, %q, %q)` trace line.
///
/// Only with `GIT_TRACE` set, so commands such as `git lfs lock` keep
/// stderr clean otherwise.
fn trace_call(subcommand: &str, query: &Query) {
    if !trace_enabled() {
        return;
    }
    let mut e = std::io::stderr().lock();
    let _ = writeln!(
        e,
        "creds: git credential {subcommand} ({:?}, {:?}, {:?})",
        query.protocol, query.host, query.path,
    );
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
    ctx: Option<&FillContext>,
    protect_protocol: bool,
) -> Result<(), HelperError> {
    write_field(sink, "protocol", &query.protocol, protect_protocol)?;
    write_field(sink, "host", &query.host, protect_protocol)?;
    write_field(sink, "path", &query.path, protect_protocol)?;
    if let Some(c) = creds {
        if let (Some(at), Some(cred)) = (&c.authtype, &c.credential) {
            // Send back the fields the helper returned, without username
            // and password, like upstream.
            write_field(sink, "authtype", at, protect_protocol)?;
            write_field(sink, "credential", cred, protect_protocol)?;
        } else {
            write_field(sink, "username", &c.username, protect_protocol)?;
            // Always send the password line, even when empty, and let git
            // decide what that means.
            validate_value("password", &c.password, protect_protocol)?;
            writeln!(sink, "password={}", c.password)?;
        }
    }
    if let Some(ctx) = ctx {
        // Same order as upstream: capability[] first so the helper knows
        // what we support, then wwwauth[] and state[]. One line per entry.
        for cap in &ctx.capabilities {
            write_field(sink, "capability[]", cap, protect_protocol)?;
        }
        for w in &ctx.wwwauth {
            write_field(sink, "wwwauth[]", w, protect_protocol)?;
        }
        for s in &ctx.state {
            write_field(sink, "state[]", s, protect_protocol)?;
        }
    }
    // A blank line ends the input.
    writeln!(sink)?;
    Ok(())
}

/// Write a `key=value` line, or nothing if `value` is empty.
///
/// Fails if `value` contains bytes that would break the line-based
/// protocol (see [`validate_value`]).
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

/// Refuse a value that could inject extra lines into the protocol.
///
/// Newlines and null bytes are always refused. Carriage returns are
/// refused when `protect_protocol` is set. The error messages use
/// upstream's wording, which `t-credentials-protect` checks for.
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

/// Parse the `key=value` lines written by `git credential fill`.
///
/// Returns `None` unless the response has a password, or both `authtype`
/// and `credential`. Git can return only a username when no helper had
/// the password.
fn parse_response(stdout: &str) -> Option<Credentials> {
    let mut username = String::new();
    let mut password: Option<String> = None;
    let mut authtype: Option<String> = None;
    let mut credential: Option<String> = None;
    let mut state: Vec<String> = Vec::new();
    let mut multistage = false;
    for line in stdout.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k {
            "username" => username = v.to_owned(),
            "password" => password = Some(v.to_owned()),
            "authtype" => authtype = Some(v.to_owned()),
            "credential" => credential = Some(v.to_owned()),
            "state[]" => state.push(v.to_owned()),
            // Like upstream, only `1` and `true` count as true.
            "continue" => multistage = matches!(v.trim(), "1" | "true"),
            _ => {}
        }
    }
    // When both authtype and credential are present they win over any
    // username and password, as upstream's `Creds.IsAuthtype` does.
    if let (Some(at), Some(cred)) = (authtype, credential) {
        let mut c = Credentials::from_authtype(at, cred);
        c.state = state;
        c.multistage = multistage;
        return Some(c);
    }
    password.map(|p| {
        let mut c = Credentials::new(username, p);
        c.state = state;
        c.multistage = multistage;
        c
    })
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
        // `git credential fill` can return everything but a password if
        // no helper had one. A username-only response isn't usable.
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
        let err = write_input(&mut buf, &q, None, None, true).unwrap_err();
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
        let err = write_input(&mut buf, &q, None, None, false).unwrap_err();
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
        let err = write_input(&mut buf, &q, None, None, true).unwrap_err();
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
        write_input(&mut buf, &q, None, None, false).unwrap();
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
        write_input(&mut buf, &q, None, None, true).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.contains("path="));
        assert!(s.contains("protocol=https\n"));
        assert!(s.contains("host=h.example\n"));
    }
}
