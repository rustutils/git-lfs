//! `t-push.sh`.

use crate::harness::{Repo, Test, oid};

/// A clone of a new remote repository `name`, with `a.dat` tracked and
/// committed but not pushed.
fn push_repo_setup<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    let key = format!("lfs.{}.locksverify", t.endpoint(name));
    repo.git(["config", &key, "false"]).ok();
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "push a\n");
    repo.commit_all("add a.dat");
    repo
}

#[test]
fn push_with_good_ref() {
    let t = Test::new();
    let repo = push_repo_setup(&t, "push-main-branch-required");
    repo.lfs(["push", "origin", "main"]).ok();
}

#[test]
fn push() {
    let t = Test::new();
    let repo = t.remote_and_clone("push");
    let key = format!("lfs.{}.locksverify", t.endpoint("push"));
    repo.git(["config", &key, "true"]).ok();
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "push a\n");
    repo.commit_all("add a.dat");
    let a = format!("push {} => a.dat", oid("push a\n"));
    let b = format!("push {} => b.dat", oid("push b\n"));

    let run = repo
        .lfs(["push", "--dry-run", "origin", "main"])
        .ok()
        .has(&a);
    assert_eq!(run.count_lines("push "), 1);

    repo.lfs(["push", "origin", "main"])
        .ok()
        .has("Uploading LFS objects: 100% (1/1), 7 B");
    assert_eq!(
        t.server().object("push", &oid("push a\n")).unwrap(),
        b"push a\n"
    );

    repo.git(["checkout", "-b", "push-b"]).ok();
    repo.write("b.dat", "push b\n");
    repo.commit_all("add b.dat");

    let run = repo
        .lfs(["push", "--dry-run", "origin", "push-b"])
        .ok()
        .has(&a)
        .has(&b);
    assert_eq!(run.count_lines("push "), 2);

    let run = repo
        .command(env!("CARGO_BIN_EXE_git-lfs"))
        .args(["push", "--dry-run", "origin", "--stdin"])
        .stdin("push-b\n\n")
        .run()
        .ok()
        .has(&a)
        .has(&b);
    assert_eq!(run.count_lines("push "), 2);

    // Simulate a remote ref.
    let head = repo.git(["rev-parse", "HEAD"]).ok().stdout();
    repo.write(".git/refs/remotes/origin/HEAD", head);
    let run = repo.lfs(["push", "--dry-run", "origin", "push-b"]).ok();
    assert_eq!(run.count_lines("push "), 0);
    std::fs::remove_dir_all(repo.path().join(".git/refs/remotes")).unwrap();

    repo.lfs(["push", "origin", "push-b"])
        .ok()
        .has("Uploading LFS objects: 100% (2/2), 14 B");
}
