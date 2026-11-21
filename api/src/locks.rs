//! Create, list, verify, and delete file locks.
//!
//! The [locking spec] defines the format.
//!
//! [locking spec]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/api/locking.md

use serde::{Deserialize, Serialize};

use crate::client::{Client, decode};
use crate::error::ApiError;
use crate::models::{Lock, Ref};

// ---- create ---------------------------------------------------------------

/// Request body for `POST /locks`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateLockRequest {
    /// Path to lock, relative to the repository root.
    pub path: String,
    /// Ref the lock belongs to, for servers that authorize by ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<Ref>,
}

impl CreateLockRequest {
    /// Build a request to lock `path`.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            r#ref: None,
        }
    }

    /// Set the ref the lock belongs to.
    pub fn with_ref(mut self, r: Ref) -> Self {
        self.r#ref = Some(r);
        self
    }
}

#[derive(Debug, Deserialize)]
struct LockEnvelope {
    lock: Lock,
}

/// Response body from `POST /locks`, with both fields optional.
///
/// Upstream's test server answers an already locked path with HTTP 200 and
/// only `{"message": "lock already created"}`, so `lock` can't be required.
#[derive(Debug, Deserialize)]
struct CreateLockResponse {
    #[serde(default)]
    lock: Option<Lock>,
    #[serde(default)]
    message: Option<String>,
}

/// Errors returned by [`Client::create_lock`].
#[derive(Debug, thiserror::Error)]
pub enum CreateLockError {
    /// The path is already locked.
    ///
    /// Servers report this with a 409, or with a 2xx response that has a
    /// `message` but no `lock`.
    #[error("lock conflict: {message}")]
    Conflict {
        /// The existing lock, if the server sent it.
        existing: Option<Lock>,
        /// The server's message, or `lock conflict` if it sent none.
        message: String,
    },

    /// Any other failure.
    #[error(transparent)]
    Api(#[from] ApiError),
}

// ---- list -----------------------------------------------------------------

/// Query parameters for `GET /locks`.
///
/// Fields left as `None` are not sent.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ListLocksFilter {
    /// Only the lock on this path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Only the lock with this ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Where to continue, from a previous [`LockList::next_cursor`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Maximum number of locks to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Only locks on this fully qualified ref.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refspec: Option<String>,
}

/// Response body from `GET /locks`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockList {
    /// Locks on this page. A `null` from the server reads as empty.
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub locks: Vec<Lock>,
    /// Cursor for the next page, if there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// ---- verify ---------------------------------------------------------------

/// Request body for `POST /locks/verify`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyLocksRequest {
    /// Ref the locks belong to, for servers that authorize by ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<Ref>,
    /// Where to continue, from a previous
    /// [`VerifyLocksResponse::next_cursor`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Maximum number of locks to return.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Response body from `POST /locks/verify`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyLocksResponse {
    /// Locks held by the authenticated user. A `null` reads as empty.
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub ours: Vec<Lock>,
    /// Locks held by other users. A `null` reads as empty.
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    pub theirs: Vec<Lock>,
    /// Cursor for the next page, if there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// ---- delete ---------------------------------------------------------------

/// Request body for `POST /locks/{id}/unlock`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeleteLockRequest {
    /// Delete the lock even if another user holds it.
    ///
    /// The server decides whether the caller may do that.
    #[serde(default, skip_serializing_if = "is_false")]
    pub force: bool,
    /// Ref the lock belongs to, for servers that authorize by ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<Ref>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Deserialize a JSON `null` as `T::default()`.
///
/// Go encodes an empty slice as `null`, so Go servers, including upstream's
/// test server, can send `{"ours": null}` where the spec wants `[]`.
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
    /// Lock a path with `POST /locks`.
    ///
    /// Returns the new lock, or [`CreateLockError::Conflict`] if the path is
    /// already locked.
    pub async fn create_lock(&self, req: &CreateLockRequest) -> Result<Lock, CreateLockError> {
        // Over SSH, POST requests use upload-scoped auth, as upstream does.
        let (base, ssh) = self
            .resolve_ssh(crate::ssh::SshOperation::Upload)
            .map_err(CreateLockError::Api)?;
        let url = Client::join(&base, "locks").map_err(CreateLockError::Api)?;
        // Serialize once, since the auth retry loop may build the request
        // several times.
        let body_bytes = serde_json::to_vec(req)
            .map_err(|e| CreateLockError::Api(ApiError::Decode(e.to_string())))?;
        let resp = self
            .send_with_auth_retry_response(|| {
                self.request_with_headers(reqwest::Method::POST, url.clone(), &ssh)
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

        // The spec's conflict response, with the existing lock in the body.
        if status.as_u16() == 409 {
            let parsed: CreateLockResponse = serde_json::from_slice(&bytes)
                .map_err(|e| CreateLockError::Api(ApiError::Decode(e.to_string())))?;
            return Err(CreateLockError::Conflict {
                existing: parsed.lock,
                message: parsed.message.unwrap_or_else(|| "lock conflict".into()),
            });
        }

        if !status.is_success() {
            let body: Option<crate::error::ServerError> = serde_json::from_slice(&bytes).ok();
            return Err(CreateLockError::Api(ApiError::Status {
                status: status.as_u16(),
                url: Some(request_url),
                lfs_authenticate: None,
                body,
                retry_after: None,
            }));
        }

        // A 2xx carries either the new lock or an in-band conflict message.
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

    /// List locks with `GET /locks`.
    pub async fn list_locks(&self, filter: &ListLocksFilter) -> Result<LockList, ApiError> {
        // Over SSH, GET requests use download-scoped auth.
        self.get_json("locks", filter, crate::ssh::SshOperation::Download)
            .await
    }

    /// List locks split into ours and theirs with `POST /locks/verify`.
    ///
    /// A server without locking support answers 404, which the spec says
    /// must not stop a push. Check for it with [`ApiError::is_not_found`].
    pub async fn verify_locks(
        &self,
        req: &VerifyLocksRequest,
    ) -> Result<VerifyLocksResponse, ApiError> {
        // Over SSH, POST requests use upload-scoped auth, as upstream does.
        self.post_json("locks/verify", req, crate::ssh::SshOperation::Upload)
            .await
    }

    /// Delete the lock with ID `id` using `POST /locks/{id}/unlock`.
    ///
    /// Returns the deleted lock.
    pub async fn delete_lock(&self, id: &str, req: &DeleteLockRequest) -> Result<Lock, ApiError> {
        // Percent-encode so an ID with a `/` stays one path segment.
        let encoded = url_path_segment(id);
        let path = format!("locks/{encoded}/unlock");
        // Over SSH, POST requests use upload-scoped auth, as upstream does.
        let (base, ssh) = self.resolve_ssh(crate::ssh::SshOperation::Upload)?;
        let url = Client::join(&base, &path)?;
        let body_bytes = serde_json::to_vec(req).map_err(|e| ApiError::Decode(e.to_string()))?;
        let resp = self
            .send_with_auth_retry_response(|| {
                self.request_with_headers(reqwest::Method::POST, url.clone(), &ssh)
                    .header(reqwest::header::CONTENT_TYPE, crate::client::LFS_MEDIA_TYPE)
                    .body(body_bytes.clone())
            })
            .await?;
        let env: LockEnvelope = decode(resp).await?;
        Ok(env.lock)
    }
}

/// Percent-encode one URL path segment.
///
/// Everything except RFC 3986 unreserved characters is encoded.
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
        // serde_urlencoded, which builds the query string, honors the same
        // skip rules as serde_json.
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
