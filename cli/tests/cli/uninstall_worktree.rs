//! `t-uninstall-worktree.sh`.
//!
//! Like `install_worktree`, these assume Git 2.20 or newer.

use crate::harness::Test;
use crate::install::{
    assert_config_error, assert_default_filters, assert_other_global_filters,
    assert_scope_conflict, config, set_other_global_filters,
};
use crate::install_worktree::{assert_local_filters, linked_worktree};
use crate::uninstall::{GLOBAL_REMOVED, assert_no_filters};

#[test]
fn uninstall_worktree_outside_repository() {
    let t = Test::new();
    let dir = t.repo("");

    let run = dir
        .lfs_command()
        .args(["uninstall", "--worktree"])
        .run()
        .fails();

    assert_eq!(run.stdout(), "Not in a Git repository.\n");
}

#[test]
fn uninstall_worktree_with_single_working_tree() {
    let t = Test::new();
    set_other_global_filters(&t);
    let repo = t.init("t-uninstall-worktree-single-tree");
    repo.lfs(["install", "--worktree"]).ok();
    assert_default_filters(&repo, &[]);
    assert_default_filters(&repo, &["--local"]);
    assert_default_filters(&repo, &["--worktree"]);
    assert_other_global_filters(&repo);

    repo.lfs_command()
        .args(["uninstall", "--worktree"])
        .merged()
        .run()
        .ok()
        .lacks(GLOBAL_REMOVED);

    assert_other_global_filters(&repo);
    assert_no_filters(&repo, &["--local"]);
    assert_no_filters(&repo, &["--worktree"]);
    assert_eq!(
        config(&repo, &[], "filter.lfs.smudge").as_deref(),
        Some("global smudge")
    );
}

#[test]
fn uninstall_worktree_with_multiple_working_trees() {
    let t = Test::new();
    set_other_global_filters(&t);
    let tree = linked_worktree(&t, "t-uninstall-worktree-multi-tree", true);
    tree.lfs(["install", "--worktree"]).ok();
    assert_default_filters(&tree, &[]);
    assert_default_filters(&tree, &["--worktree"]);
    assert_local_filters(&tree, &["--local"]);
    assert_other_global_filters(&tree);

    tree.lfs_command()
        .args(["uninstall", "--worktree"])
        .merged()
        .run()
        .ok()
        .lacks(GLOBAL_REMOVED);

    assert_other_global_filters(&tree);
    assert_local_filters(&tree, &[]);
    assert_local_filters(&tree, &["--local"]);
    assert_no_filters(&tree, &["--worktree"]);
}

/// Unlike install, a setting uninstall can't remove is only a warning.
#[test]
fn uninstall_worktree_without_worktreeconfig_extension() {
    let t = Test::new();
    let tree = linked_worktree(&t, "t-uninstall-worktree-multi-tree-no-config", false);

    let run = tree
        .lfs_command()
        .args(["uninstall", "--worktree"])
        .run()
        .ok();

    assert_config_error(&run);
}

#[test]
fn uninstall_worktree_with_conflicting_scope() {
    let t = Test::new();
    let repo = t.init("t-uninstall-worktree-scope-conflict");
    assert_scope_conflict(&repo, &["uninstall", "--local", "--worktree"]);
    assert_scope_conflict(&repo, &["uninstall", "--worktree", "--system"]);
    assert_scope_conflict(&repo, &["uninstall", "--worktree", "--file", "test-file"]);
}
