//! `t-config.sh`.

use crate::harness::{Repo, Run, Test};

/// Run `git lfs env` in `repo`, with stderr merged into stdout.
fn env(repo: &Repo) -> Run {
    repo.lfs_command().args(["env"]).merged().run().ok()
}

/// Check that `git lfs env` in `repo` shows `endpoint` with `auth`.
#[track_caller]
fn assert_endpoint(repo: &Repo, endpoint: &str, auth: &str) {
    env(repo).has(&format!("Endpoint={endpoint} (auth={auth})"));
}

/// Set `key` to `value` in `repo`'s `.lfsconfig`.
fn lfsconfig(repo: &Repo, key: &str, value: &str) {
    repo.git(["config", "--file=.lfsconfig", key, value]).ok();
}

/// Unset `key` in `repo`'s `.lfsconfig`.
fn unset_lfsconfig(repo: &Repo, key: &str) {
    repo.git(["config", "--file=.lfsconfig", "--unset", key])
        .ok();
}

/// A new repository `name` with `origin` at the remote repository of the
/// same name.
fn with_origin<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.git(["remote", "add", "origin", &t.remote_url(name)])
        .ok();
    repo
}

#[test]
fn default_config() {
    let t = Test::new();
    let repo = with_origin(&t, "default-config");
    assert_endpoint(&repo, &t.endpoint("default-config"), "none");

    repo.git([
        "config",
        "--file=.gitconfig",
        "lfs.url",
        "http://gitconfig-file-ignored",
    ])
    .ok();
    lfsconfig(&repo, "lfs.url", "http://lfsconfig-file");
    lfsconfig(&repo, "lfs.http://lfsconfig-file.access", "lfsconfig");
    assert_endpoint(&repo, "http://lfsconfig-file", "lfsconfig");
    unset_lfsconfig(&repo, "lfs.url");
    unset_lfsconfig(&repo, "lfs.http://lfsconfig-file.access");

    // The local config's URL, without the access setting.
    repo.git(["config", "lfs.url", "http://local-lfsconfig"])
        .ok();
    assert_endpoint(&repo, "http://local-lfsconfig", "none");

    // The access setting from .lfsconfig.
    lfsconfig(&repo, "lfs.http://local-lfsconfig.access", "lfsconfig");
    assert_endpoint(&repo, "http://local-lfsconfig", "lfsconfig");
    unset_lfsconfig(&repo, "lfs.http://local-lfsconfig.access");

    // And from the Git config.
    repo.git(["config", "lfs.http://local-lfsconfig.access", "gitconfig"])
        .ok();
    assert_endpoint(&repo, "http://local-lfsconfig", "gitconfig");
}

/// Without a working tree copy, `.lfsconfig` is read from HEAD, or from
/// the index.
#[test]
fn config_reads_from_repository() {
    let t = Test::new();
    let name = "repository-config";
    t.remote(name);
    let repo = with_origin(&t, name);
    assert_endpoint(&repo, &t.endpoint(name), "none");
    for (branch, url) in [
        ("", "http://lfsconfig-file"),
        ("side", "http://lfsconfig-file-side"),
    ] {
        if !branch.is_empty() {
            repo.git(["checkout", "-b", branch]).ok();
        }
        lfsconfig(&repo, "lfs.url", url);
        lfsconfig(&repo, &format!("lfs.{url}.access"), "lfsconfig");
        repo.git(["add", ".lfsconfig"]).ok();
        repo.git(["commit", "-m", "Add file"]).ok();
        repo.git(["push", "origin", "HEAD"]).ok();
    }

    let other = t.init(&format!("{name}-2"));
    other
        .git(["remote", "add", "origin", &t.remote_url(name)])
        .ok();
    assert_endpoint(&other, &t.endpoint(name), "none");
    other.git(["fetch", "origin"]).ok();
    other
        .git(["symbolic-ref", "HEAD", "refs/remotes/origin/side"])
        .ok();
    other.git(["show", "HEAD:.lfsconfig"]).ok();
    assert_endpoint(&other, "http://lfsconfig-file-side", "lfsconfig");

    other.git(["read-tree", "refs/remotes/origin/main"]).ok();
    assert_endpoint(&other, "http://lfsconfig-file", "lfsconfig");
}

/// A clone smudges `.bin`, which sorts before `.lfsconfig`, with the
/// endpoint from `.lfsconfig`.
#[test]
fn can_read_lfs_file_with_name_before_lfsconfig() {
    let t = Test::new();
    let name = "early-file-config";
    t.remote(name);
    let repo = with_origin(&t, name);
    repo.lfs(["track", "*.bin"]).ok();
    lfsconfig(&repo, "lfs.url", &t.endpoint(name));
    repo.write(".bin", "abc\n");
    repo.write("a.bin", "def\n");
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", "Add files"]).ok();
    repo.git(["push", "origin", "HEAD"]).ok();
    repo.remove_local_objects();

    let clone_dir = format!("{name}-2");
    t.repo("").git(["clone", name, &clone_dir]).ok();

    let clone = t.repo(&clone_dir);
    assert_eq!(clone.read(".bin"), "abc\n");
    assert_eq!(clone.read("a.bin"), "def\n");
}

/// Check that `git lfs ext` in `repo` shows the extension `env-test`
/// with these settings.
#[track_caller]
fn assert_extension(repo: &Repo, clean: &str, smudge: &str, priority: u32) {
    let run = repo.lfs(["ext"]).ok();
    assert_eq!(
        run.stdout().trim_end(),
        format!(
            "Extension: env-test\n    \
             clean = {clean}\n    \
             smudge = {smudge}\n    \
             priority = {priority}"
        )
    );
}

#[test]
fn extension_config() {
    let t = Test::new();
    t.git_config_global("lfs.extension.env-test.clean", "env-test-clean");
    t.git_config_global("lfs.extension.env-test.smudge", "env-test-smudge");
    t.git_config_global("lfs.extension.env-test.priority", "0");
    let repo = t.init("extension-config");
    assert_extension(&repo, "env-test-clean", "env-test-smudge", 0);

    // Any Git config takes precedence over .lfsconfig.
    repo.git([
        "config",
        "--global",
        "--unset",
        "lfs.extension.env-test.priority",
    ])
    .ok();
    lfsconfig(&repo, "lfs.extension.env-test.clean", "file-env-test-clean");
    lfsconfig(
        &repo,
        "lfs.extension.env-test.smudge",
        "file-env-test-smudge",
    );
    lfsconfig(&repo, "lfs.extension.env-test.priority", "1");
    assert_extension(&repo, "env-test-clean", "env-test-smudge", 1);

    for (key, value) in [
        ("clean", "local-env-test-clean"),
        ("smudge", "local-env-test-smudge"),
        ("priority", "2"),
    ] {
        repo.git(["config", &format!("lfs.extension.env-test.{key}"), value])
            .ok();
    }
    assert_extension(&repo, "local-env-test-clean", "local-env-test-smudge", 2);
}

/// Not from the shell suite: an extension's commands in `.lfsconfig` are
/// ignored, since they would run, and only its priority is read.
#[test]
fn extension_commands_in_lfsconfig_are_ignored() {
    let t = Test::new();
    let repo = t.init("extension-lfsconfig");
    lfsconfig(&repo, "lfs.extension.evil.clean", "evil-clean");
    lfsconfig(&repo, "lfs.extension.evil.smudge", "evil-smudge");
    lfsconfig(&repo, "lfs.extension.evil.priority", "3");

    let run = repo
        .lfs_command()
        .args(["ext"])
        .merged()
        .run()
        .ok()
        .has(UNSAFE_WARNING)
        .lacks("evil-clean")
        .lacks("evil-smudge");

    assert_line(&run, "  lfs.extension.evil.clean");
    assert_line(&run, "  lfs.extension.evil.smudge");
}

/// Set the `insteadOf` aliases in `aliases`, as (URL, alias) pairs, with
/// `--add`.
fn insteadof(repo: &Repo, aliases: &[(&str, &str)]) {
    for (url, alias) in aliases {
        repo.git(["config", "--add", &format!("url.{url}.insteadOf"), alias])
            .ok();
    }
}

/// When several aliases match, the longest wins.
#[test]
fn url_alias_config() {
    let t = Test::new();
    let repo = t.init("url-alias");
    insteadof(
        &repo,
        &[
            ("http://wrong-url/", "alias"),
            ("http://actual-url/", "alias:"),
        ],
    );
    repo.git(["config", "lfs.url", "alias:rest"]).ok();

    assert_endpoint(&repo, "http://actual-url/rest", "none");
}

#[test]
fn ambiguous_url_alias() {
    let t = Test::new();
    let repo = t.init("url-alias-ambiguous");
    insteadof(
        &repo,
        &[
            ("http://actual-url/", "alias:"),
            ("http://dupe-url", "alias:"),
        ],
    );
    repo.git(["config", "lfs.url", "alias:rest"]).ok();

    // Upstream writes `url.*..insteadof`, which the shell test's pattern
    // also matches.
    let run = env(&repo);
    let warned = run
        .output()
        .lines()
        .any(|l| l.starts_with("warning: Multiple 'url.*.") && l.contains(".insteadof'"));
    assert!(warned, "{run}");
}

#[test]
fn multiple_config() {
    let t = Test::new();
    let repo = t.init("url-alias-multiple");
    insteadof(
        &repo,
        &[
            ("http://wrong-url/", "alias"),
            ("http://actual-url/", "alias:"),
            ("http://actual-url/", "alias2:"),
        ],
    );

    for url in ["alias:rest", "alias2:rest"] {
        repo.git(["config", "lfs.url", url]).ok();
        assert_endpoint(&repo, "http://actual-url/rest", "none");
    }
}

#[test]
fn url_alias_must_be_prefix() {
    let t = Test::new();
    let repo = t.init("url-alias-bad");
    insteadof(&repo, &[("http://actual-url/", "alias:")]);
    repo.git(["config", "lfs.url", "badalias:rest"]).ok();

    env(&repo).has("SSH=badalias:rest");
}

const UNSAFE_WARNING: &str = "warning: These unsafe '.lfsconfig' keys were ignored:";

/// Check that `run`'s output has `line` as a whole line.
#[track_caller]
fn assert_line(run: &Run, line: &str) {
    assert!(
        run.output().lines().any(|l| l == line),
        "no {line:?}\n{run}"
    );
}

#[test]
fn config_ignoring_unsafe_lfsconfig_keys() {
    let t = Test::new();
    let repo = t.init("config-unsafe-lfsconfig-keys");
    lfsconfig(&repo, "core.askpass", "unsafe");

    env(&repo).has(UNSAFE_WARNING).has("  core.askpass");
}

#[test]
fn config_ignoring_non_lfs_remote_keys_in_lfsconfig() {
    let t = Test::new();
    let repo = t.init("config-unsafe-lfsconfig-remote");
    for key in [
        "remote.origin.nonlfs",
        "remote.other.origin.nonlfs",
        "remote.origin",
        "remote.lfsDefault",
        "remote.pushDefault",
    ] {
        lfsconfig(&repo, key, "unsafe");
    }

    let run = env(&repo).has(UNSAFE_WARNING);

    for key in [
        "remote.origin.nonlfs",
        "remote.other.origin.nonlfs",
        "remote.origin",
        "remote.lfsdefault",
        "remote.pushdefault",
    ] {
        assert_line(&run, &format!("  {key}"));
    }
}

#[test]
fn config_ignoring_non_lfs_access_keys_in_lfsconfig() {
    let t = Test::new();
    let repo = t.init("config-unsafe-lfsconfig-access");
    lfsconfig(&repo, "other.example.access", "unsafe");

    let run = env(&repo).has(UNSAFE_WARNING);

    assert_line(&run, "  other.example.access");
}

/// Check that `git lfs env` in `repo`, with `GIT_CONFIG` set to its
/// config, shows `endpoint`.
#[track_caller]
fn assert_endpoint_with_git_config(repo: &Repo, endpoint: &str) {
    repo.lfs_command()
        .args(["env"])
        .env("GIT_CONFIG", repo.path().join(".git/config"))
        .run()
        .ok()
        .has(&format!("Endpoint={endpoint} (auth=none)"));
}

#[test]
fn config_respects_include_directives_when_git_config_is_set() {
    let t = Test::new();
    let repo = t.init("include-directives");
    repo.git(["config", "lfs.url", "http://some-url/rest"]).ok();
    assert_endpoint_with_git_config(&repo, "http://some-url/rest");

    repo.git([
        "config",
        "--file",
        "./.git/b.config",
        "url.http://other-url/.insteadOf",
        "http://some-url/",
    ])
    .ok();
    let included = repo.path().join(".git/b.config");
    repo.git(["config", "include.path", included.to_str().unwrap()])
        .ok();
    assert_endpoint_with_git_config(&repo, "http://other-url/rest");
}

#[test]
fn config_env_with_duplicate_lfs_values() {
    let t = Test::new();
    let repo = t.init("config-duplicate-lfs-warning");
    for url in ["http://first-url", "http://second-url", "http://third-url"] {
        repo.git(["config", "--add", "lfs.url", url]).ok();
    }
    for url in ["http://first-url", "http://second-url"] {
        repo.git(["config", "--add", "remote.origin.pushurl", url])
            .ok();
    }

    let run = env(&repo);

    assert_eq!(
        run.count("warning: These `git config` values clash:"),
        2,
        "{run}"
    );
    for line in [
        r#"git config "lfs.url" = ["http://first-url"]"#,
        r#"git config "lfs.url" = "http://second-url""#,
        r#"git config "lfs.url" = ["http://first-url" "http://second-url"]"#,
        r#"git config "lfs.url" = "http://third-url""#,
    ] {
        assert_eq!(run.count(line), 1, "{line}\n{run}");
    }
    assert_eq!(
        run.count(r#"git config "remote.origin.pushurl""#),
        0,
        "{run}"
    );
}

/// Check that `git lfs env` in `repo` uses the real endpoint, without a
/// clash from an injected one.
#[track_caller]
fn assert_not_injected(repo: &Repo) {
    let run = env(repo).has("Endpoint=http://real-endpoint (auth=none)");
    assert_eq!(
        run.count("warning: These `git config` values clash:"),
        0,
        "{run}"
    );
    assert_eq!(
        run.count(r#"git config "lfs.url" = "http://injected-endpoint""#),
        0,
        "{run}"
    );
}

/// A newline in a value isn't read as the start of another key, whether
/// `.lfsconfig` comes from the working tree, the index, or HEAD.
#[test]
fn config_newline_in_lfsconfig_value_does_not_inject_another_key() {
    let t = Test::new();
    let repo = t.init("config-newline-lfsconfig");
    lfsconfig(&repo, "lfs.url", "http://real-endpoint");
    lfsconfig(
        &repo,
        "lfs.fetchinclude",
        "x\nlfs.url=http://injected-endpoint",
    );
    assert_not_injected(&repo);

    repo.git(["add", ".lfsconfig"]).ok();
    std::fs::remove_file(repo.path().join(".lfsconfig")).unwrap();
    assert_not_injected(&repo);

    repo.git(["commit", "-m", "add lfsconfig"]).ok();
    repo.git(["rm", "--cached", ".lfsconfig"]).ok();
    assert_not_injected(&repo);
}
