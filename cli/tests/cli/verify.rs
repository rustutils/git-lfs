//! `t-verify.sh`.
//!
//! For repositories named `verify-fail-N-times`, the server fails the
//! first N verify requests.

use crate::harness::{Run, Test, oid};

/// Push `send-verify-action` in the remote repository `name`, with `config`
/// set first, traced.
fn push(name: &str, config: &[(&str, &str)]) -> Run {
    let t = Test::new();
    let repo = t.remote_and_clone(name);
    for (key, value) in config {
        repo.git(["config", key, value]).ok();
    }
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.write("a.dat", "send-verify-action");
    repo.git(["add", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();
    repo.command("git")
        .args(["push", "origin", "main"])
        .env("GIT_TRACE", "1")
        .env("GIT_CURL_VERBOSE", "1")
        .merged()
        .run()
}

/// The line for each verify attempt.
fn attempt() -> String {
    format!("verify {} attempt", &oid("send-verify-action")[..7])
}

/// The credentials are masked in the trace.
#[track_caller]
fn assert_verified_after(run: Run, attempts: usize) {
    let run = run.ok();
    let auth: Vec<String> = run
        .output()
        .lines()
        .filter(|l| l.contains("Authorization: Basic"))
        .map(str::to_owned)
        .collect();
    assert!(!auth.is_empty(), "{run}");
    for line in auth {
        assert!(line.ends_with("Authorization: Basic * * * * *"), "{line}");
    }
    assert_eq!(run.count(&attempt()), attempts, "{run}");
}

#[test]
fn verify_with_retries() {
    assert_verified_after(push("verify-fail-2-times", &[]), 2);
}

#[test]
fn verify_with_retries_success_without_retry() {
    assert_verified_after(push("verify-fail-0-times", &[]), 1);
}

/// It gives up after three attempts.
#[test]
fn verify_with_retries_insufficient_retries() {
    let run = push("verify-fail-10-times", &[]).fails();
    assert_eq!(run.count(&attempt()), 3, "{run}");
}

/// An invalid `lfs.transfer.maxverifies` means the default, 3.
#[test]
fn verify_with_retries_bad_gitconfig() {
    let run = push(
        "bad-config-verify-fail-2-times",
        &[("lfs.transfer.maxverifies", "-1")],
    );
    assert_verified_after(run, 2);
}
