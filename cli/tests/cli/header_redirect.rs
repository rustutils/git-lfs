//! `t-header-redirect.sh`.
//!
//! For repositories named `header-redirect-*`, the server logs which
//! headers carry the source marker, one line per request with its redirect
//! stage, to `<repo>.headers`.

use crate::harness::{Repo, Test, oid};

const SENSITIVE: [&str; 2] = ["Authorization", "Cookie"];
const MARKER: &str = "lfs-source-marker";

/// The remote repository `name`, cloned, with `test.bin` holding
/// `contents` committed, lock verification off, and an empty header log.
fn header_redirect_repo<'a>(t: &'a Test, name: &str, contents: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.bin"]).ok();
    repo.write("test.bin", contents);
    repo.git(["add", ".gitattributes", "test.bin"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    // Fewer requests make the header checks simpler.
    repo.git(["config", "lfs.locksVerify", "false"]).ok();
    std::fs::write(headers_log(t, name), "").unwrap();
    repo
}

fn headers_log(t: &Test, name: &str) -> std::path::PathBuf {
    t.dir().join("remote").join(format!("{name}.headers"))
}

/// Send the sensitive headers, and a benign one, to `url`.
fn add_sensitive_headers(repo: &Repo, url: &str) {
    let key = format!("http.{url}.extraHeader");
    for header in SENSITIVE {
        let value = if header == "Cookie" {
            format!("lfs-source-cookie={MARKER}")
        } else {
            MARKER.to_owned()
        };
        repo.git(["config", "--add", &key, &format!("{header}: {value}")])
            .ok();
    }
    repo.git(["config", "--add", &key, &format!("X-Test-Benign: {MARKER}")])
        .ok();
}

/// Run the pre-push hook for `main` against `url`.
fn pre_push(repo: &Repo, url: &str) {
    repo.lfs_command()
        .args(["pre-push", "origin", url])
        .stdin("refs/heads/main main refs/heads/main 0000000000000000000000000000000000000000\n")
        .run()
        .ok();
}

/// Whether the server logged `header` with the marker at `stage`.
fn seen(t: &Test, name: &str, stage: &str, header: &str) -> bool {
    let log = std::fs::read_to_string(headers_log(t, name)).unwrap();
    log.lines().any(|line| {
        line.strip_prefix(stage)
            .and_then(|rest| rest.strip_prefix(' '))
            .is_some_and(|names| names.split(',').any(|n| n == header))
    })
}

/// Check that the source request had every header and the redirected one
/// only the benign one.
#[track_caller]
fn assert_redirect_headers(t: &Test, name: &str, source: &str, target: &str) {
    for header in SENSITIVE.into_iter().chain(["X-Test-Benign"]) {
        assert!(seen(t, name, source, header), "{source} lacks {header}");
    }
    assert!(
        seen(t, name, target, "X-Test-Benign"),
        "{target} lacks benign"
    );
    for header in SENSITIVE {
        assert!(!seen(t, name, target, header), "{target} has {header}");
    }
}

/// Check that pushing through `source_url`, a redirect to another origin,
/// drops the sensitive headers.
fn assert_push_drops_headers(name: &str, contents: &str, source_url: impl Fn(&Test) -> String) {
    let t = Test::new();
    let repo = header_redirect_repo(&t, name, contents);
    let url = source_url(&t);
    repo.git(["config", "lfs.url", &url]).ok();
    add_sensitive_headers(&repo, &url);

    pre_push(&repo, &url);

    assert_redirect_headers(&t, name, "batch-source", "batch-target");
    t.assert_server_objects(name, &[&oid(contents)], &[]);
}

/// The SSL server's port, over plain HTTP, which it also accepts.
fn ssl_server_http(t: &Test) -> String {
    t.server().ssl_url().replacen("https:", "http:", 1)
}

/// From `localhost` to `127.0.0.1`.
#[test]
fn pre_push_redirect_to_a_different_host_drops_sensitive_headers() {
    assert_push_drops_headers("header-redirect-host", "host-test", |t| {
        format!(
            "{}/redirect307/abs/header-redirect-host.git/info/lfs",
            t.localhost_url()
        )
    });
}

/// From the SSL server's port to the main one.
#[test]
fn pre_push_redirect_to_a_different_port_drops_sensitive_headers() {
    assert_push_drops_headers("header-redirect-port", "port-test", |t| {
        format!(
            "{}/redirect307/abs/header-redirect-port.git/info/lfs",
            ssl_server_http(t)
        )
    });
}

/// To HTTPS on the same port.
#[test]
fn pre_push_http_to_https_redirect_drops_sensitive_headers() {
    assert_push_drops_headers("header-redirect-scheme", "scheme-test", |t| {
        format!(
            "{}/redirect307/ssl/header-redirect-scheme.git/info/lfs",
            ssl_server_http(t)
        )
    });
}

#[test]
fn pre_push_same_origin_redirect_retains_headers() {
    let t = Test::new();
    let name = "header-redirect-same";
    let contents = "same-test";
    let repo = header_redirect_repo(&t, name, contents);
    let url = format!("{}/redirect307/rel/{name}.git/info/lfs", t.server().url());
    repo.git(["config", "lfs.url", &url]).ok();
    add_sensitive_headers(&repo, &url);

    pre_push(&repo, &url);

    for header in SENSITIVE.into_iter().chain(["X-Test-Benign"]) {
        assert!(seen(&t, name, "batch-source", header), "{header}");
        assert!(seen(&t, name, "batch-target", header), "{header}");
    }
    t.assert_server_objects(name, &[&oid(contents)], &[]);
}

/// The server's object URLs redirect from `localhost` to `127.0.0.1`. In
/// `direct` mode, it marks downloads as authenticated, so the client
/// takes its unauthenticated path.
fn assert_download_drops_headers(mode: &str) {
    let t = Test::new();
    let name = format!("header-redirect-download-{mode}");
    let contents = "redirect-storage-upload";
    let repo = header_redirect_repo(&t, &name, contents);
    repo.git(["push", "origin", "main"]).ok();
    add_sensitive_headers(&repo, &format!("{}/redirect307/objects", t.localhost_url()));
    repo.remove_local_objects();

    repo.lfs(["fetch", "origin", "main"]).ok();

    assert_redirect_headers(&t, &name, "object-source", "object-target");
    repo.assert_local_object(&oid(contents), contents.len());
}

#[test]
fn download_redirect_does_not_forward_sensitive_headers_auth_path() {
    assert_download_drops_headers("auth");
}

#[test]
fn download_redirect_does_not_forward_sensitive_headers_direct_path() {
    assert_download_drops_headers("direct");
}

/// The batch response sets a cookie, which goes with the first object
/// request to `localhost` but not the redirected one to `127.0.0.1`.
#[test]
fn cookie_from_an_earlier_response_does_not_follow_a_cross_origin_redirect() {
    let t = Test::new();
    let name = "header-redirect-cookie";
    let contents = "redirect-storage-upload";
    let repo = header_redirect_repo(&t, name, contents);
    let url = format!("{}/{name}.git/info/lfs", t.localhost_url());
    repo.git(["config", "lfs.url", &url]).ok();
    let jar = t.dir().join("remote").join(format!("{name}-cookies.txt"));
    std::fs::write(&jar, "").unwrap();
    repo.git(["config", "http.cookieFile", jar.to_str().unwrap()])
        .ok();

    pre_push(&repo, &url);

    assert!(seen(&t, name, "object-source", "Cookie"));
    assert!(!seen(&t, name, "object-target", "Cookie"));
    t.assert_server_objects(name, &[&oid(contents)], &[]);
}

/// Not from the shell suite: an object upload that redirects to another
/// origin drops the sensitive headers too. The shell tests only redirect
/// uploads with a cookie from the jar.
#[test]
fn upload_redirect_does_not_forward_sensitive_headers() {
    let t = Test::new();
    // Its object URLs redirect from `localhost` to `127.0.0.1`.
    let name = "header-redirect-download-auth";
    let contents = "redirect-storage-upload";
    let repo = header_redirect_repo(&t, name, contents);
    add_sensitive_headers(&repo, &format!("{}/redirect307/objects", t.localhost_url()));

    repo.git(["push", "origin", "main"]).ok();

    assert_redirect_headers(&t, name, "object-source", "object-target");
    t.assert_server_objects(name, &[&oid(contents)], &[]);
}
