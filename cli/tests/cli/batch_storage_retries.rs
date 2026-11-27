//! `t-batch-storage-retries.sh`.
//!
//! The server fails or cuts off storage requests for these objects, as
//! their contents ask.

use crate::harness::{Test, oid};
use crate::transfer_fixtures::{
    assert_counts, assert_line_with, clone_without_filters, committed, pushed, traced,
};

#[test]
fn batch_storage_http_upload_causes_retries() {
    let t = Test::new();
    let name = "batch-storage-upload-retry";
    let contents = "storage-upload-retry";
    let repo = committed(
        &t,
        name,
        "batch-storage-repo-upload",
        &[("a.dat", contents)],
    );
    repo.git(["config", "--local", "lfs.transfer.maxretries", "3"])
        .ok();

    let run = traced(&repo, &["push", "origin", "main"]).ok();

    let retrying = format!(
        "tq: retrying object {}: Fatal error: Server error",
        oid(contents)
    );
    assert_counts(&run, &[(&retrying, 2)]);
    t.assert_server_objects(name, &[&oid(contents)], &[]);
}

#[test]
fn batch_storage_http_download_causes_retries() {
    let t = Test::new();
    let name = "batch-storage-download-retry";
    let contents = "storage-download-retry";
    pushed(
        &t,
        name,
        "batch-storage-repo-download",
        &[("a.dat", contents)],
    );
    t.assert_server_objects(name, &[&oid(contents)], &[]);
    let clone = clone_without_filters(&t, name);
    clone
        .git(["config", "--local", "lfs.transfer.maxretries", "3"])
        .ok();

    let run = traced(&clone, &["lfs", "pull", "origin", "main"]).ok();

    let retrying = format!(
        "tq: retrying object {}: Fatal error: Server error",
        oid(contents)
    );
    assert_counts(&run, &[(&retrying, 2)]);
    clone.assert_local_object(&oid(contents), contents.len());
}

/// The repository `name` with `a.dat` holding `contents` pushed, and the
/// local objects removed.
fn pushed_without_objects<'a>(t: &'a Test, name: &str, contents: &str) -> crate::harness::Repo<'a> {
    let repo = pushed(t, name, name, &[("a.dat", contents)]);
    t.assert_server_objects(name, &[&oid(contents)], &[]);
    repo.remove_local_objects();
    repo
}

/// The server cuts the first download off halfway. The retry asks for the
/// rest.
#[test]
fn batch_storage_http_download_retries_with_range_header() {
    let t = Test::new();
    let contents = "storage-download-retry-range";
    let oid = oid(contents);
    let len = contents.len();
    let repo = pushed_without_objects(&t, "batch-storage-download-retry-range", contents);

    let run = traced(&repo, &["lfs", "fetch"]).ok();

    let half = len / 2;
    let run = run
        .has(&format!("Attempting to resume download of \"{oid}\""))
        .has(&format!("tq: retrying object {oid}"))
        .has(&format!("Range: bytes={half}-{}", len - 1))
        .has("206 Partial Content")
        .has(&format!("Content-Range: bytes {half}-{}/{len}", len - 1));
    assert_line_with(&run, &["xfer: server accepted resume", &oid]);
    repo.assert_local_object(&oid, len);
}

/// The server cuts the first download off halfway, then rejects the
/// range, so the retry downloads it all.
#[test]
fn batch_storage_http_download_retries_after_range_header_rejected() {
    let t = Test::new();
    let contents = "storage-download-retry-range-rejected";
    let oid = oid(contents);
    let len = contents.len();
    let repo = pushed_without_objects(&t, "batch-storage-download-retry-range-rejected", contents);

    let run = traced(&repo, &["lfs", "fetch"]).ok();

    let run = run
        .has(&format!("Attempting to resume download of \"{oid}\""))
        .has(&format!("tq: retrying object {oid}"))
        .has(&format!("Range: bytes={}-{}", len / 2, len - 1))
        .has("416 Requested Range Not Satisfiable");
    assert_line_with(
        &run,
        &[
            "xfer: server rejected resume",
            &oid,
            "re-downloading from start",
        ],
    );
    repo.assert_local_object(&oid, len);
}

/// A leftover partial download smaller than the object is resumed, and
/// the corrupt result rejected. One as large as the object isn't resumed
/// with an invalid range, but downloaded again.
#[test]
fn batch_storage_http_download_retries_without_invalid_range_header() {
    let t = Test::new();
    let contents = "storage-download-retry-no-invalid-range";
    let oid = oid(contents);
    let len = contents.len();
    let repo = pushed_without_objects(
        &t,
        "batch-storage-download-retry-no-invalid-range",
        contents,
    );
    let part = format!(".git/lfs/incomplete/{oid}.part");

    repo.write(&part, "aa");
    let run = traced(&repo, &["lfs", "fetch"]).fails();

    let run = run
        .has(&format!("Attempting to resume download of \"{oid}\""))
        .has(&format!("Range: bytes=2-{}", len - 1))
        .has("206 Partial Content")
        .has(&format!("Content-Range: bytes 2-{}/{len}", len - 1))
        .has(&format!("expected OID {oid}, got"))
        .has("error: failed to fetch some objects");
    assert_line_with(&run, &["xfer: server accepted resume", &oid]);
    repo.refute_local_object(&oid);

    repo.write(&part, contents.replacen("st", "aa", 1));
    let run = traced(&repo, &["lfs", "fetch"]).ok();

    assert_counts(
        &run,
        &[
            (&format!("Attempting to resume download of \"{oid}\""), 0),
            (&format!("tq: retrying object {oid}"), 0),
            ("Range: bytes=", 0),
            ("400 Bad Request", 0),
        ],
    );
    repo.assert_local_object(&oid, len);
}

/// Not from the shell suite: against a server that always fails,
/// `lfs.transfer.maxretries` caps the retries. The shell tests' servers
/// succeed before the cap.
#[test]
fn batch_storage_http_upload_stops_after_max_retries() {
    let t = Test::new();
    let name = "batch-storage-max-retries";
    // The server fails every upload of this content with a 500.
    let repo = committed(&t, name, name, &[("a.dat", "status-storage-500")]);
    repo.git(["config", "lfs.transfer.maxretries", "2"]).ok();
    repo.git(["config", "lfs.transfer.maxretrydelay", "1"]).ok();

    let run = traced(&repo, &["push", "origin", "main"]).fails();

    assert_counts(
        &run,
        &[
            ("tq: retrying object", 2),
            ("tq: enqueue retry", 2),
            ("HTTP: PUT", 3),
        ],
    );
}
