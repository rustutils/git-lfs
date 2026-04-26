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
        // Hermetic: ignore the developer's ~/.gitconfig and /etc/gitconfig
        // so behavior doesn't change based on whether the dev has Git LFS
        // (or any other filter) installed globally.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
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

// ---------- ls-files -----------------------------------------------------

/// SHA-256 of `b"hello world\n"` — used in several ls-files / status tests
/// because it's also what `clean` produces for that content, so we can
/// cross-check the marker logic against a real store entry.
const HELLO_OID: &str = "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447";
const HELLO_LEN: usize = 12;

#[test]
fn ls_files_lists_committed_pointer_with_short_oid_and_marker() {
    let repo = fresh_repo_with_identity();
    let p = pointer_text(HELLO_OID, HELLO_LEN);
    commit_pointer_at(repo.path(), "big.bin", &p);

    let out = run_in(repo.path(), &["ls-files"], b"");
    assert!(out.status.success(), "ls-files failed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Format: "<oid10> <marker> <name>". File is on disk at `big.bin` —
    // but it's a *pointer*, not the real 12-byte content, so the marker
    // is `-` (size mismatch).
    assert_eq!(stdout.trim(), format!("{} - big.bin", &HELLO_OID[..10]), "got: {stdout:?}");
}

#[test]
fn ls_files_long_emits_full_oid() {
    let repo = fresh_repo_with_identity();
    commit_pointer_at(repo.path(), "big.bin", &pointer_text(HELLO_OID, HELLO_LEN));

    let out = run_in(repo.path(), &["ls-files", "--long"], b"");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(HELLO_OID), "long form should contain full OID: {stdout}");
}

#[test]
fn ls_files_name_only_emits_just_path() {
    let repo = fresh_repo_with_identity();
    std::fs::create_dir_all(repo.path().join("data")).unwrap();
    commit_pointer_at(repo.path(), "data/blob.bin", &pointer_text(HELLO_OID, HELLO_LEN));

    let out = run_in(repo.path(), &["ls-files", "--name-only"], b"");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "data/blob.bin");
}

#[test]
fn ls_files_size_appends_humanized_bytes() {
    let repo = fresh_repo_with_identity();
    commit_pointer_at(repo.path(), "big.bin", &pointer_text(HELLO_OID, 1_572_864));

    let out = run_in(repo.path(), &["ls-files", "--size"], b"");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // 1.5 MiB → "1.50 MB" with our humanizer.
    assert!(stdout.contains("(1.50 MB)"), "expected size suffix, got: {stdout}");
}

#[test]
fn ls_files_skips_plain_blobs() {
    let repo = fresh_repo_with_identity();
    std::fs::write(repo.path().join("plain.txt"), b"just text").unwrap();
    git_in(repo.path(), &["add", "plain.txt"]);
    git_in(repo.path(), &["commit", "-q", "-m", "plain"]);

    let out = run_in(repo.path(), &["ls-files"], b"");
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "expected empty output, got: {:?}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn ls_files_empty_repo_prints_nothing() {
    // No commits yet — must not panic, must succeed silently.
    let repo = fresh_repo_with_identity();
    let out = run_in(repo.path(), &["ls-files"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty());
}

#[test]
fn ls_files_explicit_ref_walks_that_tree() {
    let repo = fresh_repo_with_identity();
    commit_pointer_at(repo.path(), "first.bin", &pointer_text(HELLO_OID, HELLO_LEN));
    let first = head_oid_str(repo.path());
    // Second commit replaces the pointer with plain content.
    std::fs::write(repo.path().join("first.bin"), b"plain text now").unwrap();
    git_in(repo.path(), &["add", "first.bin"]);
    git_in(repo.path(), &["commit", "-q", "-m", "overwrite"]);

    // At HEAD, no pointers visible.
    let head_out = run_in(repo.path(), &["ls-files"], b"");
    assert!(head_out.stdout.is_empty(), "{:?}", String::from_utf8_lossy(&head_out.stdout));

    // At the older commit, the pointer is visible.
    let old_out = run_in(repo.path(), &["ls-files", &first], b"");
    let stdout = String::from_utf8_lossy(&old_out.stdout);
    assert!(stdout.contains("first.bin"), "expected first.bin in output, got: {stdout}");
}

#[test]
fn ls_files_all_walks_history_across_refs() {
    let repo = fresh_repo_with_identity();
    commit_pointer_at(repo.path(), "first.bin", &pointer_text(HELLO_OID, HELLO_LEN));
    // Replace with plain content. Without --all, ls-files (HEAD tree)
    // would no longer see the pointer.
    std::fs::write(repo.path().join("first.bin"), b"plain text").unwrap();
    git_in(repo.path(), &["add", "first.bin"]);
    git_in(repo.path(), &["commit", "-q", "-m", "overwrite"]);

    let out = run_in(repo.path(), &["ls-files", "--all"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&HELLO_OID[..10]), "--all should resurrect historical pointer: {stdout}");
}

#[test]
fn ls_files_json_is_parseable_and_complete() {
    let repo = fresh_repo_with_identity();
    commit_pointer_at(repo.path(), "x.bin", &pointer_text(HELLO_OID, HELLO_LEN));

    let out = run_in(repo.path(), &["ls-files", "--json"], b"");
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let files = v["files"].as_array().expect("files array");
    assert_eq!(files.len(), 1);
    let f = &files[0];
    assert_eq!(f["name"], "x.bin");
    assert_eq!(f["size"], HELLO_LEN);
    assert_eq!(f["oid"], HELLO_OID);
    assert_eq!(f["oid_type"], "sha256");
    assert_eq!(f["version"], "https://git-lfs.github.com/spec/v1");
    assert_eq!(f["checkout"], false);
    assert_eq!(f["downloaded"], false);
}

#[test]
fn ls_files_debug_emits_per_file_block() {
    let repo = fresh_repo_with_identity();
    commit_pointer_at(repo.path(), "x.bin", &pointer_text(HELLO_OID, HELLO_LEN));

    let out = run_in(repo.path(), &["ls-files", "--debug"], b"");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("filepath: x.bin"), "{stdout}");
    assert!(stdout.contains(&format!("size: {HELLO_LEN}")), "{stdout}");
    assert!(stdout.contains("oid: sha256 "), "{stdout}");
    assert!(stdout.contains("version: https://git-lfs.github.com/spec/v1"), "{stdout}");
}

// ---------- status -------------------------------------------------------

#[test]
fn status_default_lists_staged_and_unstaged_lfs_changes() {
    let repo = fresh_repo_with_identity();
    // Commit a pointer at HEAD so we have something to diff against.
    commit_pointer_at(repo.path(), "x.bin", &pointer_text(HELLO_OID, HELLO_LEN));

    // Stage a modification: new pointer pointing at a different OID.
    let other_oid = "1111111111111111111111111111111111111111111111111111111111111111";
    std::fs::write(repo.path().join("x.bin"), pointer_text(other_oid, 99)).unwrap();
    git_in(repo.path(), &["add", "x.bin"]);

    // Then make an *additional* unstaged modification on top.
    std::fs::write(repo.path().join("x.bin"), pointer_text(other_oid, 12345)).unwrap();

    let out = run_in(repo.path(), &["status"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("On branch "), "{stdout}");
    assert!(stdout.contains("Objects to be committed:"), "{stdout}");
    assert!(stdout.contains("Objects not staged for commit:"), "{stdout}");
    assert!(stdout.contains("x.bin"), "{stdout}");
    assert!(stdout.contains("LFS:"), "expected LFS classification: {stdout}");
}

#[test]
fn status_classifies_non_lfs_blob_as_git() {
    let repo = fresh_repo_with_identity();
    // Plain (non-pointer) blob, modified+staged.
    std::fs::write(repo.path().join("plain.txt"), b"first\n").unwrap();
    git_in(repo.path(), &["add", "plain.txt"]);
    git_in(repo.path(), &["commit", "-q", "-m", "add plain"]);

    std::fs::write(repo.path().join("plain.txt"), b"second\n").unwrap();
    git_in(repo.path(), &["add", "plain.txt"]);

    let out = run_in(repo.path(), &["status"], b"");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("plain.txt"), "{stdout}");
    assert!(stdout.contains("Git:"), "expected Git classification, got: {stdout}");
    assert!(!stdout.contains("LFS:"), "non-pointer should not be LFS: {stdout}");
}

#[test]
fn status_porcelain_one_line_per_change() {
    let repo = fresh_repo_with_identity();
    commit_pointer_at(repo.path(), "x.bin", &pointer_text(HELLO_OID, HELLO_LEN));
    let other = "1111111111111111111111111111111111111111111111111111111111111111";
    std::fs::write(repo.path().join("x.bin"), pointer_text(other, 99)).unwrap();
    git_in(repo.path(), &["add", "x.bin"]);

    let out = run_in(repo.path(), &["status", "--porcelain"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // 'M' modification → " M x.bin" (leading space, single-letter status).
    // trim_end keeps the leading space, which is significant.
    assert_eq!(stdout.trim_end(), " M x.bin", "got: {stdout:?}");
}

#[test]
fn status_json_only_emits_lfs_entries() {
    let repo = fresh_repo_with_identity();
    // Two committed files: one LFS pointer, one plain.
    commit_pointer_at(repo.path(), "p.bin", &pointer_text(HELLO_OID, HELLO_LEN));
    std::fs::write(repo.path().join("plain.txt"), b"first\n").unwrap();
    git_in(repo.path(), &["add", "plain.txt"]);
    git_in(repo.path(), &["commit", "-q", "-m", "add plain"]);

    // Stage a modification to BOTH.
    let other = "2222222222222222222222222222222222222222222222222222222222222222";
    std::fs::write(repo.path().join("p.bin"), pointer_text(other, 99)).unwrap();
    std::fs::write(repo.path().join("plain.txt"), b"second\n").unwrap();
    git_in(repo.path(), &["add", "p.bin", "plain.txt"]);

    let out = run_in(repo.path(), &["status", "--json"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let files = v["files"].as_object().expect("files object");
    // Only the LFS file should appear.
    assert!(files.contains_key("p.bin"), "missing p.bin: {v}");
    assert!(!files.contains_key("plain.txt"), "plain.txt leaked into JSON: {v}");
    assert_eq!(files["p.bin"]["status"], "M");
}

#[test]
fn status_empty_repo_says_no_commits() {
    let repo = fresh_repo_with_identity();
    let out = run_in(repo.path(), &["status"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("No commits yet"), "{stdout}");
}

#[test]
fn status_handles_addition_with_zero_src_sha() {
    // Stage a brand-new pointer file. Diff-index will report src_sha as
    // zero; status must classify the dst side and not crash on the
    // "from" lookup.
    let repo = fresh_repo_with_identity();
    // Need an initial commit so diff-index has a HEAD to compare against.
    std::fs::write(repo.path().join("seed"), b"x").unwrap();
    git_in(repo.path(), &["add", "seed"]);
    git_in(repo.path(), &["commit", "-q", "-m", "seed"]);

    std::fs::write(repo.path().join("new.bin"), pointer_text(HELLO_OID, HELLO_LEN)).unwrap();
    git_in(repo.path(), &["add", "new.bin"]);

    let out = run_in(repo.path(), &["status"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("new.bin"), "{stdout}");
    assert!(stdout.contains("LFS:"), "{stdout}");
}

// ---------- env ----------------------------------------------------------

#[test]
fn env_in_repo_emits_version_paths_and_filter_config() {
    let repo = fresh_repo_with_identity();
    git_in(repo.path(), &["config", "--local", "lfs.url", "https://example.test/lfs"]);
    // Pretend `git lfs install --local` ran by setting one filter
    // explicitly — env should reflect it back.
    git_in(repo.path(), &["config", "--local", "filter.lfs.clean", "git-lfs clean -- %f"]);

    let out = run_in(repo.path(), &["env"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(stdout.starts_with("git-lfs/"), "missing version banner: {stdout}");
    assert!(stdout.contains("git version "), "missing git version: {stdout}");
    assert!(stdout.contains("Endpoint=https://example.test/lfs"), "missing endpoint: {stdout}");
    assert!(stdout.contains("LocalGitDir="), "missing LocalGitDir: {stdout}");
    assert!(stdout.contains("LocalMediaDir="), "missing LocalMediaDir: {stdout}");
    assert!(stdout.contains("TempDir="), "missing TempDir: {stdout}");
    assert!(
        stdout.contains(r#"git config filter.lfs.clean = "git-lfs clean -- %f""#),
        "missing filter config line: {stdout}",
    );
}

#[test]
fn env_outside_repo_skips_repo_specific_lines_and_succeeds() {
    let tmp = TempDir::new().unwrap();
    // Note: NOT a git repo. env should still run.
    let out = run_in(tmp.path(), &["env"], b"");
    assert!(
        out.status.success(),
        "env outside repo should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("git-lfs/"), "version still expected: {stdout}");
    // No repo-specific paths; the filter config block still prints
    // (with empty values) because it's read from global scope.
    assert!(!stdout.contains("LocalGitDir="), "should not emit LocalGitDir: {stdout}");
    assert!(stdout.contains("git config filter.lfs.process ="), "{stdout}");
}

#[test]
fn ls_files_marker_star_when_real_content_present_at_right_size() {
    // Simulate "checkout already happened": commit a pointer in git, but
    // also write the real content (12 bytes of "hello world\n") at the
    // working-tree path. Then the marker should flip from `-` to `*`.
    let repo = fresh_repo_with_identity();
    commit_pointer_at(repo.path(), "x.bin", &pointer_text(HELLO_OID, HELLO_LEN));
    std::fs::write(repo.path().join("x.bin"), b"hello world\n").unwrap();

    let out = run_in(repo.path(), &["ls-files"], b"");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(" * x.bin"), "expected `*` marker, got: {stdout}");
}

// ---------- migrate export -----------------------------------------------
//
// Round-trip: run import to convert plain blobs into pointers, then
// run export to convert them back. The end state of the working tree
// should match the original content, and the .gitattributes lines
// should reflect the un-tracked patterns.

#[test]
fn migrate_export_inverts_import_via_round_trip() {
    let repo = fresh_repo_with_identity();
    let original = vec![b'Z'; 256];
    commit_plain_file(repo.path(), "data.bin", &original);

    // Forward: convert to LFS.
    let imp = run_in(
        repo.path(),
        &["migrate", "import", "--include", "*.bin"],
        b"",
    );
    assert!(imp.status.success(), "import stderr: {}", String::from_utf8_lossy(&imp.stderr));
    let after_import = std::fs::read(repo.path().join("data.bin")).unwrap();
    assert!(
        String::from_utf8_lossy(&after_import).starts_with("version https://"),
        "import should leave pointer text",
    );

    // Inverse: expand pointers back to raw content.
    let exp = run_in(
        repo.path(),
        &["migrate", "export", "--include", "*.bin"],
        b"",
    );
    assert!(exp.status.success(), "export stderr: {}", String::from_utf8_lossy(&exp.stderr));
    let after_export = std::fs::read(repo.path().join("data.bin")).unwrap();
    assert_eq!(after_export, original, "round-trip should restore original bytes");

    // .gitattributes should now declare the path as un-tracked.
    let attrs = std::fs::read_to_string(repo.path().join(".gitattributes")).unwrap();
    assert!(
        attrs.contains("*.bin !text !filter !merge !diff"),
        "expected un-track line: {attrs}",
    );
    // And the original `filter=lfs` line should be gone.
    assert!(
        !attrs.contains("*.bin filter=lfs"),
        "filter=lfs line should be removed: {attrs}",
    );
}

#[test]
fn migrate_export_requires_include() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "x.bin", b"x");

    let out = run_in(repo.path(), &["migrate", "export"], b"");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("requires --include"), "{stderr}");
}

#[test]
fn migrate_export_refuses_dirty_working_tree() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "x.bin", b"x");
    std::fs::write(repo.path().join("x.bin"), b"changed").unwrap();

    let out = run_in(
        repo.path(),
        &["migrate", "export", "--include", "*.bin"],
        b"",
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("uncommitted changes"), "{stderr}");
}

#[test]
fn migrate_export_leaves_pointer_alone_when_object_missing_from_store() {
    // Hand-craft a repo where the working tree contains a pointer
    // file referencing an OID we never put in the store. Export
    // should leave the pointer in place rather than silently
    // truncating it.
    let repo = fresh_repo_with_identity();
    commit_pointer_at(
        repo.path(),
        "missing.bin",
        &pointer_text(HELLO_OID, HELLO_LEN),
    );

    let out = run_in(
        repo.path(),
        &["migrate", "export", "--include", "*.bin"],
        b"",
    );
    assert!(
        out.status.success(),
        "export should still succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Expanded 0 pointer"), "expected zero conversions: {stdout}");
    // Working-tree file is still pointer text — not truncated.
    let bin = std::fs::read(repo.path().join("missing.bin")).unwrap();
    assert!(
        String::from_utf8_lossy(&bin).starts_with("version https://"),
        "missing.bin should still be pointer text",
    );
}

#[test]
fn migrate_export_only_unconverts_paths_matching_include() {
    let repo = fresh_repo_with_identity();
    // Distinct content per file so each gets its own git blob OID —
    // see the "first-reference wins" deferral in NOTES.md for why
    // identical content across paths can't be split selectively.
    commit_plain_file(repo.path(), "convert.bin", &[b'A'; 200]);
    commit_plain_file(repo.path(), "keep.bin", &[b'B'; 200]);

    // Import both into LFS.
    let imp = run_in(
        repo.path(),
        &["migrate", "import", "--include", "*.bin"],
        b"",
    );
    assert!(imp.status.success(), "{}", String::from_utf8_lossy(&imp.stderr));

    // Export only convert.bin back. keep.bin should stay as a pointer.
    let exp = run_in(
        repo.path(),
        &["migrate", "export", "--include", "convert.bin"],
        b"",
    );
    assert!(exp.status.success(), "{}", String::from_utf8_lossy(&exp.stderr));

    let convert = std::fs::read(repo.path().join("convert.bin")).unwrap();
    assert_eq!(convert, vec![b'A'; 200], "convert.bin restored");

    let keep = std::fs::read(repo.path().join("keep.bin")).unwrap();
    assert!(
        String::from_utf8_lossy(&keep).starts_with("version https://"),
        "keep.bin must stay pointer-form: {:?}",
        String::from_utf8_lossy(&keep),
    );
}

// ---------- migrate import -----------------------------------------------

#[test]
fn migrate_import_rewrites_history_so_matching_blobs_become_pointers() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "readme.txt", b"plain text\n");
    commit_plain_file(repo.path(), "data.bin", &vec![b'X'; 256]);

    let out = run_in(
        repo.path(),
        &["migrate", "import", "--include", "*.bin"],
        b"",
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Converted 1 blob"), "{stdout}");

    let bin_after = std::fs::read(repo.path().join("data.bin")).unwrap();
    let s = String::from_utf8_lossy(&bin_after);
    assert!(
        s.starts_with("version https://git-lfs.github.com/spec/v1\n"),
        "data.bin should be pointer text: {s:?}",
    );
    let attrs = std::fs::read_to_string(repo.path().join(".gitattributes")).unwrap();
    assert!(
        attrs.contains("*.bin filter=lfs diff=lfs merge=lfs -text"),
        "{attrs}",
    );
}

#[test]
fn migrate_import_preserves_non_matching_files() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "keep.txt", b"plain content\n");
    commit_plain_file(repo.path(), "data.bin", &[b'X'; 100]);

    let out = run_in(
        repo.path(),
        &["migrate", "import", "--include", "*.bin"],
        b"",
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let txt = std::fs::read(repo.path().join("keep.txt")).unwrap();
    assert_eq!(txt, b"plain content\n");
}

#[test]
fn migrate_import_above_filters_by_size() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "small.bin", &[0u8; 50]);
    commit_plain_file(repo.path(), "large.bin", &vec![0u8; 5_000]);

    let out = run_in(
        repo.path(),
        &["migrate", "import", "--include", "*.bin", "--above", "1k"],
        b"",
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let small = std::fs::read(repo.path().join("small.bin")).unwrap();
    assert_eq!(small.len(), 50, "small.bin should remain plain");

    let large = std::fs::read(repo.path().join("large.bin")).unwrap();
    let s = String::from_utf8_lossy(&large);
    assert!(
        s.starts_with("version https://git-lfs.github.com/spec/v1\n"),
        "large.bin should be pointer text: {s:?}",
    );
}

#[test]
fn migrate_import_refuses_with_no_filter_or_threshold() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "x.bin", b"x");

    let out = run_in(repo.path(), &["migrate", "import"], b"");
    assert!(!out.status.success(), "expected refusal");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("requires --include or --above"),
        "{stderr}",
    );
}

#[test]
fn migrate_import_refuses_dirty_working_tree() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "x.bin", b"x");
    std::fs::write(repo.path().join("x.bin"), b"changed").unwrap();

    let out = run_in(
        repo.path(),
        &["migrate", "import", "--include", "*.bin"],
        b"",
    );
    assert!(!out.status.success(), "expected refusal");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("uncommitted changes"), "{stderr}");
}

#[test]
fn migrate_import_no_rewrite_appends_one_commit() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "x.bin", &[b'X'; 100]);

    let head_before = head_oid_str(repo.path());

    let out = run_in(
        repo.path(),
        &["migrate", "import", "--no-rewrite", "x.bin"],
        b"",
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );

    let head_after = head_oid_str(repo.path());
    assert_ne!(head_before, head_after, "HEAD should advance");

    let bin = std::fs::read(repo.path().join("x.bin")).unwrap();
    let s = String::from_utf8_lossy(&bin);
    assert!(s.starts_with("version https://git-lfs.github.com/spec/v1\n"), "{s:?}");

    let parent_out = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["rev-parse", "HEAD~1"])
        .output()
        .unwrap();
    let parent = String::from_utf8_lossy(&parent_out.stdout).trim().to_owned();
    assert_eq!(parent, head_before);
}

#[test]
fn migrate_import_no_rewrite_skips_already_pointer_files() {
    let repo = fresh_repo_with_identity();
    commit_pointer_at(repo.path(), "x.bin", &pointer_text(HELLO_OID, HELLO_LEN));
    let head_before = head_oid_str(repo.path());

    let out = run_in(
        repo.path(),
        &["migrate", "import", "--no-rewrite", "x.bin"],
        b"",
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Nothing to convert"),
        "expected no-op message: {stdout}",
    );
    let head_after = head_oid_str(repo.path());
    assert_eq!(head_before, head_after, "no commit should be appended");
}

#[test]
fn migrate_import_no_rewrite_requires_paths() {
    let repo = fresh_repo_with_identity();
    let out = run_in(repo.path(), &["migrate", "import", "--no-rewrite"], b"");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("requires one or more paths"), "{stderr}");
}

#[test]
fn migrate_import_writes_lfs_object_to_store() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "data.bin", &[b'Y'; 200]);

    let out = run_in(
        repo.path(),
        &["migrate", "import", "--include", "*.bin"],
        b"",
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    use sha2::{Digest, Sha256};
    let oid_bytes: [u8; 32] = Sha256::digest(vec![b'Y'; 200].as_slice()).into();
    let oid_hex: String = oid_bytes.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    });
    let stored = repo
        .path()
        .join(".git/lfs/objects")
        .join(&oid_hex[0..2])
        .join(&oid_hex[2..4])
        .join(&oid_hex);
    assert!(stored.is_file(), "expected stored object at {stored:?}");
    let content = std::fs::read(&stored).unwrap();
    assert_eq!(content, vec![b'Y'; 200]);
}

// ---------- migrate info -------------------------------------------------

/// Helper: write `path` with `content` and commit it. Used for migrate
/// info fixtures where we don't care about LFS pointer-ness.
fn commit_plain_file(repo: &Path, path: &str, content: &[u8]) {
    if let Some(parent) = std::path::Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(repo.join(parent)).unwrap();
    }
    std::fs::write(repo.join(path), content).unwrap();
    git_in(repo, &["add", path]);
    git_in(repo, &["commit", "-q", "-m", &format!("add {path}")]);
}

#[test]
fn migrate_info_groups_by_extension_and_sorts_by_size() {
    let repo = fresh_repo_with_identity();
    // Two .png files (totaling 30 bytes), one .jpg (5 bytes).
    commit_plain_file(repo.path(), "a.png", &[b'a'; 20]);
    commit_plain_file(repo.path(), "b.png", &[b'b'; 10]);
    commit_plain_file(repo.path(), "c.jpg", &[b'c'; 5]);

    let out = run_in(repo.path(), &["migrate", "info"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let png_pos = stdout.find("*.png").unwrap_or(usize::MAX);
    let jpg_pos = stdout.find("*.jpg").unwrap_or(usize::MAX);
    assert!(png_pos < jpg_pos, "*.png should sort above *.jpg by size: {stdout}");
    assert!(stdout.contains("2/2 files"), "expected png to count 2 files: {stdout}");
}

#[test]
fn migrate_info_above_threshold_excludes_smaller_files() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "small.bin", &[0u8; 50]);
    commit_plain_file(repo.path(), "large.bin", &vec![0u8; 5_000]);

    let out = run_in(repo.path(), &["migrate", "info", "--above", "1k"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Two total .bin, one above threshold.
    assert!(stdout.contains("1/2 files") || stdout.contains("1/1 files"), "{stdout}");
    // The percentage shown should reflect "1 above out of 2 total" =
    // 50%. (If our pipeline only ever sees files matching the filter,
    // it'd report 100% — assert against the wrong-path interpretation.)
    assert!(stdout.contains("50%"), "expected 50% (1/2 above), got: {stdout}");
}

#[test]
fn migrate_info_top_n_caps_extension_rows() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "a.aaa", b"x");
    commit_plain_file(repo.path(), "b.bbb", b"x");
    commit_plain_file(repo.path(), "c.ccc", b"x");

    let out = run_in(repo.path(), &["migrate", "info", "--top", "1"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Exactly one *.<ext> row should appear.
    let ext_lines: Vec<_> = stdout.lines().filter(|l| l.starts_with("*.")).collect();
    assert_eq!(ext_lines.len(), 1, "expected 1 row, got: {ext_lines:?}");
}

#[test]
fn migrate_info_include_filter_restricts_to_matching_paths() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "data.png", b"X");
    commit_plain_file(repo.path(), "other.txt", b"Y");

    let out = run_in(
        repo.path(),
        &["migrate", "info", "--include", "*.png"],
        b"",
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("*.png"), "{stdout}");
    assert!(!stdout.contains("*.txt"), "*.txt should be excluded by include filter: {stdout}");
}

#[test]
fn migrate_info_pointers_follow_buckets_lfs_separately() {
    let repo = fresh_repo_with_identity();
    // Plain files vs. LFS pointer files — pointer should land in the
    // "LFS Objects" bucket, not under *.bin.
    commit_plain_file(repo.path(), "plain.bin", &[b'X'; 100]);
    commit_pointer_at(repo.path(), "pointer.bin", &pointer_text(HELLO_OID, 999_999));

    let out = run_in(repo.path(), &["migrate", "info"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("LFS Objects"), "expected LFS Objects bucket: {stdout}");
    // *.bin should still appear (plain.bin), but with only 1 file (not 2).
    let bin_line = stdout.lines().find(|l| l.starts_with("*.bin"));
    assert!(bin_line.is_some(), "expected *.bin row: {stdout}");
    assert!(bin_line.unwrap().contains("1/1"), "expected only plain.bin in *.bin: {stdout}");
}

#[test]
fn migrate_info_pointers_ignore_drops_lfs_files_entirely() {
    let repo = fresh_repo_with_identity();
    commit_plain_file(repo.path(), "plain.bin", &[b'X'; 100]);
    commit_pointer_at(repo.path(), "pointer.bin", &pointer_text(HELLO_OID, 999_999));

    let out = run_in(repo.path(), &["migrate", "info", "--pointers", "ignore"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("LFS Objects"), "ignore should drop LFS bucket: {stdout}");
}

#[test]
fn migrate_info_pointers_no_follow_treats_pointers_as_regular_blobs() {
    let repo = fresh_repo_with_identity();
    commit_pointer_at(repo.path(), "x.bin", &pointer_text(HELLO_OID, 999_999));

    let out = run_in(
        repo.path(),
        &["migrate", "info", "--pointers", "no-follow"],
        b"",
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    // No special LFS bucket; the pointer's blob is counted under *.bin
    // using the actual blob size on disk (~133 bytes), not the pointer's
    // recorded 999999.
    assert!(!stdout.contains("LFS Objects"), "{stdout}");
    assert!(stdout.contains("*.bin"), "{stdout}");
}

#[test]
fn migrate_info_empty_repo_prints_nothing() {
    let repo = fresh_repo_with_identity();
    let out = run_in(repo.path(), &["migrate", "info"], b"");
    // No commits; HEAD doesn't exist. Should succeed silently.
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty(), "stdout: {:?}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn migrate_info_above_with_invalid_size_errors() {
    let repo = fresh_repo_with_identity();
    let out = run_in(repo.path(), &["migrate", "info", "--above", "garbage"], b"");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("invalid size"), "{stderr}");
}

#[test]
fn migrate_info_unknown_pointers_value_errors() {
    let repo = fresh_repo_with_identity();
    let out = run_in(repo.path(), &["migrate", "info", "--pointers", "yolo"], b"");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown value"), "{stderr}");
}

// ---------- post-checkout / post-commit / post-merge --------------------
//
// v0 ships these as exit-0 stubs. The real reason they exist now: our
// `git lfs install` writes hook scripts that invoke `git lfs
// post-checkout` etc., so without the subcommands every git checkout
// would fail with "unrecognized subcommand". These tests pin the
// argument shapes upstream's hooks expect, so when real lockable
// behavior lands we don't accidentally change the surface.

#[test]
fn post_checkout_accepts_three_args_and_exits_zero() {
    let repo = fresh_repo();
    let out = run_in(
        repo.path(),
        &[
            "post-checkout",
            "0000000000000000000000000000000000000000",
            "1111111111111111111111111111111111111111",
            "1",
        ],
        b"",
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn post_checkout_with_wrong_arg_count_fails() {
    let repo = fresh_repo();
    let out = run_in(repo.path(), &["post-checkout", "only-one-arg"], b"");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("expected 3 args"), "{stderr}");
}

#[test]
fn post_commit_accepts_no_args_and_exits_zero() {
    let repo = fresh_repo();
    let out = run_in(repo.path(), &["post-commit"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn post_merge_accepts_one_arg_and_exits_zero() {
    let repo = fresh_repo();
    let out = run_in(repo.path(), &["post-merge", "0"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn post_merge_with_no_args_fails() {
    let repo = fresh_repo();
    let out = run_in(repo.path(), &["post-merge"], b"");
    assert!(!out.status.success());
}

#[test]
fn install_then_real_git_checkout_does_not_fail_via_post_checkout_hook() {
    // This is the test that justifies the exit-0 stubs. Without them,
    // installing the hooks then running `git checkout -b new-branch`
    // (which fires post-checkout) would error out from inside git.
    let repo = fresh_repo_with_identity();
    install_lfs(repo.path());
    std::fs::write(repo.path().join("a.txt"), b"hi").unwrap();
    git_in(repo.path(), &["add", "a.txt"]);
    git_in(repo.path(), &["commit", "-q", "-m", "seed"]);
    // Trigger the post-checkout hook: switch to a new branch.
    let bin_dir = std::path::Path::new(BIN).parent().unwrap();
    let path_var = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{path_var}", bin_dir.display());
    let status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["checkout", "-b", "new-branch", "-q"])
        .env("PATH", new_path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "git checkout -b failed (post-checkout hook errored?)");
}

// ---------- checkout -----------------------------------------------------

/// `git lfs install --local` so the smudge filter is configured. Without
/// this, checkout returns early with the "not installed" message, which
/// is a useful safety net but obscures the assertions we want to make.
fn install_lfs(repo: &Path) {
    let bin_dir = std::path::Path::new(BIN).parent().unwrap();
    let path_var = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{path_var}", bin_dir.display());
    let status = Command::new(BIN)
        .args(["install", "--local"])
        .current_dir(repo)
        .env("PATH", new_path)
        .status()
        .unwrap();
    assert!(status.success(), "git lfs install --local failed");
}

#[test]
fn checkout_without_install_emits_friendly_message() {
    let repo = fresh_repo_with_identity();
    let oid = put_object_in_store(repo.path(), b"hello world\n");
    commit_pointer_at(repo.path(), "x.bin", &pointer_text(&oid, 12));

    let out = run_in(repo.path(), &["checkout"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Git LFS is not installed"), "{stdout}");
}

#[test]
fn checkout_materializes_pointer_text_into_real_content() {
    let repo = fresh_repo_with_identity();
    install_lfs(repo.path());
    let oid = put_object_in_store(repo.path(), b"hello world\n");
    // Working-tree file is currently the pointer text (just-after-clone state).
    commit_pointer_at(repo.path(), "x.bin", &pointer_text(&oid, 12));

    let before = std::fs::read(repo.path().join("x.bin")).unwrap();
    assert!(before.starts_with(b"version https://git-lfs.github.com/spec/v1\n"));

    let out = run_in(repo.path(), &["checkout"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let after = std::fs::read(repo.path().join("x.bin")).unwrap();
    assert_eq!(after, b"hello world\n");
}

#[test]
fn checkout_with_path_filters_only_those_files() {
    let repo = fresh_repo_with_identity();
    install_lfs(repo.path());
    let oid_a = put_object_in_store(repo.path(), b"alpha bytes\n");
    let oid_b = put_object_in_store(repo.path(), b"beta bytes!\n");
    commit_pointer_at(repo.path(), "a.bin", &pointer_text(&oid_a, 12));
    commit_pointer_at(repo.path(), "b.bin", &pointer_text(&oid_b, 12));

    // Only check out a.bin.
    let out = run_in(repo.path(), &["checkout", "a.bin"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read(repo.path().join("a.bin")).unwrap(), b"alpha bytes\n");
    // b.bin still has its pointer text — we filtered it out.
    let b = std::fs::read(repo.path().join("b.bin")).unwrap();
    assert!(b.starts_with(b"version https://git-lfs.github.com/spec/v1\n"), "b.bin should still be a pointer");
}

#[test]
fn checkout_with_directory_pattern_matches_subtree() {
    let repo = fresh_repo_with_identity();
    install_lfs(repo.path());
    std::fs::create_dir_all(repo.path().join("data")).unwrap();
    let oid_top = put_object_in_store(repo.path(), b"top-level!!!\n");
    let oid_sub = put_object_in_store(repo.path(), b"in subtree!!\n");
    commit_pointer_at(repo.path(), "top.bin", &pointer_text(&oid_top, 13));
    commit_pointer_at(repo.path(), "data/sub.bin", &pointer_text(&oid_sub, 13));

    let out = run_in(repo.path(), &["checkout", "data/"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        std::fs::read(repo.path().join("data/sub.bin")).unwrap(),
        b"in subtree!!\n",
    );
    // top.bin is outside the data/ pattern → still pointer text.
    let top = std::fs::read(repo.path().join("top.bin")).unwrap();
    assert!(top.starts_with(b"version https://git-lfs.github.com/spec/v1\n"));
}

#[test]
fn checkout_no_pointers_says_nothing_to_checkout() {
    let repo = fresh_repo_with_identity();
    install_lfs(repo.path());
    std::fs::write(repo.path().join("plain.txt"), b"plain\n").unwrap();
    git_in(repo.path(), &["add", "plain.txt"]);
    git_in(repo.path(), &["commit", "-q", "-m", "plain"]);

    let out = run_in(repo.path(), &["checkout"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Nothing to checkout"), "{stdout}");
}

#[test]
fn checkout_skips_pointer_when_object_missing_locally() {
    // Pointer references an OID we never put in the store, and there's
    // no remote to fetch from. checkout should leave the pointer text
    // alone rather than truncating the file.
    let repo = fresh_repo_with_identity();
    install_lfs(repo.path());
    commit_pointer_at(repo.path(), "x.bin", &pointer_text(HELLO_OID, HELLO_LEN));

    let out = run_in(repo.path(), &["checkout"], b"");
    // The command exits non-zero because the fetch attempt fails (no remote).
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Whichever way it exits, the file must still be pointer text — never truncated.
    let after = std::fs::read(repo.path().join("x.bin")).unwrap();
    assert!(
        after.starts_with(b"version https://git-lfs.github.com/spec/v1\n"),
        "x.bin should remain pointer text, got: {:?}, stderr: {stderr}",
        String::from_utf8_lossy(&after),
    );
}

// ---------- prune --------------------------------------------------------

#[test]
fn prune_no_objects_says_so() {
    let repo = fresh_repo_with_identity();
    let out = run_in(repo.path(), &["prune"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("No local LFS objects to prune"), "{stdout}");
}

#[test]
fn prune_retains_objects_referenced_by_head_tree() {
    let repo = fresh_repo_with_identity();
    let oid = put_object_in_store(repo.path(), b"hello world\n");
    commit_pointer_at(repo.path(), "x.bin", &pointer_text(&oid, 12));

    let out = run_in(repo.path(), &["prune"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Nothing to prune"), "{stdout}");

    // Object still on disk.
    let path = repo
        .path()
        .join(".git/lfs/objects")
        .join(&oid[0..2])
        .join(&oid[2..4])
        .join(&oid);
    assert!(path.is_file(), "expected object preserved at {path:?}");
}

#[test]
fn prune_deletes_object_not_referenced_anywhere() {
    let repo = fresh_repo_with_identity();
    // Make a HEAD with no LFS pointers (just plain content) — anything
    // in the store is fair game.
    std::fs::write(repo.path().join("plain.txt"), b"hi").unwrap();
    git_in(repo.path(), &["add", "plain.txt"]);
    git_in(repo.path(), &["commit", "-q", "-m", "plain"]);

    let orphan_oid = put_object_in_store(repo.path(), b"orphan content");
    let path = repo
        .path()
        .join(".git/lfs/objects")
        .join(&orphan_oid[0..2])
        .join(&orphan_oid[2..4])
        .join(&orphan_oid);
    assert!(path.is_file(), "fixture pre-condition");

    let out = run_in(repo.path(), &["prune"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Pruning 1 object"), "{stdout}");
    assert!(!path.is_file(), "orphan object should be deleted at {path:?}");
}

#[test]
fn prune_dry_run_does_not_delete() {
    let repo = fresh_repo_with_identity();
    std::fs::write(repo.path().join("plain.txt"), b"hi").unwrap();
    git_in(repo.path(), &["add", "plain.txt"]);
    git_in(repo.path(), &["commit", "-q", "-m", "plain"]);

    let orphan_oid = put_object_in_store(repo.path(), b"orphan");
    let path = repo
        .path()
        .join(".git/lfs/objects")
        .join(&orphan_oid[0..2])
        .join(&orphan_oid[2..4])
        .join(&orphan_oid);

    let out = run_in(repo.path(), &["prune", "--dry-run"], b"");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Would prune 1 object"), "{stdout}");
    // File still there.
    assert!(path.is_file(), "dry-run should not delete: {path:?}");
}

#[test]
fn prune_verbose_lists_each_pruned_object() {
    let repo = fresh_repo_with_identity();
    std::fs::write(repo.path().join("plain.txt"), b"hi").unwrap();
    git_in(repo.path(), &["add", "plain.txt"]);
    git_in(repo.path(), &["commit", "-q", "-m", "plain"]);

    let orphan_oid = put_object_in_store(repo.path(), b"orphan content goes here");

    let out = run_in(repo.path(), &["prune", "--verbose"], b"");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // First 10 chars of the OID appear (full OID actually) on a `* ` line.
    assert!(stdout.contains(&orphan_oid), "expected OID in verbose output: {stdout}");
}

#[test]
fn prune_retains_unpushed_commits() {
    // The pointer is in HEAD but NOT in any old version of HEAD's tree
    // — it's only reachable via the most recent commit, which hasn't
    // been pushed. Prune must retain it.
    let repo = fresh_repo_with_identity();
    // First commit: plain content, simulates "what's on the remote".
    std::fs::write(repo.path().join("plain.txt"), b"hi").unwrap();
    git_in(repo.path(), &["add", "plain.txt"]);
    git_in(repo.path(), &["commit", "-q", "-m", "plain"]);
    // Mark this commit as the remote tip.
    git_in(repo.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);

    // Now add an LFS pointer locally and commit (unpushed).
    let oid = put_object_in_store(repo.path(), b"unpushed content");
    let p = pointer_text(&oid, b"unpushed content".len());
    std::fs::write(repo.path().join("data.bin"), &p).unwrap();
    git_in(repo.path(), &["add", "data.bin"]);
    git_in(repo.path(), &["commit", "-q", "-m", "add data"]);

    // Replace HEAD's data.bin with plain content so HEAD's *tree* no
    // longer references the LFS pointer — but the unpushed commit still
    // does, so we must retain.
    std::fs::write(repo.path().join("data.bin"), b"plain replacement").unwrap();
    git_in(repo.path(), &["add", "data.bin"]);
    git_in(repo.path(), &["commit", "-q", "-m", "replace"]);

    let path = repo
        .path()
        .join(".git/lfs/objects")
        .join(&oid[0..2])
        .join(&oid[2..4])
        .join(&oid);

    let out = run_in(repo.path(), &["prune", "--dry-run"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Nothing to prune"),
        "expected unpushed pointer to be retained, got: {stdout}",
    );
    assert!(path.is_file(), "object must still exist");
}

#[test]
fn prune_with_no_remote_falls_back_to_head_tree_only() {
    // No remote configured — unpushed retain step degrades to
    // "everything in HEAD's tree." An object ONLY in earlier history
    // is then prunable, just like upstream.
    let repo = fresh_repo_with_identity();
    let oid = put_object_in_store(repo.path(), b"old content");
    commit_pointer_at(repo.path(), "old.bin", &pointer_text(&oid, b"old content".len()));
    // Replace with plain content. Earlier commit still references the
    // pointer (history), but HEAD's tree doesn't.
    std::fs::write(repo.path().join("old.bin"), b"plain text").unwrap();
    git_in(repo.path(), &["add", "old.bin"]);
    git_in(repo.path(), &["commit", "-q", "-m", "replace"]);

    let path = repo
        .path()
        .join(".git/lfs/objects")
        .join(&oid[0..2])
        .join(&oid[2..4])
        .join(&oid);
    assert!(path.is_file(), "fixture pre-condition");

    let out = run_in(repo.path(), &["prune"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    // No origin remote → unpushed scan returns the FULL HEAD history
    // (since exclude set is empty), which still references this OID,
    // so it's retained even without a remote.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Nothing to prune"), "{stdout}");
    assert!(path.is_file());
}

// ---------- fsck ---------------------------------------------------------

/// Helper: write the LFS object for `content` directly into a repo's
/// store, sharded under `.git/lfs/objects/<aa>/<bb>/<oid>`. Sidesteps
/// having to wire the clean filter just to populate test fixtures.
fn put_object_in_store(repo: &Path, content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let oid_bytes: [u8; 32] = Sha256::digest(content).into();
    let oid = oid_bytes.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    });
    let dir = repo.join(".git/lfs/objects").join(&oid[0..2]).join(&oid[2..4]);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(&oid), content).unwrap();
    oid
}

#[test]
fn fsck_reports_ok_when_pointers_match_store() {
    let repo = fresh_repo_with_identity();
    let oid = put_object_in_store(repo.path(), b"hello world\n");
    commit_pointer_at(repo.path(), "x.bin", &pointer_text(&oid, 12));

    let out = run_in(repo.path(), &["fsck"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Git LFS fsck OK"), "{stdout}");
}

#[test]
fn fsck_reports_missing_object_and_exits_one() {
    let repo = fresh_repo_with_identity();
    // Pointer references an OID we never put in the store.
    commit_pointer_at(
        repo.path(),
        "missing.bin",
        &pointer_text(HELLO_OID, HELLO_LEN),
    );

    let out = run_in(repo.path(), &["fsck", "--dry-run"], b"");
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("openError"), "expected openError, got: {stdout}");
    assert!(stdout.contains("missing.bin"), "{stdout}");
}

#[test]
fn fsck_reports_corrupt_object_and_quarantines_it() {
    let repo = fresh_repo_with_identity();
    // Put an object whose contents don't match its filename's OID.
    let claimed_oid = HELLO_OID; // OID of "hello world\n"
    let dir = repo
        .path()
        .join(".git/lfs/objects")
        .join(&claimed_oid[0..2])
        .join(&claimed_oid[2..4]);
    std::fs::create_dir_all(&dir).unwrap();
    // Wrong content — would hash to something else entirely.
    std::fs::write(dir.join(claimed_oid), b"wrong content").unwrap();

    commit_pointer_at(
        repo.path(),
        "tamper.bin",
        &pointer_text(claimed_oid, HELLO_LEN),
    );

    let out = run_in(repo.path(), &["fsck"], b"");
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("corruptObject"), "{stdout}");
    assert!(stdout.contains("moving corrupt objects"), "{stdout}");
    // Quarantined file lives at .git/lfs/bad/<oid>.
    let bad = repo.path().join(".git/lfs/bad").join(claimed_oid);
    assert!(bad.is_file(), "expected quarantined file at {bad:?}");
    // Original location is gone.
    assert!(!dir.join(claimed_oid).exists(), "store still has corrupt file");
}

#[test]
fn fsck_dry_run_does_not_quarantine() {
    let repo = fresh_repo_with_identity();
    let claimed_oid = HELLO_OID;
    let dir = repo
        .path()
        .join(".git/lfs/objects")
        .join(&claimed_oid[0..2])
        .join(&claimed_oid[2..4]);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(claimed_oid), b"wrong content").unwrap();
    commit_pointer_at(
        repo.path(),
        "tamper.bin",
        &pointer_text(claimed_oid, HELLO_LEN),
    );

    let out = run_in(repo.path(), &["fsck", "--dry-run"], b"");
    assert_eq!(out.status.code(), Some(1));
    // File is still in the store (not quarantined).
    assert!(dir.join(claimed_oid).is_file(), "dry-run should not move files");
    // No bad/ directory created at all.
    assert!(!repo.path().join(".git/lfs/bad").exists());
}

#[test]
fn fsck_pointers_only_skips_object_check() {
    // A pointer that references a missing object would normally fail
    // `--objects`; with `--pointers` only, we ignore that and only
    // report non-canonical pointers.
    let repo = fresh_repo_with_identity();
    commit_pointer_at(
        repo.path(),
        "missing.bin",
        &pointer_text(HELLO_OID, HELLO_LEN),
    );

    let out = run_in(repo.path(), &["fsck", "--pointers"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Git LFS fsck OK"), "{stdout}");
}

#[test]
fn fsck_pointers_flags_non_canonical_pointer() {
    let repo = fresh_repo_with_identity();
    // Pointer with no trailing newline — parses, isn't canonical.
    let mut p = pointer_text(HELLO_OID, HELLO_LEN);
    assert_eq!(p.last(), Some(&b'\n'));
    p.pop();
    commit_pointer_at(repo.path(), "non.bin", &p);

    let out = run_in(repo.path(), &["fsck", "--pointers"], b"");
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("nonCanonicalPointer"), "{stdout}");
    assert!(stdout.contains("non.bin"), "{stdout}");
}

#[test]
fn fsck_objects_only_skips_pointer_canonicality_check() {
    // A non-canonical pointer should NOT fail --objects; the missing
    // object it references should.
    let repo = fresh_repo_with_identity();
    let mut p = pointer_text(HELLO_OID, HELLO_LEN);
    p.pop(); // strip trailing newline → non-canonical
    commit_pointer_at(repo.path(), "non.bin", &p);

    let out = run_in(repo.path(), &["fsck", "--objects", "--dry-run"], b"");
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("openError"), "expected openError: {stdout}");
    assert!(!stdout.contains("nonCanonicalPointer"), "should skip canonical check: {stdout}");
}

// ---------- version ------------------------------------------------------

#[test]
fn version_prints_banner_and_succeeds() {
    let repo = fresh_repo();
    let out = run_in(repo.path(), &["version"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("git-lfs/"), "{stdout}");
}

#[test]
fn version_works_outside_repo_too() {
    let tmp = TempDir::new().unwrap();
    let out = run_in(tmp.path(), &["version"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

// ---------- pointer ------------------------------------------------------

#[test]
fn pointer_check_returns_zero_for_valid_pointer() {
    let repo = fresh_repo();
    let p = pointer_text(HELLO_OID, HELLO_LEN);
    std::fs::write(repo.path().join("p.txt"), &p).unwrap();
    let out = run_in(repo.path(), &["pointer", "--check", "--file", "p.txt"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn pointer_check_exits_one_for_non_pointer() {
    let repo = fresh_repo();
    std::fs::write(repo.path().join("p.txt"), b"this is plain text\n").unwrap();
    let out = run_in(repo.path(), &["pointer", "--check", "--file", "p.txt"], b"");
    assert_eq!(out.status.code(), Some(1), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn pointer_check_strict_exits_two_for_noncanonical() {
    let repo = fresh_repo();
    // Missing trailing newline parses but isn't byte-canonical.
    let mut p = pointer_text(HELLO_OID, HELLO_LEN);
    assert_eq!(p.last(), Some(&b'\n'));
    p.pop(); // strip trailing newline
    std::fs::write(repo.path().join("p.txt"), &p).unwrap();
    let out = run_in(
        repo.path(),
        &["pointer", "--check", "--strict", "--file", "p.txt"],
        b"",
    );
    assert_eq!(out.status.code(), Some(2), "stderr: {}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn pointer_file_emits_canonical_pointer_for_a_blob() {
    let repo = fresh_repo();
    std::fs::write(repo.path().join("data.bin"), b"hello world\n").unwrap();
    let out = run_in(repo.path(), &["pointer", "--file", "data.bin"], b"");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let expected = pointer_text(HELLO_OID, HELLO_LEN);
    let expected_str = String::from_utf8_lossy(&expected);
    assert_eq!(stdout, expected_str, "got: {stdout:?}");
    // The "Git LFS pointer for ..." banner goes to stderr.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Git LFS pointer for data.bin"), "{stderr}");
}

#[test]
fn pointer_compare_succeeds_when_canonical_match() {
    let repo = fresh_repo();
    std::fs::write(repo.path().join("data.bin"), b"hello world\n").unwrap();
    // Pre-built canonical pointer for that exact content.
    std::fs::write(repo.path().join("ref.ptr"), pointer_text(HELLO_OID, HELLO_LEN)).unwrap();

    let out = run_in(
        repo.path(),
        &["pointer", "--file", "data.bin", "--pointer", "ref.ptr"],
        b"",
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("Pointers do not match"), "{stderr}");
}

#[test]
fn pointer_compare_fails_on_mismatch() {
    let repo = fresh_repo();
    std::fs::write(repo.path().join("data.bin"), b"hello world\n").unwrap();
    // Pointer for a *different* OID — should mismatch.
    let other_oid = "0000000000000000000000000000000000000000000000000000000000000001";
    std::fs::write(repo.path().join("ref.ptr"), pointer_text(other_oid, HELLO_LEN)).unwrap();

    let out = run_in(
        repo.path(),
        &["pointer", "--file", "data.bin", "--pointer", "ref.ptr"],
        b"",
    );
    assert_eq!(out.status.code(), Some(1), "expected mismatch exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Pointers do not match"), "{stderr}");
}

#[test]
fn pointer_stdin_mode_parses_pointer_from_stdin() {
    let repo = fresh_repo();
    let p = pointer_text(HELLO_OID, HELLO_LEN);
    let out = run_in(repo.path(), &["pointer", "--stdin"], &p);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Pointer from STDIN"), "{stderr}");
    // The echoed pointer text appears in stderr.
    assert!(stderr.contains(HELLO_OID), "{stderr}");
}

#[test]
fn pointer_no_args_says_nothing_to_do() {
    let repo = fresh_repo();
    let out = run_in(repo.path(), &["pointer"], b"");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Nothing to do"), "{stderr}");
}

// ---------- lock / locks / unlock ----------------------------------------
//
// All three speak the locking JSON API; we wiremock the server side and
// assert on what the binary prints + which endpoints it hits. Each test
// uses `lfs.url` pointed at the mock so the endpoint resolver doesn't
// need a real remote.

/// Create a fresh repo + configure `lfs.url` to point at the mock.
async fn lock_test_repo(server_uri: &str) -> TempDir {
    let repo = fresh_repo_with_identity();
    let status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["config", "--local", "lfs.url", server_uri])
        .status()
        .unwrap();
    assert!(status.success());
    repo
}

#[tokio::test]
async fn lock_creates_lock_and_prints_locked_message() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(m_method("POST"))
        .and(m_path("/locks"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "lock": {
                "id": "lock-id-1",
                "path": "data.bin",
                "locked_at": "2026-04-25T12:00:00Z",
                "owner": { "name": "test" }
            }
        })))
        .mount(&server)
        .await;

    let repo = lock_test_repo(&server.uri()).await;
    // Need the file to exist so resolve_lock_path doesn't reject it.
    std::fs::write(repo.path().join("data.bin"), b"x").unwrap();

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["lock", "data.bin"], b""))
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "lock failed: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim_end(), "Locked data.bin");
}

#[tokio::test]
async fn lock_conflict_surfaces_existing_owner() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(m_method("POST"))
        .and(m_path("/locks"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "lock": {
                "id": "existing",
                "path": "data.bin",
                "locked_at": "2026-04-25T12:00:00Z",
                "owner": { "name": "alice" }
            },
            "message": "already created lock"
        })))
        .mount(&server)
        .await;

    let repo = lock_test_repo(&server.uri()).await;
    std::fs::write(repo.path().join("data.bin"), b"x").unwrap();

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["lock", "data.bin"], b""))
        .await
        .unwrap();
    // Conflict makes the command fail.
    assert!(!out.status.success(), "expected conflict to fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already created lock"), "{stderr}");
    assert!(stderr.contains("alice"), "should name conflict owner: {stderr}");
}

#[tokio::test]
async fn lock_json_emits_array_of_locks() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(m_method("POST"))
        .and(m_path("/locks"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "lock": {
                "id": "json-lock",
                "path": "x.bin",
                "locked_at": "2026-04-25T12:00:00Z",
                "owner": { "name": "test" }
            }
        })))
        .mount(&server)
        .await;

    let repo = lock_test_repo(&server.uri()).await;
    std::fs::write(repo.path().join("x.bin"), b"x").unwrap();

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["lock", "x.bin", "--json"], b""))
        .await
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], "json-lock");
    assert_eq!(arr[0]["path"], "x.bin");
}

#[tokio::test]
async fn locks_lists_and_paginates_via_cursor() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // Page 1: returns one lock + a next cursor.
    Mock::given(m_method("GET"))
        .and(m_path("/locks"))
        .and(query_param_absent("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "locks": [
                { "id": "id-a", "path": "a.bin", "locked_at": "2026-04-25T12:00:00Z",
                  "owner": { "name": "alice" } }
            ],
            "next_cursor": "page2"
        })))
        .mount(&server)
        .await;

    // Page 2: returns the second lock + no next cursor.
    Mock::given(m_method("GET"))
        .and(m_path("/locks"))
        .and(query_param("cursor", "page2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "locks": [
                { "id": "id-b", "path": "b.bin", "locked_at": "2026-04-25T12:00:00Z",
                  "owner": { "name": "bob" } }
            ]
        })))
        .mount(&server)
        .await;

    let repo = lock_test_repo(&server.uri()).await;
    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["locks"], b""))
        .await
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("a.bin"), "page 1 missing: {stdout}");
    assert!(stdout.contains("b.bin"), "page 2 missing: {stdout}");
    assert!(stdout.contains("ID:id-a"), "{stdout}");
    assert!(stdout.contains("ID:id-b"), "{stdout}");
}

/// `wiremock` does not provide a built-in matcher for "query param absent",
/// so we build one. Used by the pagination test to ensure page 1 is the
/// request without a `cursor` parameter.
fn query_param_absent(name: &'static str) -> impl wiremock::Match {
    struct Absent(&'static str);
    impl wiremock::Match for Absent {
        fn matches(&self, req: &wiremock::Request) -> bool {
            !req.url
                .query_pairs()
                .any(|(k, _)| k == self.0)
        }
    }
    Absent(name)
}

#[tokio::test]
async fn locks_verify_prefixes_owned_with_capital_o() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(m_method("POST"))
        .and(m_path("/locks/verify"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ours": [
                { "id": "mine", "path": "mine.bin", "locked_at": "2026-04-25T12:00:00Z",
                  "owner": { "name": "me" } }
            ],
            "theirs": [
                { "id": "theirs", "path": "their.bin", "locked_at": "2026-04-25T12:00:00Z",
                  "owner": { "name": "them" } }
            ]
        })))
        .mount(&server)
        .await;

    let repo = lock_test_repo(&server.uri()).await;
    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["locks", "--verify"], b""))
        .await
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Owned line starts with "O ", others with two spaces.
    let mine_line = stdout.lines().find(|l| l.contains("mine.bin")).expect("mine line");
    let their_line = stdout.lines().find(|l| l.contains("their.bin")).expect("their line");
    assert!(mine_line.starts_with("O "), "expected `O ` prefix on owned: {mine_line:?}");
    assert!(their_line.starts_with("  "), "expected `  ` prefix on others: {their_line:?}");
}

#[tokio::test]
async fn unlock_by_id_calls_delete_and_prints_message() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(m_method("POST"))
        .and(m_path("/locks/abc123/unlock"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "lock": {
                "id": "abc123",
                "path": "data.bin",
                "locked_at": "2026-04-25T12:00:00Z",
                "owner": { "name": "test" }
            }
        })))
        .mount(&server)
        .await;

    let repo = lock_test_repo(&server.uri()).await;
    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || {
        run_in(&path, &["unlock", "--id", "abc123"], b"")
    })
    .await
    .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Unlocked Lock abc123"), "{stdout}");
}

#[tokio::test]
async fn unlock_by_path_looks_up_id_then_deletes() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // Path → id lookup.
    Mock::given(m_method("GET"))
        .and(m_path("/locks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "locks": [
                { "id": "by-path-id", "path": "data.bin",
                  "locked_at": "2026-04-25T12:00:00Z", "owner": { "name": "test" } }
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(m_method("POST"))
        .and(m_path("/locks/by-path-id/unlock"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "lock": {
                "id": "by-path-id", "path": "data.bin",
                "locked_at": "2026-04-25T12:00:00Z", "owner": { "name": "test" }
            }
        })))
        .mount(&server)
        .await;

    let repo = lock_test_repo(&server.uri()).await;
    std::fs::write(repo.path().join("data.bin"), b"x").unwrap();

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["unlock", "data.bin"], b""))
        .await
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Unlocked data.bin"), "{stdout}");
}

#[tokio::test]
async fn unlock_by_path_when_not_locked_fails() {
    use serde_json::json;
    use wiremock::matchers::{method as m_method, path as m_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    // List returns no matches for the path.
    Mock::given(m_method("GET"))
        .and(m_path("/locks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"locks": []})))
        .mount(&server)
        .await;

    let repo = lock_test_repo(&server.uri()).await;
    std::fs::write(repo.path().join("data.bin"), b"x").unwrap();

    let path = repo.path().to_owned();
    let out = tokio::task::spawn_blocking(move || run_in(&path, &["unlock", "data.bin"], b""))
        .await
        .unwrap();
    assert!(!out.status.success(), "expected failure when path not locked");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not locked"), "{stderr}");
}

#[test]
fn unlock_requires_either_id_or_path() {
    let repo = fresh_repo_with_identity();
    let out = run_in(repo.path(), &["unlock"], b"");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--id or a set of paths"), "{stderr}");
}
