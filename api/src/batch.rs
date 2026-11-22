//! Request transfer actions for LFS objects.
//!
//! The [batch spec] defines the format.
//!
//! [batch spec]: https://gitlab.com/rustutils/git-lfs/-/blob/master/docs/api/batch.md

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Deserializer, Serialize};

use crate::client::Client;
use crate::error::ApiError;
use crate::models::Ref;

/// Deserialize an object size, rejecting negative values.
///
/// The error wording matches upstream, which `t-push.sh` checks.
fn deserialize_object_size<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    use serde::de::Error;
    let v = i64::deserialize(d)?;
    if v < 0 {
        return Err(D::Error::custom(format!("invalid size (got: {v})")));
    }
    Ok(v as u64)
}

/// Operation requested from the batch endpoint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    /// Fetch objects from the server.
    Download,
    /// Send objects to the server.
    Upload,
}

/// One object the client wants to transfer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectSpec {
    /// Object ID, the SHA-256 of the content as lowercase hex.
    pub oid: String,
    /// Size of the object in bytes.
    pub size: u64,
}

/// Request body for `POST /objects/batch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchRequest {
    /// Whether to upload or download the objects.
    pub operation: Operation,
    /// Transfer adapters the client supports.
    ///
    /// Omitted from the request when empty, in which case the server assumes
    /// `basic`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transfers: Vec<String>,
    /// Ref the objects belong to, for servers that authorize by ref.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<Ref>,
    /// The objects to transfer.
    pub objects: Vec<ObjectSpec>,
    /// Hash algorithm naming the objects. The server assumes `sha256` when
    /// it is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_algo: Option<String>,
}

impl BatchRequest {
    /// Build a request for `operation` over `objects`.
    ///
    /// `transfers`, `ref`, and `hash_algo` start empty.
    pub fn new(operation: Operation, objects: Vec<ObjectSpec>) -> Self {
        Self {
            operation,
            transfers: Vec::new(),
            r#ref: None,
            objects,
            hash_algo: None,
        }
    }

    /// Set the transfer adapters the client supports.
    pub fn with_transfers(mut self, transfers: impl IntoIterator<Item = String>) -> Self {
        self.transfers = transfers.into_iter().collect();
        self
    }

    /// Set the ref the objects belong to.
    pub fn with_ref(mut self, r: Ref) -> Self {
        self.r#ref = Some(r);
        self
    }
}

/// Response body from `POST /objects/batch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchResponse {
    /// Transfer adapter the server picked. `None` means `basic`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer: Option<String>,
    /// One result per object in the request.
    pub objects: Vec<ObjectResult>,
    /// Hash algorithm naming the objects. `None` means `sha256`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_algo: Option<String>,
}

/// Result for one object in a batch response.
///
/// Holds either `actions` or `error`. Neither means the server already has
/// the object, which happens on uploads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectResult {
    /// OID of the matching [`ObjectSpec`].
    pub oid: String,
    /// Size in bytes.
    ///
    /// The spec requires it, but some servers (including upstream's test
    /// server) omit it. It is then 0, and callers that need the size should
    /// take it from the request.
    #[serde(default, deserialize_with = "deserialize_object_size")]
    pub size: u64,
    /// Whether the action requests are already authenticated.
    ///
    /// When absent or false, the client should find credentials for the
    /// action URLs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authenticated: Option<bool>,
    /// What to do next for this object. Also accepted as `_links`, the name
    /// used by older servers.
    #[serde(default, alias = "_links", skip_serializing_if = "Option::is_none")]
    pub actions: Option<Actions>,
    /// Why the server can't transfer this object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ObjectError>,
}

/// Error for one object in a batch response.
///
/// The spec uses these codes: 404 (not found), 409 (hash algorithm
/// mismatch), 410 (removed by the owner), and 422 (validation error).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectError {
    /// HTTP status code classifying the error.
    pub code: u32,
    /// Human-readable description.
    pub message: String,
}

/// Next actions the server returned for one object.
///
/// Download batches set `download`. Upload batches set `upload` and
/// optionally `verify`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Actions {
    /// Where to fetch the object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<Action>,
    /// Where to send the object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload: Option<Action>,
    /// Where to confirm an upload after it completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<Action>,
}

/// One request the transfer adapter should make.
///
/// Over HTTP, `href` is the URL and `header` holds the headers to send. Over
/// the pure SSH transfer protocol, `href` is empty and `id` and `token`
/// identify the granted action in follow-up commands.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Action {
    /// URL to request. Empty over pure SSH.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub href: String,
    /// Headers to send with the request, typically `Authorization`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub header: HashMap<String, String>,
    /// Seconds from now until the action expires.
    ///
    /// Takes precedence over `expires_at` when both are set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
    /// RFC 3339 timestamp at which the action expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// Pure SSH only: identifier the server gave this action.
    ///
    /// Sent back with `get-object`, `put-object`, and `verify-object` so the
    /// server can match the command to the action it granted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Pure SSH only: token sent back along with [`id`](Self::id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl Action {
    /// Whether the action has expired, or will within `buffer` of `now`.
    ///
    /// A nonzero `expires_in` counts from `now` and takes precedence.
    /// Otherwise `expires_at` is used. An action with neither, or with an
    /// `expires_at` that doesn't parse, never expires.
    pub fn is_expired_within(&self, now: SystemTime, buffer: Duration) -> bool {
        let expiration = match (self.expires_in, self.expires_at.as_deref()) {
            (Some(secs), _) if secs != 0 => {
                // Negative means already expired.
                if secs < 0 {
                    SystemTime::UNIX_EPOCH
                } else {
                    now.checked_add(Duration::from_secs(secs as u64))
                        .unwrap_or(SystemTime::UNIX_EPOCH)
                }
            }
            (_, Some(s)) => match parse_rfc3339(s) {
                Some(t) => t,
                None => return false,
            },
            _ => return false,
        };
        expiration < now + buffer
    }
}

/// Parse an RFC 3339 timestamp of the form
/// `YYYY-MM-DDThh:mm:ss[.fff](Z|±hh:mm)`.
///
/// Returns `None` for malformed input and for times before the Unix epoch.
fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let bytes = s.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year: i32 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    let min: u32 = s.get(14..16)?.parse().ok()?;
    let sec: u32 = s.get(17..19)?.parse().ok()?;

    let mut idx = 19;
    if bytes.get(idx) == Some(&b'.') {
        idx += 1;
        while bytes.get(idx).is_some_and(|b| b.is_ascii_digit()) {
            idx += 1;
        }
    }
    let tz_secs: i64 = match bytes.get(idx) {
        Some(b'Z') | Some(b'z') => 0,
        Some(b'+') | Some(b'-') => {
            let sign = if bytes[idx] == b'+' { 1 } else { -1 };
            let h: i64 = s.get(idx + 1..idx + 3)?.parse().ok()?;
            let m: i64 = s.get(idx + 4..idx + 6)?.parse().ok()?;
            sign * (h * 3600 + m * 60)
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    let secs_of_day = (hour as i64) * 3600 + (min as i64) * 60 + (sec as i64);
    let unix = days * 86400 + secs_of_day - tz_secs;
    if unix < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(unix as u64))
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
///
/// Howard Hinnant's days-from-civil algorithm.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = (if month <= 2 { year - 1 } else { year }) as i64;
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

impl Client {
    /// Send a batch request to `POST /objects/batch`.
    ///
    /// A failure for a single object comes back as
    /// [`ObjectResult::error`], not as an `Err`.
    pub async fn batch(&self, req: &BatchRequest) -> Result<BatchResponse, ApiError> {
        // Over SSH, an upload batch needs upload-scoped auth and a download
        // batch needs download-scoped auth.
        let op = match req.operation {
            Operation::Upload => crate::ssh::SshOperation::Upload,
            Operation::Download => crate::ssh::SshOperation::Download,
        };
        self.post_json("objects/batch", req, op, self.batch_retries)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_serializes_lowercase() {
        let s = serde_json::to_string(&Operation::Download).unwrap();
        assert_eq!(s, "\"download\"");
    }

    #[test]
    fn request_skips_empty_optional_fields() {
        let req = BatchRequest::new(
            Operation::Download,
            vec![ObjectSpec {
                oid: "abc".into(),
                size: 10,
            }],
        );
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("transfers").is_none());
        assert!(v.get("ref").is_none());
        assert!(v.get("hash_algo").is_none());
    }

    #[test]
    fn parses_canonical_download_response() {
        let body = r#"{
            "transfer": "basic",
            "objects": [{
                "oid": "1111111",
                "size": 123,
                "authenticated": true,
                "actions": {
                    "download": {
                        "href": "https://some-download.com",
                        "header": { "Key": "value" },
                        "expires_at": "2016-11-10T15:29:07Z"
                    }
                }
            }],
            "hash_algo": "sha256"
        }"#;
        let resp: BatchResponse = serde_json::from_str(body).unwrap();
        assert_eq!(resp.transfer.as_deref(), Some("basic"));
        let obj = &resp.objects[0];
        assert_eq!(obj.authenticated, Some(true));
        let action = obj.actions.as_ref().unwrap().download.as_ref().unwrap();
        assert_eq!(action.href, "https://some-download.com");
        assert_eq!(action.header.get("Key").unwrap(), "value");
        assert!(action.expires_in.is_none());
    }

    #[test]
    fn action_with_no_expiry_never_expires() {
        let action = Action::default();
        let action = Action {
            href: "x".into(),
            ..action
        };
        assert!(!action.is_expired_within(SystemTime::now(), Duration::from_secs(5)));
    }

    #[test]
    fn action_with_negative_expires_in_is_expired() {
        let action = Action {
            href: "x".into(),
            expires_in: Some(-5),
            ..Default::default()
        };
        assert!(action.is_expired_within(SystemTime::now(), Duration::from_secs(5)));
    }

    #[test]
    fn action_with_past_expires_at_is_expired() {
        let action = Action {
            href: "x".into(),
            expires_at: Some("2016-11-10T15:29:07Z".into()),
            ..Default::default()
        };
        assert!(action.is_expired_within(SystemTime::now(), Duration::from_secs(5)));
    }

    #[test]
    fn action_with_far_future_expires_at_is_not_expired() {
        let action = Action {
            href: "x".into(),
            expires_at: Some("2099-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        assert!(!action.is_expired_within(SystemTime::now(), Duration::from_secs(5)));
    }

    #[test]
    fn action_expires_in_takes_precedence_over_expires_at() {
        // Past expires_at, future expires_in → not expired (expires_in wins).
        let action = Action {
            href: "x".into(),
            expires_in: Some(3600),
            expires_at: Some("2016-11-10T15:29:07Z".into()),
            ..Default::default()
        };
        assert!(!action.is_expired_within(SystemTime::now(), Duration::from_secs(5)));
    }

    #[test]
    fn parses_per_object_error() {
        let body = r#"{
            "transfer": "basic",
            "objects": [{
                "oid": "1111111", "size": 123,
                "error": { "code": 404, "message": "Object does not exist" }
            }]
        }"#;
        let resp: BatchResponse = serde_json::from_str(body).unwrap();
        let err = resp.objects[0].error.as_ref().unwrap();
        assert_eq!(err.code, 404);
        assert_eq!(err.message, "Object does not exist");
    }

    #[test]
    fn parses_upload_already_present_no_actions() {
        let body = r#"{
            "objects": [{ "oid": "1111111", "size": 123 }]
        }"#;
        let resp: BatchResponse = serde_json::from_str(body).unwrap();
        let obj = &resp.objects[0];
        assert!(obj.actions.is_none());
        assert!(obj.error.is_none());
    }
}
