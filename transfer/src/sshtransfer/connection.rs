//! Persistent SSH subprocess that speaks the pure-SSH transfer
//! protocol.
//!
//! Each [`Connection`] owns one long-lived `ssh user@host
//! git-lfs-transfer <path> <op>` subprocess, with pkt-line framed
//! stdin/stdout. On spawn the connection performs the version
//! handshake (`version=1` capability → `version 1` → `status 200`);
//! [`Connection::end`] sends `quit`, drains the response, closes
//! the pipes, and waits for the child.
//!
//! Multiplexing follows upstream's `ssh/connection.go` pattern: the
//! first connection in a transfer session creates a control socket
//! (`-oControlMaster=yes -oControlPath=<sock>`); later connections
//! to the same endpoint reuse that socket
//! (`-oControlMaster=no -oControlPath=<sock>`) so each subsequent
//! SSH spawn skips the handshake. Disabled on Windows by default
//! (matches `lfs.ssh.automultiplex` upstream default) and when the
//! user picks a non-OpenSSH variant.
//!
//! Trace lines emitted at the `GIT_TRACE` level — `t-batch-transfer.sh`
//! greps them by name:
//! - `spawning pure SSH connection (#N)`
//! - `pure SSH connection successful (#N)` / `... unsuccessful (#N)`
//! - `terminating pure SSH connection (#N)`
//! - `exec: <prog> <args>` (mirrors `subprocess.ExecCommand`)

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use crate::sshtransfer::pktline::{Reader, TextPacket, Writer};

/// The two SSH-transfer operations. Mirrors
/// [`creds::SshOperation`](https://docs.rs/git-lfs-creds) so callers
/// that already pick a `SshOperation` upstack don't have to translate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    /// `git-lfs-transfer <path> upload` — push side.
    Upload,
    /// `git-lfs-transfer <path> download` — fetch side.
    Download,
}

impl Operation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Download => "download",
        }
    }
}

/// SSH client variant. Drives port-flag selection (`-p` vs `-P`),
/// the dash-leading user-and-host defense, and whether multiplexing
/// is supported.
///
/// Putty / TortoisePlink are accepted for argv shaping but multiplexing
/// is disabled for them (matching upstream).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Variant {
    /// OpenSSH `ssh`. Default. Supports `--` separator and
    /// `-oControlMaster` multiplexing.
    #[default]
    Default,
    /// "Simple" SSH (e.g. busybox `dropbear`). Doesn't support `--`,
    /// so leading-dash user-and-host is sanitized by stripping the
    /// dashes.
    Simple,
    /// PuTTY `plink`. Uses `-P` for port; no `--` support; no
    /// multiplexing.
    Putty,
    /// TortoisePlink. Same as PuTTY but prepends `-batch`.
    Tortoise,
}

impl Variant {
    fn port_flag(self) -> &'static str {
        match self {
            Self::Putty | Self::Tortoise => "-P",
            _ => "-p",
        }
    }

    fn supports_multiplex(self) -> bool {
        matches!(self, Self::Default)
    }

    fn supports_dash_separator(self) -> bool {
        matches!(self, Self::Default)
    }
}

/// Endpoint addressing for the SSH command.
#[derive(Debug, Clone)]
pub struct Metadata {
    /// `user@host` form (or just `host`). The bit that goes onto
    /// the ssh command line after any flags.
    pub user_and_host: String,
    /// Optional port; emitted as `-p N` (or `-P N` for PuTTY).
    pub port: Option<String>,
    /// Server-side path. Becomes the first positional argument to
    /// the remote `git-lfs-transfer` command.
    pub path: String,
}

/// Things that can go wrong while spawning or talking to the SSH
/// subprocess.
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    /// Failed to spawn or talk to the ssh subprocess.
    #[error("io error talking to ssh: {0}")]
    Io(#[from] io::Error),
    /// SSH protocol-level error — the subprocess started but the
    /// handshake or a subsequent command failed.
    #[error("ssh protocol error: {0}")]
    Protocol(String),
    /// SSH subprocess exited non-zero with the given stderr.
    #[error("ssh subprocess failed: {0}")]
    Failed(String),
}

/// Configuration for a single connection spawn. `program` is the
/// resolved SSH command (e.g. from `GIT_SSH_COMMAND` /
/// `GIT_SSH` / `ssh`); `variant` controls argv shaping; `multiplex`
/// chooses control-master role.
#[derive(Debug, Clone)]
pub struct Config {
    /// Connection sequence number for trace lines (`#0`, `#1`, …).
    pub id: u32,
    /// SSH executable. Split on whitespace — first token is the
    /// program, remainder are pre-arguments (matches the
    /// `GIT_SSH_COMMAND` shape that upstream parses with
    /// `tools.QuotedFields`).
    pub program: String,
    /// SSH client variant. See [`Variant`].
    pub variant: Variant,
    /// Endpoint addressing.
    pub metadata: Metadata,
    /// Transfer operation. Forwarded to `git-lfs-transfer` as the
    /// second positional argument.
    pub operation: Operation,
    /// Multiplexing role for this connection. See [`Multiplex`].
    pub multiplex: Multiplex,
    /// Remote command name. Defaults to `"git-lfs-transfer"` in
    /// [`Config::new`]; tests can swap in a stub.
    pub remote_command: String,
}

/// Role this connection plays in SSH multiplexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Multiplex {
    /// Not multiplexing — plain SSH connection. Used for non-OpenSSH
    /// variants and when the user disabled `lfs.ssh.automultiplex`.
    Disabled,
    /// First connection in the session; creates a new control
    /// socket at `path`. Subsequent connections share it.
    Master { path: PathBuf },
    /// Reuse an existing control socket created by a master.
    Client { path: PathBuf },
}

impl Config {
    /// Build a config with `remote_command = "git-lfs-transfer"`.
    pub fn new(
        id: u32,
        program: impl Into<String>,
        metadata: Metadata,
        operation: Operation,
    ) -> Self {
        Self {
            id,
            program: program.into(),
            variant: Variant::default(),
            metadata,
            operation,
            multiplex: Multiplex::Disabled,
            remote_command: "git-lfs-transfer".to_owned(),
        }
    }
}

/// One SSH subprocess speaking the pure-SSH transfer protocol.
///
/// Holds the [`Child`] handle plus pkt-line framed `Reader` /
/// `Writer` over its stdio. Drop semantics: the inner pipes are
/// closed when the struct drops, which signals the remote
/// `git-lfs-transfer` process to exit; callers should normally
/// invoke [`Connection::end`] instead so the protocol's `quit`
/// handshake completes cleanly.
pub struct Connection {
    id: u32,
    child: Child,
    reader: Reader<ChildStdout>,
    writer: Writer<ChildStdin>,
}

impl Connection {
    /// Spawn the SSH subprocess and complete the version handshake.
    ///
    /// Emits the `exec:` and `spawning pure SSH connection (#N)`
    /// trace lines before the spawn, and `pure SSH connection
    /// successful (#N)` / `... unsuccessful (#N)` after the
    /// handshake outcome.
    pub fn spawn(config: &Config) -> Result<Self, ConnectionError> {
        let argv = build_argv(config);
        let (prog, args) = argv.split_first().expect("argv is non-empty");

        trace(format_args!(
            "spawning pure SSH connection (#{id})",
            id = config.id
        ));
        trace_exec(prog, args);

        let mut cmd = Command::new(prog);
        cmd.args(args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let stdin = child.stdin.take().expect("stdin was piped");

        let mut conn = Self {
            id: config.id,
            child,
            reader: Reader::new(stdout),
            writer: Writer::new(stdin),
        };
        match conn.negotiate_version() {
            Ok(()) => {
                trace(format_args!(
                    "pure SSH connection successful (#{id})",
                    id = config.id
                ));
                Ok(conn)
            }
            Err(e) => {
                trace(format_args!(
                    "pure SSH connection unsuccessful (#{id})",
                    id = config.id
                ));
                Err(e)
            }
        }
    }

    /// Sequence number assigned at spawn time (used in trace lines).
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Borrow the pkt-line writer. Callers that need to issue raw
    /// commands (e.g. the protocol layer above) drive the connection
    /// through this.
    pub fn writer(&mut self) -> &mut Writer<ChildStdin> {
        &mut self.writer
    }

    /// Borrow the pkt-line reader.
    pub fn reader(&mut self) -> &mut Reader<ChildStdout> {
        &mut self.reader
    }

    /// Send `quit`, read the success response, and tear down the
    /// subprocess.
    ///
    /// Emits `terminating pure SSH connection (#N)` before sending
    /// the quit packet. Errors during the protocol exchange are
    /// surfaced, but the subprocess is always reaped before
    /// returning so we don't leave zombies.
    pub fn end(mut self) -> Result<(), ConnectionError> {
        trace(format_args!(
            "terminating pure SSH connection (#{id})",
            id = self.id
        ));
        let proto_result = (|| {
            self.send_command("quit", &[])?;
            let (status, _args) = self.read_status()?;
            if status != 200 {
                return Err(ConnectionError::Protocol(format!(
                    "quit returned status {status}"
                )));
            }
            Ok::<_, ConnectionError>(())
        })();

        // Drop pipes so the child notices EOF, then wait for it.
        drop(self.writer);
        drop(self.reader);
        let wait_result = self.child.wait();

        proto_result?;
        wait_result?;
        Ok(())
    }

    fn negotiate_version(&mut self) -> Result<(), ConnectionError> {
        let caps = self.read_packet_list()?;
        if !caps.iter().any(|c| c == "version=1") {
            return Err(ConnectionError::Protocol(
                "remote did not advertise version=1".into(),
            ));
        }
        self.send_command("version 1", &[])?;
        let (status, args, _lines) = self.read_status_with_lines()?;
        if status != 200 {
            let detail = args
                .first()
                .map(|a| format!("server said: {a:?}"))
                .unwrap_or_else(|| "no error provided".into());
            return Err(ConnectionError::Protocol(format!(
                "version negotiation returned status {status}; {detail}"
            )));
        }
        Ok(())
    }

    /// Send a command + optional arguments terminated by a flush
    /// packet (no delim, no data).
    pub fn send_command(&mut self, command: &str, args: &[&str]) -> io::Result<()> {
        self.writer.write_text(command)?;
        for arg in args {
            self.writer.write_text(arg)?;
        }
        self.writer.write_flush()?;
        self.writer.get_mut().flush()
    }

    /// Read packets until the next flush, returning them as text
    /// strings. Used for capability advertisements (server hello).
    pub fn read_packet_list(&mut self) -> Result<Vec<String>, ConnectionError> {
        let mut out = Vec::new();
        loop {
            match self.reader.read_text()? {
                TextPacket::Flush => return Ok(out),
                TextPacket::Delim => {
                    return Err(ConnectionError::Protocol(
                        "unexpected delim in packet list".into(),
                    ));
                }
                TextPacket::Text(s) => out.push(s),
            }
        }
    }

    /// Read a `status <code>` line followed by a flush. Used after
    /// commands that return no other data (`version`, `quit`,
    /// `verify-object` success). Returns the parsed code plus any
    /// intermediate args.
    pub fn read_status(&mut self) -> Result<(u16, Vec<String>), ConnectionError> {
        let mut status: Option<u16> = None;
        let mut args = Vec::new();
        loop {
            match self.reader.read_text()? {
                TextPacket::Flush => {
                    return status
                        .ok_or_else(|| ConnectionError::Protocol("no status received".into()))
                        .map(|s| (s, args));
                }
                TextPacket::Delim => {
                    return Err(ConnectionError::Protocol("unexpected delim".into()));
                }
                TextPacket::Text(s) => {
                    if status.is_none() {
                        status = Some(parse_status(&s)?);
                    } else {
                        args.push(s);
                    }
                }
            }
        }
    }

    /// Read `status <code>`, optional args, delim, optional
    /// follow-up text lines, flush. Used for `version`, `batch`
    /// (error path), and `list-lock`.
    pub fn read_status_with_lines(
        &mut self,
    ) -> Result<(u16, Vec<String>, Vec<String>), ConnectionError> {
        let mut status: Option<u16> = None;
        let mut args = Vec::new();
        let mut lines = Vec::new();
        let mut seen_delim = false;
        loop {
            match self.reader.read_text()? {
                TextPacket::Flush => {
                    return status
                        .ok_or_else(|| ConnectionError::Protocol("no status received".into()))
                        .map(|s| (s, args, lines));
                }
                TextPacket::Delim => {
                    if seen_delim {
                        return Err(ConnectionError::Protocol("duplicate delim".into()));
                    }
                    seen_delim = true;
                }
                TextPacket::Text(s) => {
                    if seen_delim {
                        lines.push(s);
                    } else if status.is_none() {
                        status = Some(parse_status(&s)?);
                    } else {
                        args.push(s);
                    }
                }
            }
        }
    }
}

fn parse_status(line: &str) -> Result<u16, ConnectionError> {
    let rest = line
        .strip_prefix("status ")
        .ok_or_else(|| ConnectionError::Protocol(format!("expected status line, got {line:?}")))?;
    rest.parse::<u16>()
        .map_err(|_| ConnectionError::Protocol(format!("malformed status code in {line:?}")))
}

/// Assemble the argv for the `ssh ... git-lfs-transfer <path>
/// <op>` invocation. Mirrors upstream's `GetLFSExeAndArgs` in
/// `ssh/ssh.go`.
fn build_argv(config: &Config) -> Vec<String> {
    let mut parts = config.program.split_whitespace();
    let prog = parts.next().unwrap_or("ssh").to_owned();
    let mut argv: Vec<String> = std::iter::once(prog)
        .chain(parts.map(str::to_owned))
        .collect();

    if config.variant == Variant::Tortoise {
        argv.push("-batch".to_owned());
    }

    if config.variant.supports_multiplex() {
        match &config.multiplex {
            Multiplex::Disabled => {}
            Multiplex::Master { path } => {
                argv.push("-oControlMaster=yes".to_owned());
                argv.push(format!("-oControlPath={}", path.display()));
            }
            Multiplex::Client { path } => {
                argv.push("-oControlMaster=no".to_owned());
                argv.push(format!("-oControlPath={}", path.display()));
            }
        }
    }

    if let Some(port) = &config.metadata.port {
        argv.push(config.variant.port_flag().to_owned());
        argv.push(port.clone());
    }

    // Defense against `ssh://-oProxyCommand=...` style URLs that
    // would otherwise inject SSH options via the user-and-host
    // slot. Default variant uses `--` to mark end-of-options;
    // simple/putty/tortoise don't support that, so we strip the
    // leading dashes instead — matches upstream's
    // `sshOptPrefixRE.ReplaceAllString` in `ssh/ssh.go`.
    let user_and_host = config.metadata.user_and_host.as_str();
    if user_and_host.starts_with('-') {
        if config.variant.supports_dash_separator() {
            argv.push("--".to_owned());
            argv.push(user_and_host.to_owned());
        } else {
            argv.push(user_and_host.trim_start_matches('-').to_owned());
        }
    } else {
        argv.push(user_and_host.to_owned());
    }

    argv.push(format!(
        "{cmd} {path} {op}",
        cmd = config.remote_command,
        path = config.metadata.path,
        op = config.operation.as_str(),
    ));

    argv
}

fn trace_enabled() -> bool {
    std::env::var_os("GIT_TRACE").is_some_and(|v| !v.is_empty() && v != "0")
}

fn trace(args: std::fmt::Arguments) {
    if !trace_enabled() {
        return;
    }
    let mut e = std::io::stderr().lock();
    let _ = writeln!(e, "{args}");
}

fn trace_exec(prog: &str, args: &[String]) {
    if !trace_enabled() {
        return;
    }
    let mut e = std::io::stderr().lock();
    let _ = write!(e, "exec: {prog}");
    for a in args {
        let _ = write!(e, " {a}");
    }
    let _ = writeln!(e);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(user_and_host: &str) -> Metadata {
        Metadata {
            user_and_host: user_and_host.to_owned(),
            port: None,
            path: "/repo".to_owned(),
        }
    }

    #[test]
    fn argv_default_variant_simple_host() {
        let c = Config::new(0, "ssh", meta("git@host"), Operation::Upload);
        let argv = build_argv(&c);
        assert_eq!(
            argv,
            vec![
                "ssh".to_owned(),
                "git@host".to_owned(),
                "git-lfs-transfer /repo upload".to_owned(),
            ],
        );
    }

    #[test]
    fn argv_with_port() {
        let mut m = meta("git@host");
        m.port = Some("2222".to_owned());
        let c = Config::new(0, "ssh", m, Operation::Download);
        let argv = build_argv(&c);
        assert_eq!(
            argv,
            vec![
                "ssh".to_owned(),
                "-p".to_owned(),
                "2222".to_owned(),
                "git@host".to_owned(),
                "git-lfs-transfer /repo download".to_owned(),
            ],
        );
    }

    #[test]
    fn argv_putty_uses_uppercase_p() {
        let mut m = meta("git@host");
        m.port = Some("2222".to_owned());
        let mut c = Config::new(0, "plink", m, Operation::Download);
        c.variant = Variant::Putty;
        let argv = build_argv(&c);
        assert!(argv.contains(&"-P".to_owned()));
        assert!(!argv.contains(&"-p".to_owned()));
    }

    #[test]
    fn argv_tortoise_prepends_batch() {
        let mut c = Config::new(0, "tortoiseplink", meta("git@host"), Operation::Upload);
        c.variant = Variant::Tortoise;
        let argv = build_argv(&c);
        assert_eq!(argv[1], "-batch");
    }

    #[test]
    fn argv_multiplex_master() {
        let mut c = Config::new(0, "ssh", meta("git@host"), Operation::Upload);
        c.multiplex = Multiplex::Master {
            path: PathBuf::from("/tmp/sock"),
        };
        let argv = build_argv(&c);
        assert!(argv.contains(&"-oControlMaster=yes".to_owned()));
        assert!(argv.contains(&"-oControlPath=/tmp/sock".to_owned()));
    }

    #[test]
    fn argv_multiplex_client() {
        let mut c = Config::new(1, "ssh", meta("git@host"), Operation::Download);
        c.multiplex = Multiplex::Client {
            path: PathBuf::from("/tmp/sock"),
        };
        let argv = build_argv(&c);
        assert!(argv.contains(&"-oControlMaster=no".to_owned()));
        assert!(argv.contains(&"-oControlPath=/tmp/sock".to_owned()));
    }

    #[test]
    fn argv_multiplex_ignored_for_non_default_variant() {
        // PuTTY / Tortoise don't speak OpenSSH multiplexing, so the
        // -oControl* options would error out. Builder drops them.
        let mut c = Config::new(0, "plink", meta("git@host"), Operation::Upload);
        c.variant = Variant::Putty;
        c.multiplex = Multiplex::Master {
            path: PathBuf::from("/tmp/sock"),
        };
        let argv = build_argv(&c);
        assert!(!argv.iter().any(|a| a.starts_with("-oControl")));
    }

    #[test]
    fn argv_dash_leading_host_uses_separator_in_default() {
        let c = Config::new(0, "ssh", meta("-oProxyCommand=evil"), Operation::Upload);
        let argv = build_argv(&c);
        let host_pos = argv
            .iter()
            .position(|a| a == "-oProxyCommand=evil")
            .unwrap();
        // The `--` separator immediately precedes the dash-leading host.
        assert_eq!(argv[host_pos - 1], "--");
    }

    #[test]
    fn argv_dash_leading_host_stripped_for_simple() {
        let mut c = Config::new(
            0,
            "dropbear",
            meta("-oProxyCommand=evil"),
            Operation::Upload,
        );
        c.variant = Variant::Simple;
        let argv = build_argv(&c);
        // Dashes stripped; no `--` separator added.
        assert!(argv.iter().any(|a| a == "oProxyCommand=evil"));
        assert!(!argv.iter().any(|a| a == "--"));
        assert!(!argv.iter().any(|a| a == "-oProxyCommand=evil"));
    }

    #[test]
    fn argv_program_split_passes_extra_args() {
        // `GIT_SSH_COMMAND="ssh -i mykey -v"` style — split on
        // whitespace, first token is the program, rest are
        // pre-arguments.
        let c = Config::new(0, "ssh -i mykey -v", meta("git@host"), Operation::Upload);
        let argv = build_argv(&c);
        assert_eq!(argv[0], "ssh");
        assert!(argv.iter().any(|a| a == "-i"));
        assert!(argv.iter().any(|a| a == "mykey"));
        assert!(argv.iter().any(|a| a == "-v"));
    }

    #[test]
    fn parse_status_accepts_valid_line() {
        assert_eq!(parse_status("status 200").unwrap(), 200);
        assert_eq!(parse_status("status 404").unwrap(), 404);
    }

    #[test]
    fn parse_status_rejects_non_status_prefix() {
        assert!(matches!(
            parse_status("hello"),
            Err(ConnectionError::Protocol(_))
        ));
    }

    #[test]
    fn parse_status_rejects_non_numeric_code() {
        assert!(matches!(
            parse_status("status abc"),
            Err(ConnectionError::Protocol(_))
        ));
    }

    /// End-to-end test against a tiny "remote" implemented as a
    /// shell script: writes its capability advertisement, reads the
    /// version command + flush, writes `status 200`. Verifies the
    /// version handshake works against real subprocess stdio.
    #[test]
    #[cfg(unix)]
    fn handshake_against_stub_server() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let script = tmp.path().join("fakessh");
        // Write the capability advertisement, then echo input back
        // until we see "version 1" + flush, then ack with status 200.
        // Wire dimensions:
        //   client → server "version 1": 0004+("version 1\n"=10) = 14 bytes, +flush(4) = 18
        //   client → server "quit":      0004+("quit\n"=5)        = 9 bytes,  +flush(4) = 13
        //   server → client "version=1" cap + flush: 14 + 4 = 18
        //   server → client status 200 + delim + flush: 15 + 4 + 4 = 23
        //   server → client status 200 + flush: 15 + 4 = 19
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             # 1. capability advertisement + flush\n\
             printf '000eversion=1\\n0000'\n\
             # 2. drain client's `version 1` + flush (18 bytes)\n\
             dd bs=1 count=18 of=/dev/null 2>/dev/null\n\
             # 3. version response: status 200 + delim + flush\n\
             printf '000fstatus 200\\n00010000'\n\
             # 4. drain client's `quit` + flush (13 bytes)\n\
             dd bs=1 count=13 of=/dev/null 2>/dev/null\n\
             # 5. quit response: status 200 + flush\n\
             printf '000fstatus 200\\n0000'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let config = Config::new(
            0,
            script.to_string_lossy().into_owned(),
            meta("git@host"),
            Operation::Upload,
        );
        let conn = Connection::spawn(&config).expect("handshake should succeed");
        conn.end().expect("quit should succeed");
    }

    #[test]
    #[cfg(unix)]
    fn handshake_fails_when_capability_missing() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let script = tmp.path().join("fakessh");
        // Capability advertisement with a different version.
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             printf '000eversion=2\\n0000'\n\
             sleep 1\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();

        let config = Config::new(
            0,
            script.to_string_lossy().into_owned(),
            meta("git@host"),
            Operation::Upload,
        );
        match Connection::spawn(&config) {
            Err(ConnectionError::Protocol(msg)) => {
                assert!(msg.contains("version=1"), "got: {msg}");
            }
            Err(other) => panic!("unexpected error: {other:?}"),
            Ok(_) => panic!("expected handshake to fail"),
        }
    }
}
