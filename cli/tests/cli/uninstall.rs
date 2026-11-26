//! `t-uninstall.sh`.
//!
//! Where the shell tests check one hook, these check all four.

use crate::harness::{Repo, Test};
use crate::install::{
    HOOKS, assert_default_filters, assert_other_global_filters, config, mkdir,
    set_other_global_filters,
};

const GLOBAL_REMOVED: &str = "Global Git LFS configuration has been removed.";

/// Check that none of the three filter settings that `git config` reads in
/// `dir` with `args` is set, or each is empty, as the shell's `[ "" = ...
/// ]` checks.
#[track_caller]
fn assert_no_filters(dir: &Repo, args: &[&str]) {
    for key in [
        "filter.lfs.smudge",
        "filter.lfs.clean",
        "filter.lfs.process",
    ] {
        assert_eq!(config(dir, args, key).unwrap_or_default(), "", "{key}");
    }
}

/// Check that each of the four hooks in `repo` exists or not, as `exists`
/// says.
#[track_caller]
fn assert_hooks_exist(repo: &Repo, exists: bool) {
    for name in HOOKS {
        let path = repo.path().join(".git/hooks").join(name);
        assert_eq!(path.exists(), exists, "{name}");
    }
}

/// A repository `name` with `git lfs install` run in it.
fn installed_repo<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.lfs(["install"]).ok();
    assert_hooks_exist(&repo, true);
    assert!(repo.read(".git/hooks/pre-push").contains("git-lfs"));
    assert_default_filters(&repo, &[]);
    repo
}

#[test]
fn uninstall_outside_repository() {
    let t = Test::new();
    let dir = mkdir(&t, "uninstall-test");
    assert_default_filters(&dir, &[]);

    // Uninstall more than once, for upstream's issue 529. The shell test
    // never runs it twice in a row, so this test adds that. Upstream warns
    // the second time, since the section is gone, but still succeeds.
    dir.lfs(["uninstall"]).ok();
    assert!(!dir.path().join("lfs").exists(), "lfs directory made");
    dir.lfs(["uninstall"]).ok();

    for opts in [&[][..], &["--skip-repo"]] {
        dir.lfs(["install"]).ok();

        let run = dir.lfs_command().args(["uninstall"]).args(opts).run().ok();

        assert!(run.stdout().contains(GLOBAL_REMOVED), "{run}");
        assert_no_filters(&dir, &["--global"]);
        let gitconfig = std::fs::read_to_string(t.home().join(".gitconfig")).unwrap();
        assert!(!gitconfig.contains("filter \"lfs\""), "{gitconfig}");
    }
}

#[test]
fn uninstall_outside_repository_without_access_to_git_lfs() {
    let t = Test::new();
    let dir = mkdir(&t, "uninstall-no-lfs");
    dir.write(".git/lfs", "");
    dir.write("lfs", "");
    for key in [
        "filter.lfs.smudge",
        "filter.lfs.clean",
        "filter.lfs.process",
    ] {
        assert_ne!(
            config(&dir, &["--global"], key).unwrap_or_default(),
            "",
            "{key}"
        );
    }

    dir.lfs(["uninstall"]).ok();

    assert_no_filters(&dir, &["--global"]);
}

#[test]
fn uninstall_inside_repository_with_skip_repo() {
    let t = Test::new();
    let repo = installed_repo(&t, "t-uninstall-skip-repo");

    repo.lfs(["uninstall", "--skip-repo"]).ok();

    assert_hooks_exist(&repo, true);
    assert_no_filters(&repo, &[]);
}

#[test]
fn uninstall_inside_repository_with_default_pre_push_hook() {
    let t = Test::new();
    let repo = installed_repo(&t, "t-uninstall-hook");

    repo.lfs(["uninstall"]).ok();

    assert_hooks_exist(&repo, false);
    assert_no_filters(&repo, &[]);
}

#[test]
fn uninstall_inside_repository_without_lfs_pre_push_hook() {
    let t = Test::new();
    let repo = installed_repo(&t, "t-uninstall-no-hook");
    repo.write(".git/hooks/pre-push", "something something git-lfs\n");

    repo.lfs(["uninstall"]).ok();

    assert_eq!(
        repo.read(".git/hooks/pre-push"),
        "something something git-lfs\n"
    );
    assert_no_filters(&repo, &[]);
}

#[test]
fn uninstall_hooks_inside_repository() {
    let t = Test::new();
    let repo = installed_repo(&t, "t-uninstall-only-hook");

    repo.lfs(["uninstall", "hooks"]).ok();

    assert_hooks_exist(&repo, false);
    assert_default_filters(&repo, &[]);
}

#[test]
fn uninstall_local_outside_repository() {
    let t = Test::new();
    let dir = t.repo("");

    let run = dir
        .lfs_command()
        .args(["uninstall", "--local"])
        .run()
        .fails();

    assert_eq!(run.stdout(), "Not in a Git repository.\n");
}

#[test]
fn uninstall_local_with_conflicting_scope() {
    let t = Test::new();
    let repo = t.init("t-uninstall-scope-conflict");

    let run = repo
        .lfs_command()
        .args(["uninstall", "--local", "--system"])
        .run()
        .fails();

    assert_eq!(
        run.stderr(),
        "Only one of the --local, --system, --worktree, and --file options \
         can be specified.\n"
    );
}

#[test]
fn uninstall_local() {
    let t = Test::new();
    set_other_global_filters(&t);
    let repo = t.init("t-uninstall-local");
    repo.lfs(["install", "--local"]).ok();
    assert_default_filters(&repo, &[]);
    assert_default_filters(&repo, &["--local"]);
    assert_other_global_filters(&repo);

    repo.lfs_command()
        .args(["uninstall", "--local"])
        .merged()
        .run()
        .ok()
        .lacks(GLOBAL_REMOVED);

    assert_other_global_filters(&repo);
    assert_no_filters(&repo, &["--local"]);
    assert_eq!(
        config(&repo, &[], "filter.lfs.smudge").as_deref(),
        Some("global smudge")
    );
}

/// `--file` is meant for other global config files, such as
/// `$XDG_CONFIG_HOME/git/config`, so it says the global config was
/// removed.
#[test]
fn uninstall_file() {
    let t = Test::new();
    set_other_global_filters(&t);
    let repo = t.init("t-uninstall-file");
    repo.lfs(["install", "--file=test-file"]).ok();
    assert_default_filters(&repo, &["--file", "test-file"]);
    assert_other_global_filters(&repo);

    repo.lfs_command()
        .args(["uninstall", "--file=test-file"])
        .merged()
        .run()
        .ok()
        .has(GLOBAL_REMOVED);

    assert_other_global_filters(&repo);
    assert_no_filters(&repo, &["--file", "test-file"]);
    assert_eq!(
        config(&repo, &[], "filter.lfs.smudge").as_deref(),
        Some("global smudge")
    );
}
