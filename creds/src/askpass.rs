//! Prompt for credentials with an askpass program.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::helper::{Credentials, Helper, HelperError, HelperOutcome};
use crate::query::Query;

/// A [`Helper`] that asks an askpass program for the username and password.
///
/// The program runs once per field, with the prompt `Username for "<url>"`
/// or `Password for "<url>"` as its last argument, and prints the answer on
/// stdout. If it exits non-zero or writes anything to stderr, fill fails. An
/// empty answer makes fill return `Ok(None)`. Approve and reject do nothing.
///
/// Before each run, `creds: filling with GIT_ASKPASS: <command>` is printed
/// to stderr, even without `GIT_TRACE`.
///
/// The Git LFS CLI takes the program from `GIT_ASKPASS`, `core.askpass`, or
/// `SSH_ASKPASS`, in that order, and uses this helper only when no
/// `credential.helper` is configured.
#[derive(Debug, Clone)]
pub struct AskpassHelper {
    program: String,
}

impl AskpassHelper {
    /// Build a helper that runs `program`.
    ///
    /// `program` is split on whitespace: the first word is the executable
    /// and the rest are arguments before the prompt. A path containing
    /// spaces can't be used.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn spawn(&self, prompt: &str) -> Result<String, HelperError> {
        let mut parts = self.program.split_whitespace();
        let prog = parts
            .next()
            .ok_or_else(|| HelperError::Failed("askpass program is empty".into()))?;
        let mut args: Vec<&str> = parts.collect();
        args.push(prompt);

        let mut e = std::io::stderr().lock();
        let _ = write!(e, "creds: filling with GIT_ASKPASS: {prog}");
        for a in &args {
            let _ = write!(e, " {a}");
        }
        let _ = writeln!(e);
        drop(e);

        let out = match Command::new(prog)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
        {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // t-credentials-no-prompt.sh greps for this line.
                let mut e2 = std::io::stderr().lock();
                let _ = writeln!(e2, "creds: failed to find GIT_ASKPASS command: {prog}");
                return Err(e.into());
            }
            Err(e) => return Err(e.into()),
        };
        if !out.status.success() {
            return Err(HelperError::Failed(format!(
                "askpass {prog:?} exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim(),
            )));
        }
        // Like upstream, any stderr output means failure.
        if !out.stderr.is_empty() {
            return Err(HelperError::Failed(
                String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
}

impl Helper for AskpassHelper {
    fn fill(&self, query: &Query) -> Result<Option<Credentials>, HelperError> {
        // The password prompt's URL includes the username, as upstream's
        // does.
        let bare_url = format_url(query, None);
        let username = self.spawn(&format!("Username for \"{bare_url}\""))?;
        if username.is_empty() {
            return Ok(None);
        }
        let user_url = format_url(query, Some(&username));
        let password = self.spawn(&format!("Password for \"{user_url}\""))?;
        if password.is_empty() {
            return Ok(None);
        }
        Ok(Some(Credentials::new(username, password)))
    }

    fn approve(&self, _query: &Query, _creds: &Credentials) -> Result<HelperOutcome, HelperError> {
        Ok(HelperOutcome::Continue)
    }

    fn reject(&self, _query: &Query, _creds: &Credentials) -> Result<HelperOutcome, HelperError> {
        Ok(HelperOutcome::Continue)
    }
}

/// Format `query` as a URL for a prompt, with `username` before the host
/// if given.
fn format_url(query: &Query, username: Option<&str>) -> String {
    let mut s = String::with_capacity(query.host.len() + query.path.len() + 16);
    s.push_str(&query.protocol);
    s.push_str("://");
    if let Some(u) = username {
        s.push_str(u);
        s.push('@');
    }
    s.push_str(&query.host);
    if !query.path.is_empty() {
        s.push('/');
        s.push_str(&query.path);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_url_no_username() {
        let q = Query {
            protocol: "https".into(),
            host: "git.example.com".into(),
            path: "foo/bar.git".into(),
        };
        assert_eq!(format_url(&q, None), "https://git.example.com/foo/bar.git");
    }

    #[test]
    fn format_url_with_username() {
        let q = Query {
            protocol: "https".into(),
            host: "git.example.com".into(),
            path: "foo/bar.git".into(),
        };
        assert_eq!(
            format_url(&q, Some("alice")),
            "https://alice@git.example.com/foo/bar.git",
        );
    }

    #[test]
    fn format_url_no_path() {
        let q = Query {
            protocol: "http".into(),
            host: "h:42".into(),
            path: String::new(),
        };
        assert_eq!(format_url(&q, None), "http://h:42");
    }

    #[test]
    fn fill_runs_program_and_returns_credentials() {
        // Answers each prompt differently, so both must have run.
        let tmp = tempfile::TempDir::new().unwrap();
        let prog = tmp.path().join("ask");
        std::fs::write(
            &prog,
            "#!/bin/sh\n\
             case \"$1\" in\n\
               Username*) echo alice;;\n\
               Password*) echo s3cret;;\n\
             esac\n",
        )
        .unwrap();
        let helper = AskpassHelper::new(format!("sh {}", prog.display()));
        let q = Query {
            protocol: "https".into(),
            host: "h.example".into(),
            path: "repo".into(),
        };
        let creds = helper.fill(&q).unwrap().expect("creds");
        assert_eq!(creds.username, "alice");
        assert_eq!(creds.password, "s3cret");
    }

    #[test]
    fn fill_returns_none_on_empty_username() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prog = tmp.path().join("ask");
        std::fs::write(&prog, "#!/bin/sh\necho\n").unwrap();
        let helper = AskpassHelper::new(format!("sh {}", prog.display()));
        let q = Query {
            protocol: "https".into(),
            host: "h.example".into(),
            path: String::new(),
        };
        assert_eq!(helper.fill(&q).unwrap(), None);
    }
}
