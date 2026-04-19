//! End-to-end tests against the built `git-lfs` binary.
//!
//! These spawn the binary in a fresh git repo via `std::process::Command`,
//! pipe in stdin, and assert on stdout/stderr/exit-status. Mirrors what the
//! upstream shell tests in `tests/t-clean.sh` and `tests/t-smudge.sh` would
//! check, without needing the upstream Go test infrastructure.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_git-lfs");

/// Initialize a fresh git repo in a tempdir and return it.
fn fresh_repo() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .arg(tmp.path())
        .status()
        .unwrap();
    assert!(status.success(), "git init failed");
    tmp
}

/// Run `git-lfs <args>` in `cwd` with `input` on stdin and capture the result.
fn run_in(cwd: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

#[test]
fn clean_smudge_round_trip() {
    let repo = fresh_repo();
    let content = b"hello world\n";

    let cleaned = run_in(repo.path(), &["clean"], content);
    assert!(
        cleaned.status.success(),
        "clean failed: {}",
        String::from_utf8_lossy(&cleaned.stderr),
    );
    let pointer = cleaned.stdout;
    assert!(pointer.starts_with(b"version https://git-lfs.github.com/spec/v1\n"));

    let smudged = run_in(repo.path(), &["smudge"], &pointer);
    assert!(
        smudged.status.success(),
        "smudge failed: {}",
        String::from_utf8_lossy(&smudged.stderr),
    );
    assert_eq!(smudged.stdout, content);
}

#[test]
fn matches_upstream_t_smudge_fixture() {
    // Cross-check against the exact OID/size that upstream's t-smudge.sh uses
    // for "smudge a\n": pointer fcf5015df... 9.
    let repo = fresh_repo();
    let cleaned = run_in(repo.path(), &["clean"], b"smudge a\n");
    let expected = "version https://git-lfs.github.com/spec/v1\n\
                    oid sha256:fcf5015df7a9089a7aa7fe74139d4b8f7d62e52d5a34f9a87aeffc8e8c668254\n\
                    size 9\n";
    assert_eq!(
        String::from_utf8_lossy(&cleaned.stdout),
        expected,
        "pointer encoding diverges from upstream fixture",
    );

    let smudged = run_in(repo.path(), &["smudge"], &cleaned.stdout);
    assert_eq!(smudged.stdout, b"smudge a\n");
}

#[test]
fn clean_writes_object_to_sharded_path() {
    let repo = fresh_repo();
    run_in(repo.path(), &["clean"], b"abc");
    // SHA-256("abc")
    let oid = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let object_path = repo
        .path()
        .join(".git/lfs/objects")
        .join(&oid[0..2])
        .join(&oid[2..4])
        .join(oid);
    assert!(object_path.is_file(), "expected object at {object_path:?}");
}

#[test]
fn clean_passes_through_existing_pointer() {
    // Mirrors t-clean.sh "clean a pointer": piping a pointer through clean
    // emits the same bytes and inserts nothing into the store.
    let repo = fresh_repo();
    let pointer = b"version https://git-lfs.github.com/spec/v1\n\
                    oid sha256:cd293be6cea034bd45a0352775a219ef5dc7825ce55d1f7dae9762d80ce64411\n\
                    size 9\n";
    let out = run_in(repo.path(), &["clean"], pointer);
    assert!(out.status.success());
    assert_eq!(out.stdout, pointer);
    assert!(!repo.path().join(".git/lfs/objects").exists());
}

#[test]
fn smudge_passes_through_non_pointer() {
    // Mirrors t-smudge.sh "smudge with invalid pointer": short non-pointer
    // input flows out unchanged.
    let repo = fresh_repo();
    for input in [&b"wat"[..], b"not a git-lfs file", b"version "] {
        let out = run_in(repo.path(), &["smudge"], input);
        assert!(out.status.success(), "smudge failed for {input:?}");
        assert_eq!(out.stdout, input);
    }
}

#[test]
fn smudge_missing_object_errors() {
    let repo = fresh_repo();
    let pointer = b"version https://git-lfs.github.com/spec/v1\n\
                    oid sha256:0000000000000000000000000000000000000000000000000000000000000001\n\
                    size 5\n";
    let out = run_in(repo.path(), &["smudge"], pointer);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "no partial output on miss");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not present"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn outside_repo_errors() {
    // Not a git repo — `git rev-parse` fails, we should exit 1 with a useful
    // error on stderr (and not write garbage to stdout).
    let tmp = TempDir::new().unwrap();
    let out = run_in(tmp.path(), &["clean"], b"x");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a git repository"),
        "unexpected stderr: {stderr}"
    );
}
