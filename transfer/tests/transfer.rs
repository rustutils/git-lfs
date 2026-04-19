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
            vec![ObjectSpec { oid: ABC_OID.into(), size: 3 }],
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
    assert!(matches!(events.first(), Some(Event::Started { size: 3, .. })));
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
            vec![ObjectSpec { oid: ABC_OID.into(), size: 3 }],
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
            vec![ObjectSpec { oid: ABC_OID.into(), size: 3 }],
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
            vec![ObjectSpec { oid: ABC_OID.into(), size: 3 }],
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
            vec![ObjectSpec { oid: ABC_OID.into(), size: 3 }],
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
            vec![ObjectSpec { oid: ABC_OID.into(), size: 3 }],
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
            vec![ObjectSpec { oid: ABC_OID.into(), size: 3 }],
            None,
            None,
        )
        .await
        .unwrap();
    assert!(report.is_complete_success(), "{:?}", report.failed);
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
            vec![ObjectSpec { oid: ABC_OID.into(), size: 3 }],
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
            vec![ObjectSpec { oid: ABC_OID.into(), size: 3 }],
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
            vec![ObjectSpec { oid: ABC_OID.into(), size: 3 }],
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
                ObjectSpec { oid: ABC_OID.into(), size: 3 },
                ObjectSpec { oid: other_oid.into(), size: 99 },
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

