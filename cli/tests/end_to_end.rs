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

// ---------- install ----------
//
// All install tests use `--local` so they only touch the test repo's
// `.git/config` and never the developer's `~/.gitconfig`.

/// Read a single config value from the local repo. Helper for assertions.
fn read_local_config(repo: &Path, key: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["config", "--local", "--get", key])
        .output()
        .unwrap();
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        None
    }
}

#[test]
fn install_local_sets_filter_config() {
    let repo = fresh_repo();
    let out = run_in(repo.path(), &["install", "--local"], b"");
    assert!(
        out.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("Git LFS initialized"));

    assert_eq!(
        read_local_config(repo.path(), "filter.lfs.clean").as_deref(),
        Some("git-lfs clean -- %f"),
    );
    assert_eq!(
        read_local_config(repo.path(), "filter.lfs.smudge").as_deref(),
        Some("git-lfs smudge -- %f"),
    );
    assert_eq!(
        read_local_config(repo.path(), "filter.lfs.process").as_deref(),
        Some("git-lfs filter-process"),
    );
    assert_eq!(
        read_local_config(repo.path(), "filter.lfs.required").as_deref(),
        Some("true"),
    );
}

#[test]
fn install_local_writes_executable_hooks() {
    let repo = fresh_repo();
    run_in(repo.path(), &["install", "--local"], b"");

    for hook in ["pre-push", "post-checkout", "post-commit", "post-merge"] {
        let path = repo.path().join(".git/hooks").join(hook);
        assert!(path.is_file(), "missing hook: {path:?}");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("#!/bin/sh\n"));
        assert!(
            content.contains(&format!("git lfs {hook} \"$@\"")),
            "hook {hook} missing dispatch line",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755, "hook {hook} not executable");
        }
    }
}

#[test]
fn install_is_idempotent() {
    let repo = fresh_repo();
    let first = run_in(repo.path(), &["install", "--local"], b"");
    assert!(first.status.success());
    let second = run_in(repo.path(), &["install", "--local"], b"");
    assert!(
        second.status.success(),
        "second install failed: {}",
        String::from_utf8_lossy(&second.stderr),
    );
}

#[test]
fn install_errors_on_conflicting_config_without_force() {
    let repo = fresh_repo();
    // Pre-populate one of the keys with a different value.
    let status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["config", "--local", "filter.lfs.clean", "/usr/local/bin/old-lfs clean"])
        .status()
        .unwrap();
    assert!(status.success());

    let out = run_in(repo.path(), &["install", "--local"], b"");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--force"),
        "stderr should suggest --force: {stderr}"
    );
}

#[test]
fn install_force_overwrites_conflicting_config() {
    let repo = fresh_repo();
    Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["config", "--local", "filter.lfs.clean", "old"])
        .status()
        .unwrap();

    let out = run_in(repo.path(), &["install", "--local", "--force"], b"");
    assert!(out.status.success());
    assert_eq!(
        read_local_config(repo.path(), "filter.lfs.clean").as_deref(),
        Some("git-lfs clean -- %f"),
    );
}

#[test]
fn install_skip_repo_writes_no_hooks() {
    let repo = fresh_repo();
    run_in(repo.path(), &["install", "--local", "--skip-repo"], b"");
    // Config is set, but no hooks were written.
    assert!(read_local_config(repo.path(), "filter.lfs.clean").is_some());
    assert!(!repo.path().join(".git/hooks/pre-push").exists());
}

// ---------- track ----------

#[test]
fn track_creates_gitattributes_and_emits_message() {
    let repo = fresh_repo();
    let out = run_in(repo.path(), &["track", "*.jpg"], b"");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Matches the grep in upstream's t-track.sh: "Tracking \"\*.jpg\"".
    assert!(stdout.contains(r#"Tracking "*.jpg""#), "unexpected stdout: {stdout}");

    let content = std::fs::read_to_string(repo.path().join(".gitattributes")).unwrap();
    assert_eq!(content, "*.jpg filter=lfs diff=lfs merge=lfs -text\n");
}

#[test]
fn track_already_supported_is_idempotent() {
    let repo = fresh_repo();
    run_in(repo.path(), &["track", "*.jpg"], b"");
    let out = run_in(repo.path(), &["track", "*.jpg"], b"");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Matches upstream's grep: "\"*.jpg\" already supported".
    assert!(
        stdout.contains(r#""*.jpg" already supported"#),
        "unexpected stdout: {stdout}",
    );
    let content = std::fs::read_to_string(repo.path().join(".gitattributes")).unwrap();
    assert_eq!(content.matches("*.jpg").count(), 1);
}

#[test]
fn track_preserves_existing_gitattributes() {
    let repo = fresh_repo();
    let initial = "* text=auto\n#*.cs diff=csharp\n";
    std::fs::write(repo.path().join(".gitattributes"), initial).unwrap();
    run_in(repo.path(), &["track", "*.jpg"], b"");
    let content = std::fs::read_to_string(repo.path().join(".gitattributes")).unwrap();
    assert!(content.starts_with("* text=auto\n#*.cs diff=csharp\n"));
    assert!(content.contains("*.jpg filter=lfs"));
}

#[test]
fn track_no_args_lists_patterns() {
    let repo = fresh_repo();
    run_in(repo.path(), &["track", "*.jpg"], b"");
    run_in(repo.path(), &["track", "*.png"], b"");
    let out = run_in(repo.path(), &["track"], b"");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Listing tracked patterns"));
    assert!(stdout.contains("*.jpg (.gitattributes)"));
    assert!(stdout.contains("*.png (.gitattributes)"));
}

#[test]
fn track_then_clean_filter_path() {
    // Track a pattern and then clean a matching file's content. This proves
    // the two pieces compose: track sets up .gitattributes, the clean filter
    // turns content into a pointer + populates the store.
    let repo = fresh_repo();
    run_in(repo.path(), &["track", "*.bin"], b"");
    let out = run_in(repo.path(), &["clean", "data.bin"], b"binary blob");
    assert!(out.status.success());
    assert!(out.stdout.starts_with(b"version https://git-lfs.github.com/spec/v1\n"));
}
