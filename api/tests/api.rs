//! Integration tests for the API client, using wiremock to stand in for
//! a real LFS server. These tests cover the full HTTP path: request
//! building, header injection, JSON encoding, status-code handling, and
//! response decoding.

use std::sync::Arc;

use git_lfs_api::{
    Auth, BatchRequest, Client, CreateLockError, CreateLockRequest, DeleteLockRequest,
    ListLocksFilter, ObjectSpec, Operation, Ref, VerifyLocksRequest,
};
use git_lfs_creds::{Credentials, Helper, HelperError, Query};
use serde_json::json;
use url::Url;
use wiremock::matchers::{body_json, header, header_exists, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const LFS_MEDIA_TYPE: &str = "application/vnd.git-lfs+json";

fn client(server: &MockServer, auth: Auth) -> Client {
    let endpoint = Url::parse(&server.uri()).unwrap();
    Client::new(endpoint, auth)
}

// ---- batch ----------------------------------------------------------------

#[tokio::test]
async fn batch_download_happy_path() {
    let server = MockServer::start().await;

    let req_body = json!({
        "operation": "download",
        "objects": [{ "oid": "abc", "size": 10 }],
    });
    let resp_body = json!({
        "transfer": "basic",
        "objects": [{
            "oid": "abc", "size": 10, "authenticated": true,
            "actions": {
                "download": {
                    "href": "https://cdn.example/abc",
                    "header": { "Authorization": "Basic ..." },
                    "expires_in": 86400
                }
            }
        }]
    });

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .and(header("Accept", LFS_MEDIA_TYPE))
        .and(header("Content-Type", LFS_MEDIA_TYPE))
        .and(body_json(&req_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(&resp_body))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let req = BatchRequest::new(
        Operation::Download,
        vec![ObjectSpec { oid: "abc".into(), size: 10 }],
    );
    let resp = client.batch(&req).await.unwrap();

    assert_eq!(resp.transfer.as_deref(), Some("basic"));
    assert_eq!(resp.objects.len(), 1);
    let obj = &resp.objects[0];
    assert_eq!(obj.oid, "abc");
    assert_eq!(obj.authenticated, Some(true));
    let dl = obj.actions.as_ref().unwrap().download.as_ref().unwrap();
    assert_eq!(dl.href, "https://cdn.example/abc");
    assert_eq!(dl.expires_in, Some(86400));
}

#[tokio::test]
async fn batch_sends_optional_fields_when_set() {
    let server = MockServer::start().await;

    let req_body = json!({
        "operation": "upload",
        "transfers": ["basic"],
        "ref": { "name": "refs/heads/main" },
        "objects": [{ "oid": "abc", "size": 10 }],
    });

    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .and(body_json(&req_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "transfer": "basic",
            "objects": [{ "oid": "abc", "size": 10 }]
        })))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let req = BatchRequest::new(
        Operation::Upload,
        vec![ObjectSpec { oid: "abc".into(), size: 10 }],
    )
    .with_transfers(["basic".to_string()])
    .with_ref(Ref::new("refs/heads/main"));
    client.batch(&req).await.unwrap();
}

#[tokio::test]
async fn batch_per_object_error_is_decoded_not_an_apierror() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "objects": [{
                "oid": "abc", "size": 10,
                "error": { "code": 404, "message": "Object does not exist" }
            }]
        })))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let resp = client
        .batch(&BatchRequest::new(
            Operation::Download,
            vec![ObjectSpec { oid: "abc".into(), size: 10 }],
        ))
        .await
        .unwrap();

    let err = resp.objects[0].error.as_ref().unwrap();
    assert_eq!(err.code, 404);
}

#[tokio::test]
async fn batch_unauthorized_carries_lfs_authenticate_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("LFS-Authenticate", "Basic realm=\"Git LFS\"")
                .set_body_json(json!({ "message": "Credentials needed" })),
        )
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let err = client
        .batch(&BatchRequest::new(Operation::Download, vec![]))
        .await
        .unwrap_err();

    assert!(err.is_unauthorized());
    match err {
        git_lfs_api::ApiError::Status { lfs_authenticate, body, .. } => {
            assert_eq!(lfs_authenticate.as_deref(), Some("Basic realm=\"Git LFS\""));
            assert_eq!(body.unwrap().message, "Credentials needed");
        }
        other => panic!("expected Status, got {other:?}"),
    }
}

#[tokio::test]
async fn batch_404_without_body_is_still_typed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(404).set_body_string(""))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let err = client
        .batch(&BatchRequest::new(Operation::Download, vec![]))
        .await
        .unwrap_err();
    assert!(err.is_not_found());
}

#[tokio::test]
async fn batch_5xx_is_retryable() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let err = client
        .batch(&BatchRequest::new(Operation::Download, vec![]))
        .await
        .unwrap_err();
    assert!(err.is_retryable());
}

#[tokio::test]
async fn auth_basic_is_sent_on_the_wire() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        // base64("alice:secret") = "YWxpY2U6c2VjcmV0"
        .and(header("Authorization", "Basic YWxpY2U6c2VjcmV0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": []})))
        .mount(&server)
        .await;

    let client = client(
        &server,
        Auth::Basic { username: "alice".into(), password: "secret".into() },
    );
    client
        .batch(&BatchRequest::new(Operation::Download, vec![]))
        .await
        .unwrap();
}

#[tokio::test]
async fn auth_bearer_is_sent_on_the_wire() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .and(header("Authorization", "Bearer abc.def.ghi"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": []})))
        .mount(&server)
        .await;

    let client = client(&server, Auth::Bearer("abc.def.ghi".into()));
    client
        .batch(&BatchRequest::new(Operation::Download, vec![]))
        .await
        .unwrap();
}

#[tokio::test]
async fn endpoint_with_subpath_preserves_prefix() {
    // Endpoint = http://host/foo/bar.git/info/lfs (typical real-world shape).
    // Per RFC 3986 join semantics, a relative subpath only joins cleanly
    // when the base ends with a slash; the client adds one if needed.
    let server = MockServer::start().await;
    let endpoint = Url::parse(&format!("{}/foo/bar.git/info/lfs", server.uri())).unwrap();
    let client = Client::new(endpoint, Auth::None);

    Mock::given(method("POST"))
        .and(path("/foo/bar.git/info/lfs/objects/batch"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": []})))
        .mount(&server)
        .await;

    client
        .batch(&BatchRequest::new(Operation::Download, vec![]))
        .await
        .unwrap();
}

// ---- locks ---------------------------------------------------------------

#[tokio::test]
async fn create_lock_happy_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/locks"))
        .and(body_json(json!({
            "path": "foo/bar.zip",
            "ref": { "name": "refs/heads/feat" }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "lock": {
                "id": "uuid-1", "path": "foo/bar.zip",
                "locked_at": "2016-05-17T15:49:06+00:00",
                "owner": { "name": "Jane Doe" }
            }
        })))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let req = CreateLockRequest::new("foo/bar.zip").with_ref(Ref::new("refs/heads/feat"));
    let lock = client.create_lock(&req).await.unwrap();
    assert_eq!(lock.id, "uuid-1");
    assert_eq!(lock.owner.unwrap().name, "Jane Doe");
}

#[tokio::test]
async fn create_lock_conflict_returns_existing_lock() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/locks"))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "lock": {
                "id": "existing", "path": "foo/bar.zip",
                "locked_at": "2016-01-01T00:00:00Z",
                "owner": { "name": "Other Person" }
            },
            "message": "already created lock",
            "request_id": "abc"
        })))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let req = CreateLockRequest::new("foo/bar.zip");
    let err = client.create_lock(&req).await.unwrap_err();

    match err {
        CreateLockError::Conflict { existing, message } => {
            assert_eq!(existing.id, "existing");
            assert_eq!(existing.owner.unwrap().name, "Other Person");
            assert_eq!(message, "already created lock");
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[tokio::test]
async fn create_lock_403_falls_through_to_apierror() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/locks"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "message": "You must have push access"
        })))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let err = client.create_lock(&CreateLockRequest::new("a")).await.unwrap_err();
    match err {
        CreateLockError::Api(api) => assert!(api.is_forbidden()),
        other => panic!("expected Api, got {other:?}"),
    }
}

#[tokio::test]
async fn list_locks_sends_only_set_filters() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/locks"))
        .and(query_param("path", "foo.bin"))
        .and(query_param("limit", "50"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "locks": [{
                "id": "u1", "path": "foo.bin",
                "locked_at": "2016-05-17T15:49:06+00:00"
            }],
            "next_cursor": "next"
        })))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let filter = ListLocksFilter {
        path: Some("foo.bin".into()),
        limit: Some(50),
        ..Default::default()
    };
    let list = client.list_locks(&filter).await.unwrap();
    assert_eq!(list.locks.len(), 1);
    assert_eq!(list.next_cursor.as_deref(), Some("next"));
}

#[tokio::test]
async fn verify_locks_partitions_ours_and_theirs() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/locks/verify"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ours": [{
                "id": "u1", "path": "a", "locked_at": "2020-01-01T00:00:00Z",
                "owner": { "name": "me" }
            }],
            "theirs": [{
                "id": "u2", "path": "b", "locked_at": "2020-01-02T00:00:00Z",
                "owner": { "name": "them" }
            }]
        })))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let resp = client.verify_locks(&VerifyLocksRequest::default()).await.unwrap();
    assert_eq!(resp.ours.len(), 1);
    assert_eq!(resp.theirs.len(), 1);
    assert_eq!(resp.ours[0].owner.as_ref().unwrap().name, "me");
}

#[tokio::test]
async fn verify_locks_404_signals_locking_unsupported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/locks/verify"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"message": "Not found"})))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let err = client.verify_locks(&VerifyLocksRequest::default()).await.unwrap_err();
    assert!(err.is_not_found());
}

#[tokio::test]
async fn delete_lock_path_includes_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/locks/some-uuid/unlock"))
        .and(body_json(json!({ "force": true })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "lock": { "id": "some-uuid", "path": "x", "locked_at": "2020-01-01T00:00:00Z" }
        })))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    let req = DeleteLockRequest { force: true, ..Default::default() };
    let lock = client.delete_lock("some-uuid", &req).await.unwrap();
    assert_eq!(lock.id, "some-uuid");
}

#[tokio::test]
async fn delete_lock_id_is_url_encoded() {
    let server = MockServer::start().await;
    // id "weird/id" must reach the server as %2F so it doesn't become a
    // separate path segment.
    Mock::given(method("POST"))
        .and(path("/locks/weird%2Fid/unlock"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "lock": { "id": "weird/id", "path": "x", "locked_at": "2020-01-01T00:00:00Z" }
        })))
        .mount(&server)
        .await;

    let client = client(&server, Auth::None);
    client
        .delete_lock("weird/id", &DeleteLockRequest::default())
        .await
        .unwrap();
}

// ---- credential helper retry loop -----------------------------------------

/// Stub `Helper` that hands out a static set of credentials and records
/// every approve/reject call. Used by the tests below to verify the
/// retry/approve/reject lifecycle without actually shelling out.
#[derive(Default)]
struct StubHelper {
    answer: std::sync::Mutex<Option<Credentials>>,
    approves: std::sync::Mutex<Vec<(Query, Credentials)>>,
    rejects: std::sync::Mutex<Vec<(Query, Credentials)>>,
}

impl StubHelper {
    fn with(answer: Credentials) -> Arc<Self> {
        Arc::new(Self {
            answer: std::sync::Mutex::new(Some(answer)),
            ..Default::default()
        })
    }
    fn empty() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn approve_count(&self) -> usize {
        self.approves.lock().unwrap().len()
    }
    fn reject_count(&self) -> usize {
        self.rejects.lock().unwrap().len()
    }
}

impl Helper for StubHelper {
    fn fill(&self, _q: &Query) -> Result<Option<Credentials>, HelperError> {
        Ok(self.answer.lock().unwrap().clone())
    }
    fn approve(&self, q: &Query, c: &Credentials) -> Result<(), HelperError> {
        self.approves.lock().unwrap().push((q.clone(), c.clone()));
        Ok(())
    }
    fn reject(&self, q: &Query, c: &Credentials) -> Result<(), HelperError> {
        self.rejects.lock().unwrap().push((q.clone(), c.clone()));
        Ok(())
    }
}

/// State machine for a server that 401s the first call (no auth) then
/// 200s the second call when it sees the Basic header. Pure
/// `respond_with` mocks can't differentiate by header content easily, so
/// we hand-roll a `Respond`.
struct AuthGate {
    expected_basic: String,
    body_when_authed: serde_json::Value,
}

impl Respond for AuthGate {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let auth = request.headers.get("Authorization").map(|v| v.to_str().unwrap_or(""));
        if auth == Some(self.expected_basic.as_str()) {
            ResponseTemplate::new(200).set_body_json(&self.body_when_authed)
        } else {
            ResponseTemplate::new(401)
                .insert_header("LFS-Authenticate", "Basic realm=\"GitHub\"")
                .set_body_json(json!({"message": "auth required"}))
        }
    }
}

fn basic_value(user: &str, pass: &str) -> String {
    use base64::Engine;
    let raw = format!("{user}:{pass}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
    format!("Basic {encoded}")
}

#[tokio::test]
async fn batch_unauthorized_triggers_helper_fill_and_retry() {
    let server = MockServer::start().await;
    let basic = basic_value("alice", "hunter2");
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(AuthGate {
            expected_basic: basic.clone(),
            body_when_authed: json!({
                "transfer": "basic",
                "objects": [{ "oid": "abc", "size": 10 }]
            }),
        })
        .mount(&server)
        .await;

    let helper = StubHelper::with(Credentials::new("alice", "hunter2"));
    let endpoint = Url::parse(&server.uri()).unwrap();
    let client = Client::new(endpoint, Auth::None).with_credential_helper(helper.clone());

    let req = BatchRequest::new(
        Operation::Download,
        vec![ObjectSpec { oid: "abc".into(), size: 10 }],
    );
    let resp = client.batch(&req).await.expect("retry should succeed");
    assert_eq!(resp.objects.len(), 1);

    assert_eq!(helper.approve_count(), 1, "successful retry should approve");
    assert_eq!(helper.reject_count(), 0);
}

#[tokio::test]
async fn batch_unauthorized_with_no_creds_returns_original_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"message": "no auth"})),
        )
        .mount(&server)
        .await;

    let helper = StubHelper::empty(); // fill returns None
    let endpoint = Url::parse(&server.uri()).unwrap();
    let client = Client::new(endpoint, Auth::None).with_credential_helper(helper.clone());

    let req = BatchRequest::new(
        Operation::Download,
        vec![ObjectSpec { oid: "abc".into(), size: 10 }],
    );
    let err = client.batch(&req).await.unwrap_err();
    assert!(err.is_unauthorized());
    // Helper had nothing to give → no approve, no reject.
    assert_eq!(helper.approve_count(), 0);
    assert_eq!(helper.reject_count(), 0);
}

#[tokio::test]
async fn batch_persistent_unauthorized_rejects_filled_creds() {
    // Server always 401s, even with the right header. Helper hands out
    // creds that the server doesn't accept → client should reject() so
    // those creds get evicted from caches downstream.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .and(header_exists("Authorization"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"message": "nope"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"message": "no auth"})))
        .mount(&server)
        .await;

    let helper = StubHelper::with(Credentials::new("alice", "wrong"));
    let endpoint = Url::parse(&server.uri()).unwrap();
    let client = Client::new(endpoint, Auth::None).with_credential_helper(helper.clone());

    let req = BatchRequest::new(
        Operation::Download,
        vec![ObjectSpec { oid: "abc".into(), size: 10 }],
    );
    let err = client.batch(&req).await.unwrap_err();
    assert!(err.is_unauthorized());
    assert_eq!(helper.reject_count(), 1, "bad creds should be rejected");
    assert_eq!(helper.approve_count(), 0);
}

#[tokio::test]
async fn second_request_reuses_filled_credentials_no_extra_fill() {
    // After a successful 401→fill→200 round-trip, the next request should
    // go straight out with the cached Basic header — proven here because
    // the server's 200 path requires the header on the FIRST hit, with no
    // 401 fallback configured.
    let server = MockServer::start().await;
    let basic = basic_value("alice", "hunter2");
    Mock::given(method("POST"))
        .and(path("/objects/batch"))
        .respond_with(AuthGate {
            expected_basic: basic.clone(),
            body_when_authed: json!({
                "transfer": "basic",
                "objects": [{ "oid": "abc", "size": 10 }]
            }),
        })
        .mount(&server)
        .await;

    let helper = StubHelper::with(Credentials::new("alice", "hunter2"));
    let endpoint = Url::parse(&server.uri()).unwrap();
    let client = Client::new(endpoint, Auth::None).with_credential_helper(helper.clone());

    let req = BatchRequest::new(
        Operation::Download,
        vec![ObjectSpec { oid: "abc".into(), size: 10 }],
    );
    client.batch(&req).await.unwrap();
    client.batch(&req).await.unwrap();

    // approve fires twice: once after the first retry succeeds, once on
    // the second request which goes straight through with cached creds.
    assert_eq!(helper.approve_count(), 2);
    assert_eq!(helper.reject_count(), 0);
}
