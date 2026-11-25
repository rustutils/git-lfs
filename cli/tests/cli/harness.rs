//! What the shell suites' `testlib.sh` and `testhelpers.sh` set up, for
//! tests in Rust.
//!
//! A [`Test`] is one test's world: a home directory with the global Git
//! config the shell suites use, and a [`Server`] for its remote
//! repositories. [`Test::clone`] gives a [`Repo`] to run `git` and
//! `git lfs` in. Each command runs with only the environment set here, so
//! the developer's config doesn't leak in.
//!
//! Tests only reach `git-lfs` through its command line, as the shell suites
//! do. They can also look at the server's state, which the shell suites
//! can't.

use std::ffi::OsStr;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use lfstest_server::Server;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const GIT_LFS: &str = env!("CARGO_BIN_EXE_git-lfs");

/// One test's home directory, server, and working directories.
pub struct Test {
    // Declared first, so the server stops before its directories go.
    server: Server,
    root: TempDir,
}

impl Test {
    /// Set up a home directory with the shell suites' global Git config,
    /// and start a server.
    pub fn new() -> Self {
        let root = TempDir::new().unwrap();
        for dir in ["home", "remote", "tmp"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        let server = Server::start(root.path().join("remote")).unwrap();
        let test = Self { server, root };

        let cert = test.root.path().join("cert.pem");
        std::fs::write(&cert, test.server.cert_pem()).unwrap();
        // Git's credential store: user:pass for each of the server's URLs,
        // as the shell suites' credential helper gives for 127.0.0.1.
        let creds = test.root.path().join("creds");
        let entries: String = [
            test.server.url(),
            test.server.ssl_url(),
            test.server.client_cert_url(),
        ]
        .iter()
        .map(|url| url.replacen("://", "://user:pass@", 1) + "\n")
        .collect();
        std::fs::write(&creds, entries).unwrap();

        let home = test.home();
        test.command(&home, GIT_LFS)
            .args(["install", "--skip-repo"])
            .run()
            .ok();
        for (key, value) in [
            (
                "credential.helper",
                format!("store --file={}", creds.display()),
            ),
            ("user.name", "Git LFS Tests".into()),
            ("user.email", "git-lfs@example.com".into()),
            ("http.sslcainfo", cert.display().to_string()),
            ("init.defaultBranch", "main".into()),
        ] {
            test.command(&home, "git")
                .args(["config", "--global", key, &value])
                .run()
                .ok();
        }
        test
    }

    /// The server the remote repositories live on.
    pub fn server(&self) -> &Server {
        &self.server
    }

    /// The home directory, with the global Git config.
    pub fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// The URL to clone the remote repository `name` from.
    pub fn remote_url(&self, name: &str) -> String {
        format!("{}/{name}", self.server.url())
    }

    /// The LFS endpoint of the remote repository `name`, as the shell
    /// suites' `repo_endpoint` gives it.
    pub fn endpoint(&self, name: &str) -> String {
        format!("{}.git/info/lfs", self.remote_url(name))
    }

    /// Create an empty bare repository `name` on the server, like the shell
    /// suites' `setup_remote_repo`.
    pub fn remote(&self, name: &str) {
        let dir = self.root.path().join("remote").join(format!("{name}.git"));
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "--bare"][..],
            &["config", "http.receivepack", "true"],
            &["config", "receive.denyCurrentBranch", "ignore"],
        ] {
            self.command(&dir, "git").args(args).run().ok();
        }
    }

    /// Clone the remote repository `name` into `dir`, like the shell
    /// suites' `clone_repo`.
    pub fn clone(&self, name: &str, dir: &str) -> Repo<'_> {
        self.command(self.root.path(), "git")
            .args(["clone", &self.remote_url(name), dir])
            .run()
            .ok();
        self.repo(dir)
    }

    /// Create the remote repository `name` and clone it into a directory
    /// of the same name.
    pub fn remote_and_clone(&self, name: &str) -> Repo<'_> {
        self.remote(name);
        self.clone(name, name)
    }

    /// A repository already in `dir`.
    pub fn repo(&self, dir: &str) -> Repo<'_> {
        Repo {
            test: self,
            dir: self.root.path().join(dir),
        }
    }

    /// A command that runs `program` in `cwd`, with the test's environment.
    pub fn command(&self, cwd: &Path, program: impl AsRef<OsStr>) -> Cmd {
        let bin_dir = Path::new(GIT_LFS).parent().unwrap();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(bin_dir.to_owned()).chain(std::env::split_paths(&path)),
        )
        .unwrap();
        let templates = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/templates");

        let mut command = Command::new(program);
        command
            .current_dir(cwd)
            .env_clear()
            .env("PATH", path)
            .env("HOME", self.home())
            .env("TMPDIR", self.root.path().join("tmp"))
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_LFS_FORCE_PROGRESS", "1")
            .env("GIT_TEMPLATE_DIR", templates);
        Cmd {
            command,
            stdin: Vec::new(),
        }
    }
}

/// A working directory, usually a clone.
pub struct Repo<'a> {
    test: &'a Test,
    dir: PathBuf,
}

impl Repo<'_> {
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Run `git` with `args`.
    pub fn git<I, S>(&self, args: I) -> Run
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command("git").args(args).run()
    }

    /// Run `git lfs` with `args`, through the built binary.
    pub fn lfs<I, S>(&self, args: I) -> Run
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command(GIT_LFS).args(args).run()
    }

    /// A command to run here, for when it needs stdin.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Cmd {
        self.test.command(&self.dir, program)
    }

    /// Write `contents` to `path`, creating its directory.
    pub fn write(&self, path: &str, contents: impl AsRef<[u8]>) {
        let path = self.dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// Stage everything and commit it.
    pub fn commit_all(&self, message: &str) {
        self.git(["add", "--all"]).ok();
        self.git(["commit", "-m", message]).ok();
    }
}

/// A command with its stdin, ready to run.
pub struct Cmd {
    command: Command,
    stdin: Vec<u8>,
}

impl Cmd {
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(args);
        self
    }

    pub fn stdin(mut self, input: impl Into<Vec<u8>>) -> Self {
        self.stdin = input.into();
        self
    }

    pub fn run(mut self) -> Run {
        let description = std::iter::once(self.command.get_program())
            .chain(self.command.get_args())
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        let mut child = self
            .command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("{description}: {e}"));
        let mut stdin = child.stdin.take().unwrap();
        let input = std::mem::take(&mut self.stdin);
        // Write from another thread, so a command that doesn't read its
        // stdin can't block on a full pipe.
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
        let output = child.wait_with_output().unwrap();
        writer.join().unwrap();
        Run {
            description,
            output,
        }
    }
}

/// A finished command. Its checks panic with the command and its output.
pub struct Run {
    description: String,
    output: Output,
}

impl Run {
    /// Check that the command succeeded.
    #[track_caller]
    pub fn ok(self) -> Self {
        assert!(self.output.status.success(), "command failed\n{self}");
        self
    }

    pub fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).into_owned()
    }

    pub fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).into_owned()
    }

    /// Stdout then stderr, for the shell suites' `2>&1 | tee` checks.
    pub fn output(&self) -> String {
        self.stdout() + &self.stderr()
    }

    /// Check that stdout or stderr has a line containing `text`.
    #[track_caller]
    pub fn has(self, text: &str) -> Self {
        assert!(
            self.output().contains(text),
            "no {text:?} in output\n{self}"
        );
        self
    }

    /// The number of lines in stdout and stderr that start with `prefix`.
    pub fn count_lines(&self, prefix: &str) -> usize {
        self.output()
            .lines()
            .filter(|l| l.starts_with(prefix))
            .count()
    }
}

impl fmt::Display for Run {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}\n{}\n--- stdout ---\n{}--- stderr ---\n{}",
            self.description,
            self.output.status,
            self.stdout(),
            self.stderr()
        )
    }
}

/// The SHA-256 of `data` in hex, like the shell suites' `calc_oid`.
pub fn oid(data: impl AsRef<[u8]>) -> String {
    Sha256::digest(data.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
