//! `t-progress.sh`.
//!
//! `GIT_LFS_PROGRESS` names a file that each transfer logs a line to, such
//! as `upload 1/5 2/2 a.dat`.

use std::path::{Path, PathBuf};

use crate::harness::{Repo, Run, Test};

/// Run `program` with `args` in `repo`, logging progress to `log`, with
/// stderr merged into stdout.
fn logged(repo: &Repo, log: &Path, program: &str, args: &[&str]) -> Run {
    repo.command(program)
        .args(args)
        .env("GIT_LFS_PROGRESS", log)
        .env("GIT_TRACE", "1")
        .merged()
        .run()
        .ok()
}

/// The progress log `name` for the repository `repo`.
fn log_path(t: &Test, repo: &str, name: &str) -> PathBuf {
    t.dir().join(format!("{repo}-logs")).join(name)
}

fn read(log: &Path) -> String {
    std::fs::read_to_string(log).unwrap()
}

/// Check that `log` has a line with `{verb} N/5` for each of the 5 files.
#[track_caller]
fn assert_five(log: &Path, verb: &str) {
    let text = read(log);
    for n in 1..=5 {
        assert!(
            text.contains(&format!("{verb} {n}/5")),
            "{verb} {n}/5\n{text}"
        );
    }
}

#[test]
fn progress_log_file_counts() {
    let t = Test::new();
    let name = "progress-log-file-counts";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    for f in ["a", "b", "c", "d", "e"] {
        repo.write(&format!("{f}.dat"), format!("{f}\n"));
    }

    let log = log_path(&t, name, "progress-clean.log");
    logged(
        &repo,
        &log,
        "git",
        &[
            "add",
            ".gitattributes",
            "a.dat",
            "b.dat",
            "c.dat",
            "d.dat",
            "e.dat",
        ],
    );
    let clean = read(&log);
    assert_eq!(clean.matches("clean 1/1").count(), 5, "{clean}");
    repo.git(["commit", "-m", "add files"]).ok();

    let log = log_path(&t, name, "progress-push.log");
    logged(&repo, &log, "git", &["push", "origin", "main"])
        .has("Uploading LFS objects: 100% (5/5), 10 B");
    assert_five(&log, "upload");

    let log = log_path(&t, name, "progress-clone.log");
    let assert_dir = format!("{name}-assert");
    let git_lfs = crate::harness::git_lfs();
    logged(
        &t.repo(""),
        &log,
        &git_lfs.to_string_lossy(),
        &["clone", &t.remote_url(name), &assert_dir],
    );
    assert_five(&log, "download");
    std::fs::remove_dir_all(t.dir().join(&assert_dir)).unwrap();

    let clone = t.clone_skip_smudge(name, &assert_dir);
    clone.remove_local_objects();
    let log = log_path(&t, name, "progress-fetch.log");
    logged(
        &clone,
        &log,
        &git_lfs.to_string_lossy(),
        &["fetch", "--all"],
    );
    assert_five(&log, "download");

    let log = log_path(&t, name, "progress-checkout.log");
    logged(&clone, &log, &git_lfs.to_string_lossy(), &["checkout"]);
    assert_five(&log, "checkout");
}

/// The input is larger than the pointer read buffer, and the log still
/// has the final size (upstream's PR 6011).
#[test]
fn progress_log_unknown_initial_size_clean() {
    let t = Test::new();
    let name = "progress-log-unknown-total-clean";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    let size = 1024 * 2;
    repo.write("large.dat", vec![0u8; size]);

    let log = log_path(&t, name, "progress.log");
    logged(&repo, &log, "git", &["add", "large.dat"]);

    let text = read(&log);
    assert!(
        text.contains(&format!("clean 1/1 {size}/{size} large.dat")),
        "{text}"
    );
}

/// The server gzips this content, so there's no `Content-Length`.
const GZIPPED: &str = "storage-download-encoding-gzip";

/// The remote repository `name` with `a.dat` holding [`GZIPPED`] pushed.
fn pushed_gzipped<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", GZIPPED);
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();
    repo.git(["push", "origin", "main"]).ok();
    repo
}

/// Check that `run` downloaded a gzipped response and `log` has its final
/// size.
#[track_caller]
fn assert_logged_final_size(run: &Run, log: &Path) {
    assert!(run.count("decompressed gzipped response") > 0, "{run}");
    let text = read(log);
    let n = GZIPPED.len();
    assert!(
        text.contains(&format!("download 1/1 {n}/{n} a.dat")),
        "{text}"
    );
}

/// Through `git lfs smudge`, which logs as the download comes in, not
/// `filter-process`, which logs from the stored file.
#[test]
fn progress_log_unknown_initial_size_smudge() {
    let t = Test::new();
    let name = "progress-log-unknown-total-smudge";
    pushed_gzipped(&t, name);
    t.repo("")
        .git(["config", "--global", "--unset", "filter.lfs.process"])
        .ok();

    let log = log_path(&t, name, "progress.log");
    let run = logged(
        &t.repo(""),
        &log,
        "git",
        &["clone", &t.remote_url(name), &format!("{name}-assert")],
    );

    assert_logged_final_size(&run, &log);
}

#[test]
fn progress_log_unknown_initial_size_fetch_pull() {
    let t = Test::new();
    let name = "progress-log-unknown-total-pull";
    let repo = pushed_gzipped(&t, name);
    repo.remove_local_objects();
    std::fs::remove_file(repo.path().join("a.dat")).unwrap();
    let git_lfs = crate::harness::git_lfs();

    let log = log_path(&t, name, "progress-fetch.log");
    let run = logged(&repo, &log, &git_lfs.to_string_lossy(), &["fetch"]);
    assert_logged_final_size(&run, &log);

    repo.remove_local_objects();
    let log = log_path(&t, name, "progress-pull.log");
    let run = logged(&repo, &log, &git_lfs.to_string_lossy(), &["pull"]);
    assert_logged_final_size(&run, &log);
}
