//! `t-fetch-paths.sh`.
//!
//! The shell tests share one clone, each changing its config for the next.
//! Here each test sets up its own.

use crate::harness::{Repo, Test, oid};

const NAME: &str = "t-fetch-paths";

/// The remote repository with `dir/a.dat` pushed, and a clone of it
/// without local objects.
fn clone(t: &Test) -> Repo<'_> {
    t.remote(NAME);
    let repo = t.clone(NAME, "repo");
    repo.lfs_command()
        .args(["track", "*.dat"])
        .merged()
        .run()
        .ok()
        .has("Tracking \"*.dat\"");
    repo.write("dir/a.dat", "a");
    repo.git(["add", "dir/a.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "add dir/a.dat"])
        .ok()
        .has("main (root-commit)")
        .has("2 files changed")
        .has("create mode 100644 dir/a.dat")
        .has("create mode 100644 .gitattributes");
    assert_eq!(repo.read("dir/a.dat"), "a");
    repo.assert_local_object(&oid("a"), 1);
    t.assert_server_objects(NAME, &[], &[&oid("a")]);
    repo.command("git")
        .args(["push", "origin", "main"])
        .merged()
        .run()
        .ok()
        .has("Uploading LFS objects: 100% (1/1), 1 B")
        .has("main -> main");
    t.assert_server_objects(NAME, &[&oid("a")], &[]);

    let clone = t.clone(NAME, "clone");
    clone.remove_local_objects();
    clone
}

#[test]
fn init_fetch_unclean_paths() {
    let t = Test::new();
    clone(&t);
}

/// `dir/` has a trailing slash, which the filters still match.
#[test]
fn fetch_unclean_paths_with_include_filter_in_gitconfig() {
    let t = Test::new();
    let clone = clone(&t);
    clone.git(["config", "lfs.fetchinclude", "dir/"]).ok();

    clone.lfs(["fetch"]).ok();

    clone.assert_local_object(&oid("a"), 1);
}

#[test]
fn fetch_unclean_paths_with_exclude_filter_in_gitconfig() {
    let t = Test::new();
    let clone = clone(&t);
    clone.git(["config", "lfs.fetchexclude", "dir/"]).ok();

    clone.lfs(["fetch"]).ok();

    clone.refute_local_object(&oid("a"));
}

#[test]
fn fetch_unclean_paths_with_include_filter_in_cli() {
    let t = Test::new();
    let clone = clone(&t);

    clone.lfs(["fetch", "-I=dir/"]).ok();

    clone.assert_local_object(&oid("a"), 1);
}

#[test]
fn fetch_unclean_paths_with_exclude_filter_in_cli() {
    let t = Test::new();
    let clone = clone(&t);

    clone.lfs(["fetch", "-X=dir/"]).ok();

    clone.refute_local_object(&oid("a"));
}
