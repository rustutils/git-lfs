//! `t-install-worktree.sh`.
//!
//! The shell suite skips itself with Git older than 2.20, which lacks the
//! worktreeConfig extension. These assume a newer Git. The fixtures are
//! shared with `t-uninstall-worktree.sh`.

use crate::harness::{Repo, Test};
use crate::install::{
    assert_config_error, assert_default_filters, assert_filters, assert_other_global_filters,
    assert_scope_conflict, set_other_global_filters,
};

/// A repository `name` with one commit, and a linked working tree for it in
/// `name-wt`, which this returns.
///
/// With `worktree_config`, the repository has the worktreeConfig extension
/// and the local filter settings [`assert_local_filters`] checks.
pub(crate) fn linked_worktree<'a>(t: &'a Test, name: &str, worktree_config: bool) -> Repo<'a> {
    let repo = t.init(name);
    if worktree_config {
        for (key, value) in [
            ("filter.lfs.smudge", "local smudge"),
            ("filter.lfs.clean", "local clean"),
            ("filter.lfs.process", "local filter"),
        ] {
            repo.git(["config", "--local", key, value]).ok();
        }
    }
    repo.write("a.txt", "");
    repo.git(["add", "a.txt"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    if worktree_config {
        repo.git(["config", "core.repositoryformatversion", "1"])
            .ok();
        repo.git(["config", "extensions.worktreeConfig", "true"])
            .ok();
    }
    let tree = format!("{name}-wt");
    repo.git(["worktree", "add", &format!("../{tree}")]).ok();
    t.repo(&tree)
}

/// Check that the local filter settings are the ones [`linked_worktree`]
/// set.
#[track_caller]
pub(crate) fn assert_local_filters(dir: &Repo, args: &[&str]) {
    assert_filters(dir, args, "local smudge", "local clean", "local filter");
}

#[test]
fn install_worktree_outside_repository() {
    let t = Test::new();
    let dir = t.repo("");

    let run = dir
        .lfs_command()
        .args(["install", "--worktree"])
        .run()
        .fails();

    assert_eq!(run.stdout(), "Not in a Git repository.\n");
}

#[test]
fn install_worktree_with_single_working_tree() {
    let t = Test::new();
    set_other_global_filters(&t);
    let repo = t.init("t-install-worktree-single-tree");

    repo.lfs(["install", "--worktree"]).ok();

    // Without the extension, --worktree is the local config.
    assert_default_filters(&repo, &[]);
    assert_default_filters(&repo, &["--local"]);
    assert_default_filters(&repo, &["--worktree"]);
    assert_other_global_filters(&repo);
}

#[test]
fn install_worktree_with_multiple_working_trees() {
    let t = Test::new();
    set_other_global_filters(&t);
    let tree = linked_worktree(&t, "t-install-worktree-multi-tree", true);

    tree.lfs(["install", "--worktree"]).ok();

    assert_default_filters(&tree, &[]);
    assert_default_filters(&tree, &["--worktree"]);
    assert_local_filters(&tree, &["--local"]);
    assert_other_global_filters(&tree);
}

#[test]
fn install_worktree_without_worktreeconfig_extension() {
    let t = Test::new();
    let tree = linked_worktree(&t, "t-install-worktree-multi-tree-no-config", false);

    let run = tree.lfs_command().args(["install", "--worktree"]).run();

    assert_eq!(run.code(), Some(2), "{run}");
    assert_config_error(&run);
}

#[test]
fn install_worktree_with_conflicting_scope() {
    let t = Test::new();
    let repo = t.init("t-install-worktree-scope-conflict");
    assert_scope_conflict(&repo, &["install", "--local", "--worktree"]);
    assert_scope_conflict(&repo, &["install", "--worktree", "--system"]);
    assert_scope_conflict(&repo, &["install", "--worktree", "--file", "test-file"]);
}
