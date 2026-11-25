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
use std::time::{SystemTime, UNIX_EPOCH};

use lfstest_server::Server;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

/// The `git-lfs` binary the tests run: the one Cargo built, or the one in
/// `LFSTEST_GIT_LFS`, such as upstream's, to check a test against it.
pub fn git_lfs() -> PathBuf {
    std::env::var_os("LFSTEST_GIT_LFS")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_git-lfs").into())
}

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
        test.command(&home, git_lfs())
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

    /// Check which objects the remote repository `name` has.
    #[track_caller]
    pub fn assert_server_objects(&self, name: &str, present: &[&str], absent: &[&str]) {
        for oid in present {
            assert!(
                self.server.object(name, oid).is_some(),
                "{name} lacks {oid}"
            );
        }
        for oid in absent {
            assert!(self.server.object(name, oid).is_none(), "{name} has {oid}");
        }
    }

    /// The home directory, with the global Git config.
    pub fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// The directory the test's clones are in.
    pub fn dir(&self) -> &Path {
        self.root.path()
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

    /// Create the remote repository `name` with `file` tracked, containing
    /// its own name and a newline, and pushed, like the shell suites'
    /// `setup_remote_repo_with_file`. The clone is in `clone_<name>`.
    pub fn remote_with_file(&self, name: &str, file: &str) -> Repo<'_> {
        self.remote(name);
        let repo = self.clone(name, &format!("clone_{name}"));
        repo.lfs(["track", file]).ok();
        repo.write(file, format!("{file}\n"));
        repo.git(["add", ".gitattributes", file]).ok();
        repo.git(["commit", "-m", &format!("add {file}")])
            .ok()
            .has("main (root-commit)")
            .has("2 files changed")
            .has(&format!("create mode 100644 {file}"))
            .has("create mode 100644 .gitattributes");
        repo.git(["push", "origin", "main"])
            .ok()
            .has("main -> main");
        repo
    }

    /// Clone the remote repository `name` into `dir` without downloading
    /// LFS objects, so the working tree has pointers.
    pub fn clone_skip_smudge(&self, name: &str, dir: &str) -> Repo<'_> {
        self.command(self.root.path(), "git")
            .args(["clone", &self.remote_url(name), dir])
            .env("GIT_LFS_SKIP_SMUDGE", "1")
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

    /// Whether the tests run as root, which permissions don't apply to.
    pub fn is_root(&self) -> bool {
        let uid = self.command(self.root.path(), "id").args(["-u"]).run();
        uid.ok().stdout().trim() == "0"
    }

    /// Git's major and minor version.
    pub fn git_version(&self) -> (u32, u32) {
        let version = self
            .command(self.root.path(), "git")
            .args(["version"])
            .run()
            .ok()
            .stdout();
        let mut parts = version
            .trim()
            .trim_start_matches("git version ")
            .split('.')
            .map(|p| p.parse().unwrap_or(0));
        (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
    }

    /// A command that runs `program` in `cwd`, with the test's environment.
    pub fn command(&self, cwd: &Path, program: impl AsRef<OsStr>) -> Cmd {
        let git_lfs = git_lfs();
        let bin_dir = git_lfs.parent().unwrap();
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
            .env("GIT_TEMPLATE_DIR", templates)
            // Git doesn't look for a repository above the test's directory.
            .env(
                "GIT_CEILING_DIRECTORIES",
                self.root.path().parent().unwrap(),
            );
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

    /// The same repository, with commands run from the subdirectory `dir`.
    pub fn at(&self, dir: &str) -> Repo<'_> {
        Repo {
            test: self.test,
            dir: self.dir.join(dir),
        }
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
        self.command(git_lfs()).args(args).run()
    }

    /// A command to run here, for when it needs stdin or more environment.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Cmd {
        self.test.command(&self.dir, program)
    }

    /// `git lfs`, to run with stdin or more environment.
    pub fn lfs_command(&self) -> Cmd {
        self.command(git_lfs())
    }

    /// Write `contents` to `path`, creating its directory.
    pub fn write(&self, path: &str, contents: impl AsRef<[u8]>) {
        let path = self.dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    pub fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.join(path)).unwrap()
    }

    /// Stage everything and commit it.
    pub fn commit_all(&self, message: &str) {
        self.git(["add", "--all"]).ok();
        self.git(["commit", "-m", message]).ok();
    }

    /// Make the commits in `commits`, in order, like the shell suites'
    /// `lfstest-testutils addcommits`. The files go through Git's filters,
    /// so tracked ones are stored as LFS objects.
    pub fn add_commits(&self, commits: &[Commit]) {
        for (i, commit) in commits.iter().enumerate() {
            if let Some(parent) = &commit.parent {
                self.git(["checkout", parent]).ok();
            }
            if let Some(branch) = &commit.new_branch {
                self.git(["checkout", "-b", branch]).ok();
            }
            for (path, contents) in &commit.files {
                self.write(path, contents);
                self.git(["add", path]).ok();
            }
            let mut commit_cmd = self.command("git").args([
                "commit",
                "--allow-empty",
                "-m",
                &format!("Test commit {i}"),
            ]);
            if let Some(days) = commit.days_ago {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
                let date = format!("@{} +0000", now.as_secs() - u64::from(days) * 86400);
                commit_cmd = commit_cmd
                    .env("GIT_AUTHOR_DATE", &date)
                    .env("GIT_COMMITTER_DATE", &date);
            }
            commit_cmd.run().ok();
            for tag in &commit.tags {
                self.git(["tag", "-a", "-m", "Added tag", tag]).ok();
            }
        }
    }

    /// Check that the index matches HEAD, like the shell suites'
    /// `assert_clean_index`.
    #[track_caller]
    pub fn assert_clean_index(&self) {
        let run = self.git(["diff-index", "--cached", "HEAD"]).ok();
        assert!(run.stdout().is_empty(), "index differs from HEAD\n{run}");
    }

    /// Check that the working tree matches HEAD, except for the paths in
    /// `exceptions`.
    #[track_caller]
    pub fn assert_clean_worktree_except(&self, exceptions: &[&str]) {
        let run = self
            .git(["-c", "core.quotePath=false", "diff-index", "HEAD"])
            .ok();
        let changed: Vec<String> = run
            .stdout()
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(_, path)| path.to_owned()))
            .filter(|path| !exceptions.contains(&path.as_str()))
            .collect();
        assert!(changed.is_empty(), "changed: {changed:?}\n{run}");
    }

    /// Check that the working tree and index match HEAD, like the shell
    /// suites' `assert_clean_status`.
    #[track_caller]
    pub fn assert_clean_status(&self) {
        self.assert_clean_worktree_except(&[]);
        let run = self.git(["status"]).ok();
        assert!(run.stdout().contains("working tree clean"), "{run}");
    }

    /// Check that `path` at `rev` is a pointer to the object `oid` of
    /// `size` bytes.
    #[track_caller]
    pub fn assert_pointer(&self, rev: &str, path: &str, oid: &str, size: usize) {
        let blob = self.git(["cat-file", "-p", &format!("{rev}:{path}")]).ok();
        assert_eq!(blob.stdout(), pointer(oid, size), "{rev}:{path}");
    }

    /// Configure the case-inverting pointer extension, like the shell
    /// suites' `setup_case_inverter_extension`. It logs each run to the
    /// returned file.
    pub fn case_inverter_extension(&self) -> PathBuf {
        let root = self.test.root.path();
        let script = root.join("caseinverterextension");
        let log = root.join("caseinverterextension.log");
        // Like upstream's lfstest-caseinverterextension: it runs at the
        // repository's root, and logs which file it filters.
        let body = format!(
            "#!/bin/sh\n\
             log={}\n\
             if [ ! -d .git ]; then echo '\".git\" directory not found' >>\"$log\"; exit 1; fi\n\
             echo \"$1: $3\" >>\"$log\"\n\
             exec tr '[:lower:][:upper:]' '[:upper:][:lower:]'\n",
            log.display()
        );
        std::fs::write(&script, body).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        for (key, value) in [
            ("clean", format!("{} clean -- %f", script.display())),
            ("smudge", format!("{} smudge -- %f", script.display())),
            ("priority", "0".into()),
        ] {
            let key = format!("lfs.extension.caseinverter.{key}");
            self.git(["config", &key, &value]).ok();
        }
        log
    }

    /// Point the remote `origin` at a new remote repository `name`, like
    /// the shell suites' `setup_alternate_remote`.
    pub fn alternate_remote(&self, name: &str) {
        self.test.remote(name);
        self.git(["remote", "rm", "origin"]).ok();
        self.git(["remote", "add", "origin", &self.test.remote_url(name)])
            .ok();
    }

    /// The Git directory, `.git` in a working tree.
    pub fn git_dir(&self) -> PathBuf {
        let dir = self.git(["rev-parse", "--absolute-git-dir"]).ok().stdout();
        PathBuf::from(dir.trim())
    }

    /// Where the local LFS store keeps the object `oid`.
    pub fn local_object_path(&self, oid: &str) -> PathBuf {
        self.git_dir()
            .join("lfs/objects")
            .join(&oid[0..2])
            .join(&oid[2..4])
            .join(oid)
    }

    /// The contents of the object `oid` in the local LFS store.
    pub fn local_object(&self, oid: &str) -> Option<Vec<u8>> {
        std::fs::read(self.local_object_path(oid)).ok()
    }

    /// Check that the local LFS store has the object `oid`, `size` bytes
    /// long.
    #[track_caller]
    pub fn assert_local_object(&self, oid: &str, size: usize) {
        match self.local_object(oid) {
            Some(data) => assert_eq!(data.len(), size, "local object {oid}"),
            None => panic!("no local object {oid}"),
        }
    }

    /// Check that the local LFS store doesn't have the object `oid`.
    #[track_caller]
    pub fn refute_local_object(&self, oid: &str) {
        assert!(self.local_object(oid).is_none(), "local object {oid}");
    }

    /// Delete an object from the local LFS store.
    pub fn delete_local_object(&self, oid: &str) {
        std::fs::remove_file(self.local_object_path(oid)).unwrap();
    }

    /// Empty an object in the local LFS store.
    pub fn corrupt_local_object(&self, oid: &str) {
        std::fs::write(self.local_object_path(oid), "").unwrap();
    }

    /// Delete the local LFS store's objects.
    pub fn remove_local_objects(&self) {
        let dir = self.git_dir().join("lfs/objects");
        if dir.exists() {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

/// A commit for [`Repo::add_commits`].
#[derive(Default)]
pub struct Commit {
    days_ago: Option<u32>,
    parent: Option<String>,
    new_branch: Option<String>,
    files: Vec<(String, String)>,
    tags: Vec<String>,
}

impl Commit {
    pub fn new() -> Self {
        Self::default()
    }

    /// Date the commit `days` days ago.
    pub fn days_ago(mut self, days: u32) -> Self {
        self.days_ago = Some(days);
        self
    }

    /// Commit on top of the branch `name`, checking it out first.
    pub fn on(mut self, name: &str) -> Self {
        self.parent = Some(name.into());
        self
    }

    /// Create the branch `name` and commit on it.
    pub fn new_branch(mut self, name: &str) -> Self {
        self.new_branch = Some(name.into());
        self
    }

    pub fn file(mut self, path: &str, contents: impl Into<String>) -> Self {
        self.files.push((path.into(), contents.into()));
        self
    }

    /// Tag the commit with an annotated tag.
    pub fn tag(mut self, name: &str) -> Self {
        self.tags.push(name.into());
        self
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

    pub fn env(mut self, key: &str, value: impl AsRef<OsStr>) -> Self {
        self.command.env(key, value);
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

    /// Check that the command failed.
    #[track_caller]
    pub fn fails(self) -> Self {
        assert!(!self.output.status.success(), "command succeeded\n{self}");
        self
    }

    /// The exit code, or `None` if a signal ended the command.
    pub fn code(&self) -> Option<i32> {
        self.output.status.code()
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

    /// Check that no line in stdout or stderr contains `text`.
    #[track_caller]
    pub fn lacks(self, text: &str) -> Self {
        assert!(!self.output().contains(text), "{text:?} in output\n{self}");
        self
    }

    /// The number of lines in stdout and stderr that contain `text`.
    pub fn count(&self, text: &str) -> usize {
        self.output().lines().filter(|l| l.contains(text)).count()
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

/// A pointer file's contents, like the shell suites' `pointer`.
pub fn pointer(oid: &str, size: usize) -> String {
    format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n")
}

/// The SHA-256 of `data` in hex, like the shell suites' `calc_oid`.
pub fn oid(data: impl AsRef<[u8]>) -> String {
    Sha256::digest(data.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
