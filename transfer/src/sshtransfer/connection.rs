//! One SSH subprocess speaking the pure-SSH transfer protocol.
//!
//! A [`Connection`] runs `ssh [options] <user@host> "git-lfs-transfer
//! <path> <operation>"` and speaks pkt-line over its stdin and stdout.
//! [`Connection::spawn`] does the version handshake, and
//! [`Connection::end`] sends `quit`.
//!
//! With OpenSSH, connections can share one TCP connection. The first one
//! is started with `-oControlMaster=yes -oControlPath=<socket>` and later
//! ones with `-oControlMaster=no` and the same path, so they skip the SSH
//! handshake. The caller picks the role with [`Multiplex`].
//!
//! When `GIT_TRACE` is set, these lines go to stderr. The upstream shell
//! tests match on them:
//!
//! - `spawning pure SSH connection (#N)`
//! - `exec: <program> <args>`
//! - `pure SSH connection successful (#N)` or
//!   `pure SSH connection unsuccessful (#N)`
//! - `terminating pure SSH connection (#N)`

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use git_lfs_creds::{SshProgram, exec_trace, shell_quote};

use crate::sshtransfer::pktline::{self, Packet, Reader, TextPacket, Writer};

/// The operation a connection is opened for, passed to the remote
/// `git-lfs-transfer` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    /// `upload`, for pushing objects.
    Upload,
    /// `download`, for fetching objects.
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

impl From<git_lfs_api::Operation> for Operation {
    fn from(op: git_lfs_api::Operation) -> Self {
        match op {
            git_lfs_api::Operation::Upload => Self::Upload,
            git_lfs_api::Operation::Download => Self::Download,
        }
    }
}

/// SSH client variant, which decides how the command line is built.
///
/// Only [`Variant::Default`] supports multiplexing and the `--`
/// separator. The others ignore [`Multiplex`] and strip leading dashes
/// from the host instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Variant {
    /// OpenSSH `ssh`.
    #[default]
    Default,
    /// Git's `simple` SSH variant, for clients that take no options
    /// beyond `-p`.
    Simple,
    /// PuTTY `plink`, which takes `-P` for the port.
    Putty,
    /// TortoisePlink. Like PuTTY, plus `-batch`.
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
    /// `user@host`, or just `host`.
    pub user_and_host: String,
    /// Port, passed as `-p` (or `-P` for PuTTY and TortoisePlink).
    pub port: Option<String>,
    /// Repository path on the server.
    pub path: String,
}

/// Things that can go wrong while spawning or talking to the SSH
/// subprocess.
#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    /// Failed to spawn or talk to the ssh subprocess.
    #[error("io error talking to ssh: {0}")]
    Io(#[from] io::Error),
    /// The server sent something the protocol doesn't allow, or the
    /// handshake failed.
    #[error("ssh protocol error: {0}")]
    Protocol(String),
    /// SSH subprocess exited non-zero with the given stderr.
    #[error("ssh subprocess failed: {0}")]
    Failed(String),
}

/// How to spawn one [`Connection`].
#[derive(Debug, Clone)]
pub struct Config {
    /// Connection number shown in trace lines as `#N`.
    pub id: u32,
    /// The SSH program.
    pub program: SshProgram,
    /// SSH client variant.
    pub variant: Variant,
    /// Where to connect.
    pub metadata: Metadata,
    /// Operation passed to the remote command.
    pub operation: Operation,
    /// Multiplexing role.
    pub multiplex: Multiplex,
    /// Command run on the server. [`Config::new`] sets
    /// `git-lfs-transfer`.
    pub remote_command: String,
}

/// Role this connection plays in SSH multiplexing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Multiplex {
    /// A plain SSH connection.
    Disabled,
    /// Create the control socket at `path` for later connections.
    Master { path: PathBuf },
    /// Connect through the control socket at `path`.
    Client { path: PathBuf },
}

impl Config {
    /// Build a config for the default variant, without multiplexing.
    pub fn new(id: u32, program: SshProgram, metadata: Metadata, operation: Operation) -> Self {
        Self {
            id,
            program,
            variant: Variant::default(),
            metadata,
            operation,
            multiplex: Multiplex::Disabled,
            remote_command: "git-lfs-transfer".to_owned(),
        }
    }
}

/// Request and response messages of the protocol, over any reader and
/// writer pair.
///
/// [`Connection`] runs it over the SSH subprocess's stdout and stdin.
/// Each `send_*` method writes one whole request and flushes the writer.
pub struct ProtoStream<R: Read, W: Write> {
    reader: Reader<R>,
    writer: Writer<W>,
}

impl<R: Read, W: Write> ProtoStream<R, W> {
    /// Wrap a reader for responses and a writer for requests.
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: Reader::new(reader),
            writer: Writer::new(writer),
        }
    }

    /// Send a command and its arguments, then a flush packet.
    pub fn send_command(&mut self, command: &str, args: &[&str]) -> io::Result<()> {
        self.writer.write_text(command)?;
        for arg in args {
            self.writer.write_text(arg)?;
        }
        self.writer.write_flush()?;
        self.writer.get_mut().flush()
    }

    /// Send a command and its arguments, a delim packet, text lines, and
    /// a flush packet.
    ///
    /// This is the shape of a `batch` request, whose `<oid> <size>`
    /// lines follow the delim.
    pub fn send_command_with_lines(
        &mut self,
        command: &str,
        args: &[&str],
        lines: &[&str],
    ) -> io::Result<()> {
        self.writer.write_text(command)?;
        for arg in args {
            self.writer.write_text(arg)?;
        }
        self.writer.write_delim()?;
        for line in lines {
            self.writer.write_text(line)?;
        }
        self.writer.write_flush()?;
        self.writer.get_mut().flush()
    }

    /// Send a command and its arguments, a delim packet, everything
    /// read from `data`, and a flush packet.
    ///
    /// This is the shape of a `put-object` request. `data` is read until
    /// EOF into packets of at most [`pktline::MAX_DATA`] bytes. An empty
    /// `data` sends the delim and flush with nothing between.
    ///
    /// If reading `data` fails, the request is left half sent.
    pub fn send_command_with_data<D: Read>(
        &mut self,
        command: &str,
        args: &[&str],
        data: &mut D,
    ) -> io::Result<()> {
        self.writer.write_text(command)?;
        for arg in args {
            self.writer.write_text(arg)?;
        }
        self.writer.write_delim()?;
        let mut buf = vec![0u8; pktline::MAX_DATA];
        loop {
            let n = data.read(&mut buf)?;
            if n == 0 {
                break;
            }
            self.writer.write_data(&buf[..n])?;
        }
        self.writer.write_flush()?;
        self.writer.get_mut().flush()
    }

    /// Read text packets up to a flush, such as the server's capability
    /// advertisement.
    ///
    /// A delim packet is an error.
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

    /// Read a response made of `status <code>`, arguments, and a flush.
    ///
    /// Returns the code and the arguments. This is the response to
    /// `quit`. A delim packet is an error.
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

    /// Read `status <code>` and arguments up to a delim packet.
    ///
    /// Returns the code and the arguments, leaving the rest of the
    /// response for the caller. A `get-object` reply continues with
    /// [`copy_data_until_flush`](Self::copy_data_until_flush) on success
    /// and [`read_text_lines_until_flush`](Self::read_text_lines_until_flush)
    /// on error. A flush before the delim is an error.
    pub fn read_status_until_delim(&mut self) -> Result<(u16, Vec<String>), ConnectionError> {
        let mut status: Option<u16> = None;
        let mut args = Vec::new();
        loop {
            match self.reader.read_text()? {
                TextPacket::Delim => {
                    return status
                        .ok_or_else(|| ConnectionError::Protocol("no status received".into()))
                        .map(|s| (s, args));
                }
                TextPacket::Flush => {
                    return Err(ConnectionError::Protocol(
                        "unexpected flush before delim".into(),
                    ));
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

    /// Copy data packets to `writer` up to a flush, returning the number
    /// of bytes copied.
    ///
    /// A delim packet is an error.
    pub fn copy_data_until_flush<O: Write>(
        &mut self,
        writer: &mut O,
    ) -> Result<u64, ConnectionError> {
        let mut total: u64 = 0;
        loop {
            match self.reader.read_packet()? {
                Packet::Flush => return Ok(total),
                Packet::Delim => {
                    return Err(ConnectionError::Protocol(
                        "unexpected delim mid-data".into(),
                    ));
                }
                Packet::Data(bytes) => {
                    writer.write_all(&bytes)?;
                    total += bytes.len() as u64;
                }
            }
        }
    }

    /// Read text packets up to a flush.
    ///
    /// A delim packet is an error.
    pub fn read_text_lines_until_flush(&mut self) -> Result<Vec<String>, ConnectionError> {
        let mut lines = Vec::new();
        loop {
            match self.reader.read_text()? {
                TextPacket::Flush => return Ok(lines),
                TextPacket::Delim => {
                    return Err(ConnectionError::Protocol(
                        "unexpected delim in text-lines section".into(),
                    ));
                }
                TextPacket::Text(s) => lines.push(s),
            }
        }
    }

    /// Read a response made of `status <code>`, arguments, an optional
    /// delim packet, text lines, and a flush.
    ///
    /// Returns the code, the arguments, and the lines after the delim.
    /// Most responses have this shape, including those to `version`,
    /// `batch`, `put-object`, `verify-object`, and the lock commands.
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

/// One SSH subprocess speaking the pure-SSH transfer protocol.
///
/// Dropping a connection kills the subprocess if it is still running and
/// waits for it. Use [`Connection::end`] to send `quit` first.
///
/// The subprocess's stderr is piped but never read.
pub struct Connection {
    id: u32,
    child: Child,
    stream: ProtoStream<ChildStdout, ChildStdin>,
}

impl Connection {
    /// Spawn the SSH subprocess and complete the version handshake.
    ///
    /// On failure the subprocess is killed.
    pub fn spawn(config: &Config) -> Result<Self, ConnectionError> {
        let (prog, args) = config.program.command_line(build_argv(config));

        trace(format_args!(
            "spawning pure SSH connection (#{id})",
            id = config.id
        ));
        trace_exec(&prog, &args);

        let mut cmd = Command::new(&prog);
        cmd.args(&args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("stdout was piped");
        let stdin = child.stdin.take().expect("stdin was piped");

        let mut conn = Self {
            id: config.id,
            child,
            stream: ProtoStream::new(stdout, stdin),
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

    /// The connection number from [`Config::id`].
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Borrow the protocol stream to send commands and read responses.
    pub fn stream(&mut self) -> &mut ProtoStream<ChildStdout, ChildStdin> {
        &mut self.stream
    }

    /// Send `quit`, check for `status 200`, and end the subprocess.
    ///
    /// Whether or not the exchange succeeds, the subprocess is then
    /// killed if still running, and reaped.
    pub fn end(mut self) -> Result<(), ConnectionError> {
        trace(format_args!(
            "terminating pure SSH connection (#{id})",
            id = self.id
        ));
        self.stream.send_command("quit", &[])?;
        let (status, _args) = self.stream.read_status()?;
        if status != 200 {
            return Err(ConnectionError::Protocol(format!(
                "quit returned status {status}"
            )));
        }
        // Dropping `self` reaps the child, killing it if it is still
        // running.
        Ok(())
    }
}

impl Drop for Connection {
    /// Kill the subprocess if it is still running, and reap it.
    ///
    /// [`Child`] does neither on drop, which would leave a multiplexing
    /// master running. This emits no trace line: the upstream tests count
    /// `terminating pure SSH connection` lines, and expect none for
    /// connections that are dropped without [`end`](Connection::end),
    /// such as the one used for locks.
    fn drop(&mut self) {
        // `try_wait` reaps a child that has already exited.
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Connection {
    fn negotiate_version(&mut self) -> Result<(), ConnectionError> {
        let caps = self.stream.read_packet_list()?;
        if !caps.iter().any(|c| c == "version=1") {
            return Err(ConnectionError::Protocol(
                "remote did not advertise version=1".into(),
            ));
        }
        self.stream.send_command("version 1", &[])?;
        let (status, args, _lines) = self.stream.read_status_with_lines()?;
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
}

fn parse_status(line: &str) -> Result<u16, ConnectionError> {
    let rest = line
        .strip_prefix("status ")
        .ok_or_else(|| ConnectionError::Protocol(format!("expected status line, got {line:?}")))?;
    rest.parse::<u16>()
        .map_err(|_| ConnectionError::Protocol(format!("malformed status code in {line:?}")))
}

/// Build the arguments to the SSH program, following upstream's
/// `GetLFSExeAndArgs`.
fn build_argv(config: &Config) -> Vec<String> {
    let mut argv = Vec::new();

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

    // A host like `-oProxyCommand=...` from an `ssh://` URL would be
    // read as an option. OpenSSH takes `--` to end the options. The
    // other variants don't, so the leading dashes are stripped, as
    // upstream does.
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

    // ssh hands this to the server's shell, so quote each word.
    argv.push(format!(
        "{} {} {}",
        shell_quote(&config.remote_command),
        shell_quote(&config.metadata.path),
        shell_quote(config.operation.as_str()),
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
    let _ = writeln!(std::io::stderr().lock(), "{}", exec_trace(prog, args));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe(program: &str) -> SshProgram {
        SshProgram::Exe(program.to_owned())
    }

    fn meta(user_and_host: &str) -> Metadata {
        Metadata {
            user_and_host: user_and_host.to_owned(),
            port: None,
            path: "/repo".to_owned(),
        }
    }

    #[test]
    fn argv_default_variant_simple_host() {
        let c = Config::new(0, exe("ssh"), meta("git@host"), Operation::Upload);
        let argv = build_argv(&c);
        assert_eq!(
            argv,
            vec![
                "git@host".to_owned(),
                "git-lfs-transfer /repo upload".to_owned(),
            ],
        );
    }

    #[test]
    fn argv_with_port() {
        let mut m = meta("git@host");
        m.port = Some("2222".to_owned());
        let c = Config::new(0, exe("ssh"), m, Operation::Download);
        let argv = build_argv(&c);
        assert_eq!(
            argv,
            vec![
                "-p".to_owned(),
                "2222".to_owned(),
                "git@host".to_owned(),
                "git-lfs-transfer /repo download".to_owned(),
            ],
        );
    }

    #[test]
    fn argv_quotes_the_path_for_the_remote_shell() {
        let mut m = meta("git@host");
        m.path = "/my repo;touch x".to_owned();
        let c = Config::new(0, exe("ssh"), m, Operation::Upload);
        let argv = build_argv(&c);
        assert_eq!(
            argv.last().unwrap(),
            "git-lfs-transfer '/my repo;touch x' upload"
        );
    }

    #[test]
    fn argv_putty_uses_uppercase_p() {
        let mut m = meta("git@host");
        m.port = Some("2222".to_owned());
        let mut c = Config::new(0, exe("plink"), m, Operation::Download);
        c.variant = Variant::Putty;
        let argv = build_argv(&c);
        assert!(argv.contains(&"-P".to_owned()));
        assert!(!argv.contains(&"-p".to_owned()));
    }

    #[test]
    fn argv_tortoise_prepends_batch() {
        let mut c = Config::new(0, exe("tortoiseplink"), meta("git@host"), Operation::Upload);
        c.variant = Variant::Tortoise;
        let argv = build_argv(&c);
        assert_eq!(argv[0], "-batch");
    }

    #[test]
    fn argv_multiplex_master() {
        let mut c = Config::new(0, exe("ssh"), meta("git@host"), Operation::Upload);
        c.multiplex = Multiplex::Master {
            path: PathBuf::from("/tmp/sock"),
        };
        let argv = build_argv(&c);
        assert!(argv.contains(&"-oControlMaster=yes".to_owned()));
        assert!(argv.contains(&"-oControlPath=/tmp/sock".to_owned()));
    }

    #[test]
    fn argv_multiplex_client() {
        let mut c = Config::new(1, exe("ssh"), meta("git@host"), Operation::Download);
        c.multiplex = Multiplex::Client {
            path: PathBuf::from("/tmp/sock"),
        };
        let argv = build_argv(&c);
        assert!(argv.contains(&"-oControlMaster=no".to_owned()));
        assert!(argv.contains(&"-oControlPath=/tmp/sock".to_owned()));
    }

    #[test]
    fn argv_multiplex_ignored_for_non_default_variant() {
        // PuTTY and TortoisePlink don't speak OpenSSH multiplexing, so the
        // -oControl* options would error out. Builder drops them.
        let mut c = Config::new(0, exe("plink"), meta("git@host"), Operation::Upload);
        c.variant = Variant::Putty;
        c.multiplex = Multiplex::Master {
            path: PathBuf::from("/tmp/sock"),
        };
        let argv = build_argv(&c);
        assert!(!argv.iter().any(|a| a.starts_with("-oControl")));
    }

    #[test]
    fn argv_dash_leading_host_uses_separator_in_default() {
        let c = Config::new(
            0,
            exe("ssh"),
            meta("-oProxyCommand=evil"),
            Operation::Upload,
        );
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
            exe("dropbear"),
            meta("-oProxyCommand=evil"),
            Operation::Upload,
        );
        c.variant = Variant::Simple;
        let argv = build_argv(&c);
        // Dashes stripped, and no `--` separator added.
        assert!(argv.iter().any(|a| a == "oProxyCommand=evil"));
        assert!(!argv.iter().any(|a| a == "--"));
        assert!(!argv.iter().any(|a| a == "-oProxyCommand=evil"));
    }

    #[test]
    fn shell_program_runs_with_quoted_arguments() {
        let program = SshProgram::Shell("ssh -i 'my key'".to_owned());
        let c = Config::new(0, program, meta("git@host"), Operation::Upload);
        let (prog, args) = c.program.command_line(build_argv(&c));
        assert_eq!(prog, "sh");
        assert_eq!(
            args,
            vec![
                "-c".to_owned(),
                "ssh -i 'my key' git@host 'git-lfs-transfer /repo upload'".to_owned(),
            ],
        );
    }

    #[test]
    fn exe_program_is_run_directly() {
        let c = Config::new(
            0,
            exe("/opt/my tools/ssh"),
            meta("git@host"),
            Operation::Upload,
        );
        let (prog, args) = c.program.command_line(build_argv(&c));
        assert_eq!(prog, "/opt/my tools/ssh");
        assert_eq!(args[0], "git@host");
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

    // ---- ProtoStream primitive tests ----
    //
    // These exercise the pkt-line framed read/write primitives
    // without spawning a subprocess. Each builds a fresh stream
    // over a `Cursor<Vec<u8>>` (canned server response) plus a
    // `Vec<u8>` (capture of what the client would send) so we can
    // assert both the parsed result and the wire shape.

    use std::io::Cursor;

    type TestStream = ProtoStream<Cursor<Vec<u8>>, Vec<u8>>;

    fn stream(reader_bytes: &[u8]) -> TestStream {
        ProtoStream::new(Cursor::new(reader_bytes.to_vec()), Vec::new())
    }

    fn into_writer_bytes(stream: TestStream) -> Vec<u8> {
        // Reach into the writer and steal its captured Vec. Avoids
        // touching pktline::Writer's API surface for a test helper.
        let mut writer = stream.writer;
        std::mem::take(writer.get_mut())
    }

    #[test]
    fn send_command_emits_command_and_flush() {
        let mut s = stream(&[]);
        s.send_command("ping", &[]).unwrap();
        let bytes = into_writer_bytes(s);
        // "ping\n" is 5 bytes, so the header is 0009. Then a flush.
        assert_eq!(bytes, b"0009ping\n0000");
    }

    #[test]
    fn send_command_emits_args_between_command_and_flush() {
        let mut s = stream(&[]);
        s.send_command("batch", &["transfer=ssh", "hash-algo=sha256"])
            .unwrap();
        let bytes = into_writer_bytes(s);
        // "batch\n" is 6 bytes, so header 000a.
        // "transfer=ssh\n" is 13 bytes, so header 0011.
        // "hash-algo=sha256\n" is 17 bytes, so header 0015.
        // Then a flush.
        assert_eq!(
            bytes,
            b"000abatch\n0011transfer=ssh\n0015hash-algo=sha256\n0000",
        );
    }

    #[test]
    fn send_command_with_lines_emits_delim_separator() {
        let mut s = stream(&[]);
        s.send_command_with_lines("batch", &["transfer=ssh"], &["abc123 100", "def456 200"])
            .unwrap();
        let bytes = into_writer_bytes(s);
        // batch\n(10), transfer=ssh\n(17), 0001, abc123 100\n(15),
        // def456 200\n(15), 0000
        assert_eq!(
            bytes,
            b"000abatch\n0011transfer=ssh\n0001000fabc123 100\n000fdef456 200\n0000",
        );
    }

    #[test]
    fn send_command_with_data_chunks_payload_after_delim() {
        let mut s = stream(&[]);
        let mut data = Cursor::new(b"hello world".to_vec());
        s.send_command_with_data("put-object abc", &["size=11"], &mut data)
            .unwrap();
        let bytes = into_writer_bytes(s);
        // put-object abc\n(19), size=11\n(12), 0001, hello world(15), 0000
        assert_eq!(
            bytes,
            b"0013put-object abc\n000csize=11\n0001000fhello world0000",
        );
    }

    #[test]
    fn send_command_with_data_handles_empty_payload() {
        // A reader that immediately yields 0 bytes should still
        // still send the delim and the flush. That is an empty body.
        let mut s = stream(&[]);
        let mut data = Cursor::new(Vec::<u8>::new());
        s.send_command_with_data("put-object abc", &[], &mut data)
            .unwrap();
        let bytes = into_writer_bytes(s);
        // put-object abc\n(19), 0001, 0000
        assert_eq!(bytes, b"0013put-object abc\n00010000");
    }

    #[test]
    fn read_status_until_delim_parses_status_then_args() {
        // status 200\n(15), arg=1\n(10), 0001
        let canned = b"000fstatus 200\n000aarg=1\n0001";
        let mut s = stream(canned);
        let (status, args) = s.read_status_until_delim().unwrap();
        assert_eq!(status, 200);
        assert_eq!(args, vec!["arg=1"]);
    }

    #[test]
    fn read_status_until_delim_errors_on_unexpected_flush() {
        // status 200\n and a flush with no delim is an error.
        let canned = b"000fstatus 200\n0000";
        let mut s = stream(canned);
        match s.read_status_until_delim() {
            Err(ConnectionError::Protocol(msg)) => {
                assert!(msg.contains("flush"), "got: {msg}");
            }
            other => panic!("expected Protocol error, got {other:?}"),
        }
    }

    #[test]
    fn copy_data_until_flush_accumulates_bytes() {
        // Three data packets then flush
        let canned = b"0008abcd0008efgh0008ijkl0000";
        let mut s = stream(canned);
        let mut out = Vec::new();
        let n = s.copy_data_until_flush(&mut out).unwrap();
        assert_eq!(out, b"abcdefghijkl");
        assert_eq!(n, 12);
    }

    #[test]
    fn copy_data_until_flush_handles_immediate_flush() {
        // Empty payload, just a flush
        let mut s = stream(b"0000");
        let mut out = Vec::new();
        let n = s.copy_data_until_flush(&mut out).unwrap();
        assert_eq!(out, b"");
        assert_eq!(n, 0);
    }

    #[test]
    fn copy_data_until_flush_errors_on_delim_in_payload() {
        // 0001 mid-stream is invalid for the data section
        let canned = b"0008abcd0001";
        let mut s = stream(canned);
        let mut out = Vec::new();
        match s.copy_data_until_flush(&mut out) {
            Err(ConnectionError::Protocol(msg)) => {
                assert!(msg.contains("delim"), "got: {msg}");
            }
            other => panic!("expected Protocol error, got {other:?}"),
        }
    }

    #[test]
    fn read_text_lines_until_flush_collects_lines() {
        // Three text lines then flush. "foo\n" is 4 bytes payload,
        // so header = 4 + 4 = 0008.
        let canned = b"0008foo\n0008bar\n0008baz\n0000";
        let mut s = stream(canned);
        let lines = s.read_text_lines_until_flush().unwrap();
        assert_eq!(lines, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn read_text_lines_until_flush_returns_empty_on_immediate_flush() {
        let mut s = stream(b"0000");
        let lines = s.read_text_lines_until_flush().unwrap();
        assert_eq!(lines, Vec::<String>::new());
    }

    #[test]
    fn round_trip_get_object_shape() {
        // Drive both directions. The client sends `get-object <oid>`,
        // `size=...`, and a flush. The server replies with status 200, a
        // delim, the binary payload, and a flush.
        let server_reply = b"000fstatus 200\n000bsize=4\n00010008abcd0000";
        let mut s = stream(server_reply);

        // Client side: send `get-object <oid>` with one size arg.
        s.send_command("get-object abc123", &["size=4"]).unwrap();

        // Read the server's status, arguments, and payload.
        let (status, args) = s.read_status_until_delim().unwrap();
        assert_eq!(status, 200);
        assert_eq!(args, vec!["size=4"]);
        let mut out = Vec::new();
        s.copy_data_until_flush(&mut out).unwrap();
        assert_eq!(out, b"abcd");

        // Inspect what the client sent: `get-object abc123\n`,
        // `size=4\n`, and a flush.
        let sent = into_writer_bytes(s);
        assert_eq!(sent, b"0016get-object abc123\n000bsize=4\n0000");
    }

    /// End-to-end test against a tiny "remote" implemented as a
    /// shell script. We invoke it as `sh <path>` rather than chmod
    /// +x'ing it because the +x path races with the kernel under
    /// parallel test execution (ETXTBSY between chmod and exec).
    /// The program is the shell command `sh <path>`, as with
    /// `GIT_SSH_COMMAND="sh /path/to/script"`.
    ///
    /// Wire sizes in bytes, including the flush:
    /// - client "version 1": 4 + len("version 1\n") + 4 = 18
    /// - client "quit": 4 + len("quit\n") + 4 = 13
    /// - server "version=1" capability: 14 + 4 = 18
    /// - server status 200, delim, flush: 15 + 4 + 4 = 23
    /// - server status 200, flush: 15 + 4 = 19
    #[test]
    #[cfg(unix)]
    fn handshake_against_stub_server() {
        let tmp = tempfile::TempDir::new().unwrap();
        let script = tmp.path().join("fakessh.sh");
        std::fs::write(
            &script,
            "# 1. capability advertisement and flush\n\
             printf '000eversion=1\\n0000'\n\
             # 2. drain client's `version 1` and flush (18 bytes)\n\
             dd bs=1 count=18 of=/dev/null 2>/dev/null\n\
             # 3. version response: status 200, delim, flush\n\
             printf '000fstatus 200\\n00010000'\n\
             # 4. drain client's `quit` and flush (13 bytes)\n\
             dd bs=1 count=13 of=/dev/null 2>/dev/null\n\
             # 5. quit response: status 200 and flush\n\
             printf '000fstatus 200\\n0000'\n",
        )
        .unwrap();

        let mut config = Config::new(
            0,
            SshProgram::Shell(format!("sh {}", script.to_string_lossy())),
            meta("git@host"),
            Operation::Upload,
        );
        // The stub ignores its arguments. Keep the argv minimal.
        config.multiplex = Multiplex::Disabled;
        let conn = Connection::spawn(&config).expect("handshake should succeed");
        conn.end().expect("quit should succeed");
    }

    #[test]
    #[cfg(unix)]
    fn handshake_fails_when_capability_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let script = tmp.path().join("fakessh.sh");
        std::fs::write(
            &script,
            "# Capability advertisement with a different version.\n\
             printf '000eversion=2\\n0000'\n\
             sleep 1\n",
        )
        .unwrap();

        let config = Config::new(
            0,
            SshProgram::Shell(format!("sh {}", script.to_string_lossy())),
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
