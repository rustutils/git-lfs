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
///
/// PATH is augmented with the directory containing the test binary so
/// that `git` can find `git-lfs` when invoking the configured filters
/// (clean / smudge / process). Without this, anything that goes through
/// git's filter machinery — `git checkout` notably — silently no-ops on
/// LFS-tracked files.
fn run_in(cwd: &Path, args: &[&str], input: &[u8]) -> Output {
    let bin_dir = Path::new(BIN).parent().unwrap();
    let path_var = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{path_var}", bin_dir.display());

    let mut child = Command::new(BIN)
        .args(args)
        .current_dir(cwd)
        .env("PATH", new_path)
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
fn smudge_missing_object_without_lfs_url_errors() {
    // No `lfs.url` and no `remote.origin.url` configured + missing object →
    // smudge attempts to fetch, can't resolve an LFS endpoint, and fails
    // with a clear error. Previously (before transfer wiring) this
    // surfaced as ObjectMissing; now it surfaces as a fetch failure that
    // names the unresolved endpoint.
    let repo = fresh_repo();
    let pointer = b"version https://git-lfs.github.com/spec/v1\n\
                    oid sha256:0000000000000000000000000000000000000000000000000000000000000001\n\
                    size 5\n";
    let out = run_in(repo.path(), &["smudge"], pointer);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "no partial output on miss");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LFS endpoint") || stderr.contains("origin"),
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

// ---------- uninstall ----------

#[test]
fn uninstall_local_clears_config_and_removes_hooks() {
    let repo = fresh_repo();
    run_in(repo.path(), &["install", "--local"], b"");
    let out = run_in(repo.path(), &["uninstall", "--local"], b"");
    assert!(
        out.status.success(),
        "uninstall failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Local Git LFS configuration has been removed"),
    );

    for key in ["filter.lfs.clean", "filter.lfs.smudge", "filter.lfs.process", "filter.lfs.required"] {
        assert!(read_local_config(repo.path(), key).is_none(), "{key} still set");
    }
    for hook in ["pre-push", "post-checkout", "post-commit", "post-merge"] {
        assert!(
            !repo.path().join(".git/hooks").join(hook).exists(),
            "hook {hook} still present",
        );
    }
}

#[test]
fn uninstall_is_idempotent_when_nothing_installed() {
    let repo = fresh_repo();
    let out = run_in(repo.path(), &["uninstall", "--local"], b"");
    assert!(
        out.status.success(),
        "uninstall on clean repo failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn uninstall_preserves_user_modified_hooks() {
    let repo = fresh_repo();
    run_in(repo.path(), &["install", "--local"], b"");
    // Replace the pre-push hook with a user-customized version.
    let pre_push = repo.path().join(".git/hooks/pre-push");
    let custom = "#!/bin/sh\necho 'my custom hook'\n";
    std::fs::write(&pre_push, custom).unwrap();

    let out = run_in(repo.path(), &["uninstall", "--local"], b"");
    assert!(out.status.success());

    // Customized hook is left in place; the others (still ours) are gone.
    assert!(pre_push.exists(), "user-modified pre-push was deleted");
    assert_eq!(std::fs::read_to_string(&pre_push).unwrap(), custom);
    assert!(!repo.path().join(".git/hooks/post-checkout").exists());
}

#[test]
fn uninstall_skip_repo_leaves_hooks_alone() {
    let repo = fresh_repo();
    run_in(repo.path(), &["install", "--local"], b"");
    let out = run_in(repo.path(), &["uninstall", "--local", "--skip-repo"], b"");
    assert!(out.status.success());
    // Config gone…
    assert!(read_local_config(repo.path(), "filter.lfs.clean").is_none());
    // …but hooks still present.
    assert!(repo.path().join(".git/hooks/pre-push").exists());
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

// ---------- untrack ----------

#[test]
fn untrack_removes_pattern_and_emits_message() {
    let repo = fresh_repo();
    run_in(repo.path(), &["track", "*.jpg"], b"");
    run_in(repo.path(), &["track", "*.png"], b"");
    let out = run_in(repo.path(), &["untrack", "*.jpg"], b"");
    assert!(
        out.status.success(),
        "untrack failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#"Untracking "*.jpg""#), "unexpected stdout: {stdout}");

    let content = std::fs::read_to_string(repo.path().join(".gitattributes")).unwrap();
    assert!(!content.contains("*.jpg"));
    assert!(content.contains("*.png filter=lfs"));
}

#[test]
fn untrack_unknown_pattern_reports_not_tracked() {
    let repo = fresh_repo();
    run_in(repo.path(), &["track", "*.jpg"], b"");
    let out = run_in(repo.path(), &["untrack", "*.png"], b"");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#""*.png" was not tracked"#), "unexpected stdout: {stdout}");
    // *.jpg still tracked, file unchanged.
    let content = std::fs::read_to_string(repo.path().join(".gitattributes")).unwrap();
    assert_eq!(content, "*.jpg filter=lfs diff=lfs merge=lfs -text\n");
}

#[test]
fn untrack_no_args_errors() {
    let repo = fresh_repo();
    let out = run_in(repo.path(), &["untrack"], b"");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("untrack"), "expected usage hint: {stderr}");
}

#[test]
fn untrack_then_track_round_trips() {
    let repo = fresh_repo();
    run_in(repo.path(), &["track", "*.jpg"], b"");
    run_in(repo.path(), &["untrack", "*.jpg"], b"");
    run_in(repo.path(), &["track", "*.jpg"], b"");
    let content = std::fs::read_to_string(repo.path().join(".gitattributes")).unwrap();
    assert_eq!(content.matches("*.jpg filter=lfs").count(), 1);
}

// ---------- smudge with on-demand download --------------------------------
//
// One end-to-end test that proves the new wiring: lfs.url → batch API →
// basic transfer → store, all driven from the smudge subcommand. Lives
// next to the other smudge tests but uses tokio + wiremock, so it's
// gated as a separate module.

#[tokio::test]
async fn smudge_downloads_missing_object_via_lfs_url() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // SHA-256 of "downloaded\n" — the bytes we'll have wiremock serve.
    const OID: &str = "30031a9831674dd684c3817399acebc88a116ce5a7a3fbc0cf34d92521a534e6";
    const CONTENT: &[u8] = b"downloaded\n";

    let server = MockServer::start().await;
    let storage_url = format!("{}/storage/{OID}", server.uri());

    Mock::given(m_method("POST"))
        .and(m_path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": OID, "size": CONTENT.len(),
                "actions": { "download": { "href": storage_url } }
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(m_method("GET"))
        .and(m_path(format!("/storage/{OID}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(CONTENT))
        .mount(&server)
        .await;

    // Set lfs.url so the fetcher can find the wiremock.
    let repo = fresh_repo();
    let status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["config", "--local", "lfs.url", &server.uri()])
        .status()
        .unwrap();
    assert!(status.success());

    let pointer = format!(
        "version https://git-lfs.github.com/spec/v1\n\
         oid sha256:{OID}\n\
         size {}\n",
        CONTENT.len(),
    );

    // run_in is sync; spawn it on the blocking pool so we don't deadlock
    // the current-thread runtime that wiremock is using.
    let path = repo.path().to_owned();
    let pointer_bytes = pointer.into_bytes();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["smudge"], &pointer_bytes))
        .await
        .unwrap();

    assert!(
        out.status.success(),
        "smudge failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.stdout, CONTENT, "smudge stdout != served bytes");

    // The fetched object should now be in the local store, sharded under
    // .git/lfs/objects/<aa>/<bb>/<full-oid>.
    let stored = repo
        .path()
        .join(".git/lfs/objects")
        .join(&OID[0..2])
        .join(&OID[2..4])
        .join(OID);
    assert!(stored.is_file(), "expected stored object at {stored:?}");
}

#[tokio::test]
async fn smudge_uses_remote_origin_url_when_no_lfs_url_set() {
    // Same wiring as `smudge_downloads_missing_object_via_lfs_url`, but
    // configures `remote.origin.url` instead of `lfs.url` to prove the
    // endpoint resolver derives `<remote>.git/info/lfs` correctly. The
    // wiremock stands in for that derived URL.
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const OID: &str = "30031a9831674dd684c3817399acebc88a116ce5a7a3fbc0cf34d92521a534e6";
    const CONTENT: &[u8] = b"downloaded\n";

    let server = MockServer::start().await;
    let storage_url = format!("{}/storage/{OID}", server.uri());

    // The derived endpoint will tack `.git/info/lfs` onto the remote URL,
    // so the path the batch lands on is `/repo.git/info/lfs/objects/batch`.
    Mock::given(m_method("POST"))
        .and(m_path("/repo.git/info/lfs/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": OID, "size": CONTENT.len(),
                "actions": { "download": { "href": storage_url } }
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(m_method("GET"))
        .and(m_path(format!("/storage/{OID}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(CONTENT))
        .mount(&server)
        .await;

    let repo = fresh_repo();
    let remote_url = format!("{}/repo", server.uri());
    let status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["config", "--local", "remote.origin.url", &remote_url])
        .status()
        .unwrap();
    assert!(status.success());

    let pointer = format!(
        "version https://git-lfs.github.com/spec/v1\n\
         oid sha256:{OID}\n\
         size {}\n",
        CONTENT.len(),
    );

    let path = repo.path().to_owned();
    let pointer_bytes = pointer.into_bytes();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["smudge"], &pointer_bytes))
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "smudge failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.stdout, CONTENT);
}

#[test]
fn smudge_with_no_endpoint_fails_with_clear_message() {
    // Repo has neither `lfs.url` nor `remote.origin.url` — the resolver
    // returns `Unresolved` and the CLI should surface that as a non-zero
    // exit with a sensible message rather than panicking or hanging.
    let repo = fresh_repo();
    let pointer = b"version https://git-lfs.github.com/spec/v1\n\
                    oid sha256:30031a9831674dd684c3817399acebc88a116ce5a7a3fbc0cf34d92521a534e6\n\
                    size 11\n";
    let out = run_in(repo.path(), &["smudge"], pointer);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LFS endpoint") || stderr.contains("origin"),
        "expected endpoint-resolution error in stderr: {stderr}",
    );
}

#[tokio::test]
async fn smudge_401_with_no_credentials_fails_cleanly() {
    // Server demands auth; the configured credential helper chain (in-process
    // cache → `git credential`) has nothing to give in this throwaway repo,
    // so the smudge should propagate the 401 as a non-zero exit instead of
    // hanging or panicking.
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const OID: &str = "30031a9831674dd684c3817399acebc88a116ce5a7a3fbc0cf34d92521a534e6";
    const CONTENT: &[u8] = b"downloaded\n";

    let server = MockServer::start().await;
    Mock::given(m_method("POST"))
        .and(m_path("/objects/batch"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("LFS-Authenticate", "Basic realm=\"x\"")
                .set_body_json(json!({"message": "auth required"})),
        )
        .mount(&server)
        .await;

    let repo = fresh_repo();
    // Point lfs.url at the wiremock and disable the user's real credential
    // helpers so `git credential fill` won't successfully resolve anything
    // (which would happen on a developer machine with a global helper).
    let status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["config", "--local", "lfs.url", &server.uri()])
        .status()
        .unwrap();
    assert!(status.success());

    let pointer = format!(
        "version https://git-lfs.github.com/spec/v1\n\
         oid sha256:{OID}\n\
         size {}\n",
        CONTENT.len(),
    );

    let path = repo.path().to_owned();
    let pointer_bytes = pointer.into_bytes();
    let out = tokio::task::spawn_blocking(move || {
        // GIT_TERMINAL_PROMPT=0 + an empty GIT_CONFIG_GLOBAL stop the
        // user's globally-configured helpers from filling in creds during
        // the test (which would change the response from 401 to 200 and
        // make the assertion meaningless).
        let bin_dir = Path::new(BIN).parent().unwrap();
        let path_var = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{path_var}", bin_dir.display());
        let mut child = Command::new(BIN)
            .args(["smudge"])
            .current_dir(&path)
            .env("PATH", new_path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.as_mut().unwrap().write_all(&pointer_bytes).unwrap();
        drop(child.stdin.take());
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap();

    assert!(
        !out.status.success(),
        "expected smudge to fail with 401; stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ---------- fetch ---------------------------------------------------------

/// Init a repo and configure a deterministic identity so commits work
/// regardless of the developer's git config (or lack thereof).
fn fresh_repo_with_identity() -> TempDir {
    let repo = fresh_repo();
    git_in(repo.path(), &["config", "user.email", "test@example.com"]);
    git_in(repo.path(), &["config", "user.name", "test"]);
    git_in(repo.path(), &["config", "commit.gpgsign", "false"]);
    repo
}

fn git_in(cwd: &Path, args: &[&str]) {
    let status = Command::new("git").arg("-C").arg(cwd).args(args).status().unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Write `pointer_text` to `path` in `repo`, then add+commit.
fn commit_pointer_at(repo: &Path, path: &str, pointer_text: &[u8]) {
    std::fs::write(repo.join(path), pointer_text).unwrap();
    git_in(repo, &["add", path]);
    git_in(repo, &["commit", "-q", "-m", &format!("add {path}")]);
}

fn pointer_text(oid: &str, size: usize) -> Vec<u8> {
    format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n"
    )
    .into_bytes()
}

/// Extract the OID hex from a pointer file's `oid sha256:` line.
fn oid_from_pointer(pointer: &[u8]) -> String {
    let s = std::str::from_utf8(pointer).expect("pointer is utf-8");
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("oid sha256:") {
            return rest.trim().to_owned();
        }
    }
    panic!("no oid line in pointer: {s}");
}

/// `git rev-parse HEAD` for the given repo.
fn head_oid_str(cwd: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(out.status.success(), "rev-parse failed");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[tokio::test]
async fn fetch_downloads_objects_referenced_by_head() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Two distinct LFS objects committed under different paths.
    const OID_A: &str = "30031a9831674dd684c3817399acebc88a116ce5a7a3fbc0cf34d92521a534e6";
    const A: &[u8] = b"downloaded\n";
    // sha256("two\n")
    const OID_B: &str = "27dd8ed44a83ff94d557f9fd0412ed5a8cbca69ea04922d88c01184a07300a5a";
    const B: &[u8] = b"two\n";

    let server = MockServer::start().await;
    let url_a = format!("{}/storage/{OID_A}", server.uri());
    let url_b = format!("{}/storage/{OID_B}", server.uri());

    Mock::given(m_method("POST"))
        .and(m_path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [
                { "oid": OID_A, "size": A.len(), "actions": { "download": { "href": url_a } } },
                { "oid": OID_B, "size": B.len(), "actions": { "download": { "href": url_b } } }
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(m_method("GET"))
        .and(m_path(format!("/storage/{OID_A}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(A))
        .mount(&server)
        .await;
    Mock::given(m_method("GET"))
        .and(m_path(format!("/storage/{OID_B}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(B))
        .mount(&server)
        .await;

    let repo = fresh_repo_with_identity();
    git_in(repo.path(), &["config", "--local", "lfs.url", &server.uri()]);
    commit_pointer_at(repo.path(), "a.bin", &pointer_text(OID_A, A.len()));
    commit_pointer_at(repo.path(), "b.bin", &pointer_text(OID_B, B.len()));

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["fetch"], b""))
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "fetch failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Fetching 2 object(s)"), "unexpected stdout: {stdout}");
    assert!(stdout.contains("2 succeeded, 0 failed"), "unexpected stdout: {stdout}");

    for (oid, _content) in [(OID_A, A), (OID_B, B)] {
        let stored = repo
            .path()
            .join(".git/lfs/objects")
            .join(&oid[0..2])
            .join(&oid[2..4])
            .join(oid);
        assert!(stored.is_file(), "missing stored object: {stored:?}");
    }
}

#[tokio::test]
async fn fetch_is_noop_when_objects_already_in_store() {
    use wiremock::MockServer;

    // Wiremock with no mocks — any HTTP call would 404. We're proving the
    // fetch command short-circuits before hitting the network.
    let server = MockServer::start().await;
    let repo = fresh_repo_with_identity();
    git_in(repo.path(), &["config", "--local", "lfs.url", &server.uri()]);

    // Stage an object in the local store via the clean filter so its
    // OID + content are consistent — same path the smudge tests use.
    let cleaned = run_in(repo.path(), &["clean"], b"already-here\n");
    assert!(cleaned.status.success());
    let pointer_bytes = cleaned.stdout;
    commit_pointer_at(repo.path(), "a.bin", &pointer_bytes);

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["fetch"], b""))
        .await
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Nothing to fetch"), "unexpected stdout: {stdout}");
}

#[tokio::test]
async fn pull_materializes_pointer_files_into_real_content() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Simulates the post-clone state: working tree has pointer text,
    // store is empty, lfs.url is configured. `git lfs pull` should
    // download the object and rewrite the working-tree file.
    const OID: &str = "30031a9831674dd684c3817399acebc88a116ce5a7a3fbc0cf34d92521a534e6";
    const CONTENT: &[u8] = b"downloaded\n";

    let server = MockServer::start().await;
    let storage_url = format!("{}/storage/{OID}", server.uri());

    Mock::given(m_method("POST"))
        .and(m_path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": OID, "size": CONTENT.len(),
                "actions": { "download": { "href": storage_url } }
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(m_method("GET"))
        .and(m_path(format!("/storage/{OID}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(CONTENT))
        .mount(&server)
        .await;

    let repo = fresh_repo_with_identity();
    git_in(repo.path(), &["config", "--local", "lfs.url", &server.uri()]);

    // Commit the pointer text directly. This simulates the post-clone
    // state where the working tree holds pointer text (because clone's
    // smudge was skipped or the store was empty at the time).
    commit_pointer_at(repo.path(), "data.bin", &pointer_text(OID, CONTENT.len()));
    // Sanity: working tree currently has pointer text, not real content.
    let wt_before = std::fs::read(repo.path().join("data.bin")).unwrap();
    assert!(wt_before.starts_with(b"version https://git-lfs.github.com/spec/v1\n"));

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["pull"], b""))
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "pull failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );

    // Working tree now has actual content.
    let wt_after = std::fs::read(repo.path().join("data.bin")).unwrap();
    assert_eq!(wt_after, CONTENT, "working tree not materialized");
}

#[tokio::test]
async fn fetch_returns_failure_exit_when_some_objects_fail() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Server reports the object as missing in the batch response; should
    // not be retried, fetch should exit non-zero.
    const OID: &str = "30031a9831674dd684c3817399acebc88a116ce5a7a3fbc0cf34d92521a534e6";
    const SIZE: usize = 11;

    let server = MockServer::start().await;
    Mock::given(m_method("POST"))
        .and(m_path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": OID, "size": SIZE,
                "error": { "code": 404, "message": "not on server" }
            }]
        })))
        .mount(&server)
        .await;

    let repo = fresh_repo_with_identity();
    git_in(repo.path(), &["config", "--local", "lfs.url", &server.uri()]);
    commit_pointer_at(repo.path(), "a.bin", &pointer_text(OID, SIZE));

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["fetch"], b""))
        .await
        .unwrap();
    assert!(!out.status.success(), "fetch should have exited non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not on server") || stderr.contains("failed to download"),
        "unexpected stderr: {stderr}"
    );
}

// ---------- push ---------------------------------------------------------

#[tokio::test]
async fn push_uploads_only_objects_not_in_remote_tracking() {
    use serde_json::json;
    use wiremock::matchers::{body_bytes, method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Two commits: an "old" pointer (already on the remote) and a
    // "new" pointer (about to be pushed). A fake refs/remotes/origin/main
    // pointing at the first commit tells push that's the remote's state.
    const OLD: &[u8] = b"old payload\n";
    const NEW: &[u8] = b"new payload\n";

    let server = MockServer::start().await;
    let repo = fresh_repo_with_identity();
    git_in(repo.path(), &["config", "--local", "lfs.url", &server.uri()]);

    // Use clean to populate the local store + emit canonical pointer text.
    let cleaned_old = run_in(repo.path(), &["clean"], OLD);
    assert!(cleaned_old.status.success());
    commit_pointer_at(repo.path(), "old.bin", &cleaned_old.stdout);
    let first_commit = head_oid_str(repo.path());

    let cleaned_new = run_in(repo.path(), &["clean"], NEW);
    assert!(cleaned_new.status.success());
    commit_pointer_at(repo.path(), "new.bin", &cleaned_new.stdout);

    let new_oid = oid_from_pointer(&cleaned_new.stdout);
    let old_oid = oid_from_pointer(&cleaned_old.stdout);

    // Fake "origin" tracking ref at the first commit.
    git_in(
        repo.path(),
        &["update-ref", "refs/remotes/origin/main", &first_commit],
    );

    // Batch should only see the NEW oid in the request — and we'll
    // assert that with body_bytes-style matching by checking that
    // wiremock's PUT mock for `old_oid` sees zero hits.
    let upload_url = format!("{}/storage/{new_oid}", server.uri());
    Mock::given(m_method("POST"))
        .and(m_path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": new_oid, "size": NEW.len(),
                "actions": { "upload": { "href": upload_url } }
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(m_method("PUT"))
        .and(m_path(format!("/storage/{new_oid}")))
        .and(body_bytes(NEW))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    // No mock for the OLD oid's storage URL — if push attempts a PUT for
    // it, wiremock returns 404 by default and the test will fail.

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || {
        run_in(&path, &["push", "origin", "HEAD"], b"")
    })
    .await
    .unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Pushing 1 object(s)"), "unexpected stdout: {stdout}");
    assert!(stdout.contains("1 succeeded, 0 failed"), "unexpected stdout: {stdout}");
    assert_ne!(new_oid, old_oid, "test fixture sanity");
}

#[tokio::test]
async fn push_is_noop_when_remote_tracking_matches_head() {
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let repo = fresh_repo_with_identity();
    git_in(repo.path(), &["config", "--local", "lfs.url", &server.uri()]);

    let cleaned = run_in(repo.path(), &["clean"], b"only commit\n");
    commit_pointer_at(repo.path(), "a.bin", &cleaned.stdout);
    let head = head_oid_str(repo.path());
    // Fake remote already at HEAD → nothing new to push.
    git_in(repo.path(), &["update-ref", "refs/remotes/origin/main", &head]);

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || {
        run_in(&path, &["push", "origin", "HEAD"], b"")
    })
    .await
    .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Nothing to push"), "unexpected stdout: {stdout}");
}

#[tokio::test]
async fn pre_push_uploads_new_commit_objects_via_stdin_protocol() {
    use serde_json::json;
    use wiremock::matchers::{body_bytes, method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Two commits: pre-push driven by stdin like git would.
    const OLD: &[u8] = b"old\n";
    const NEW: &[u8] = b"new\n";

    let server = MockServer::start().await;
    let repo = fresh_repo_with_identity();
    git_in(repo.path(), &["config", "--local", "lfs.url", &server.uri()]);

    let cleaned_old = run_in(repo.path(), &["clean"], OLD);
    commit_pointer_at(repo.path(), "old.bin", &cleaned_old.stdout);
    let first_commit = head_oid_str(repo.path());

    let cleaned_new = run_in(repo.path(), &["clean"], NEW);
    commit_pointer_at(repo.path(), "new.bin", &cleaned_new.stdout);
    let head = head_oid_str(repo.path());

    let new_oid = oid_from_pointer(&cleaned_new.stdout);
    let upload_url = format!("{}/storage/{new_oid}", server.uri());

    Mock::given(m_method("POST"))
        .and(m_path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": new_oid, "size": NEW.len(),
                "actions": { "upload": { "href": upload_url } }
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(m_method("PUT"))
        .and(m_path(format!("/storage/{new_oid}")))
        .and(body_bytes(NEW))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    // git's pre-push stdin format: <local-ref> <local-sha> <remote-ref> <remote-sha>
    let stdin = format!("refs/heads/main {head} refs/heads/main {first_commit}\n");
    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || {
        run_in(&path, &["pre-push", "origin", "https://example/dummy"], stdin.as_bytes())
    })
    .await
    .unwrap();
    assert!(
        out.status.success(),
        "pre-push failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[tokio::test]
async fn pre_push_skips_branch_deletes() {
    // Local sha is all zeros → branch delete → nothing to push.
    // No mocks: any HTTP call would 404 and the test would fail.
    use wiremock::MockServer;

    let server = MockServer::start().await;
    let repo = fresh_repo_with_identity();
    git_in(repo.path(), &["config", "--local", "lfs.url", &server.uri()]);
    // Need at least one commit for git rev-parse to work later — but
    // for the pre-push call itself the stdin alone drives behavior.
    let cleaned = run_in(repo.path(), &["clean"], b"x\n");
    commit_pointer_at(repo.path(), "x.bin", &cleaned.stdout);

    let zero = "0000000000000000000000000000000000000000";
    let some_remote = head_oid_str(repo.path());
    let stdin = format!(
        "(delete) {zero} refs/heads/dead {some_remote}\n"
    );
    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || {
        run_in(&path, &["pre-push", "origin", "https://example/dummy"], stdin.as_bytes())
    })
    .await
    .unwrap();
    assert!(
        out.status.success(),
        "pre-push should succeed for delete-only push: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[tokio::test]
async fn pre_push_new_branch_uses_remote_tracking_as_exclude() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Brand-new branch: remote-sha is all zeros. We should fall back
    // to refs/remotes/origin/* as the exclude set. Set up a remote
    // tracking ref at commit 1; only commit 2's object should upload.
    const OLD: &[u8] = b"old payload\n";
    const NEW: &[u8] = b"new payload\n";

    let server = MockServer::start().await;
    let repo = fresh_repo_with_identity();
    git_in(repo.path(), &["config", "--local", "lfs.url", &server.uri()]);

    let cleaned_old = run_in(repo.path(), &["clean"], OLD);
    commit_pointer_at(repo.path(), "old.bin", &cleaned_old.stdout);
    let first_commit = head_oid_str(repo.path());

    let cleaned_new = run_in(repo.path(), &["clean"], NEW);
    commit_pointer_at(repo.path(), "new.bin", &cleaned_new.stdout);
    let head = head_oid_str(repo.path());

    git_in(
        repo.path(),
        &["update-ref", "refs/remotes/origin/main", &first_commit],
    );

    let new_oid = oid_from_pointer(&cleaned_new.stdout);
    let upload_url = format!("{}/storage/{new_oid}", server.uri());

    Mock::given(m_method("POST"))
        .and(m_path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": new_oid, "size": NEW.len(),
                "actions": { "upload": { "href": upload_url } }
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(m_method("PUT"))
        .and(m_path(format!("/storage/{new_oid}")))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    // Push of a new branch (refs/heads/feature) — remote-sha all zeros.
    let zero = "0000000000000000000000000000000000000000";
    let stdin = format!("refs/heads/feature {head} refs/heads/feature {zero}\n");
    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || {
        run_in(&path, &["pre-push", "origin", "https://example/dummy"], stdin.as_bytes())
    })
    .await
    .unwrap();
    assert!(
        out.status.success(),
        "pre-push failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[tokio::test]
async fn pre_push_respects_git_lfs_skip_push_env() {
    use wiremock::MockServer;

    // Even with a real refspec on stdin, GIT_LFS_SKIP_PUSH=1 should
    // make pre-push exit cleanly without scanning or uploading.
    let server = MockServer::start().await;
    let repo = fresh_repo_with_identity();
    git_in(repo.path(), &["config", "--local", "lfs.url", &server.uri()]);
    let cleaned = run_in(repo.path(), &["clean"], b"payload\n");
    commit_pointer_at(repo.path(), "x.bin", &cleaned.stdout);
    let head = head_oid_str(repo.path());

    let zero = "0000000000000000000000000000000000000000";
    let stdin = format!("refs/heads/main {head} refs/heads/main {zero}\n");

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || {
        // run_in already augments PATH. We construct Command directly
        // here to add the env var. Mirrors run_in's PATH handling.
        let bin_dir = Path::new(BIN).parent().unwrap();
        let path_var = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{path_var}", bin_dir.display());
        let mut child = Command::new(BIN)
            .args(["pre-push", "origin", "https://example/dummy"])
            .current_dir(&path)
            .env("PATH", new_path)
            .env("GIT_LFS_SKIP_PUSH", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.as_mut().unwrap().write_all(stdin.as_bytes()).unwrap();
        drop(child.stdin.take());
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[tokio::test]
async fn push_handles_server_already_has_object() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Server returns the object with no `actions` → batch's "I already
    // have this" semantics. Transfer should treat as success without
    // attempting the PUT.
    let server = MockServer::start().await;
    let repo = fresh_repo_with_identity();
    git_in(repo.path(), &["config", "--local", "lfs.url", &server.uri()]);

    let cleaned = run_in(repo.path(), &["clean"], b"already on server\n");
    commit_pointer_at(repo.path(), "x.bin", &cleaned.stdout);
    let oid = oid_from_pointer(&cleaned.stdout);

    Mock::given(m_method("POST"))
        .and(m_path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{ "oid": oid, "size": "already on server\n".len() }]
        })))
        .mount(&server)
        .await;

    // Note: NO mount for any PUT path. If push attempts an upload,
    // wiremock 404s and the test fails.

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || {
        run_in(&path, &["push", "origin", "HEAD"], b"")
    })
    .await
    .unwrap();
    assert!(
        out.status.success(),
        "push failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("1 succeeded, 0 failed"), "unexpected stdout: {stdout}");
}
