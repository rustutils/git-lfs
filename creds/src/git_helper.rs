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
#[derive(Debug, Clone)]
pub struct GitCredentialHelper {
    git_program: String,
}

impl Default for GitCredentialHelper {
    fn default() -> Self {
        Self {
            git_program: "git".to_owned(),
        }
    }
}

impl GitCredentialHelper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the helper around a custom `git`-compatible binary. Used by
    /// the integration tests, which point at a shell script that fakes
    /// the protocol.
    pub fn with_program(git_program: impl Into<String>) -> Self {
        Self {
            git_program: git_program.into(),
        }
    }

    fn run(&self, subcommand: &str, query: &Query) -> Result<String, HelperError> {
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
            write_input(stdin, query, None)?;
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
            write_input(stdin, query, Some(creds))?;
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
            write_input(stdin, query, Some(creds))?;
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
) -> Result<(), HelperError> {
    if !query.protocol.is_empty() {
        writeln!(sink, "protocol={}", query.protocol)?;
    }
    if !query.host.is_empty() {
        writeln!(sink, "host={}", query.host)?;
    }
    if !query.path.is_empty() {
        writeln!(sink, "path={}", query.path)?;
    }
    if let Some(c) = creds {
        if !c.username.is_empty() {
            writeln!(sink, "username={}", c.username)?;
        }
        // password is required for approve/reject to be meaningful, even if
        // somehow empty — let git decide what to do.
        writeln!(sink, "password={}", c.password)?;
    }
    // Trailing blank line tells git "end of input".
    writeln!(sink)?;
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
}
