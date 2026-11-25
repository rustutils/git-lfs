//! State the handlers share across requests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Contents that make the server misbehave for that object. A test stores
/// an object with one of these as its contents, and the server recognizes
/// the object by its OID.
const CONTENT_HANDLERS: &[&str] = &[
    "status-batch-403",
    "status-batch-404",
    "status-batch-410",
    "status-batch-422",
    "status-batch-500",
    "status-storage-403",
    "status-storage-404",
    "status-storage-410",
    "status-storage-422",
    "status-storage-500",
    "status-storage-503",
    "return-expired-action",
    "return-expired-action-forever",
    "return-invalid-size",
    "object-authenticated",
    "storage-upload-retry",
    "storage-upload-retry-later",
    "storage-upload-retry-later-no-header",
    "unknown-oid",
    "storage-download-corrupt",
    "storage-download-retry-later",
    "storage-download-retry-later-no-header",
    "storage-download-retry",
    "storage-download-retry-range",
    "storage-download-retry-range-rejected",
    "storage-download-retry-no-invalid-range",
    "storage-download-encoding-gzip",
    "storage-download-encoding-zstd",
    "storage-download-encoding-zstd-retry-range",
    "storage-download-encoding-zstd-1",
    "storage-download-encoding-zstd-2",
    "storage-download-encoding-zstd-3",
    "send-verify-action",
    "send-deprecated-links",
    "redirect-storage-upload",
    "batch-hash-algo-empty",
    "batch-hash-algo-invalid",
    "auth-bearer",
    "auth-multistage",
];

/// Seconds a rate-limited resource waits before it gets more requests.
const SECONDS_TO_REFILL_TOKENS: u64 = 10;
/// Requests a rate-limited resource gets after each wait.
pub(crate) const REFILL_TOKEN_COUNT: i64 = 5;

pub(crate) struct State {
    /// The directory holding the bare repositories.
    pub repo_dir: PathBuf,
    /// The plain HTTP server's URL, such as `http://127.0.0.1:1234`.
    pub url: String,
    /// Which content handler an OID's contents name.
    handlers: HashMap<String, &'static str>,
    pub objects: Mutex<Storage>,
    pub locks: Mutex<HashMap<String, Vec<Lock>>>,
    rate_limits: Mutex<RateLimits>,
    retries: Mutex<HashMap<String, u32>>,
    /// Repositories that have served their one expired action.
    expired: Mutex<HashMap<String, bool>>,
    verify_counts: Mutex<HashMap<String, u32>>,
    /// Whether `storage-download-retry-range-rejected` has been cut short.
    pub range_rejected_attempted: Mutex<bool>,
}

impl State {
    pub(crate) fn new(repo_dir: PathBuf, url: String) -> Self {
        let handlers = CONTENT_HANDLERS
            .iter()
            .map(|c| (hex(&Sha256::digest(c.as_bytes())), *c))
            .collect();
        Self {
            repo_dir,
            url,
            handlers,
            objects: Mutex::default(),
            locks: Mutex::default(),
            rate_limits: Mutex::default(),
            retries: Mutex::default(),
            expired: Mutex::default(),
            verify_counts: Mutex::default(),
            range_rejected_attempted: Mutex::default(),
        }
    }

    /// The content handler for `oid`, or `""` for an ordinary object.
    pub(crate) fn handler(&self, oid: &str) -> &'static str {
        self.handlers.get(oid).copied().unwrap_or("")
    }

    /// Take a request from a rate-limited resource's budget.
    ///
    /// Returns how many seconds to wait when the budget is used up. The
    /// first request starts the wait. Once it has passed, the resource gets
    /// [`REFILL_TOKEN_COUNT`] more requests.
    pub(crate) fn check_rate_limit(&self, key: &str) -> Option<u64> {
        let mut limits = self.rate_limits.lock().unwrap();
        let RateLimits { tokens, started } = &mut *limits;
        match tokens.get(key).copied() {
            Some(remaining) if remaining != 0 => {
                let remaining = remaining - 1;
                tokens.insert(key.to_owned(), remaining);
                if remaining == 0 {
                    started.insert(key.to_owned(), Instant::now());
                }
                None
            }
            _ => {
                let start = *started.entry(key.to_owned()).or_insert_with(Instant::now);
                let passed = start.elapsed().as_secs_f64();
                if passed >= SECONDS_TO_REFILL_TOKENS as f64 {
                    tokens.insert(key.to_owned(), REFILL_TOKEN_COUNT);
                    return None;
                }
                Some(SECONDS_TO_REFILL_TOKENS - passed as u64 + 1)
            }
        }
    }

    /// Set a resource's remaining requests, restarting the wait at 0.
    pub(crate) fn set_rate_limit(&self, key: &str, tokens: i64) {
        let mut limits = self.rate_limits.lock().unwrap();
        limits.tokens.insert(key.to_owned(), tokens);
        if tokens == 0 {
            limits.started.insert(key.to_owned(), Instant::now());
        }
    }

    /// Count a request for an object whose contents are
    /// `<api>-<direction>-retry`, returning the count so far. `None` for
    /// any other object.
    pub(crate) fn increment_retries(
        &self,
        api: &str,
        direction: &str,
        repo: &str,
        oid: &str,
    ) -> Option<u32> {
        if self.handler(oid) != format!("{api}-{direction}-retry") {
            return None;
        }
        let mut retries = self.retries.lock().unwrap();
        let count = retries
            .entry(resource_key(api, direction, repo, oid))
            .or_default();
        *count += 1;
        Some(*count)
    }

    /// Whether to serve an expired action for `repo`: always when
    /// `forever` is set, and otherwise only the first time.
    pub(crate) fn serve_expired(&self, repo: &str, forever: bool) -> bool {
        let mut expired = self.expired.lock().unwrap();
        if forever || !expired.get(repo).copied().unwrap_or(false) {
            expired.insert(repo.to_owned(), true);
            return true;
        }
        false
    }

    /// Count a verify request for `oid` in `repo`.
    pub(crate) fn count_verify(&self, repo: &str, oid: &str) -> u32 {
        let mut counts = self.verify_counts.lock().unwrap();
        let count = counts.entry(format!("{repo}:{oid}")).or_default();
        *count += 1;
        *count
    }
}

#[derive(Default)]
struct RateLimits {
    tokens: HashMap<String, i64>,
    started: HashMap<String, Instant>,
}

/// The key for a rate-limited or retried resource.
pub(crate) fn resource_key(api: &str, direction: &str, repo: &str, oid: &str) -> String {
    [api, direction, repo, oid].join(":")
}

/// Stored objects, and partial tus uploads, by repository and OID.
#[derive(Default)]
pub(crate) struct Storage {
    objects: HashMap<String, HashMap<String, Vec<u8>>>,
    incomplete: HashMap<String, HashMap<String, Vec<u8>>>,
}

impl Storage {
    pub(crate) fn get(&self, repo: &str, oid: &str) -> Option<&Vec<u8>> {
        self.objects.get(repo)?.get(oid)
    }

    pub(crate) fn set(&mut self, repo: &str, oid: &str, data: Vec<u8>) {
        self.objects
            .entry(repo.to_owned())
            .or_default()
            .insert(oid.to_owned(), data);
    }

    pub(crate) fn delete(&mut self, repo: &str, oid: &str) {
        if let Some(objects) = self.objects.get_mut(repo) {
            objects.remove(oid);
        }
    }

    pub(crate) fn get_incomplete(&self, repo: &str, oid: &str) -> Option<&Vec<u8>> {
        self.incomplete.get(repo)?.get(oid)
    }

    pub(crate) fn set_incomplete(&mut self, repo: &str, oid: &str, data: Vec<u8>) {
        self.incomplete
            .entry(repo.to_owned())
            .or_default()
            .insert(oid.to_owned(), data);
    }

    pub(crate) fn delete_incomplete(&mut self, repo: &str, oid: &str) {
        if let Some(objects) = self.incomplete.get_mut(repo) {
            objects.remove(oid);
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Lock {
    pub id: String,
    pub path: String,
    pub owner: User,
    /// RFC 3339 with nanoseconds, in UTC.
    pub locked_at: String,
    /// For ordering. Not sent.
    #[serde(skip, default = "SystemTime::now")]
    pub created: SystemTime,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct User {
    pub name: String,
}

/// Format `time` like Go's `time.RFC3339Nano`, in UTC, such as
/// `2006-01-02T15:04:05.999999999Z`.
pub(crate) fn rfc3339(time: SystemTime) -> String {
    let since = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = since.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let mut out = format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    );
    let nanos = since.subsec_nanos();
    if nanos != 0 {
        let fraction = format!("{nanos:09}");
        out.push('.');
        out.push_str(fraction.trim_end_matches('0'));
    }
    out.push('Z');
    out
}

/// Lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn rfc3339_trims_the_fraction() {
        let t = SystemTime::UNIX_EPOCH + Duration::new(1_795_523_460, 120_000_000);
        assert_eq!(rfc3339(t), "2026-11-24T12:31:00.12Z");
        assert_eq!(rfc3339(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn handlers_are_found_by_oid() {
        let state = State::new(PathBuf::new(), String::new());
        let oid = hex(&Sha256::digest(b"unknown-oid"));
        assert_eq!(state.handler(&oid), "unknown-oid");
        assert_eq!(state.handler("0000"), "");
    }

    #[test]
    fn rate_limit_waits_then_refills() {
        let state = State::new(PathBuf::new(), String::new());
        state.set_rate_limit("k", 1);
        assert_eq!(state.check_rate_limit("k"), None);
        assert_eq!(state.check_rate_limit("k"), Some(11));
    }
}
