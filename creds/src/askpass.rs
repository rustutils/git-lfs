//! `GIT_ASKPASS` / `core.askpass` / `SSH_ASKPASS` credential helper.
//!
//! Spawns the configured program once per credential field, with a single
//! argument formatted as `Username for "<url>"` or `Password for "<url>"`,
//! and reads the result from stdout. The askpass protocol has no
//! approve/reject step — both are no-ops.
//!
//! Selection priority (resolved by the caller before constructing this
//! helper):
//!
//! 1. `GIT_ASKPASS` env var — interactive Git's standard hook.
//! 2. `core.askpass` git config — same idea, persisted in config.
//! 3. `SSH_ASKPASS` env var — last-resort fallback that pre-existed Git.
//!
//! Skipped entirely when `credential.<url>.helper` is set, so callers
//! can keep this slot in the helper chain without it stomping on a
//! purpose-built credential helper. Trace lines (`creds: filling with
//! GIT_ASKPASS: <argv>`) match upstream's wording — `t-askpass.sh`
//! greps them verbatim.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::helper::{Credentials, Helper, HelperError};
use crate::query::Query;

/// Spawns `program` with one argument per call (the prompt string) and
/// reads username/password from stdout.
///
/// `program` is the raw command string — split on whitespace just like
/// upstream's `subprocess.ExecCommand` shells expand it. The first token
/// is the executable; subsequent tokens are passed as additional args
/// before the prompt.
#[derive(Debug, Clone)]
pub struct AskpassHelper {
    program: String,
}

impl AskpassHelper {
    /// Build a helper around the given askpass command.
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

        // Trace line greppable by upstream's shell tests:
        // `creds: filling with GIT_ASKPASS: <prog> <args...>`.
        // Stderr (not stdout) — stdout is reserved for the helper's
        // own protocol output.
        let mut e = std::io::stderr().lock();
        let _ = write!(e, "creds: filling with GIT_ASKPASS: {prog}");
        for a in &args {
            let _ = write!(e, " {a}");
        }
        let _ = writeln!(e);
        drop(e);

        let out = Command::new(prog)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        if !out.status.success() {
            return Err(HelperError::Failed(format!(
                "askpass {prog:?} exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim(),
            )));
        }
        // A non-empty stderr from the askpass program is treated as an
        // error message (matches upstream's `getFromProgram`).
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
        // Prompts mirror upstream byte-for-byte:
        // `Username for "<scheme>://<host>[/<path>]"`
        // `Password for "<scheme>://<username>@<host>[/<path>]"`
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

    /// Askpass has no persistence, so approve is a no-op.
    fn approve(&self, _query: &Query, _creds: &Credentials) -> Result<(), HelperError> {
        Ok(())
    }

    /// Askpass has no persistence, so reject is a no-op.
    fn reject(&self, _query: &Query, _creds: &Credentials) -> Result<(), HelperError> {
        Ok(())
    }
}

/// Build the URL string that goes into the prompt argument. With
/// `username = Some(...)`, the URL is rendered as
/// `<scheme>://<user>@<host>[/<path>]` — same form upstream's
/// `net/url.URL.String()` produces for a `User`-bearing URL.
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
        // Stand-in askpass: a shell script that echoes a fixed value
        // based on the argv so we can verify both prompts ran.
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&prog).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&prog, perms).unwrap();
        }
        let helper = AskpassHelper::new(prog.to_string_lossy().into_owned());
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&prog).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&prog, perms).unwrap();
        }
        let helper = AskpassHelper::new(prog.to_string_lossy().into_owned());
        let q = Query {
            protocol: "https".into(),
            host: "h.example".into(),
            path: String::new(),
        };
        assert_eq!(helper.fill(&q).unwrap(), None);
    }
}
