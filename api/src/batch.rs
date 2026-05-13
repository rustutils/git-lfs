//! Batch API: request the ability to transfer LFS objects.
//!
//! See `docs/api/batch.md` for the wire-protocol contract.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Deserializer, Serialize};

use crate::client::Client;
use crate::error::ApiError;
use crate::models::Ref;

/// Deserialize a size field as `u64`, but reject negative values with the
/// exact wording upstream's `git-lfs` emits so test fixtures keyed on it
/// (notably `t-push.sh::push (with invalid object size)`) keep matching.
fn deserialize_object_size<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    use serde::de::Error;
    let v = i64::deserialize(d)?;
    if v < 0 {
        return Err(D::Error::custom(format!("invalid size (got: {v})")));
    }
    Ok(v as u64)
}

/// `ObjectResult.size` is missing entirely from some servers (see the
/// `serde(default)` comment), so the deserializer must also tolerate
/// absence. `#[serde(default, deserialize_with = ...)]` requires the
/// `deserialize_with` to handle the present case only — defaulting
/// happens before this runs — so this is structurally the same as
/// `deserialize_object_size` but kept separate to keep the call sites
/// self-documenting.
fn deserialize_optional_object_size<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    deserialize_object_size(d)
}

/// Operation requested from the batch endpoint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Download,
    Upload,
}

/// One object the client wants to transfer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectSpec {
    pub oid: String,
    pub size: u64,
}

/// A POST body for `/objects/batch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchRequest {
    pub operation: Operation,
    /// Transfer adapter identifiers the client supports. If empty, the spec
    /// says the server MUST assume `basic`. We send the field unconditionally
    /// so the server's preferred adapter is well-defined.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transfers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<Ref>,
    pub objects: Vec<ObjectSpec>,
    /// Optional hash algorithm. Defaults to `sha256` per the spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_algo: Option<String>,
}

impl BatchRequest {
    pub fn new(operation: Operation, objects: Vec<ObjectSpec>) -> Self {
        Self {
            operation,
            transfers: Vec::new(),
            r#ref: None,
            objects,
            hash_algo: None,
        }
    }

    pub fn with_transfers(mut self, transfers: impl IntoIterator<Item = String>) -> Self {
        self.transfers = transfers.into_iter().collect();
        self
    }

    pub fn with_ref(mut self, r: Ref) -> Self {
        self.r#ref = Some(r);
        self
    }
}

/// Response body from `/objects/batch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchResponse {
    /// Transfer adapter the server picked. `None` means the server omitted
    /// it; per the spec the client should assume `basic`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer: Option<String>,
    pub objects: Vec<ObjectResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash_algo: Option<String>,
}

/// Per-object result inside a batch response. Either `actions` or `error`
/// is populated; both being absent means "server already has this object"
/// (an upload no-op).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectResult {
    pub oid: String,
    /// Size in bytes. Per the spec this is required, but the upstream
    /// `lfstest-gitserver` (and at least one production server in the
    /// wild) omit it on the action path — they assume the client
    /// already knows. Default to 0 so we don't refuse the response;
    /// callers that need the real size should look it up from the
    /// matching request entry.
    #[serde(default, deserialize_with = "deserialize_optional_object_size")]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authenticated: Option<bool>,
    #[serde(default, alias = "_links", skip_serializing_if = "Option::is_none")]
    pub actions: Option<Actions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ObjectError>,
}

/// Per-object error inside a batch response.
///
/// Codes mirror HTTP status codes per `docs/api/batch.md`:
/// 404 = not found, 409 = hash-algo mismatch, 410 = removed, 422 = invalid.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectError {
    pub code: u32,
    pub message: String,
}

/// The set of next-step actions the server returned for one object.
///
/// Field set depends on `operation`: `download` populates `download`;
/// `upload` populates `upload` and optionally `verify`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Actions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload: Option<Action>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<Action>,
}

/// One concrete HTTP request the transfer adapter should make.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Action {
    pub href: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub header: HashMap<String, String>,
    /// Seconds until the action URL stops being valid. Preferred over
    /// `expires_at` when both are given. Per the spec, range is roughly
    /// ±2^31 seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
    /// Absolute uppercase RFC 3339 timestamp at which the action URL stops
    /// being valid. Carried as a string — see [`Lock`](crate::Lock).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

impl Action {
    /// Has this action expired (or will it within `buffer`)?
    ///
    /// Mirrors upstream's `tq.Action.IsExpiredWithin` /
    /// `tools.IsExpiredAtOrIn`: `expires_in` is taken relative to
    /// `now` (preferred when non-zero), otherwise `expires_at` is
    /// parsed as RFC 3339. An action without either field never
    /// expires. The check is `expiration < now + buffer` — i.e. the
    /// action must have at least `buffer` of validity left.
    pub fn is_expired_within(&self, now: SystemTime, buffer: Duration) -> bool {
        let expiration = match (self.expires_in, self.expires_at.as_deref()) {
            (Some(secs), _) if secs != 0 => {
                // Negative `expires_in` means "already expired" — saturate
                // at UNIX_EPOCH so the comparison below trivially fires.
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

/// Minimal RFC 3339 parser — accepts `YYYY-MM-DDThh:mm:ss[.fff][Z|±hh:mm]`.
/// Pre-epoch / malformed → `None` (treated as "no expiration set" by
/// callers, matching upstream's `IsZero` short-circuit).
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

/// Days since 1970-01-01 for the proleptic Gregorian date `(y, m, d)`.
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
    /// POST `/objects/batch` to negotiate transfer URLs.
    pub async fn batch(&self, req: &BatchRequest) -> Result<BatchResponse, ApiError> {
        // Match the SSH `git-lfs-authenticate` operation to the batch
        // operation: an upload batch needs upload-scoped auth, a
        // download batch needs download-scoped auth.
        let op = match req.operation {
            Operation::Upload => crate::ssh::SshOperation::Upload,
            Operation::Download => crate::ssh::SshOperation::Download,
        };
        self.post_json("objects/batch", req, op).await
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
        // Default is `expires_in: None`, `expires_at: None` (after
        // adding manual construction); a SystemTime in the distant
        // future doesn't matter either way.
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
