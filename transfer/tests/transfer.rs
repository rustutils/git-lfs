//! End-to-end tests for the transfer queue. Wiremock impersonates both
//! the LFS server (`/objects/batch`) and the storage backend
//! (`/storage/{oid}`), so we exercise the full pipeline:
//! batch → action URL → byte movement → store / verify.

use std::sync::Arc;
use std::time::Duration;

use git_lfs_api::{Auth, Client as ApiClient, ObjectSpec};
use git_lfs_pointer::Oid;
use git_lfs_store::Store;
use git_lfs_transfer::{Event, Transfer, TransferConfig};
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use url::Url;
use wiremock::matchers::{body_bytes, body_json, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

// ---------- helpers --------------------------------------------------------

/// Pre-known fixture: SHA-256("abc") = ba78... ; content = b"abc".
const ABC_OID: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const ABC_BYTES: &[u8] = b"abc";

fn fast_config() -> TransferConfig {
    // Tiny backoff so retry tests don't sleep hundreds of ms in CI.
    TransferConfig {
        concurrency: 4,
        max_attempts: 3,
        initial_backoff: Duration::from_millis(1),
        backoff_max: Duration::from_millis(5),
        url_rewriter: None,
        ..TransferConfig::default()
    }
}

fn make_transfer(server: &MockServer) -> (TempDir, Store, Transfer) {
    let tmp = TempDir::new().unwrap();
    let store = Store::new(tmp.path().join("lfs"));
    let api = ApiClient::new(Url::parse(&server.uri()).unwrap(), Auth::None);
    let transfer = Transfer::new(api, store.clone(), fast_config());
    (tmp, store, transfer)
}

fn drain(rx: &mut UnboundedReceiver<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

// ---------- downloads ------------------------------------------------------

#[tokio::test]
async fn download_happy_path_writes_verified_bytes_to_store() {
    let server = MockServer::start().await;
    let download_url = format!("{}/storage/{ABC_OID}", server.uri());

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "actions": { "download": { "href": download_url } }
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/storage/{ABC_OID}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(ABC_BYTES))
        .mount(&server)
        .await;

    let (_tmp, store, transfer) = make_transfer(&server);
    let (tx, mut rx) = unbounded_channel();
    let report = transfer
        .download(
            vec![ObjectSpec {
                oid: ABC_OID.into(),
                size: 3,
            }],
            None,
            Some(tx),
        )
        .await
        .unwrap();

    assert!(report.is_complete_success(), "{:?}", report.failed);
    assert_eq!(report.succeeded, vec![ABC_OID.to_string()]);

    // Bytes are in the store and match the expected OID.
    let oid = Oid::from_hex(ABC_OID).unwrap();
    assert!(store.contains_with_size(oid, 3));

    let events = drain(&mut rx);
    assert!(matches!(
        events.first(),
        Some(Event::Started { size: 3, .. })
    ));
    assert!(matches!(events.last(), Some(Event::Completed { .. })));
}

#[tokio::test]
async fn download_hash_mismatch_is_failed_and_not_retried() {
    let server = MockServer::start().await;
    // Server returns wrong content — won't hash to ABC_OID.
    let download_url = format!("{}/storage/{ABC_OID}", server.uri());

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "actions": { "download": { "href": download_url } }
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/storage/{ABC_OID}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"xyz"))
        .expect(1) // critical: hash mismatch is not retryable
        .mount(&server)
        .await;

    let (_tmp, store, transfer) = make_transfer(&server);
    let report = transfer
        .download(
            vec![ObjectSpec {
                oid: ABC_OID.into(),
                size: 3,
            }],
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(report.failed.len(), 1);
    let oid = Oid::from_hex(ABC_OID).unwrap();
    assert!(!store.contains(oid));
}

#[tokio::test]
async fn download_5xx_retries_then_succeeds() {
    let server = MockServer::start().await;
    let download_url = format!("{}/storage/{ABC_OID}", server.uri());

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "actions": { "download": { "href": download_url } }
            }]
        })))
        .mount(&server)
        .await;

    // First GET returns 503; subsequent attempts return the body. Wiremock's
    // first-match-wins semantics make this work without ordering tricks.
    Mock::given(method("GET"))
        .and(path(format!("/storage/{ABC_OID}")))
        .respond_with(FlakyResponder::new(vec![
            ResponseTemplate::new(503),
            ResponseTemplate::new(200).set_body_bytes(ABC_BYTES),
        ]))
        .mount(&server)
        .await;

    let (_tmp, store, transfer) = make_transfer(&server);
    let report = transfer
        .download(
            vec![ObjectSpec {
                oid: ABC_OID.into(),
                size: 3,
            }],
            None,
            None,
        )
        .await
        .unwrap();

    assert!(report.is_complete_success(), "{:?}", report.failed);
    assert!(store.contains(Oid::from_hex(ABC_OID).unwrap()));
}

#[tokio::test]
async fn download_per_object_error_surfaces_in_report() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "error": { "code": 404, "message": "Object does not exist" }
            }]
        })))
        .mount(&server)
        .await;

    let (_tmp, _store, transfer) = make_transfer(&server);
    let report = transfer
        .download(
            vec![ObjectSpec {
                oid: ABC_OID.into(),
                size: 3,
            }],
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(report.failed.len(), 1);
    let (oid, err) = &report.failed[0];
    assert_eq!(oid, ABC_OID);
    assert!(err.to_string().contains("Object does not exist"));
}

#[tokio::test]
async fn download_action_4xx_is_failed_without_retry() {
    let server = MockServer::start().await;
    let download_url = format!("{}/storage/{ABC_OID}", server.uri());

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "actions": { "download": { "href": download_url } }
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/storage/{ABC_OID}")))
        .respond_with(ResponseTemplate::new(403))
        .expect(1) // 403 is not retryable
        .mount(&server)
        .await;

    let (_tmp, _store, transfer) = make_transfer(&server);
    let report = transfer
        .download(
            vec![ObjectSpec {
                oid: ABC_OID.into(),
                size: 3,
            }],
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(report.failed.len(), 1);
}

#[tokio::test]
async fn download_action_header_is_forwarded() {
    let server = MockServer::start().await;
    let download_url = format!("{}/storage/{ABC_OID}", server.uri());

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "actions": {
                    "download": {
                        "href": download_url,
                        "header": { "X-Cdn-Token": "abc123" }
                    }
                }
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/storage/{ABC_OID}")))
        .and(wiremock::matchers::header("X-Cdn-Token", "abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(ABC_BYTES))
        .mount(&server)
        .await;

    let (_tmp, _store, transfer) = make_transfer(&server);
    let report = transfer
        .download(
            vec![ObjectSpec {
                oid: ABC_OID.into(),
                size: 3,
            }],
            None,
            None,
        )
        .await
        .unwrap();
    assert!(report.is_complete_success(), "{:?}", report.failed);
}

// ---------- uploads --------------------------------------------------------

#[tokio::test]
async fn upload_happy_path_streams_object_bytes() {
    let server = MockServer::start().await;
    let upload_url = format!("{}/storage/{ABC_OID}", server.uri());

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "actions": { "upload": { "href": upload_url } }
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path(format!("/storage/{ABC_OID}")))
        .and(body_bytes(ABC_BYTES))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let (_tmp, store, transfer) = make_transfer(&server);
    // Stage the bytes locally first — uploads stream from the store.
    let (oid, _) = store.insert(&mut ABC_BYTES.to_vec().as_slice()).unwrap();
    assert_eq!(oid.to_string(), ABC_OID);

    let report = transfer
        .upload(
            vec![ObjectSpec {
                oid: ABC_OID.into(),
                size: 3,
            }],
            None,
            None,
        )
        .await
        .unwrap();
    assert!(report.is_complete_success(), "{:?}", report.failed);
}

/// Accept one HTTP request on a raw socket, answer 200, and return the
/// request head as sent, before any HTTP parsing.
async fn capture_request_head(listener: tokio::net::TcpListener) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut sock, _) = listener.accept().await.unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = sock.read(&mut chunk).await.unwrap();
        assert!(n > 0, "connection closed before the request head");
        buf.extend_from_slice(&chunk[..n]);
    }
    // Let the body arrive, then answer without reading it all.
    let _ =
        tokio::time::timeout(std::time::Duration::from_millis(100), sock.read(&mut chunk)).await;
    sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
        .await
        .unwrap();
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    String::from_utf8_lossy(&buf[..end]).to_lowercase()
}

#[tokio::test]
async fn chunked_upload_has_no_content_length() {
    let server = MockServer::start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upload_url = format!(
        "http://{}/storage/{ABC_OID}",
        listener.local_addr().unwrap()
    );
    let head = tokio::spawn(capture_request_head(listener));
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "actions": { "upload": {
                    "href": upload_url,
                    "header": { "Transfer-Encoding": "chunked" }
                } }
            }]
        })))
        .mount(&server)
        .await;

    let (_tmp, store, transfer) = make_transfer(&server);
    store.insert(&mut ABC_BYTES.to_vec().as_slice()).unwrap();
    let spec = ObjectSpec {
        oid: ABC_OID.into(),
        size: 3,
    };
    transfer.upload(vec![spec], None, None).await.unwrap();

    let head = head.await.unwrap();
    assert!(head.contains("\r\ntransfer-encoding: chunked"), "{head}");
    assert!(!head.contains("content-length"), "{head}");
}

/// Run an upload whose action sends `Authorization: Bearer SECRET` to
/// `upload_url` on `server`.
async fn upload_with_auth(server: &MockServer, upload_url: &str) -> bool {
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "actions": { "upload": {
                    "href": upload_url,
                    "header": { "Authorization": "Bearer SECRET" }
                } }
            }]
        })))
        .mount(server)
        .await;

    let (_tmp, store, transfer) = make_transfer(server);
    store.insert(&mut ABC_BYTES.to_vec().as_slice()).unwrap();
    let spec = ObjectSpec {
        oid: ABC_OID.into(),
        size: 3,
    };
    let report = transfer.upload(vec![spec], None, None).await.unwrap();
    report.is_complete_success()
}

#[tokio::test]
async fn upload_redirect_to_another_host_drops_credentials() {
    let server = MockServer::start().await;
    let other = MockServer::start().await;
    let target = format!("{}/storage/{ABC_OID}", other.uri());

    Mock::given(method("PUT"))
        .and(path(format!("/storage/{ABC_OID}")))
        .respond_with(ResponseTemplate::new(307).insert_header("Location", target.as_str()))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/storage/{ABC_OID}")))
        .respond_with(ResponseTemplate::new(200))
        .mount(&other)
        .await;

    let upload_url = format!("{}/storage/{ABC_OID}", server.uri());
    assert!(upload_with_auth(&server, &upload_url).await);

    let received = other.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    assert!(
        !received[0].headers.contains_key("authorization"),
        "credentials sent to the redirect target"
    );
}

#[tokio::test]
async fn upload_redirect_on_the_same_host_keeps_credentials() {
    let server = MockServer::start().await;
    let target = format!("{}/moved/{ABC_OID}", server.uri());

    Mock::given(method("PUT"))
        .and(path(format!("/storage/{ABC_OID}")))
        .respond_with(ResponseTemplate::new(307).insert_header("Location", target.as_str()))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/moved/{ABC_OID}")))
        .and(wiremock::matchers::header("Authorization", "Bearer SECRET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let upload_url = format!("{}/storage/{ABC_OID}", server.uri());
    assert!(upload_with_auth(&server, &upload_url).await);
}

/// A transfer against the LFS server `server` whose extra headers give
/// `name: value` only for URLs on `scope`.
fn transfer_with_extra_header(
    server: &MockServer,
    scope: &MockServer,
    name: &'static str,
    value: &'static str,
) -> (TempDir, Store, Transfer) {
    let tmp = TempDir::new().unwrap();
    let store = Store::new(tmp.path().join("lfs"));
    let api = ApiClient::new(Url::parse(&server.uri()).unwrap(), Auth::None);
    let prefix = scope.uri();
    let source: git_lfs_transfer::HeaderSource = Arc::new(move |url: &str| {
        if url.starts_with(&prefix) {
            vec![(name.to_owned(), value.to_owned())]
        } else {
            Vec::new()
        }
    });
    let config = TransferConfig {
        extra_headers: Some(source),
        ..fast_config()
    };
    let transfer = Transfer::new(api, store.clone(), config);
    (tmp, store, transfer)
}

#[tokio::test]
async fn extra_headers_are_looked_up_for_each_action_url() {
    let server = MockServer::start().await;
    let storage = MockServer::start().await;
    let href = format!("{}/storage/{ABC_OID}", storage.uri());

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "actions": { "download": { "href": href } }
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(ABC_BYTES))
        .mount(&storage)
        .await;

    // Scoped to the LFS server, so it must not reach the storage host.
    let (_tmp, _store, transfer) =
        transfer_with_extra_header(&server, &server, "X-Token", "secret");
    let spec = ObjectSpec {
        oid: ABC_OID.into(),
        size: 3,
    };
    let report = transfer.download(vec![spec], None, None).await.unwrap();
    assert!(report.is_complete_success(), "{:?}", report.failed);
    let received = storage.received_requests().await.unwrap();
    assert!(!received[0].headers.contains_key("x-token"));

    // Scoped to the storage host, so it must be sent there.
    let (_tmp, _store, transfer) =
        transfer_with_extra_header(&server, &storage, "X-Token", "secret");
    let spec = ObjectSpec {
        oid: ABC_OID.into(),
        size: 3,
    };
    transfer.download(vec![spec], None, None).await.unwrap();
    let received = storage.received_requests().await.unwrap();
    assert_eq!(received.last().unwrap().headers["x-token"], "secret");
}

#[tokio::test]
async fn upload_redirect_drops_extra_header_credentials() {
    let server = MockServer::start().await;
    let other = MockServer::start().await;
    let target = format!("{}/storage/{ABC_OID}", other.uri());
    let href = format!("{}/storage/{ABC_OID}", server.uri());

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "actions": { "upload": { "href": href } }
            }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/storage/{ABC_OID}")))
        .respond_with(ResponseTemplate::new(307).insert_header("Location", target.as_str()))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&other)
        .await;

    let (_tmp, store, transfer) =
        transfer_with_extra_header(&server, &server, "Authorization", "Bearer EXTRA");
    store.insert(&mut ABC_BYTES.to_vec().as_slice()).unwrap();
    let spec = ObjectSpec {
        oid: ABC_OID.into(),
        size: 3,
    };
    let report = transfer.upload(vec![spec], None, None).await.unwrap();
    assert!(report.is_complete_success(), "{:?}", report.failed);
    let received = other.received_requests().await.unwrap();
    assert!(!received[0].headers.contains_key("authorization"));
}

#[tokio::test]
async fn upload_with_verify_callback_posts_oid_and_size() {
    let server = MockServer::start().await;
    let upload_url = format!("{}/storage/{ABC_OID}", server.uri());
    let verify_url = format!("{}/verify", server.uri());

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "actions": {
                    "upload": { "href": upload_url },
                    "verify": { "href": verify_url }
                }
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path(format!("/storage/{ABC_OID}")))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/verify"))
        .and(body_json(json!({ "oid": ABC_OID, "size": 3 })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let (_tmp, store, transfer) = make_transfer(&server);
    store.insert(&mut ABC_BYTES.to_vec().as_slice()).unwrap();

    let report = transfer
        .upload(
            vec![ObjectSpec {
                oid: ABC_OID.into(),
                size: 3,
            }],
            None,
            None,
        )
        .await
        .unwrap();
    assert!(report.is_complete_success(), "{:?}", report.failed);
}

#[tokio::test]
async fn upload_no_actions_means_server_already_has_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{ "oid": ABC_OID, "size": 3 }]
        })))
        .mount(&server)
        .await;

    let (_tmp, _store, transfer) = make_transfer(&server);
    let report = transfer
        .upload(
            vec![ObjectSpec {
                oid: ABC_OID.into(),
                size: 3,
            }],
            None,
            None,
        )
        .await
        .unwrap();
    assert!(report.is_complete_success());
    assert_eq!(report.succeeded, vec![ABC_OID.to_string()]);
}

#[tokio::test]
async fn upload_verify_failure_marks_transfer_failed() {
    let server = MockServer::start().await;
    let upload_url = format!("{}/storage/{ABC_OID}", server.uri());
    let verify_url = format!("{}/verify", server.uri());

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": ABC_OID, "size": 3,
                "actions": {
                    "upload": { "href": upload_url },
                    "verify": { "href": verify_url }
                }
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("PUT"))
        .and(path(format!("/storage/{ABC_OID}")))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    // Verify returns 422 — not retryable.
    Mock::given(method("POST"))
        .and(path("/verify"))
        .respond_with(ResponseTemplate::new(422))
        .mount(&server)
        .await;

    let (_tmp, store, transfer) = make_transfer(&server);
    store.insert(&mut ABC_BYTES.to_vec().as_slice()).unwrap();

    let report = transfer
        .upload(
            vec![ObjectSpec {
                oid: ABC_OID.into(),
                size: 3,
            }],
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(report.failed.len(), 1);
}

// ---------- mixed batches --------------------------------------------------

#[tokio::test]
async fn mixed_success_and_failure_in_one_batch() {
    let server = MockServer::start().await;
    let other_oid = "0000000000000000000000000000000000000000000000000000000000000abc";
    let download_url = format!("{}/storage/{ABC_OID}", server.uri());

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [
                {
                    "oid": ABC_OID, "size": 3,
                    "actions": { "download": { "href": download_url } }
                },
                {
                    "oid": other_oid, "size": 99,
                    "error": { "code": 404, "message": "missing" }
                }
            ]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path(format!("/storage/{ABC_OID}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(ABC_BYTES))
        .mount(&server)
        .await;

    let (_tmp, _store, transfer) = make_transfer(&server);
    let report = transfer
        .download(
            vec![
                ObjectSpec {
                    oid: ABC_OID.into(),
                    size: 3,
                },
                ObjectSpec {
                    oid: other_oid.into(),
                    size: 99,
                },
            ],
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(report.succeeded, vec![ABC_OID.to_string()]);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, other_oid);
}

// ---------- empty batch ----------------------------------------------------

#[tokio::test]
async fn empty_batch_returns_empty_report_without_calling_api() {
    let server = MockServer::start().await;
    // No mocks registered: any HTTP call would 404. The point is to verify
    // we short-circuit before making one.
    let (_tmp, _store, transfer) = make_transfer(&server);
    let report = transfer.download(vec![], None, None).await.unwrap();
    assert!(report.is_complete_success());
    assert!(report.succeeded.is_empty());
}

// ---------- helpers: flaky responder for retry tests ----------------------

/// Returns each template in sequence, sticking on the last one for any
/// further requests. Used to simulate transient failures.
struct FlakyResponder {
    templates: Arc<Mutex<Vec<ResponseTemplate>>>,
    fallback: ResponseTemplate,
}

impl FlakyResponder {
    fn new(mut templates: Vec<ResponseTemplate>) -> Self {
        let fallback = templates.last().cloned().expect("at least one template");
        templates.reverse(); // we pop from the end
        Self {
            templates: Arc::new(Mutex::new(templates)),
            fallback,
        }
    }
}

impl Respond for FlakyResponder {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        // try_lock: this responder runs sync inside wiremock, so we can't
        // await here. The Mutex contention is negligible — wiremock
        // serializes responder invocations in practice.
        let mut guard = match self.templates.try_lock() {
            Ok(g) => g,
            Err(_) => return self.fallback.clone(),
        };
        guard.pop().unwrap_or_else(|| self.fallback.clone())
    }
}

// ---------- Retry-After limit ----------------------------------------------

#[tokio::test]
async fn retry_after_beyond_max_retry_time_fails_at_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "10"))
        .mount(&server)
        .await;
    let tmp = TempDir::new().unwrap();
    let store = Store::new(tmp.path().join("lfs"));
    let api = ApiClient::new(Url::parse(&server.uri()).unwrap(), Auth::None);
    let config = TransferConfig {
        max_retry_time: Duration::from_secs(5),
        ..fast_config()
    };
    let transfer = Transfer::new(api, store, config);
    let spec = ObjectSpec {
        oid: ABC_OID.into(),
        size: 3,
    };

    let started = std::time::Instant::now();
    let result = transfer.download(vec![spec], None, None).await;
    assert!(result.is_err(), "{result:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "waited for Retry-After"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

// ---------- unknown OIDs ---------------------------------------------------

#[tokio::test]
async fn object_the_batch_didnt_ask_for_is_not_uploaded() {
    let server = MockServer::start().await;
    let (_tmp, store, transfer) = make_transfer(&server);
    store.insert(&mut ABC_BYTES.to_vec().as_slice()).unwrap();
    // A second object in the store that the server shouldn't get.
    let (other, _) = store.insert(&mut b"secret".as_slice()).unwrap();
    let other_oid = other.to_string();
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{
                "oid": other_oid, "size": 6,
                "actions": { "upload": { "href": format!("{}/storage/x", server.uri()) } }
            }]
        })))
        .mount(&server)
        .await;

    let spec = ObjectSpec {
        oid: ABC_OID.into(),
        size: 3,
    };
    let report = transfer.upload(vec![spec], None, None).await.unwrap();

    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, other_oid);
    assert_eq!(
        report.failed[0].1.to_string(),
        "The server returned an unknown OID."
    );
    let puts = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "PUT")
        .count();
    assert_eq!(puts, 0, "uploaded an object that wasn't requested");
}
