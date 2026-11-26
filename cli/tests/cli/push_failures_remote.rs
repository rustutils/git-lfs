//! `t-push-failures-remote.sh`.
//!
//! The server fails a request for an object whose contents name the
//! failure, such as `status-storage-403`.

use crate::harness::{Test, oid};

/// Check that pushing `bad.dat` holding `contents` fails, with `message`
/// in the output if given, and leaves the object off the server.
fn assert_push_fails(contents: &str, message: Option<&str>) {
    let t = Test::new();
    let name = format!("t-push-failures-remote-{contents}");
    let repo = t.remote_and_clone(&name);
    // Wait at most a second between the eight retries of a 500 or 503,
    // instead of ten. Zero would mean the default, as in upstream.
    repo.git(["config", "lfs.transfer.maxretrydelay", "1"]).ok();
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("good.dat", "hi");
    repo.write("bad.dat", contents);
    repo.git(["add", ".gitattributes", "good.dat", "bad.dat"])
        .ok();
    repo.git(["commit", "-m", "welp"]).ok();

    let run = repo
        .command("git")
        .args(["push", "origin", "main"])
        .merged()
        .run()
        .fails();

    if let Some(message) = message {
        run.has(message);
    }
    t.assert_server_objects(&name, &[], &[&oid(contents)]);
}

#[test]
fn push_upload_file_with_storage_403() {
    assert_push_fails("status-storage-403", None);
}

#[test]
fn push_upload_file_with_storage_404() {
    assert_push_fails("status-storage-404", None);
}

#[test]
fn push_upload_file_with_storage_410() {
    assert_push_fails("status-storage-410", None);
}

#[test]
fn push_upload_file_with_storage_500() {
    assert_push_fails("status-storage-500", None);
}

#[test]
fn push_upload_file_with_storage_503() {
    assert_push_fails("status-storage-503", Some("LFS is temporarily unavailable"));
}

#[test]
fn push_upload_file_with_api_403() {
    assert_push_fails("status-batch-403", None);
}

#[test]
fn push_upload_file_with_api_404() {
    assert_push_fails("status-batch-404", None);
}

#[test]
fn push_upload_file_with_api_410() {
    assert_push_fails("status-batch-410", None);
}

#[test]
fn push_upload_file_with_api_422() {
    assert_push_fails("status-batch-422", None);
}

#[test]
fn push_upload_file_with_api_500() {
    assert_push_fails("status-batch-500", None);
}
