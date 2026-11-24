//! Auth for SSH remotes, from the `git-lfs-authenticate` command.
//!
//! [`SshAuthClient`] runs `ssh [-p <port>] <user@host> "git-lfs-authenticate
//! <path> <operation>"` and parses the JSON it prints: an HTTPS endpoint and
//! headers to send to it. The user's SSH key is the only credential needed.
//!
//! The caller picks the [`SshProgram`], and the [`SshVariant`] if it's
//! configured. Otherwise the variant is guessed from the program's name.
//! The remote command is quoted for the server's shell with [`shell_quote`].
//!
//! Responses are cached per user and host, port, path, and operation. A
//! failed command is retried, 5 times by default, unless the server has no
//! `git-lfs-authenticate`. After the last try it's an error, with no
//! fallback to HTTP Basic auth. The caller passes `lfs.ssh.retries` with
//! [`SshAuthClient::with_retries`] and `lfs.defaulttokenttl` with
//! [`SshAuthClient::with_default_ttl`].
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
//! `expires_at` otherwise. As there, an `expires_in` of 0 counts as unset,
//! so with no `expires_at` the response never expires.
//! An `expires_at` that isn't RFC 3339 or is before 1970 counts as unset.

use std::borrow::Cow;
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
    /// The server has no `git-lfs-authenticate` command.
    ///
    /// Callers should use the endpoint guessed from the SSH URL instead.
    /// Holds the command's stderr.
    #[error("git-lfs-authenticate isn't available on the server: {0}")]
    Unavailable(String),
}

/// Stderr messages, in lowercase, that mean the server has no
/// `git-lfs-authenticate`, as in upstream. Some servers, such as Gerrit,
/// exit with 1 instead of the shell's 127.
const NOT_FOUND_MESSAGES: &[&str] = &[
    "git-lfs-authenticate: not found",
    "git-lfs-authenticate: command not found",
    "git-lfs-authenticate: no such file",
    "command not found: git-lfs-authenticate",
];

/// Whether a failed run means the server has no `git-lfs-authenticate`.
fn is_unavailable(status: std::process::ExitStatus, stderr: &str) -> bool {
    if status.code() == Some(127) {
        return true;
    }
    let stderr = stderr.to_lowercase();
    NOT_FOUND_MESSAGES.iter().any(|m| stderr.contains(m))
}

/// The SSH program to run, as chosen by Git's rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshProgram {
    /// A command line, from `GIT_SSH_COMMAND` or `core.sshCommand`.
    ///
    /// It runs with `sh -c`, so it can hold arguments and quotes.
    Shell(String),
    /// A program path, from `GIT_SSH`, run directly.
    Exe(String),
}

impl Default for SshProgram {
    fn default() -> Self {
        Self::Exe("ssh".to_owned())
    }
}

impl SshProgram {
    /// The program to execute and its arguments, to run this with `args`.
    ///
    /// A [`Shell`](Self::Shell) command becomes `sh -c '<command> <args>'`,
    /// with each argument quoted by [`shell_quote`], as in upstream.
    pub fn command_line(&self, args: Vec<String>) -> (String, Vec<String>) {
        match self {
            Self::Exe(program) => (program.clone(), args),
            Self::Shell(command) => {
                let mut line = command.clone();
                for arg in &args {
                    line.push(' ');
                    line.push_str(&shell_quote(arg));
                }
                ("sh".to_owned(), vec!["-c".to_owned(), line])
            }
        }
    }
}

impl SshProgram {
    /// The SSH variant to assume when none is configured, from the
    /// program's name, as upstream guesses it.
    ///
    /// `plink` is PuTTY and `tortoiseplink` is TortoisePlink, ignoring case
    /// and any extension. Anything else is OpenSSH.
    pub fn detect_variant(&self) -> SshVariant {
        let program = match self {
            Self::Exe(path) => path.as_str(),
            Self::Shell(command) => first_word(command),
        };
        let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
        let stem = match base.rsplit_once('.') {
            Some((stem, _)) if !stem.is_empty() => stem,
            _ => base,
        };
        if stem.eq_ignore_ascii_case("plink") {
            SshVariant::Putty
        } else if stem.eq_ignore_ascii_case("tortoiseplink") {
            SshVariant::Tortoise
        } else {
            SshVariant::Ssh
        }
    }
}

/// The first word of a shell command, without the quotes around it.
fn first_word(command: &str) -> &str {
    let command = command.trim_start();
    for quote in ['\'', '"'] {
        if let Some(rest) = command.strip_prefix(quote) {
            return rest.split(quote).next().unwrap_or(rest);
        }
    }
    command.split_whitespace().next().unwrap_or("")
}

/// The kind of SSH client, which decides how its arguments are built.
///
/// Set by `GIT_SSH_VARIANT` or `ssh.variant`, or guessed from the program
/// name with [`SshProgram::detect_variant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SshVariant {
    /// OpenSSH `ssh`, the default. The only variant that multiplexes
    /// connections and takes `--` before the host.
    #[default]
    Ssh,
    /// A client that takes no options but `-p`.
    Simple,
    /// PuTTY `plink`, which takes `-P` for the port.
    Putty,
    /// TortoisePlink: like PuTTY, plus `-batch`.
    Tortoise,
}

impl SshVariant {
    /// Read a `GIT_SSH_VARIANT` or `ssh.variant` value.
    ///
    /// `auto` gives `None`, to detect the variant from the program. An
    /// unknown value means OpenSSH, as in upstream.
    pub fn from_config(value: &str) -> Option<Self> {
        Some(match value {
            "auto" => return None,
            "simple" => Self::Simple,
            "putty" | "plink" => Self::Putty,
            "tortoiseplink" => Self::Tortoise,
            _ => Self::Ssh,
        })
    }

    /// Whether this client can share connections through a control
    /// socket.
    pub fn supports_multiplex(self) -> bool {
        self == Self::Ssh
    }

    /// Arguments that go before any others, such as TortoisePlink's
    /// `-batch`.
    pub fn leading_args(self) -> &'static [&'static str] {
        match self {
            Self::Tortoise => &["-batch"],
            _ => &[],
        }
    }

    /// The port option, if any, then the host, as upstream builds them.
    ///
    /// A host starting with `-`, such as `-oProxyCommand=...` from an
    /// `ssh://` URL, would be read as an option. OpenSSH gets `--` before
    /// it, and the other variants get it with the dashes stripped.
    pub fn host_args(self, port: Option<&str>, user_and_host: &str) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(port) = port {
            let flag = match self {
                Self::Putty | Self::Tortoise => "-P",
                Self::Ssh | Self::Simple => "-p",
            };
            args.push(flag.to_owned());
            args.push(port.to_owned());
        }
        match user_and_host.strip_prefix('-') {
            Some(_) if self == Self::Ssh => {
                args.push("--".to_owned());
                args.push(user_and_host.to_owned());
            }
            Some(_) => args.push(user_and_host.trim_start_matches('-').to_owned()),
            None => args.push(user_and_host.to_owned()),
        }
        args
    }
}

/// Runs `git-lfs-authenticate` over SSH and caches the responses.
///
/// A cached response with less than 5 seconds left counts as expired, as in
/// upstream, so it doesn't expire between the cache check and the request.
#[derive(Debug)]
pub struct SshAuthClient {
    program: SshProgram,
    variant: SshVariant,
    default_ttl: Option<Duration>,
    retries: u32,
    cache: Mutex<HashMap<CacheKey, SshAuth>>,
}

/// How many times a failed command is retried, as upstream's default for
/// `lfs.ssh.retries`.
const DEFAULT_RETRIES: u32 = 5;

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
    pub fn new(program: SshProgram) -> Self {
        Self {
            variant: program.detect_variant(),
            default_ttl: None,
            retries: DEFAULT_RETRIES,
            program,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Treat a response without `expires_in` or `expires_at` as expiring
    /// after `ttl`, like upstream's `lfs.defaulttokenttl`.
    ///
    /// Without it, such a response never expires.
    pub fn with_default_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = Some(ttl);
        self
    }

    /// Retry a failed command `retries` times, like upstream's
    /// `lfs.ssh.retries`. The default is 5.
    ///
    /// A server without `git-lfs-authenticate` isn't retried.
    pub fn with_retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    /// Use `variant` instead of the one detected from the program.
    pub fn with_variant(mut self, variant: SshVariant) -> Self {
        self.variant = variant;
        self
    }

    /// Get the auth response for an SSH remote and operation.
    ///
    /// Returns the cached response unless it's missing or expired.
    /// Otherwise runs the command, retrying it if it fails, and caches the
    /// result. Errors aren't cached.
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

        let tries = self.retries.saturating_add(1);
        let mut attempt = 1;
        let resolved = loop {
            match self.spawn(user_and_host, port, path, operation) {
                Ok(r) => break r,
                Err(e @ SshAuthError::Unavailable(_)) => return Err(e),
                Err(e) if attempt >= tries => return Err(e),
                Err(e) => trace(format_args!(
                    "ssh: {user_and_host} failed, error: {e} (try: {attempt}/{tries})"
                )),
            }
            attempt += 1;
        };
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
        let mut argv: Vec<String> = self
            .variant
            .leading_args()
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        argv.extend(self.variant.host_args(port, user_and_host));
        // The remote command is one argument, for the remote shell.
        argv.push(remote_command(path, operation));
        let (prog, argv) = self.program.command_line(argv);

        if trace_enabled() {
            let _ = writeln!(std::io::stderr().lock(), "{}", exec_trace(&prog, &argv));
        }

        let now = SystemTime::now();
        let out = Command::new(&prog)
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            if is_unavailable(out.status, &stderr) {
                return Err(SshAuthError::Unavailable(stderr));
            }
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
            expires_at: compute_expires_at(now, wire.expires_at.as_deref(), wire.expires_in)
                .or_else(|| self.default_ttl.and_then(|ttl| now.checked_add(ttl))),
        })
    }
}

/// Combine `expires_at` and `expires_in` into one expiry time.
///
/// When both are set, the earlier wins. An `expires_in` of 0 is unset.
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
    if let Some(secs) = expires_in.filter(|&s| s != 0) {
        let t = if secs > 0 {
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

/// The command run on the server, with each word quoted for its shell.
fn remote_command(path: &str, operation: SshOperation) -> String {
    format!(
        "{} {} {}",
        shell_quote("git-lfs-authenticate"),
        shell_quote(path),
        shell_quote(operation.as_str())
    )
}

/// Format the `exec:` trace of a command, like upstream's `subprocess.Trace`.
///
/// Each argument is quoted with [`shell_quote`], so the line shows exactly
/// what was run. A line break inside an argument continues the trace on an
/// `exec (cont.):` line.
pub fn exec_trace(program: &str, args: &[String]) -> String {
    let mut command = program.to_owned();
    for arg in args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }
    command
        .split('\n')
        .enumerate()
        .map(|(i, line)| {
            let prefix = if i == 0 { "exec" } else { "exec (cont.)" };
            format!("{prefix}: {line}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Quote a word for a POSIX shell, like upstream's `ShellQuoteSingle`.
///
/// ssh passes the remote command to the server's shell, so paths from
/// remote URLs must be quoted. Words made only of ASCII letters, digits, and
/// `_@/.-` are returned unchanged. Anything else is wrapped in single quotes.
pub fn shell_quote(word: &str) -> Cow<'_, str> {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@/.-".contains(&b));
    if plain {
        Cow::Borrowed(word)
    } else {
        Cow::Owned(format!("'{}'", word.replace('\'', "'\\''")))
    }
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
    fn expires_in_zero_is_unset() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert!(compute_expires_at(now, None, Some(0)).is_none());
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_030);
        let at_str = format_unix_for_test(at);
        assert_eq!(compute_expires_at(now, Some(&at_str), Some(0)), Some(at));
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

        let client = SshAuthClient::new(SshProgram::Shell(format!("sh {}", prog.display())));
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

        let client = SshAuthClient::new(SshProgram::Shell(format!("sh {}", prog.display())));
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
    fn expires_in_zero_is_cached() {
        // Upstream reads `expires_in: 0` as no expiry, so ssh runs once.
        let tmp = tempfile::TempDir::new().unwrap();
        let counter = tmp.path().join("count");
        let prog = tmp.path().join("fakessh");
        std::fs::write(
            &prog,
            format!(
                "#!/bin/sh\n\
                 echo invoked >> {counter}\n\
                 cat <<'EOF'\n\
                 {{\"href\":\"https://lfs.example/repo.git/info/lfs\",\"expires_in\":0}}\n\
                 EOF\n",
                counter = counter.display(),
            ),
        )
        .unwrap();

        let client = SshAuthClient::new(SshProgram::Shell(format!("sh {}", prog.display())));
        for _ in 0..2 {
            client
                .resolve("git@host", None, "/repo", SshOperation::Upload)
                .unwrap();
        }
        let lines = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(lines.lines().count(), 1, "expected exactly one ssh spawn");
    }

    #[test]
    fn default_ttl_applies_without_an_expiry() {
        // The response has no expiry. With a default TTL inside the 5s
        // buffer it's expired at once, so ssh runs for each call. Without
        // one, it never expires.
        let tmp = tempfile::TempDir::new().unwrap();
        let counter = tmp.path().join("count");
        let prog = tmp.path().join("fakessh");
        std::fs::write(
            &prog,
            format!(
                "#!/bin/sh\n\
                 echo invoked >> {counter}\n\
                 echo '{{\"href\":\"https://lfs.example/repo.git/info/lfs\"}}'\n",
                counter = counter.display(),
            ),
        )
        .unwrap();
        let program = SshProgram::Shell(format!("sh {}", prog.display()));
        let runs = |client: SshAuthClient| {
            let _ = std::fs::remove_file(&counter);
            for _ in 0..2 {
                client
                    .resolve("git@host", None, "/repo", SshOperation::Upload)
                    .unwrap();
            }
            std::fs::read_to_string(&counter).unwrap().lines().count()
        };

        assert_eq!(runs(SshAuthClient::new(program.clone())), 1);
        let short = SshAuthClient::new(program.clone()).with_default_ttl(Duration::from_secs(1));
        assert_eq!(runs(short), 2);

        let auth = SshAuthClient::new(program)
            .with_default_ttl(Duration::from_secs(3600))
            .resolve("git@host", None, "/repo", SshOperation::Upload)
            .unwrap();
        assert!(auth.expires_at.is_some());
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

        let client = SshAuthClient::new(SshProgram::Shell(format!("sh {}", prog.display())));
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

        let client = SshAuthClient::new(SshProgram::Shell(format!("sh {}", prog.display())));
        let err = client
            .resolve("git@host", None, "/repo", SshOperation::Download)
            .unwrap_err();
        match err {
            SshAuthError::Failed(msg) => assert!(msg.contains("permission denied"), "got {msg}"),
            other => panic!("unexpected: {other}"),
        }
    }

    /// A fake ssh that counts its runs in a file next to it, fails the
    /// first `failures` with `fail`, and then succeeds.
    fn counting_ssh(tmp: &tempfile::TempDir, failures: u32, fail: &str) -> SshProgram {
        let prog = tmp.path().join("fakessh");
        let count = tmp.path().join("count");
        std::fs::write(
            &prog,
            format!(
                "#!/bin/sh\n\
                 n=$(($(cat {c} 2>/dev/null || echo 0) + 1))\n\
                 echo $n > {c}\n\
                 if [ $n -le {failures} ]; then {fail}; fi\n\
                 echo '{{\"href\":\"https://lfs.example/repo\"}}'\n",
                c = count.display(),
            ),
        )
        .unwrap();
        SshProgram::Shell(format!("sh {}", prog.display()))
    }

    fn runs(tmp: &tempfile::TempDir) -> u32 {
        std::fs::read_to_string(tmp.path().join("count"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    #[test]
    fn failed_command_is_retried() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fail = "echo 'connection reset' >&2; exit 255";
        let client = SshAuthClient::new(counting_ssh(&tmp, 3, fail));
        let auth = client
            .resolve("git@host", None, "/repo", SshOperation::Download)
            .unwrap();
        assert_eq!(auth.href, "https://lfs.example/repo");
        assert_eq!(runs(&tmp), 4);
    }

    #[test]
    fn retries_stop_after_the_configured_count() {
        let tmp = tempfile::TempDir::new().unwrap();
        let fail = "echo 'connection reset' >&2; exit 255";
        let client = SshAuthClient::new(counting_ssh(&tmp, 10, fail)).with_retries(2);
        let err = client
            .resolve("git@host", None, "/repo", SshOperation::Download)
            .unwrap_err();
        assert!(matches!(err, SshAuthError::Failed(_)), "{err}");
        assert_eq!(runs(&tmp), 3);

        let tmp = tempfile::TempDir::new().unwrap();
        let client = SshAuthClient::new(counting_ssh(&tmp, 10, fail)).with_retries(0);
        assert!(
            client
                .resolve("git@host", None, "/repo", SshOperation::Download)
                .is_err()
        );
        assert_eq!(runs(&tmp), 1);
    }

    #[test]
    fn missing_git_lfs_authenticate_is_not_retried() {
        let tmp = tempfile::TempDir::new().unwrap();
        let client = SshAuthClient::new(counting_ssh(&tmp, 10, "exit 127"));
        let err = client
            .resolve("git@host", None, "/repo", SshOperation::Download)
            .unwrap_err();
        assert!(matches!(err, SshAuthError::Unavailable(_)), "{err}");
        assert_eq!(runs(&tmp), 1);
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

    #[test]
    fn shell_quote_leaves_plain_words() {
        assert_eq!(shell_quote("/foo/bar.git"), "/foo/bar.git");
        assert_eq!(shell_quote("git@host-1_x"), "git@host-1_x");
    }

    #[test]
    fn shell_quote_quotes_everything_else() {
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("/my repo"), "'/my repo'");
        assert_eq!(shell_quote("/a;touch x"), "'/a;touch x'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn remote_command_quotes_the_path() {
        assert_eq!(
            remote_command("/repo;touch x", SshOperation::Upload),
            "git-lfs-authenticate '/repo;touch x' upload"
        );
        assert_eq!(
            remote_command("/foo/bar.git", SshOperation::Download),
            "git-lfs-authenticate /foo/bar.git download"
        );
    }

    #[test]
    fn exec_trace_quotes_arguments() {
        let args = [
            "-p".to_owned(),
            "22".to_owned(),
            "git-lfs-authenticate /r upload".to_owned(),
        ];
        assert_eq!(
            exec_trace("ssh", &args),
            "exec: ssh -p 22 'git-lfs-authenticate /r upload'"
        );
    }

    #[test]
    fn exec_trace_continues_after_a_newline() {
        let args = ["a\nb".to_owned()];
        assert_eq!(exec_trace("ssh", &args), "exec: ssh 'a\nexec (cont.): b'");
    }

    #[test]
    fn host_starting_with_a_dash_is_not_an_option() {
        // The fake ssh records its arguments, one per line.
        let tmp = tempfile::TempDir::new().unwrap();
        let prog = tmp.path().join("fakessh");
        let args_file = tmp.path().join("args");
        std::fs::write(
            &prog,
            format!(
                "for a in \"$@\"; do echo \"$a\"; done > {}\n\
                 echo '{{\"href\":\"https://lfs.example/r\"}}'\n",
                args_file.display()
            ),
        )
        .unwrap();

        let client = SshAuthClient::new(SshProgram::Shell(format!("sh {}", prog.display())));
        client
            .resolve("-oProxyCommand=evil", None, "/repo", SshOperation::Upload)
            .unwrap();
        let args = std::fs::read_to_string(&args_file).unwrap();
        let args: Vec<&str> = args.lines().collect();
        let host = args
            .iter()
            .position(|a| *a == "-oProxyCommand=evil")
            .unwrap();
        assert_eq!(args[host - 1], "--", "{args:?}");
    }

    #[test]
    fn missing_command_is_unavailable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cases = [
            ("exit 127", true),
            (
                "echo 'fatal: Gerrit: git-lfs-authenticate: not found' >&2; exit 1",
                true,
            ),
            ("echo 'repository not found' >&2; exit 1", false),
        ];
        for (script, unavailable) in cases {
            let prog = tmp.path().join("fakessh");
            std::fs::write(&prog, script).unwrap();
            let client = SshAuthClient::new(SshProgram::Shell(format!("sh {}", prog.display())));
            let err = client
                .resolve("git@host", None, "/repo", SshOperation::Upload)
                .unwrap_err();
            assert_eq!(
                matches!(err, SshAuthError::Unavailable(_)),
                unavailable,
                "{script}: {err}"
            );
        }
    }

    #[test]
    fn variant_is_detected_from_the_program_name() {
        let cases = [
            (SshProgram::Exe("/usr/bin/ssh".into()), SshVariant::Ssh),
            (
                SshProgram::Exe("C:\\PuTTY\\PLINK.EXE".into()),
                SshVariant::Putty,
            ),
            (
                SshProgram::Shell("'/opt/my tools/TortoisePlink.exe' -v".into()),
                SshVariant::Tortoise,
            ),
            (SshProgram::Shell("plink -batch".into()), SshVariant::Putty),
            (SshProgram::Shell("simple-ssh".into()), SshVariant::Ssh),
        ];
        for (program, want) in cases {
            assert_eq!(program.detect_variant(), want, "{program:?}");
        }
    }

    #[test]
    fn configured_variants() {
        assert_eq!(SshVariant::from_config("auto"), None);
        assert_eq!(SshVariant::from_config("plink"), Some(SshVariant::Putty));
        assert_eq!(SshVariant::from_config("simple"), Some(SshVariant::Simple));
        assert_eq!(SshVariant::from_config("bogus"), Some(SshVariant::Ssh));
    }

    #[test]
    fn host_args_per_variant() {
        let host = "-oProxyCommand=x";
        assert_eq!(
            SshVariant::Ssh.host_args(Some("22"), host),
            ["-p", "22", "--", "-oProxyCommand=x"]
        );
        assert_eq!(
            SshVariant::Simple.host_args(None, host),
            ["oProxyCommand=x"]
        );
        assert_eq!(
            SshVariant::Putty.host_args(Some("22"), "git@h"),
            ["-P", "22", "git@h"]
        );
    }
}
