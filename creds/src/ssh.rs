//! SSH-based credential resolution via the `git-lfs-authenticate` command.
//!
//! For SSH remotes, upstream LFS shells out to `ssh user@host
//! git-lfs-authenticate <path> <upload|download>` and parses a JSON
//! response containing a replacement HTTPS endpoint plus headers to
//! merge into batch/locks requests. The SSH key is the only credential
//! the user has to manage — no separate HTTPS password.
//!
//! Selection priority (resolved by the caller before constructing this
//! helper):
//!
//! 1. `GIT_SSH_COMMAND` env var (full command line).
//! 2. `GIT_SSH` env var (single program path).
//! 3. Default: `ssh`.
//!
//! Caching is per `(user_and_host, port, path, operation)` with a
//! 5-second buffer before expiry — matches upstream's `sshCache` in
//! `lfshttp/ssh.go`. Trace lines (`exec: <prog> <args>`,
//! `ssh cache: ...`, `ssh cache expired: ...`) match upstream
//! verbatim — `t-batch-transfer.sh:161`, `t-expired.sh`, and
//! `t-locks.sh:74` grep them by name.
//!
//! # Wire format
//!
//! `git-lfs-authenticate` stdout is one JSON object:
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
//! All four fields are optional. `expires_in` is seconds; when both
//! `expires_at` and `expires_in` are present we pick the earlier one
//! (matches `tools.IsExpiredAtOrIn`).

use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use crate::trace::trace_enabled;

/// `git-lfs-authenticate <path> <operation>` operation argument.
///
/// Wire form is lowercase `upload` / `download`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SshOperation {
    /// Upload operation; auth token must be scoped for sending objects.
    Upload,
    /// Download operation; auth token must be scoped for fetching objects.
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

/// Parsed `git-lfs-authenticate` response with absolute expiry resolved.
#[derive(Debug, Clone)]
pub struct SshAuth {
    /// Replacement HTTPS endpoint. Empty when the server expects the
    /// original URL to be used as-is.
    pub href: String,
    /// Headers to merge into LFS API requests (commonly `Authorization`).
    pub header: HashMap<String, String>,
    /// Absolute expiration time. `None` means "no server-side TTL" — we
    /// keep the entry until process exit.
    pub expires_at: Option<SystemTime>,
}

/// Things that can go wrong while resolving SSH-based credentials.
#[derive(Debug, thiserror::Error)]
pub enum SshAuthError {
    /// Failed to spawn or talk to the ssh subprocess.
    #[error("io error invoking ssh: {0}")]
    Io(#[from] std::io::Error),
    /// `git-lfs-authenticate` ran but exited non-zero.
    #[error("ssh git-lfs-authenticate failed: {0}")]
    Failed(String),
    /// `git-lfs-authenticate` stdout wasn't a parseable JSON response.
    #[error("ssh git-lfs-authenticate returned malformed JSON: {0}")]
    Json(String),
}

/// Spawns `ssh user@host git-lfs-authenticate <path> <operation>` and
/// caches the result. Cloneable (cache is `Arc`-backed via `Mutex`).
///
/// Cache key is `(user_and_host, port, path, operation)`. Entries with
/// less than 5s remaining are considered expired — same buffer upstream
/// uses to absorb network delay between cache check and HTTP send.
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
    /// Optional so we tolerate `"header": null` — that's what Go's
    /// `json.Marshal` emits for a nil `map[string]string`, and the
    /// reference test server (`lfs-ssh-echo`) does exactly that for
    /// repos that don't override headers. `#[serde(default)]` alone
    /// only handles the *missing* case, not an explicit null.
    #[serde(default)]
    header: Option<HashMap<String, String>>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

impl SshAuthClient {
    /// Build with a resolved SSH program string. Whitespace-separated —
    /// the first token is the executable, subsequent tokens are extra
    /// args prepended before the per-call `[-p PORT] user@host
    /// git-lfs-authenticate ...` arguments. Same shape as upstream's
    /// `subprocess.ExecCommand`.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve auth for `(user_and_host, port, path, operation)`. Hits
    /// the cache first; on miss or expired entry, spawns ssh and stores
    /// the new response.
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

        // Cache check. Clone out so we drop the lock before maybe
        // spawning ssh (which can take a while).
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
        // Argv mirrors `ssh.GetLFSExeAndArgs` in upstream:
        //   <ssh_program> [<extra args from $GIT_SSH_COMMAND>]
        //                 [-p <port>] <user@host>
        //                 "git-lfs-authenticate <path> <operation>"
        // Remote command goes as ONE argument — ssh's standard "shell on
        // remote" form. (Mirrors `t-batch-transfer.sh:161`'s grep
        // pattern.)
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

        // `exec: <prog> <args>` matches upstream's `subprocess.ExecCommand`
        // tracing — `t-batch-transfer.sh:161` greps for it.
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

/// Combine `expires_at` (absolute) and `expires_in` (seconds-from-now)
/// into a single `SystemTime`. When both are set, the earlier wins —
/// upstream's `tools.IsExpiredAtOrIn` evaluates both conditions.
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

/// Minimal RFC 3339 parser — accepts `YYYY-MM-DDThh:mm:ss[.fff][Z|±hh:mm]`.
/// Returns `Some(SystemTime)` for valid post-epoch timestamps, `None`
/// for malformed input *or* pre-epoch values. Pre-epoch is the
/// "no expiry set" sentinel: Go's `time.Time` zero value
/// (`0001-01-01T00:00:00Z`) is what `lfs-ssh-echo` emits when
/// `expires_at` was never assigned, since `omitempty` doesn't omit
/// struct fields. Treating it as "unset" (rather than "epoch =
/// expired") matches upstream's `IsExpiredAtOrIn` IsZero gate, so the
/// cache doesn't pessimistically refresh after every request.
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
        // Pre-epoch ⇒ treat as "no expiry set" (see fn doc).
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(unix as u64))
}

/// Howard Hinnant's days-from-civil algorithm. Returns days since
/// 1970-01-01 for the proleptic Gregorian date `(y, m, d)`.
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
        // 2026-05-04 12:34:56 UTC = 1777898096 (verifiable via `date -u -d ...`).
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
        // Go's `time.Time` zero value JSON-encodes to this. We map it
        // to None so `compute_expires_at` treats the field as "no
        // expiry set" — matches upstream's `IsExpiredAtOrIn` IsZero
        // gate (see parse_rfc3339 doc comment).
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
        let at_30 = Some("1970-01-12T13:46:40Z"); // 1_000_000 + 30 seconds = 1_000_030
        // Recompute manually: 1_000_030 unix = 1970-01-12T13:47:10Z
        // Let's use a value we can verify: 30s after epoch+1M.
        let _ = at_30;
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
        // Already past — expired.
        assert!(is_expired_within(
            Some(now - Duration::from_secs(1)),
            Duration::from_secs(5),
        ));
        // No expiry set — never expired.
        assert!(!is_expired_within(None, Duration::from_secs(5)));
    }

    #[test]
    fn fill_invokes_ssh_and_parses_response() {
        // Stand-in ssh: a shell script that prints a fixed JSON response
        // regardless of args, so we can verify the spawn + parse path.
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
        // Two calls, but the script writes a fresh timestamp into a file
        // each invocation — if the cache works, the file should have one
        // line, not two.
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

    /// Test helper: format a `SystemTime` as an RFC 3339 string in UTC.
    /// We only use this for round-trip tests, so the implementation is
    /// the inverse of [`days_from_civil`] / [`parse_rfc3339`] above.
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

    /// Inverse of [`days_from_civil`] — Howard Hinnant's algorithm.
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
