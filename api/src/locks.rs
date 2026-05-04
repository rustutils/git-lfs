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
        Self {
            path: path.into(),
            r#ref: None,
        }
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

/// Flexible POST `/locks` response decoder. The reference test server
/// returns `{"message": "lock already created"}` at HTTP 200 for the
/// "path is already locked" case (no `lock` field, no 409 status), so a
/// strict envelope deserialize would blow up with a missing-field
/// error. We accept `lock` and `message` independently and let
/// [`Client::create_lock`] interpret which arrived.
#[derive(Debug, Deserialize)]
struct CreateLockResponse {
    #[serde(default)]
    lock: Option<Lock>,
    #[serde(default)]
    message: Option<String>,
}

/// Errors specific to [`Client::create_lock`].
///
/// Wraps [`ApiError`] but adds a typed `Conflict` for the in-band
/// "already locked" case. `existing` is `Some` for servers that return
/// HTTP 409 with the conflicting lock attached; `None` for servers that
/// only ship a message.
#[derive(Debug, thiserror::Error)]
pub enum CreateLockError {
    #[error("lock conflict: {message}")]
    Conflict {
        existing: Option<Lock>,
        message: String,
    },

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
    /// Go LFS servers serialize an empty result as `"locks": null`
    /// rather than `"locks": []`; treat null as the empty list.
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
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
    /// Locks owned by the authenticated user. Servers may serialize an
    /// empty list as `null`; `deserialize_null_as_default` normalizes
    /// that to `Vec::new()`.
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub ours: Vec<Lock>,
    /// Locks owned by other users. Same null-handling as `ours`.
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
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

/// Treat a JSON `null` as `T::default()`. Go's `encoding/json` serializes
/// a `nil` slice as `null` rather than `[]`, and the LFS reference server
/// (and lfstest-gitserver) inherits that — so a request that legitimately
/// returns "no locks" looks like `{"ours": null}`. Without this, our
/// `Vec<Lock>` deserialize bombs on the null.
fn deserialize_null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + serde::Deserialize<'de>,
{
    let opt = Option::<T>::deserialize(d)?;
    Ok(opt.unwrap_or_default())
}

// ---- client ---------------------------------------------------------------

impl Client {
    /// POST `/locks` to create a new lock.
    ///
    /// Body decoding is flexible to accommodate both spec'd 409 → existing
    /// lock responses and the reference test server's "200 with `message`
    /// but no `lock`" in-band-conflict pattern.
    pub async fn create_lock(&self, req: &CreateLockRequest) -> Result<Lock, CreateLockError> {
        let url = self.url("locks").map_err(CreateLockError::Api)?;
        // Serialize once so the closure (which may run twice — once
        // with current auth, once after a 401 → fill) doesn't re-encode
        // the body each time.
        let body_bytes = serde_json::to_vec(req)
            .map_err(|e| CreateLockError::Api(ApiError::Decode(e.to_string())))?;
        let resp = self
            .send_with_auth_retry_response(|| {
                self.request(reqwest::Method::POST, url.clone())
                    .header(reqwest::header::CONTENT_TYPE, crate::client::LFS_MEDIA_TYPE)
                    .body(body_bytes.clone())
            })
            .await
            .map_err(CreateLockError::Api)?;

        let status = resp.status();
        let request_url = resp.url().to_string();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| CreateLockError::Api(ApiError::Transport(e)))?;

        // 409 = standard conflict, with the existing lock spelled out in
        // the body. Decode flexibly: server may or may not include a
        // `message` alongside the lock.
        if status.as_u16() == 409 {
            let parsed: CreateLockResponse = serde_json::from_slice(&bytes)
                .map_err(|e| CreateLockError::Api(ApiError::Decode(e.to_string())))?;
            return Err(CreateLockError::Conflict {
                existing: parsed.lock,
                message: parsed.message.unwrap_or_else(|| "lock conflict".into()),
            });
        }

        // Other non-success statuses fall through as plain ApiError::Status.
        if !status.is_success() {
            let body: Option<crate::error::ServerError> = serde_json::from_slice(&bytes).ok();
            return Err(CreateLockError::Api(ApiError::Status {
                status: status.as_u16(),
                url: Some(request_url),
                lfs_authenticate: None,
                body,
            }));
        }

        // 2xx — could be {lock: ...} success or {message: ...}
        // in-band conflict.
        let parsed: CreateLockResponse = serde_json::from_slice(&bytes)
            .map_err(|e| CreateLockError::Api(ApiError::Decode(e.to_string())))?;
        if let Some(lock) = parsed.lock {
            return Ok(lock);
        }
        if let Some(message) = parsed.message {
            return Err(CreateLockError::Conflict {
                existing: None,
                message,
            });
        }
        Err(CreateLockError::Api(ApiError::Decode(
            "create-lock response had neither lock nor message".into(),
        )))
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
    pub async fn delete_lock(&self, id: &str, req: &DeleteLockRequest) -> Result<Lock, ApiError> {
        // Percent-encode the id to keep nested path segments safe.
        let encoded = url_path_segment(id);
        let path = format!("locks/{encoded}/unlock");
        let url = self.url(&path)?;
        let body_bytes = serde_json::to_vec(req).map_err(|e| ApiError::Decode(e.to_string()))?;
        let resp = self
            .send_with_auth_retry_response(|| {
                self.request(reqwest::Method::POST, url.clone())
                    .header(reqwest::header::CONTENT_TYPE, crate::client::LFS_MEDIA_TYPE)
                    .body(body_bytes.clone())
            })
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
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
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
        let f = ListLocksFilter {
            path: Some("a.bin".into()),
            ..Default::default()
        };
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
        let r = DeleteLockRequest {
            force: true,
            ..Default::default()
        };
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
    fn parses_create_lock_response_with_lock() {
        let body = r#"{
            "lock": { "id": "x", "path": "foo", "locked_at": "2016-01-01T00:00:00Z" }
        }"#;
        let parsed: CreateLockResponse = serde_json::from_str(body).unwrap();
        assert!(parsed.lock.is_some());
        assert_eq!(parsed.lock.unwrap().id, "x");
        assert!(parsed.message.is_none());
    }

    #[test]
    fn parses_create_lock_response_message_only() {
        // Reference test server's "already locked" response shape.
        let body = r#"{"message":"lock already created"}"#;
        let parsed: CreateLockResponse = serde_json::from_str(body).unwrap();
        assert!(parsed.lock.is_none());
        assert_eq!(parsed.message.as_deref(), Some("lock already created"));
    }

    #[test]
    fn url_path_segment_encodes_special() {
        assert_eq!(url_path_segment("abc-123_xyz.~"), "abc-123_xyz.~");
        assert_eq!(url_path_segment("a/b"), "a%2Fb");
        assert_eq!(url_path_segment("hello world"), "hello%20world");
    }
}
