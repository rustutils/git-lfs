//! Locking API: create, list, verify, and delete file locks.
//!
//! See `docs/api/locking.md` for the wire-protocol contract.

use serde::{Deserialize, Serialize};

use crate::client::{Client, decode};
use crate::error::ApiError;
use crate::models::{Lock, Ref};

// ---- create ---------------------------------------------------------------

/// POST `/locks` body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateLockRequest {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<Ref>,
}

impl CreateLockRequest {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into(), r#ref: None }
    }

    pub fn with_ref(mut self, r: Ref) -> Self {
        self.r#ref = Some(r);
        self
    }
}

#[derive(Debug, Deserialize)]
struct LockEnvelope {
    lock: Lock,
}

#[derive(Debug, Deserialize)]
struct LockExistsBody {
    lock: Lock,
    message: String,
}

/// Errors specific to [`Client::create_lock`].
///
/// Wraps [`ApiError`] but adds a typed `Conflict` for the 409 case where
/// the server returns the existing lock alongside the error message.
#[derive(Debug, thiserror::Error)]
pub enum CreateLockError {
    /// The path is already locked. Returned for HTTP 409.
    #[error("lock conflict: {message}")]
    Conflict { existing: Lock, message: String },

    #[error(transparent)]
    Api(#[from] ApiError),
}

// ---- list -----------------------------------------------------------------

/// Filter for `GET /locks`. All fields are optional; absent ones are not
/// sent on the wire.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ListLocksFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refspec: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockList {
    pub locks: Vec<Lock>,
    /// Opaque cursor; pass back as `cursor` in the next request to continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// ---- verify ---------------------------------------------------------------

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyLocksRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<Ref>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyLocksResponse {
    /// Locks owned by the authenticated user.
    pub ours: Vec<Lock>,
    /// Locks owned by other users.
    pub theirs: Vec<Lock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// ---- delete ---------------------------------------------------------------

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeleteLockRequest {
    /// True to delete a lock owned by another user. Server enforces auth.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<Ref>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

// ---- client ---------------------------------------------------------------

impl Client {
    /// POST `/locks` to create a new lock.
    pub async fn create_lock(&self, req: &CreateLockRequest) -> Result<Lock, CreateLockError> {
        let url = self.url("locks").map_err(CreateLockError::Api)?;
        let resp = self
            .request(reqwest::Method::POST, url)
            .header(reqwest::header::CONTENT_TYPE, crate::client::LFS_MEDIA_TYPE)
            .json(req)
            .send()
            .await
            .map_err(|e| CreateLockError::Api(ApiError::Transport(e)))?;

        if resp.status() == reqwest::StatusCode::CONFLICT {
            let bytes = resp.bytes().await.unwrap_or_default();
            return match serde_json::from_slice::<LockExistsBody>(&bytes) {
                Ok(body) => Err(CreateLockError::Conflict {
                    existing: body.lock,
                    message: body.message,
                }),
                Err(_) => Err(CreateLockError::Api(ApiError::Status {
                    status: 409,
                    lfs_authenticate: None,
                    body: serde_json::from_slice(&bytes).ok(),
                })),
            };
        }

        let env: LockEnvelope = decode(resp).await?;
        Ok(env.lock)
    }

    /// GET `/locks` with optional filters.
    pub async fn list_locks(&self, filter: &ListLocksFilter) -> Result<LockList, ApiError> {
        self.get_json("locks", filter).await
    }

    /// POST `/locks/verify` to list locks partitioned into ours/theirs.
    ///
    /// Per the spec, servers that don't implement locking can return 404
    /// here; that surfaces as `ApiError::Status { status: 404, .. }`. The
    /// caller (typically push) should treat that as "no locks to verify"
    /// rather than a hard failure — see `is_not_found()`.
    pub async fn verify_locks(
        &self,
        req: &VerifyLocksRequest,
    ) -> Result<VerifyLocksResponse, ApiError> {
        self.post_json("locks/verify", req).await
    }

    /// POST `/locks/{id}/unlock` to delete a lock.
    pub async fn delete_lock(
        &self,
        id: &str,
        req: &DeleteLockRequest,
    ) -> Result<Lock, ApiError> {
        // Percent-encode the id to keep nested path segments safe.
        let encoded = url_path_segment(id);
        let path = format!("locks/{encoded}/unlock");
        let url = self.url(&path)?;
        let resp = self
            .request(reqwest::Method::POST, url)
            .header(reqwest::header::CONTENT_TYPE, crate::client::LFS_MEDIA_TYPE)
            .json(req)
            .send()
            .await?;
        let env: LockEnvelope = decode(resp).await?;
        Ok(env.lock)
    }
}

/// Minimal percent-encoder for one URL path segment. Encodes anything that
/// isn't an unreserved character per RFC 3986.
fn url_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let unreserved = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~');
        if unreserved {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_filter_omits_none_fields() {
        // serde_json round-trip keeps only the fields we actually want on
        // the wire — same omission rule reqwest applies when building the
        // query string.
        let f = ListLocksFilter { path: Some("a.bin".into()), ..Default::default() };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["path"], "a.bin");
        assert!(v.get("id").is_none());
        assert!(v.get("cursor").is_none());
        assert!(v.get("limit").is_none());
        assert!(v.get("refspec").is_none());
    }

    #[test]
    fn delete_request_omits_force_when_false() {
        let r = DeleteLockRequest::default();
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("force").is_none());
    }

    #[test]
    fn delete_request_includes_force_when_true() {
        let r = DeleteLockRequest { force: true, ..Default::default() };
        assert_eq!(serde_json::to_value(&r).unwrap()["force"], true);
    }

    #[test]
    fn parses_create_lock_envelope() {
        let body = r#"{
            "lock": {
                "id": "some-uuid", "path": "foo/bar.zip",
                "locked_at": "2016-05-17T15:49:06+00:00",
                "owner": { "name": "Jane Doe" }
            }
        }"#;
        let env: LockEnvelope = serde_json::from_str(body).unwrap();
        assert_eq!(env.lock.path, "foo/bar.zip");
        assert_eq!(env.lock.owner.unwrap().name, "Jane Doe");
    }

    #[test]
    fn parses_lock_exists_body() {
        let body = r#"{
            "lock": { "id": "x", "path": "foo", "locked_at": "2016-01-01T00:00:00Z" },
            "message": "already created lock",
            "documentation_url": "https://example.com/docs",
            "request_id": "req-1"
        }"#;
        let parsed: LockExistsBody = serde_json::from_str(body).unwrap();
        assert_eq!(parsed.message, "already created lock");
        assert_eq!(parsed.lock.id, "x");
    }

    #[test]
    fn url_path_segment_encodes_special() {
        assert_eq!(url_path_segment("abc-123_xyz.~"), "abc-123_xyz.~");
        assert_eq!(url_path_segment("a/b"), "a%2Fb");
        assert_eq!(url_path_segment("hello world"), "hello%20world");
    }
}
