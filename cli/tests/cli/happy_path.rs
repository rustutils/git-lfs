//! `t-happy-path.sh`.
//!
//! "happy path on good ref" and "happy path on tracked ref" use servers
//! that need the ref in batch requests, so the pushes only succeed if it's
//! sent.

use crate::harness::{Test, oid};

#[test]
fn happy_path() {
    let t = Test::new();
    let name = "t-happy-path";
    t.remote(name);
    let clone = t.clone(name, "clone");
    let repo = t.clone(name, "repo");
    repo.lfs_command()
        .args(["track", "*.dat"])
        .merged()
        .run()
        .ok()
        .has("Tracking \"*.dat\"");
    let a = oid("a");
    repo.write("a.dat", "a");
    repo.git(["add", "a.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "add a.dat"])
        .ok()
        .has("main (root-commit)")
        .has("2 files changed")
        .has("create mode 100644 a.dat")
        .has("create mode 100644 .gitattributes");
    assert_eq!(repo.read("a.dat"), "a");
    repo.assert_pointer("main", "a.dat", &a, 1);
    t.assert_server_objects(name, &[], &[&a]);

    repo.command("git")
        .args(["push", "origin", "main"])
        .merged()
        .run()
        .ok()
        .has("Uploading LFS objects: 100% (1/1), 1 B")
        .has("main -> main");
    t.assert_server_objects(name, &[&a], &[]);

    clone.git(["pull", "origin", "main"]).ok();
    assert_eq!(clone.read("a.dat"), "a");
    clone.assert_pointer("main", "a.dat", &a, 1);
}

#[test]
fn happy_path_on_non_origin_remote() {
    let t = Test::new();
    let name = "happy-without-origin";
    t.remote(name);
    let repo = t.clone(name, "repo-without-origin");
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "track"]).ok();
    repo.git(["push", "origin", "main"]).ok();
    let clone = t.clone(name, "clone-without-origin");
    clone.git(["remote", "rename", "origin", "happy-path"]).ok();
    repo.write("a.dat", "a\n");
    repo.git(["add", "a.dat"]).ok();
    repo.git(["commit", "-m", "boom"]).ok();
    repo.git(["push", "origin", "main"]).ok();

    clone.git(["pull", "happy-path", "main"]).ok();

    assert_eq!(clone.read("a.dat"), "a\n");
}

/// The object ID of `a\n`.
const A_OID: &str = "87428fc522803d31065e7bce3cf03fe475096631e5e07bbd7a0fde60c4cf25c7";

#[test]
fn happy_path_on_good_ref() {
    let t = Test::new();
    let name = "happy-path-main-branch-required";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "a\n");
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();

    repo.git(["push", "origin", "main"]).ok();

    repo.assert_local_object(A_OID, 2);
    t.assert_server_objects(name, &[A_OID], &[]);
    let clone = t.clone(name, &format!("{name}-clone"));
    clone.assert_local_object(A_OID, 2);
}

#[test]
fn happy_path_on_tracked_ref() {
    let t = Test::new();
    let name = "happy-path-tracked-branch-required";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "a\n");
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();

    repo.git(["push", "origin", "main:tracked"]).ok();

    repo.assert_local_object(A_OID, 2);
    t.assert_server_objects(name, &[A_OID], &[]);
    repo.lfs(["clone", &t.remote_url(name), "--exclude", "*"])
        .ok();
    repo.git(["config", "push.default", "upstream"]).ok();
    repo.git(["config", "branch.main.merge", "refs/heads/tracked"])
        .ok();
    repo.git(["checkout"]).ok();
    repo.assert_local_object(A_OID, 2);
}

/// `git lfs env` removes temporary files for objects that are already in
/// the store, and leaves the others.
#[test]
fn clears_local_temp_objects() {
    let t = Test::new();
    let repo = t.init("repo-temp-objects");
    let good = "goodabcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwx";
    let bad = "badabcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxy";
    let object = format!(".git/lfs/objects/go/od/{good}");
    repo.write(&object, "");
    let tmp = |name: &str, suffix: &str| format!(".git/lfs/tmp/objects/{name}-{suffix}");
    for name in [good, bad] {
        for suffix in ["rand123", "rand456"] {
            repo.write(&tmp(name, suffix), "");
        }
    }

    repo.lfs_command()
        .args(["env"])
        .env("GIT_TRACE", "5")
        .run()
        .ok();

    let exists = |path: &str| repo.path().join(path).exists();
    assert!(exists(&object));
    for suffix in ["rand123", "rand456"] {
        assert!(exists(&tmp(bad, suffix)), "{bad}-{suffix}");
        assert!(!exists(&tmp(good, suffix)), "{good}-{suffix}");
    }
}
