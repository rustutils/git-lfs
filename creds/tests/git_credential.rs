//! End-to-end tests for [`GitCredentialHelper`] driven by a fake `git`
//! binary (a shell script).
//!
//! We script the fake `git` to either echo back canned creds for `fill`,
//! exit with the documented "no creds" status (128), or record its stdin
//! to a file we then assert against. This mirrors what real `git
//! credential` exposes without requiring a configured helper on the test
//! machine.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use git_lfs_creds::{Credentials, GitCredentialHelper, Helper, HelperError, Query};
use tempfile::TempDir;

/// Drop a script at `dir/git` that runs `body` and exits. Returns the
/// path to the script for passing to [`GitCredentialHelper::with_program`].
fn install_fake_git(dir: &Path, body: &str) -> String {
    let path = dir.join("git");
    let mut f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o755)
        .open(&path)
        .unwrap();
    f.write_all(body.as_bytes()).unwrap();
    f.sync_all().unwrap();
    drop(f);
    path.to_string_lossy().into_owned()
}

/// Wrap `op` in a small retry loop that swallows Linux's `ETXTBSY` /
/// "Text file busy" — Linux can briefly refuse to `exec` a file that was
/// written-and-closed moments earlier, even from a different fd. Real
/// users of `GitCredentialHelper` exec long-installed `git` binaries
/// where this isn't a problem; the race only shows up in the test's
/// write-script-then-exec-it pattern.
fn with_etxtbsy_retry<T>(mut op: impl FnMut() -> Result<T, HelperError>) -> Result<T, HelperError> {
    let mut delay_ms = 10;
    for _ in 0..6 {
        match op() {
            Err(HelperError::Io(e)) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                delay_ms *= 2;
            }
            other => return other,
        }
    }
    op()
}

fn q() -> Query {
    Query {
        protocol: "https".into(),
        host: "git.example.com".into(),
        path: String::new(),
    }
}

#[test]
fn fill_parses_credentials_from_fake_git() {
    let tmp = TempDir::new().unwrap();
    // The fake `git` ignores stdin and prints a canned response.
    let git = install_fake_git(
        tmp.path(),
        "#!/bin/sh\n\
         cat > /dev/null\n\
         printf 'protocol=https\\nhost=git.example.com\\nusername=alice\\npassword=hunter2\\n'\n",
    );
    let h = GitCredentialHelper::with_program(git);
    assert_eq!(
        with_etxtbsy_retry(|| h.fill(&q())).unwrap(),
        Some(Credentials::new("alice", "hunter2")),
    );
}

#[test]
fn fill_returns_none_when_git_exits_128() {
    let tmp = TempDir::new().unwrap();
    let git = install_fake_git(
        tmp.path(),
        "#!/bin/sh\n\
         cat > /dev/null\n\
         exit 128\n",
    );
    let h = GitCredentialHelper::with_program(git);
    assert_eq!(with_etxtbsy_retry(|| h.fill(&q())).unwrap(), None);
}

#[test]
fn fill_writes_protocol_host_to_stdin() {
    let tmp = TempDir::new().unwrap();
    let input_log = tmp.path().join("input").to_string_lossy().into_owned();
    let git = install_fake_git(
        tmp.path(),
        &format!(
            "#!/bin/sh\n\
             cat > {input_log}\n\
             printf 'username=u\\npassword=p\\n'\n",
        ),
    );
    let h = GitCredentialHelper::with_program(git);
    with_etxtbsy_retry(|| h.fill(&q())).unwrap();

    let captured = fs::read_to_string(&input_log).unwrap();
    assert!(captured.contains("protocol=https\n"));
    assert!(captured.contains("host=git.example.com\n"));
    // Trailing blank line per git-credential protocol.
    assert!(captured.ends_with("\n\n"));
}

#[test]
fn approve_passes_credentials_to_git() {
    let tmp = TempDir::new().unwrap();
    let input_log = tmp.path().join("input").to_string_lossy().into_owned();
    let git = install_fake_git(tmp.path(), &format!("#!/bin/sh\ncat > {input_log}\n"));
    let h = GitCredentialHelper::with_program(git);
    with_etxtbsy_retry(|| h.approve(&q(), &Credentials::new("alice", "hunter2"))).unwrap();

    let captured = fs::read_to_string(&input_log).unwrap();
    assert!(captured.contains("username=alice\n"));
    assert!(captured.contains("password=hunter2\n"));
}

#[test]
fn nonzero_exit_other_than_128_is_an_error() {
    let tmp = TempDir::new().unwrap();
    let git = install_fake_git(tmp.path(), "#!/bin/sh\ncat > /dev/null\nexit 1\n");
    let h = GitCredentialHelper::with_program(git);
    assert!(with_etxtbsy_retry(|| h.fill(&q())).is_err());
}
