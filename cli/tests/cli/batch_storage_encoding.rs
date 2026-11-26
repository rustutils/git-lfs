//! `t-batch-storage-encoding.sh`.
//!
//! The server picks its encoding from the object's contents and the
//! repository's name. The tests count lines in the trace.

use crate::harness::{Repo, Run, Test, oid};

/// Run `git` with `args` in `repo`, traced, with stderr merged into
/// stdout.
fn traced(repo: &Repo, args: &[&str]) -> Run {
    repo.command("git")
        .args(args)
        .env("GIT_TRACE", "1")
        .env("GIT_CURL_VERBOSE", "1")
        .merged()
        .run()
}

/// The remote repository `name`, cloned, with `files` committed under
/// `*.dat` tracking.
fn committed<'a>(t: &'a Test, name: &str, files: &[(&str, &str)]) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    for (path, contents) in files {
        repo.write(path, contents);
        repo.git(["add", path]).ok();
    }
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo
}

/// [`committed`] with `files`, pushed, and the local objects removed.
fn pushed<'a>(t: &'a Test, name: &str, files: &[(&str, &str)]) -> Repo<'a> {
    let repo = committed(t, name, files);
    repo.git(["push", "origin", "main"]).ok();
    repo.remove_local_objects();
    repo
}

/// Check that `run`'s output has `count` lines containing each of `lines`.
#[track_caller]
fn assert_counts(run: &Run, lines: &[(&str, usize)]) {
    for (text, count) in lines {
        assert_eq!(run.count(text), *count, "{text:?}\n{run}");
    }
}

/// Without any setting, with the setting, and with it for the server's
/// URL only, the download is gzipped.
#[test]
fn batch_storage_http_download_with_gzip_encoding() {
    let t = Test::new();
    let contents = "storage-download-encoding-gzip";
    let repo = pushed(
        &t,
        "batch-storage-download-encoding-gzip",
        &[("a.dat", contents)],
    );
    let url_key = format!("lfs.transfer.{}.httpDownloadEncoding", t.server().url());

    for config in [
        None,
        Some("lfs.transfer.httpDownloadEncoding"),
        Some(&url_key),
    ] {
        repo.remove_local_objects();
        repo.git(["config", "--unset-all", "lfs.transfer.httpDownloadEncoding"]);
        if let Some(key) = config {
            repo.git(["config", key, "gzip"]).ok();
        }

        let run = traced(&repo, &["lfs", "pull"]).ok();

        // One from the batch request, one from the download.
        assert_counts(
            &run,
            &[
                ("Accept-Encoding: gzip", 2),
                ("decompressed gzipped response", 1),
            ],
        );
        repo.assert_local_object(&oid(contents), contents.len());
    }
}

/// With the setting, and with it for the server's URL only.
#[test]
fn batch_storage_http_download_with_zstd_encoding() {
    let t = Test::new();
    let contents = "storage-download-encoding-zstd";
    let repo = pushed(
        &t,
        "batch-storage-download-encoding-zstd",
        &[("a.dat", contents)],
    );
    let url_key = format!("lfs.transfer.{}.httpDownloadEncoding", t.server().url());

    for key in ["lfs.transfer.httpDownloadEncoding", url_key.as_str()] {
        repo.remove_local_objects();
        repo.git(["config", "--unset-all", "lfs.transfer.httpDownloadEncoding"]);
        repo.git(["config", key, "zstd"]).ok();

        let run = traced(&repo, &["lfs", "pull"]).ok();

        assert_counts(
            &run,
            &[
                ("Accept-Encoding: gzip", 1),
                ("Accept-Encoding: zstd", 1),
                ("Content-Encoding: zstd", 1),
                ("decompressing zstd-encoded response", 1),
            ],
        );
        repo.assert_local_object(&oid(contents), contents.len());
    }
}

/// Two transfer workers, each with its own decoder.
#[test]
fn batch_storage_http_download_with_zstd_encoding_and_multiple_objects() {
    let t = Test::new();
    let contents = [
        "storage-download-encoding-zstd-1",
        "storage-download-encoding-zstd-2",
        "storage-download-encoding-zstd-3",
    ];
    let repo = pushed(
        &t,
        "batch-storage-download-encoding-zstd-multiple",
        &[
            ("test1.dat", contents[0]),
            ("test2.dat", contents[1]),
            ("test3.dat", contents[2]),
        ],
    );
    repo.git(["config", "lfs.transfer.httpDownloadEncoding", "zstd"])
        .ok();
    repo.git(["config", "lfs.concurrentTransfers", "2"]).ok();

    let run = traced(&repo, &["lfs", "pull"]).ok();

    assert_counts(
        &run,
        &[
            ("Accept-Encoding: gzip", 1),
            ("Accept-Encoding: zstd", 3),
            ("Content-Encoding: zstd", 3),
            ("decompressing zstd-encoded response", 3),
            ("initialized zstd decoder", 2),
            ("closed zstd decoder", 2),
        ],
    );
    for contents in contents {
        repo.assert_local_object(&oid(contents), contents.len());
    }
}

/// The server cuts the zstd response off after its first frame, half the
/// object. The retry asks for the rest with a Range header and no
/// encoding.
#[test]
fn batch_storage_http_download_with_zstd_encoding_retries_with_range_header_and_without_encoding() {
    let t = Test::new();
    let contents = "storage-download-encoding-zstd-retry-range";
    let oid = oid(contents);
    let len = contents.len();
    let repo = pushed(
        &t,
        "batch-storage-download-encoding-zstd-retry-range",
        &[("a.dat", contents)],
    );
    repo.git(["config", "lfs.transfer.httpDownloadEncoding", "zstd"])
        .ok();

    let run = traced(&repo, &["lfs", "fetch"]).ok();

    assert_counts(
        &run,
        &[
            ("Accept-Encoding: gzip", 2),
            ("Accept-Encoding: zstd", 1),
            ("Content-Encoding: zstd", 1),
            ("decompressing zstd-encoded response", 1),
        ],
    );
    let half = len / 2;
    let run = run
        .has(&format!("Attempting to resume download of \"{oid}\""))
        .has(&format!("tq: retrying object {oid}"))
        .has(&format!("Range: bytes={half}-{}", len - 1))
        .has("206 Partial Content")
        .has(&format!("Content-Range: bytes {half}-{}/{len}", len - 1));
    let accepted = run
        .output()
        .lines()
        .any(|l| l.contains("xfer: server accepted resume") && l.contains(&oid));
    assert!(accepted, "{run}");
    repo.assert_local_object(&oid, len);
}

#[test]
fn batch_storage_http_download_with_invalid_encoding() {
    let t = Test::new();
    let repo = pushed(
        &t,
        "batch-storage-download-encoding-invalid",
        &[("a.dat", "contents")],
    );
    repo.git(["config", "lfs.transfer.httpDownloadEncoding", "br"])
        .ok();

    repo.lfs_command()
        .args(["pull"])
        .merged()
        .run()
        .fails()
        .has("unsupported lfs.transfer.httpDownloadEncoding");
}

/// The trace from the first PUT to the storage on, without the headers of
/// the batch and lock requests before it.
fn put_trace(t: &Test, run: &Run) -> String {
    let marker = format!(" PUT {}/storage/", t.server().url());
    let output = run.output();
    let start = output
        .lines()
        .position(|l| l.contains(&marker))
        .unwrap_or_else(|| panic!("no PUT\n{run}"));
    output.lines().skip(start).collect::<Vec<_>>().join("\n")
}

/// The repository's name makes the server ask for chunked uploads.
#[test]
fn batch_storage_http_upload_with_chunked_transfer_encoding() {
    let t = Test::new();
    let name = "batch-storage-upload-encoding-chunked";
    let contents = "storage-upload-encoding-chunked";
    let repo = committed(&t, name, &[("a.dat", contents)]);

    let run = traced(&repo, &["push", "origin", "main"]).ok();

    let put = put_trace(&t, &run);
    assert!(put.contains("> Transfer-Encoding: chunked"), "{put}");
    assert!(!put.to_lowercase().contains("> content-length:"), "{put}");
    t.assert_server_objects(name, &[&oid(contents)], &[]);
}

#[test]
fn batch_storage_http_upload_without_transfer_encoding() {
    let t = Test::new();
    let name = "batch-storage-upload-encoding-none";
    let contents = "storage-upload-encoding-none";
    let repo = committed(&t, name, &[("a.dat", contents)]);

    let run = traced(&repo, &["push", "origin", "main"]).ok();

    let put = put_trace(&t, &run);
    let length = format!("> Content-Length: {}", contents.len());
    assert!(put.lines().any(|l| l.ends_with(&length)), "{put}");
    assert!(
        !put.to_lowercase().contains("> transfer-encoding:"),
        "{put}"
    );
    t.assert_server_objects(name, &[&oid(contents)], &[]);
}
