//! `t-batch-storage-retries-ratelimit.sh`.
//!
//! The server answers storage requests for these objects with 429 until
//! their wait is over, with or without a `Retry-After` header.

use crate::harness::{Test, oid};
use crate::transfer_fixtures::{
    assert_counts, assert_line_with, clone_without_filters, committed, lfs_clone_traced, pushed,
    traced,
};

/// Check that pushing `contents` in the repository `name` succeeds after
/// retries, with `counts` of the lines in the trace.
fn assert_upload_retries(name: &str, contents: &str, counts: &[(&str, usize)]) -> String {
    let t = Test::new();
    let repo = committed(&t, name, name, &[("a.dat", contents)]);

    let run = traced(&repo, &["push", "origin", "main"]).ok();

    assert_counts(&run, counts);
    t.assert_server_objects(name, &[&oid(contents)], &[]);
    run.output()
}

/// Check that `git lfs pull` of `contents` in a clone of the repository
/// `name` succeeds after retries, with `counts` of the lines in the trace.
fn assert_download_retries(name: &str, contents: &str, counts: &[(&str, usize)]) -> String {
    let t = Test::new();
    pushed(&t, name, name, &[("a.dat", contents)]);
    t.assert_server_objects(name, &[&oid(contents)], &[]);
    let clone = clone_without_filters(&t, name);

    let run = traced(&clone, &["lfs", "pull", "origin", "main"]).ok();

    assert_counts(&run, counts);
    clone.assert_local_object(&oid(contents), contents.len());
    run.output()
}

const ONE_RETRY: [(&str, usize); 2] = [("tq: retrying object", 1), ("tq: enqueue retry", 1)];

#[test]
fn batch_storage_http_upload_causes_delayed_retries() {
    assert_upload_retries(
        "batch-storage-upload-retry-later",
        "storage-upload-retry-later",
        &ONE_RETRY,
    );
}

#[test]
fn batch_storage_http_download_causes_delayed_retries() {
    assert_download_retries(
        "batch-storage-download-retry-later",
        "storage-download-retry-later",
        &ONE_RETRY,
    );
}

#[test]
fn batch_storage_http_download_causes_delayed_retries_during_clone() {
    let t = Test::new();
    let name = "batch-storage-clone-retry-later";
    let contents = "storage-download-retry-later";
    pushed(&t, name, "batch-storage-repo-clone", &[("a.dat", contents)]);
    t.assert_server_objects(name, &[&oid(contents)], &[]);

    let (run, clone) = lfs_clone_traced(&t, name);

    assert_counts(&run.ok(), &ONE_RETRY);
    clone.assert_local_object(&oid(contents), contents.len());
}

/// Without `Retry-After`, the backoff retries more than once.
const RETRIES_WITHOUT_HEADER: [(&str, usize); 2] =
    [("tq: enqueue retry #1", 1), ("tq: enqueue retry #2", 1)];

/// Check that `output` has more than one retry.
#[track_caller]
fn assert_several_retries(output: &str) {
    for text in ["tq: retrying object", "tq: enqueue retry"] {
        let count = output.lines().filter(|l| l.contains(text)).count();
        assert!(count > 1, "{count} of {text:?}\n{output}");
    }
}

#[test]
fn batch_storage_http_upload_causes_retries_missing_header() {
    let output = assert_upload_retries(
        "batch-storage-upload-retry-later-no-header",
        "storage-upload-retry-later-no-header",
        &RETRIES_WITHOUT_HEADER,
    );
    assert_several_retries(&output);
}

#[test]
fn batch_storage_http_download_causes_retries_missing_header() {
    let output = assert_download_retries(
        "batch-storage-download-retry-later-no-header",
        "storage-download-retry-later-no-header",
        &RETRIES_WITHOUT_HEADER,
    );
    assert_several_retries(&output);
}

/// The server's wait is 10 seconds, more than `lfs.transfer.maxRetryTime`.
const REFUSED: &str = "\", retry after ";
const OVER_MAX: &str = "s exceeds maximum 5s";

#[test]
fn batch_storage_http_upload_fails_when_retry_after_exceeds_max_retry_time() {
    let t = Test::new();
    let name = "batch-storage-upload-retry-later-exceeds-max";
    let contents = "storage-upload-retry-later";
    t.remote(name);
    let repo = t.clone(name, name);
    repo.git(["config", "lfs.transfer.maxRetryTime", "5"]).ok();
    repo.write("a.dat", contents);
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();

    let run = traced(&repo, &["push", "origin", "main"]).fails();

    let refusing = format!("tq: refusing to retry \"{}", oid(contents));
    assert_line_with(&run, &[&refusing, REFUSED, OVER_MAX]);
    t.assert_server_objects(name, &[], &[&oid(contents)]);
}

#[test]
fn batch_storage_http_download_fails_when_retry_after_exceeds_max_retry_time() {
    let t = Test::new();
    let name = "batch-storage-download-retry-later-exceeds-max";
    let contents = "storage-download-retry-later";
    pushed(&t, name, name, &[("a.dat", contents)]);
    t.assert_server_objects(name, &[&oid(contents)], &[]);
    let clone = clone_without_filters(&t, name);
    clone.git(["config", "lfs.transfer.maxRetryTime", "5"]).ok();

    let run = traced(&clone, &["lfs", "pull", "origin", "main"]).fails();

    let refusing = format!("tq: refusing to retry \"{}", oid(contents));
    assert_line_with(&run, &[&refusing, REFUSED, OVER_MAX]);
    clone.refute_local_object(&oid(contents));
}
