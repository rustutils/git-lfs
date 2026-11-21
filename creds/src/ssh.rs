//! Auth for SSH remotes, from the `git-lfs-authenticate` command.
//!
//! [`SshAuthClient`] runs `ssh [-p <port>] <user@host> "git-lfs-authenticate
//! <path> <operation>"` and parses the JSON it prints: an HTTPS endpoint and
//! headers to send to it. The user's SSH key is the only credential needed.
//!
//! The caller picks the SSH program. The CLI uses `GIT_SSH_COMMAND`, then
//! `GIT_SSH`, then `ssh`, and doesn't read `core.sshCommand`. The program
//! string is split on whitespace and run without a shell. The user, host
//! and path are passed as given: a host starting with `-` reaches ssh as an
//! option, and the path isn't quoted for the remote shell.
//!
//! Responses are cached per user and host, port, path, and operation.
//! Unlike upstream, the client doesn't read `lfs.ssh.retries`,
//! `lfs.ssh.automultiplex` or `lfs.defaulttokenttl`, and a failed command is
//! an error, with no fallback to HTTP Basic auth.
//!
//! The `exec: …`, `ssh cache: …` and `ssh cache expired: …` trace lines use
//! upstream's wording, which `t-batch-transfer.sh`, `t-expired.sh` and
//! `t-locks.sh` check for.
//!
//! # Wire format
//!
//! `git-lfs-authenticate` prints one JSON object:
//!
//! ```json
//! {
//!   "href": "https://lfs.example/repo.git/info/lfs",
//!   "header": { "Authorization": "Bearer ..." },
//!   "expires_at": "2026-05-04T12:34:56Z",
//!   "expires_in": 3600
//! }
//! ```
//!
//! All fields are optional, and `header` may be `null`. `expires_in` is in
//! seconds and may be negative. When both expiry fields are set, the
//! earlier time wins. Upstream uses `expires_in` whenever it isn't 0, and
//! `expires_at` otherwise. Here, an `expires_in` of 0 means expired at once.
//! An `expires_at` that isn't RFC 3339 or is before 1970 counts as unset.

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use crate::trace::trace_enabled;

/// The operation argument of `git-lfs-authenticate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SshOperation {
    /// Sent as `upload`.
    Upload,
    /// Sent as `download`.
    Download,
}

impl SshOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Download => "download",
        }
    }
}

/// A parsed `git-lfs-authenticate` response.
#[derive(Debug, Clone)]
pub struct SshAuth {
    /// LFS API endpoint to use instead of the remote's URL.
    ///
    /// Empty if the server didn't send one.
    pub href: String,
    /// Headers to add to LFS API requests, usually `Authorization`.
    pub header: HashMap<String, String>,
    /// When the response expires.
    ///
    /// `None` means no expiry: the response stays cached for the life of the
    /// [`SshAuthClient`].
    pub expires_at: Option<SystemTime>,
}

/// Error from [`SshAuthClient::resolve`].
#[derive(Debug, thiserror::Error)]
pub enum SshAuthError {
    /// The SSH program couldn't be run.
    #[error("io error invoking ssh: {0}")]
    Io(#[from] std::io::Error),
    /// The SSH program string is empty, or the command exited non-zero.
    ///
    /// Holds the command's stderr, or its exit status if stderr was empty.
    #[error("ssh git-lfs-authenticate failed: {0}")]
    Failed(String),
    /// The command's output wasn't a valid JSON response.
    #[error("ssh git-lfs-authenticate returned malformed JSON: {0}")]
    Json(String),
}

/// Runs `git-lfs-authenticate` over SSH and caches the responses.
///
/// A cached response with less than 5 seconds left counts as expired, as in
/// upstream, so it doesn't expire between the cache check and the request.
#[derive(Debug)]
pub struct SshAuthClient {
    program: String,
    cache: Mutex<HashMap<CacheKey, SshAuth>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    user_and_host: String,
    port: String,
    path: String,
    operation: SshOperation,
}

#[derive(Debug, Default, Deserialize)]
struct WireResponse {
    #[serde(default)]
    href: String,
    /// An `Option` so that `"header": null` parses.
    ///
    /// Go encodes a nil map as `null`, and the test server `lfs-ssh-echo`
    /// sends it. `#[serde(default)]` alone only covers a missing field.
    #[serde(default)]
    header: Option<HashMap<String, String>>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

impl SshAuthClient {
    /// Build a client that runs `program`.
    ///
    /// `program` is split on whitespace, with no quoting. The first word is
    /// the executable and the rest go before the `-p`, host and command
    /// arguments.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Get the auth response for an SSH remote and operation.
    ///
    /// Returns the cached response unless it's missing or expired.
    /// Otherwise runs the command and caches the result. Errors aren't
    /// cached.
    pub fn resolve(
        &self,
        user_and_host: &str,
        port: Option<&str>,
        path: &str,
        operation: SshOperation,
    ) -> Result<SshAuth, SshAuthError> {
        let key = CacheKey {
            user_and_host: user_and_host.to_owned(),
            port: port.unwrap_or("").to_owned(),
            path: path.to_owned(),
            operation,
        };

        // Clone so the lock is released before running ssh.
        let cached = self.cache.lock().unwrap().get(&key).cloned();
        if let Some(c) = cached {
            if !is_expired_within(c.expires_at, Duration::from_secs(5)) {
                trace(format_args!(
                    "ssh cache: {user_and_host} git-lfs-authenticate {path} {}",
                    operation.as_str()
                ));
                return Ok(c);
            }
            trace(format_args!(
                "ssh cache expired: {user_and_host} git-lfs-authenticate {path} {}",
                operation.as_str()
            ));
        }

        let resolved = self.spawn(user_and_host, port, path, operation)?;
        self.cache.lock().unwrap().insert(key, resolved.clone());
        Ok(resolved)
    }

    fn spawn(
        &self,
        user_and_host: &str,
        port: Option<&str>,
        path: &str,
        operation: SshOperation,
    ) -> Result<SshAuth, SshAuthError> {
        // The remote command is one argument, for the remote shell.
        let mut parts = self.program.split_whitespace();
        let prog = parts
            .next()
            .ok_or_else(|| SshAuthError::Failed("ssh program is empty".into()))?;
        let mut argv: Vec<String> = parts.map(str::to_owned).collect();
        if let Some(p) = port {
            argv.push("-p".to_owned());
            argv.push(p.to_owned());
        }
        argv.push(user_and_host.to_owned());
        argv.push(format!(
            "git-lfs-authenticate {path} {}",
            operation.as_str()
        ));

        // Upstream shell-quotes arguments that contain special characters.
        // The shell tests only match parts that need no quoting.
        if trace_enabled() {
            let mut e = std::io::stderr().lock();
            let _ = write!(e, "exec: {prog}");
            for a in &argv {
                let _ = write!(e, " {a}");
            }
            let _ = writeln!(e);
        }

        let now = SystemTime::now();
        let out = Command::new(prog)
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            return Err(SshAuthError::Failed(if stderr.is_empty() {
                format!("ssh {prog:?} exited {}", out.status)
            } else {
                stderr
            }));
        }

        let wire: WireResponse =
            serde_json::from_slice(&out.stdout).map_err(|e| SshAuthError::Json(e.to_string()))?;

        Ok(SshAuth {
            href: wire.href,
            header: wire.header.unwrap_or_default(),
            expires_at: compute_expires_at(now, wire.expires_at.as_deref(), wire.expires_in),
        })
    }
}

/// Combine `expires_at` and `expires_in` into one expiry time.
///
/// When both are set, the earlier wins.
fn compute_expires_at(
    now: SystemTime,
    expires_at: Option<&str>,
    expires_in: Option<i64>,
) -> Option<SystemTime> {
    let mut earliest: Option<SystemTime> = None;
    if let Some(s) = expires_at
        && !s.is_empty()
        && let Some(t) = parse_rfc3339(s)
    {
        earliest = Some(t);
    }
    if let Some(secs) = expires_in {
        let t = if secs >= 0 {
            now.checked_add(Duration::from_secs(secs as u64))
        } else {
            now.checked_sub(Duration::from_secs(secs.unsigned_abs()))
        };
        if let Some(t) = t {
            earliest = Some(match earliest {
                Some(e) => e.min(t),
                None => t,
            });
        }
    }
    earliest
}

fn is_expired_within(expires_at: Option<SystemTime>, buffer: Duration) -> bool {
    let Some(t) = expires_at else { return false };
    let now = SystemTime::now();
    match t.duration_since(now) {
        Ok(remaining) => remaining < buffer,
        Err(_) => true,
    }
}

/// Parse an RFC 3339 timestamp such as `2026-05-04T12:34:56.789+02:00`.
///
/// Returns `None` for malformed input and for times before 1970. Go
/// encodes an unset `time.Time` as `0001-01-01T00:00:00Z`, and
/// `lfs-ssh-echo` sends that, so it has to mean no expiry rather than
/// expired. Field ranges aren't checked.
fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let bytes = s.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    if bytes[4] != b'-'
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
        // Before 1970 means unset (see the function docs).
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(unix as u64))
}

/// Days since 1970-01-01 for a Gregorian date.
///
/// Howard Hinnant's `days_from_civil` algorithm.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = (if month <= 2 { year - 1 } else { year }) as i64;
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn trace(args: std::fmt::Arguments) {
    if !trace_enabled() {
        return;
    }
    let mut e = std::io::stderr().lock();
    let _ = writeln!(e, "{args}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rfc3339_z() {
        let t = parse_rfc3339("2026-05-04T12:34:56Z").unwrap();
        let unix = t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        // `date -u -d 2026-05-04T12:34:56Z +%s` gives 1777898096.
        assert_eq!(unix, 1777898096);
    }

    #[test]
    fn parse_rfc3339_with_fraction() {
        let a = parse_rfc3339("2026-05-04T12:34:56.789Z").unwrap();
        let b = parse_rfc3339("2026-05-04T12:34:56Z").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn parse_rfc3339_offset() {
        let plus = parse_rfc3339("2026-05-04T14:34:56+02:00").unwrap();
        let utc = parse_rfc3339("2026-05-04T12:34:56Z").unwrap();
        assert_eq!(plus, utc);
    }

    #[test]
    fn parse_rfc3339_zero_value_is_unset() {
        // Go's zero `time.Time` encodes to this. It means no expiry.
        assert_eq!(parse_rfc3339("0001-01-01T00:00:00Z"), None);
    }

    #[test]
    fn parse_rfc3339_rejects_garbage() {
        assert!(parse_rfc3339("").is_none());
        assert!(parse_rfc3339("not a timestamp").is_none());
        assert!(parse_rfc3339("2026-13-99T00:00:00Z").is_some()); // we don't validate ranges
    }

    #[test]
    fn compute_expires_at_picks_earliest() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        // `expires_in` says +60s, `expires_at` says +30s. Earlier wins.
        let in_60 = Some(60);
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_030);
        let at_str = format_unix_for_test(at);
        let combined = compute_expires_at(now, Some(&at_str), in_60).unwrap();
        assert_eq!(combined, at);
    }

    #[test]
    fn compute_expires_at_handles_negative_in() {
        // Negative `expires_in` (server already-expired token).
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let result = compute_expires_at(now, None, Some(-5)).unwrap();
        assert_eq!(result, SystemTime::UNIX_EPOCH + Duration::from_secs(95));
    }

    #[test]
    fn compute_expires_at_returns_none_when_unset() {
        let now = SystemTime::UNIX_EPOCH;
        assert!(compute_expires_at(now, None, None).is_none());
        assert!(compute_expires_at(now, Some(""), None).is_none());
    }

    #[test]
    fn is_expired_within_buffer() {
        let now = SystemTime::now();
        // 10 seconds in the future, with a 5s buffer → not expired.
        assert!(!is_expired_within(
            Some(now + Duration::from_secs(10)),
            Duration::from_secs(5),
        ));
        // 2 seconds in the future, with a 5s buffer → expired.
        assert!(is_expired_within(
            Some(now + Duration::from_secs(2)),
            Duration::from_secs(5),
        ));
        // Already past, so expired.
        assert!(is_expired_within(
            Some(now - Duration::from_secs(1)),
            Duration::from_secs(5),
        ));
        // No expiry set, so never expired.
        assert!(!is_expired_within(None, Duration::from_secs(5)));
    }

    #[test]
    fn fill_invokes_ssh_and_parses_response() {
        // Stand-in ssh: a shell script that prints a fixed JSON response
        // regardless of args, to test running and parsing.
        let tmp = tempfile::TempDir::new().unwrap();
        let prog = tmp.path().join("fakessh");
        std::fs::write(
            &prog,
            "#!/bin/sh\n\
             cat <<'EOF'\n\
             {\"href\":\"https://lfs.example/repo.git/info/lfs\",\
              \"header\":{\"Authorization\":\"Bearer abc\"},\
              \"expires_in\":3600}\n\
             EOF\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&prog).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&prog, perms).unwrap();
        }

        let client = SshAuthClient::new(prog.to_string_lossy().into_owned());
        let auth = client
            .resolve("git@host", None, "/repo", SshOperation::Upload)
            .unwrap();
        assert_eq!(auth.href, "https://lfs.example/repo.git/info/lfs");
        assert_eq!(
            auth.header.get("Authorization").map(String::as_str),
            Some("Bearer abc")
        );
        assert!(auth.expires_at.is_some());
    }

    #[test]
    fn cache_returns_same_response_within_ttl() {
        // Two calls, but the script appends a line to a file each time it
        // runs. If the cache works, the file has one line, not two.
        let tmp = tempfile::TempDir::new().unwrap();
        let counter = tmp.path().join("count");
        let prog = tmp.path().join("fakessh");
        std::fs::write(
            &prog,
            format!(
                "#!/bin/sh\n\
                 echo invoked >> {counter}\n\
                 cat <<'EOF'\n\
                 {{\"href\":\"https://lfs.example/repo.git/info/lfs\",\"expires_in\":3600}}\n\
                 EOF\n",
                counter = counter.display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&prog).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&prog, perms).unwrap();
        }

        let client = SshAuthClient::new(prog.to_string_lossy().into_owned());
        let _ = client
            .resolve("git@host", None, "/repo", SshOperation::Upload)
            .unwrap();
        let _ = client
            .resolve("git@host", None, "/repo", SshOperation::Upload)
            .unwrap();
        let lines = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(lines.lines().count(), 1, "expected exactly one ssh spawn");
    }

    #[test]
    fn cache_re_resolves_when_expired() {
        // Server returns `expires_in: -5` so the entry is born already
        // expired. Second call should re-spawn.
        let tmp = tempfile::TempDir::new().unwrap();
        let counter = tmp.path().join("count");
        let prog = tmp.path().join("fakessh");
        std::fs::write(
            &prog,
            format!(
                "#!/bin/sh\n\
                 echo invoked >> {counter}\n\
                 cat <<'EOF'\n\
                 {{\"href\":\"https://lfs.example/repo.git/info/lfs\",\"expires_in\":-5}}\n\
                 EOF\n",
                counter = counter.display(),
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&prog).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&prog, perms).unwrap();
        }

        let client = SshAuthClient::new(prog.to_string_lossy().into_owned());
        let _ = client
            .resolve("git@host", None, "/repo", SshOperation::Upload)
            .unwrap();
        let _ = client
            .resolve("git@host", None, "/repo", SshOperation::Upload)
            .unwrap();
        let lines = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(
            lines.lines().count(),
            2,
            "expected ssh to re-spawn after expiry"
        );
    }

    #[test]
    fn ssh_failure_surfaces_stderr() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prog = tmp.path().join("fakessh");
        std::fs::write(&prog, "#!/bin/sh\necho 'permission denied' >&2\nexit 255\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&prog).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&prog, perms).unwrap();
        }

        let client = SshAuthClient::new(prog.to_string_lossy().into_owned());
        let err = client
            .resolve("git@host", None, "/repo", SshOperation::Download)
            .unwrap_err();
        match err {
            SshAuthError::Failed(msg) => assert!(msg.contains("permission denied"), "got {msg}"),
            other => panic!("unexpected: {other}"),
        }
    }

    /// Format a `SystemTime` as an RFC 3339 string in UTC.
    fn format_unix_for_test(t: SystemTime) -> String {
        let secs = t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64;
        let days = secs.div_euclid(86400);
        let sod = secs.rem_euclid(86400);
        let (y, m, d) = civil_from_days(days);
        let h = sod / 3600;
        let mi = (sod % 3600) / 60;
        let se = sod % 60;
        format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{se:02}Z")
    }

    /// Inverse of [`days_from_civil`], also by Howard Hinnant.
    fn civil_from_days(z: i64) -> (i32, u32, u32) {
        let z = z + 719468;
        let era = if z >= 0 { z } else { z - 146096 } / 146097;
        let doe = (z - era * 146097) as u64; // [0, 146096]
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        (y as i32, m as u32, d as u32)
    }
}
