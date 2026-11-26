//! `t-env.sh`.
//!
//! Like the shell tests, these compare the lines of `git lfs env` in any
//! order, without blank lines or `GIT_EXEC_PATH`. The `GIT_*` lines come
//! from running `env` with the same environment, as the shell tests do.
//!
//! "env with multiple ssh remotes" sets `GIT_SSH` to `lfs-ssh-echo`, as
//! the shell suites do, but never runs it.

use std::path::{Path, PathBuf};

use crate::harness::{Repo, Test};

/// The shell suite sets this for every test. `git lfs env` should leave it
/// out despite the `GIT_` in its name and value.
const TEST_GIT_EXAMPLE: (&str, &str) = ("TEST_GIT_EXAMPLE", "GIT_EXAMPLE");

/// The settings `git lfs env` shows, with their defaults, in its order.
const SETTINGS: [(&str, &str); 17] = [
    ("ConcurrentTransfers", "8"),
    ("TusTransfers", "false"),
    ("BasicTransfersOnly", "false"),
    ("SkipDownloadErrors", "false"),
    ("FetchRecentAlways", "false"),
    ("FetchRecentRefsDays", "7"),
    ("FetchRecentCommitsDays", "0"),
    ("FetchRecentRefsIncludeRemotes", "true"),
    ("PruneOffsetDays", "3"),
    ("PruneVerifyRemoteAlways", "false"),
    ("PruneVerifyUnreachableAlways", "false"),
    ("PruneRemoteName", "origin"),
    ("AccessDownload", "none"),
    ("AccessUpload", "none"),
    ("DownloadTransfers", "basic,lfs-standalone-file,ssh"),
    ("UploadTransfers", "basic,lfs-standalone-file,ssh"),
    ("LocalReferenceDirs", ""),
];

/// Where `git lfs env` finds the repository.
enum Layout<'a> {
    /// A working tree, if any, and the Git directory.
    Repo(Option<&'a Path>, &'a Path),
    /// No repository, so the paths are relative or empty.
    Outside,
}

/// What one run of `git lfs env` should print.
struct Expected<'a> {
    cwd: &'a Path,
    env: Vec<(&'a str, String)>,
    layout: Layout<'a>,
    endpoints: Vec<String>,
    settings: Vec<(&'a str, &'a str)>,
    /// Whether the filter settings can be read. They print as `""` when
    /// they can't.
    filters: bool,
}

impl<'a> Expected<'a> {
    fn new(cwd: &'a Path, layout: Layout<'a>) -> Self {
        Self {
            cwd,
            env: Vec::new(),
            layout,
            endpoints: Vec::new(),
            settings: Vec::new(),
            filters: true,
        }
    }

    fn env(mut self, key: &'a str, value: impl Into<String>) -> Self {
        self.env.push((key, value.into()));
        self
    }

    fn endpoint(mut self, line: impl Into<String>) -> Self {
        self.endpoints.push(line.into());
        self
    }

    fn setting(mut self, key: &'a str, value: &'a str) -> Self {
        self.settings.push((key, value));
        self
    }

    /// The lines `git lfs env` should print, sorted.
    fn lines(&self, t: &Test) -> Vec<String> {
        let mut lines = Vec::new();
        let run = |program: &str, args: &[&str]| {
            self.command(t, program)
                .args(args)
                .run()
                .ok()
                .stdout()
                .trim_end()
                .to_owned()
        };
        lines.push(run(
            &crate::harness::git_lfs().to_string_lossy(),
            &["version"],
        ));
        lines.push(run("git", &["version"]));
        lines.extend(self.endpoints.iter().cloned());
        let path = |p: PathBuf| p.to_string_lossy().into_owned();
        let (work, git, storage, media, temp) = match &self.layout {
            Layout::Repo(work, git) => (
                work.map(|w| path(canonical(w))).unwrap_or_default(),
                path(canonical(git)),
                path(canonical(git).join("lfs")),
                path(canonical(git).join("lfs/objects")),
                path(canonical(git).join("lfs/tmp")),
            ),
            Layout::Outside => (
                String::new(),
                String::new(),
                "lfs".into(),
                "lfs/objects".into(),
                "lfs/tmp".into(),
            ),
        };
        lines.push(format!("LocalWorkingDir={work}"));
        lines.push(format!("LocalGitDir={git}"));
        lines.push(format!("LocalGitStorageDir={git}"));
        lines.push(format!("LocalMediaDir={media}"));
        lines.push(format!("TempDir={temp}"));
        lines.push(format!("LfsStorageDir={storage}"));
        for (key, default) in SETTINGS {
            let value = self
                .settings
                .iter()
                .find(|(k, _)| *k == key)
                .map_or(default, |(_, v)| *v);
            lines.push(format!("{key}={value}"));
        }
        let env = run("env", &[]);
        lines.extend(
            env.lines()
                .filter(|l| l.starts_with("GIT_"))
                .map(str::to_owned),
        );
        for (key, value) in [
            ("process", "git-lfs filter-process"),
            ("smudge", "git-lfs smudge -- %f"),
            ("clean", "git-lfs clean -- %f"),
        ] {
            let value = if self.filters { value } else { "" };
            lines.push(format!("git config filter.lfs.{key} = \"{value}\""));
        }
        sorted(lines.join("\n").as_str())
    }

    /// A command that runs `program` with this run's directory and
    /// environment.
    fn command(&self, t: &Test, program: &str) -> crate::harness::Cmd {
        let mut cmd = t
            .command(self.cwd, program)
            .env(TEST_GIT_EXAMPLE.0, TEST_GIT_EXAMPLE.1);
        for (key, value) in &self.env {
            cmd = cmd.env(key, value);
        }
        cmd
    }

    /// The lines `git lfs env` prints, sorted.
    fn actual(&self, t: &Test) -> Vec<String> {
        let run = self
            .command(t, &crate::harness::git_lfs().to_string_lossy())
            .args(["env"])
            .run()
            .ok();
        sorted(&run.stdout())
    }

    /// Check that `git lfs env` prints what it should.
    #[track_caller]
    fn check(&self, t: &Test) {
        assert_eq!(self.actual(t), self.lines(t), "in {}", self.cwd.display());
    }
}

/// The lines of `text`, without blank ones or `GIT_EXEC_PATH`, sorted.
fn sorted(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with("GIT_EXEC_PATH="))
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap()
}

/// A repository `name` with `remotes` added, as (name, remote repository)
/// pairs.
fn with_remotes<'a>(t: &'a Test, name: &str, remotes: &[(&str, &str)]) -> Repo<'a> {
    let repo = t.init(name);
    for (remote, target) in remotes {
        repo.git(["remote", "add", remote, &t.remote_url(target)])
            .ok();
    }
    repo
}

/// The endpoint line for `remote`, `origin` without a name.
fn endpoint(remote: &str, url: &str) -> String {
    if remote == "origin" {
        format!("Endpoint={url} (auth=none)")
    } else {
        format!("Endpoint ({remote})={url} (auth=none)")
    }
}

/// Check `git lfs env` in `repo` with `endpoints` and `settings`, at its
/// top and in its `.git` directory, which has no working tree.
#[track_caller]
fn check_top_and_git_dir(t: &Test, repo: &Repo, endpoints: &[String], settings: &[(&str, &str)]) {
    let git_dir = repo.path().join(".git");
    for (cwd, work) in [(repo.path(), Some(repo.path())), (git_dir.as_path(), None)] {
        let mut expected = Expected::new(cwd, Layout::Repo(work, &git_dir));
        for line in endpoints {
            expected = expected.endpoint(line.clone());
        }
        for (key, value) in settings {
            expected = expected.setting(key, value);
        }
        expected.check(t);
    }
}

#[test]
fn env_with_no_remote() {
    let t = Test::new();
    let repo = t.init("env-no-remote");
    let git_dir = repo.path().join(".git");
    Expected::new(repo.path(), Layout::Repo(Some(repo.path()), &git_dir)).check(&t);
}

#[test]
fn env_with_origin_remote() {
    let t = Test::new();
    let repo = with_remotes(&t, "env-origin-remote", &[("origin", "env-origin-remote")]);
    let origin = endpoint("origin", &t.endpoint("env-origin-remote"));
    check_top_and_git_dir(&t, &repo, &[origin], &[]);
}

#[test]
fn env_with_multiple_remotes() {
    let t = Test::new();
    let repo = with_remotes(
        &t,
        "env-multiple-remotes",
        &[
            ("origin", "env-origin-remote"),
            ("other", "env-other-remote"),
        ],
    );
    let endpoints = [
        endpoint("origin", &t.endpoint("env-origin-remote")),
        endpoint("other", &t.endpoint("env-other-remote")),
    ];
    check_top_and_git_dir(&t, &repo, &endpoints, &[]);
}

#[test]
fn env_with_other_remote() {
    let t = Test::new();
    let repo = with_remotes(&t, "env-other-remote", &[("other", "env-other-remote")]);
    let other = endpoint("other", &t.endpoint("env-other-remote"));
    check_top_and_git_dir(&t, &repo, &[other], &[]);
}

/// Two remotes, with `config` set in the repository.
fn two_remotes_with<'a>(t: &'a Test, name: &str, config: &[(&str, &str)]) -> Repo<'a> {
    let repo = with_remotes(
        t,
        name,
        &[
            ("origin", "env-origin-remote"),
            ("other", "env-other-remote"),
        ],
    );
    for (key, value) in config {
        repo.git(["config", key, value]).ok();
    }
    repo
}

#[test]
fn env_with_multiple_remotes_and_lfs_url_config() {
    let t = Test::new();
    let repo = two_remotes_with(
        &t,
        "env-multiple-remotes-with-lfs-url",
        &[("lfs.url", "http://foo/bar")],
    );
    let endpoints = [
        endpoint("origin", "http://foo/bar"),
        endpoint("other", "http://foo/bar"),
    ];
    check_top_and_git_dir(&t, &repo, &endpoints, &[]);
}

/// `lfs.url` wins over each remote's `lfsurl`.
#[test]
fn env_with_multiple_remotes_and_lfs_configs() {
    let t = Test::new();
    let repo = two_remotes_with(
        &t,
        "env-multiple-remotes-lfs-configs",
        &[
            ("lfs.url", "http://foo/bar"),
            ("remote.origin.lfsurl", "http://custom/origin"),
            ("remote.other.lfsurl", "http://custom/other"),
        ],
    );
    let endpoints = [
        endpoint("origin", "http://foo/bar"),
        endpoint("other", "http://foo/bar"),
    ];
    check_top_and_git_dir(&t, &repo, &endpoints, &[]);
}

#[test]
fn env_with_multiple_remotes_and_batch_configs() {
    let t = Test::new();
    let repo = two_remotes_with(
        &t,
        "env-multiple-remotes-lfs-batch-configs",
        &[
            ("lfs.concurrenttransfers", "5"),
            ("remote.origin.lfsurl", "http://foo/bar"),
            ("remote.other.lfsurl", "http://custom/other"),
        ],
    );
    let endpoints = [
        endpoint("origin", "http://foo/bar"),
        endpoint("other", "http://custom/other"),
    ];
    check_top_and_git_dir(&t, &repo, &endpoints, &[("ConcurrentTransfers", "5")]);
}

/// `.lfsconfig` sets the endpoint, but not `lfs.concurrenttransfers`,
/// which isn't safe there. A `.gitconfig` in the working tree is ignored.
#[test]
fn env_with_lfsconfig() {
    let t = Test::new();
    let repo = with_remotes(&t, "env-with-lfsconfig", &[("origin", "env-origin-remote")]);
    repo.write(
        ".lfsconfig",
        "[remote \"origin\"]\n\tlfsurl = http://foobar:8080/\n\
         [lfs]\n     batch = false\n\tconcurrenttransfers = 5\n",
    );
    repo.write(
        ".gitconfig",
        "[remote \"origin\"]\nlfsurl = http://foobar:5050/\n\
         [lfs]\n   batch = true\nconcurrenttransfers = 50\n",
    );
    std::fs::create_dir(repo.path().join("a")).unwrap();
    let git_dir = repo.path().join(".git");

    for cwd in [repo.path().to_owned(), repo.path().join("a")] {
        Expected::new(&cwd, Layout::Repo(Some(repo.path()), &git_dir))
            .endpoint("Endpoint=http://foobar:8080/ (auth=none)")
            .check(&t);
    }
}

#[test]
fn env_with_environment_variables() {
    let t = Test::new();
    let repo = t.init("env-with-envvars");
    std::fs::create_dir_all(repo.path().join("a/b/c")).unwrap();
    let git_dir = canonical(&repo.path().join(".git"));
    let work_tree = canonical(&repo.path().join("a/b"));
    let git_dir_str = git_dir.to_string_lossy().into_owned();
    let work_tree_str = work_tree.to_string_lossy().into_owned();
    let layout = || Layout::Repo(Some(&work_tree), &git_dir);

    // With both set, from anywhere.
    for cwd in [
        t.dir().to_owned(),
        repo.path().to_owned(),
        repo.path().join(".git"),
        repo.path().join("a/b/c"),
    ] {
        Expected::new(&cwd, layout())
            .env("GIT_DIR", &git_dir_str)
            .env("GIT_WORK_TREE", &work_tree_str)
            .check(&t);
    }

    // `a/b` doesn't exist relative to `a/b/c`, so nothing can be read.
    let c = repo.path().join("a/b/c");
    let mut invalid = Expected::new(&c, Layout::Outside)
        .env("GIT_DIR", &git_dir_str)
        .env("GIT_WORK_TREE", "a/b");
    invalid.filters = false;
    invalid.check(&t);

    // GIT_DIR alone, from the working tree's top.
    let b = repo.path().join("a/b");
    Expected::new(&b, layout())
        .env("GIT_DIR", &git_dir_str)
        .check(&t);

    // GIT_WORK_TREE alone, from inside the repository.
    let a = repo.path().join("a");
    Expected::new(&a, layout())
        .env("GIT_WORK_TREE", &work_tree_str)
        .check(&t);
}

#[test]
fn env_with_bare_repo() {
    let t = Test::new();
    t.repo("")
        .git(["init", "--bare", "env-with-bare-repo"])
        .ok();
    let repo = t.repo("env-with-bare-repo");
    Expected::new(repo.path(), Layout::Repo(None, repo.path())).check(&t);
}

#[test]
fn env_with_multiple_ssh_remotes() {
    let t = Test::new();
    let repo = t.init("env-with-ssh");
    repo.git([
        "remote",
        "add",
        "origin",
        "git@git-server.com:user/repo.git",
    ])
    .ok();
    repo.git([
        "remote",
        "add",
        "other",
        "git@other-git-server.com:user/repo.git",
    ])
    .ok();

    let run = repo
        .lfs_command()
        .args(["env"])
        .env("GIT_SSH", "lfs-ssh-echo")
        .run()
        .ok();

    let stdout = run.stdout();
    let mut actual: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("Endpoint") || l.contains("SSH="))
        .collect();
    actual.sort();
    let mut expected = vec![
        "Endpoint=https://git-server.com/user/repo.git/info/lfs (auth=none)",
        "  SSH=git@git-server.com:user/repo.git",
        "Endpoint (other)=https://other-git-server.com/user/repo.git/info/lfs (auth=none)",
        "  SSH=git@other-git-server.com:user/repo.git",
        "GIT_SSH=lfs-ssh-echo",
    ];
    expected.sort();
    assert_eq!(actual, expected);
}

#[test]
fn env_with_skip_download_errors() {
    let t = Test::new();
    let repo = t.init("env-with-skip-dl");
    let git_dir = repo.path().join(".git");
    let expected = || Expected::new(repo.path(), Layout::Repo(Some(repo.path()), &git_dir));

    repo.git(["config", "lfs.skipdownloaderrors", "1"]).ok();
    expected().setting("SkipDownloadErrors", "true").check(&t);

    // It's usually off.
    repo.git(["config", "--unset", "lfs.skipdownloaderrors"])
        .ok();
    expected().check(&t);

    // The variable turns it on.
    expected()
        .env("GIT_LFS_SKIP_DOWNLOAD_ERRORS", "1")
        .setting("SkipDownloadErrors", "true")
        .check(&t);
}

#[test]
fn env_with_extra_transfer_methods() {
    let t = Test::new();
    let repo = t.init("env-with-transfers");
    repo.git(["config", "lfs.tustransfers", "true"]).ok();
    repo.git([
        "config",
        "lfs.customtransfer.supertransfer.path",
        "/path/to/something",
    ])
    .ok();
    let git_dir = repo.path().join(".git");

    Expected::new(repo.path(), Layout::Repo(Some(repo.path()), &git_dir))
        .setting("TusTransfers", "true")
        .setting(
            "DownloadTransfers",
            "basic,lfs-standalone-file,ssh,supertransfer",
        )
        .setting(
            "UploadTransfers",
            "basic,lfs-standalone-file,ssh,supertransfer,tus",
        )
        .check(&t);
}

/// Two remotes and a commit, in a repository named `name`.
fn two_remotes_and_commit<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = two_remotes_with(t, name, &[]);
    repo.write("a.txt", "");
    repo.git(["add", "a.txt"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo
}

/// Check `git lfs env` at the top of `repo`, which has the two remotes,
/// with `env` set.
#[track_caller]
fn check_two_remotes(t: &Test, repo: &Repo, env: &[(&str, &str)]) {
    let git_dir = repo.path().join(".git");
    let mut expected = Expected::new(repo.path(), Layout::Repo(Some(repo.path()), &git_dir))
        .endpoint(endpoint("origin", &t.endpoint("env-origin-remote")))
        .endpoint(endpoint("other", &t.endpoint("env-other-remote")));
    for (key, value) in env {
        expected = expected.env(key, *value);
    }
    expected.check(t);
}

#[test]
fn env_with_multiple_remotes_and_ref() {
    let t = Test::new();
    let repo = two_remotes_and_commit(&t, "env-multiple-remotes-ref");
    check_two_remotes(&t, &repo, &[]);
}

/// A repository whose name has characters outside ASCII, and outside
/// ISO-8859-1, with `LC_ALL` unset.
#[test]
fn env_with_unicode() {
    let t = Test::new();
    let repo = two_remotes_and_commit(&t, "env-d’autre-nom-très-bizarr€");
    let git_dir = repo.path().join(".git");
    let expected = Expected::new(repo.path(), Layout::Repo(Some(repo.path()), &git_dir))
        .endpoint(endpoint("origin", &t.endpoint("env-origin-remote")))
        .endpoint(endpoint("other", &t.endpoint("env-other-remote")));

    let actual = sorted(
        &expected
            .command(&t, &crate::harness::git_lfs().to_string_lossy())
            .env_remove("LC_ALL")
            .args(["env"])
            .run()
            .ok()
            .stdout(),
    );

    assert_eq!(actual, expected.lines(&t));
}

/// With Git's messages possibly in French, and nothing on stderr.
#[test]
fn env_outside_a_repository() {
    let t = Test::new();
    let expected = Expected::new(t.dir(), Layout::Outside).env("LC_ALL", "fr_FR.UTF-8");

    let run = expected
        .command(&t, &crate::harness::git_lfs().to_string_lossy())
        .args(["env"])
        .merged()
        .run()
        .ok();

    assert_eq!(sorted(&run.stdout()), expected.lines(&t));
}

#[test]
fn env_with_duplicate_endpoints() {
    let t = Test::new();
    let repo = two_remotes_and_commit(&t, "env-duplicate-endpoints");
    let env = |repo: &Repo| repo.lfs_command().args(["env"]).merged().run().ok();
    let warned = |run: &crate::harness::Run| {
        run.output()
            .lines()
            .any(|l| l.contains("warning") && l.contains("same alias"))
    };

    // The same alias twice for the same URL is fine.
    repo.append(
        ".git/config",
        "[url \"https://host.example/domain/\"]\n\
         \tinsteadOf = git@host.example:domain/\n\
         [url \"https://host.example/domain/\"]\n\
         \tinsteadOf = git@host.example:domain/\n",
    );
    let run = env(&repo);
    assert!(!warned(&run), "{run}");

    // For another URL, it isn't.
    repo.append(
        ".git/config",
        "[url \"https://somewhere-else.example/domain/\"]\n\
         \tinsteadOf = git@host.example:domain/\n",
    );
    let run = env(&repo);
    assert!(warned(&run), "{run}");
}

/// Not from the shell suite: the fetch and prune settings show their
/// configured values.
#[test]
#[ignore = "env always shows the default fetch and prune settings"]
fn env_shows_fetch_and_prune_settings() {
    let t = Test::new();
    let repo = t.init("env-fetch-prune-settings");
    for (key, value) in [
        ("lfs.fetchrecentalways", "true"),
        ("lfs.fetchrecentrefsdays", "3"),
        ("lfs.fetchrecentcommitsdays", "2"),
        ("lfs.fetchrecentremoterefs", "false"),
        ("lfs.pruneoffsetdays", "9"),
        ("lfs.pruneverifyremotealways", "true"),
        ("lfs.pruneverifyunreachablealways", "true"),
        ("lfs.pruneremotetocheck", "upstream"),
    ] {
        repo.git(["config", key, value]).ok();
    }
    let git_dir = repo.path().join(".git");

    Expected::new(repo.path(), Layout::Repo(Some(repo.path()), &git_dir))
        .setting("FetchRecentAlways", "true")
        .setting("FetchRecentRefsDays", "3")
        .setting("FetchRecentCommitsDays", "2")
        .setting("FetchRecentRefsIncludeRemotes", "false")
        .setting("PruneOffsetDays", "9")
        .setting("PruneVerifyRemoteAlways", "true")
        .setting("PruneVerifyUnreachableAlways", "true")
        .setting("PruneRemoteName", "upstream")
        .check(&t);
}

/// Not from the shell suite: with `lfs.basictransfersonly`, only `basic`
/// is listed.
#[test]
#[ignore = "env lists every transfer method with lfs.basictransfersonly"]
fn env_with_basic_transfers_only() {
    let t = Test::new();
    let repo = t.init("env-basic-transfers-only");
    repo.git(["config", "lfs.basictransfersonly", "true"]).ok();
    let git_dir = repo.path().join(".git");

    Expected::new(repo.path(), Layout::Repo(Some(repo.path()), &git_dir))
        .setting("BasicTransfersOnly", "true")
        .setting("DownloadTransfers", "basic")
        .setting("UploadTransfers", "basic")
        .check(&t);
}
