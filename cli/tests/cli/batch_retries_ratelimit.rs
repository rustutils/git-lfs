//! `t-batch-retries-ratelimit.sh`.
//!
//! The server answers batch requests for these repositories with 429
//! while their rate limit has no requests left.

use crate::harness::{Test, oid};
use crate::transfer_fixtures::{
    assert_counts, assert_line_with, committed, lfs_clone_traced, traced,
};

/// Check that pushing `files` in the repository `name` succeeds after
/// retries, with `counts` of the lines in the trace.
fn assert_upload_retries(
    name: &str,
    dir: &str,
    files: &[(&str, &str)],
    counts: &[(&str, usize)],
) -> String {
    let t = Test::new();
    let repo = committed(&t, name, dir, files);

    let run = traced(&repo, &["push", "origin", "main"]).ok();

    assert_counts(&run, counts);
    let oids: Vec<String> = files.iter().map(|(_, c)| oid(c)).collect();
    let oids: Vec<&str> = oids.iter().map(String::as_str).collect();
    t.assert_server_objects(name, &oids, &[]);
    run.output()
}

/// Check that `git lfs clone` of the repository `name` with `files`
/// succeeds after retries, with `counts` of the lines in the trace.
///
/// The push gets the most requests the limit allows, so only the clone
/// is limited.
fn assert_clone_retries(name: &str, dir: &str, files: &[(&str, &str)], counts: &[(&str, usize)]) {
    let t = Test::new();
    let repo = committed(&t, name, dir, files);
    t.server().set_rate_limit("batch", "", name, "", None);
    repo.git(["push", "origin", "main"]).ok();
    let oids: Vec<String> = files.iter().map(|(_, c)| oid(c)).collect();
    let oids: Vec<&str> = oids.iter().map(String::as_str).collect();
    t.assert_server_objects(name, &oids, &[]);
    t.server().set_rate_limit("batch", "", name, "", Some(0));

    let (run, clone) = lfs_clone_traced(&t, name);

    assert_counts(&run.ok(), counts);
    for (_, contents) in files {
        clone.assert_local_object(&oid(contents), contents.len());
    }
}

const TWO_FILES: [(&str, &str); 2] = [("a.dat", "content 1"), ("b.dat", "content 2")];

#[test]
fn batch_api_http_upload_causes_delayed_retries() {
    assert_upload_retries(
        "upload-batch-retry-later",
        "batch-repo-upload",
        &[("a.dat", "content")],
        &[("tq: enqueue retry", 1)],
    );
}

#[test]
fn batch_api_http_upload_with_multiple_files_causes_delayed_retries() {
    assert_upload_retries(
        "upload-multiple-batch-retry-later",
        "batch-repo-upload-multiple",
        &TWO_FILES,
        &[("tq: enqueue retry", 2), ("tq: enqueue retry #1", 2)],
    );
}

#[test]
fn batch_api_http_download_causes_delayed_retries() {
    assert_clone_retries(
        "clone-batch-retry-later",
        "batch-repo-clone",
        &[("a.dat", "content")],
        &[("tq: enqueue retry", 1)],
    );
}

#[test]
fn batch_api_http_download_with_multiple_files_causes_delayed_retries() {
    assert_clone_retries(
        "clone-multiple-batch-retry-later",
        "batch-repo-clone-multiple",
        &TWO_FILES,
        &[("tq: enqueue retry", 2), ("tq: enqueue retry #1", 2)],
    );
}

/// Without `Retry-After`, the backoff retries more than once.
#[test]
fn batch_api_http_upload_causes_retries_missing_header() {
    let output = assert_upload_retries(
        "upload-batch-retry-later-no-header",
        "batch-repo-upload-no-header",
        &[("a.dat", "content")],
        &[("tq: enqueue retry #1", 1), ("tq: enqueue retry #2", 1)],
    );
    let count = output
        .lines()
        .filter(|l| l.contains("tq: enqueue retry"))
        .count();
    assert!(count > 1, "{output}");
}

/// The server's wait is 10 seconds, more than `lfs.transfer.maxRetryTime`.
#[test]
fn batch_api_http_upload_fails_when_retry_after_exceeds_max_retry_time() {
    let t = Test::new();
    let name = "upload-batch-retry-later-exceeds-max";
    t.remote(name);
    let repo = t.clone(name, name);
    repo.git(["config", "lfs.transfer.maxRetryTime", "5"]).ok();
    repo.write("a.dat", "content");
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();

    let run = traced(&repo, &["push", "origin", "main"]).fails();

    let refusing = format!("tq: refusing to retry \"{}", oid("content"));
    assert_line_with(
        &run,
        &[&refusing, "\", retry after ", "s exceeds maximum 5s"],
    );
    t.assert_server_objects(name, &[], &[&oid("content")]);
}
