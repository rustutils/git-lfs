//! `t-migrate-import-no-rewrite.sh`.
//!
//! The shell tests mean to check that the import made a new commit, but
//! compare against an unset variable. These check it.

use crate::harness::{Repo, Run, Test, oid};
use crate::migrate_fixtures::{self as fixtures, Attrs};

/// Run `git lfs migrate import --no-rewrite --yes` with `args`, with stderr
/// merged into stdout.
fn import(repo: &Repo, args: &[&str]) -> Run {
    repo.lfs_command()
        .args(["migrate", "import", "--no-rewrite", "--yes"])
        .args(args)
        .merged()
        .run()
}

/// The object ID of the contents of `path` at `rev`.
fn oid_at(repo: &Repo, rev: &str, path: &str) -> String {
    oid(repo.blob(rev, path).expect("no such blob"))
}

/// Check that HEAD is a new commit on top of `prev`.
#[track_caller]
fn assert_new_commit(repo: &Repo, prev: &str) {
    assert_eq!(repo.rev_parse("HEAD~1"), prev, "history changed");
    assert_ne!(repo.rev_parse("HEAD"), prev, "no new commit");
}

/// The subject of the commit at HEAD.
fn subject(repo: &Repo) -> String {
    repo.git(["log", "-1", "--pretty=format:%s"]).ok().stdout()
}

#[test]
fn migrate_import_no_rewrite_default_branch() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_gitattrs(&t, Attrs::Plain);
    let txt = oid_at(&repo, "HEAD", "a.txt");
    let prev = repo.rev_parse("HEAD");

    import(&repo, &["a.txt"]).ok();

    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.assert_local_object(&txt, 120);
    assert_new_commit(&repo, &prev);
    assert_eq!(subject(&repo), "a.txt: convert to Git LFS");
    repo.git(["fsck"]).ok();
}

/// The shell test's fixture isn't bare.
#[test]
fn migrate_import_no_rewrite_bare_repository() {
    let t = Test::new();
    let repo = fixtures::single_remote_branch_with_gitattrs(&t);
    let prev = repo.rev_parse("HEAD");
    let txt = oid_at(&repo, "HEAD", "a.txt");
    let md = oid_at(&repo, "HEAD", "a.md");

    import(&repo, &["a.txt", "a.md"]).ok();

    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 30);
    repo.assert_pointer("refs/heads/main", "a.md", &md, 50);
    assert_new_commit(&repo, &prev);
}

#[test]
fn migrate_import_no_rewrite_multiple_branches() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_with_gitattrs(&t);
    let prev = repo.rev_parse("HEAD");
    let feature = repo.rev_parse("my-feature");
    let md = oid_at(&repo, "HEAD", "a.md");
    let txt = oid_at(&repo, "HEAD", "a.txt");
    let md_feature = oid_at(&repo, "my-feature", "a.md");

    import(&repo, &["a.txt", "a.md"]).ok();

    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.assert_local_object(&md, 140);
    repo.assert_local_object(&txt, 120);
    repo.refute_local_object(&md_feature);
    assert_eq!(repo.rev_parse("my-feature"), feature, "my-feature moved");
    assert_new_commit(&repo, &prev);
}

#[test]
fn migrate_import_no_rewrite_no_gitattributes() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let prev = repo.rev_parse("HEAD");

    import(&repo, &["a.txt", "a.md"])
        .fails()
        .has("No Git LFS filters found in '.gitattributes'");

    assert_eq!(repo.rev_parse("HEAD"), prev, "HEAD moved");
}

#[test]
fn migrate_import_no_rewrite_nested_gitattributes() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_nested_gitattrs(&t);
    let root = repo.blob("main", ".gitattributes").unwrap();
    assert!(!root.contains(".md"), "{root}");
    let nested = repo.blob("main", "b/.gitattributes").unwrap();
    assert!(
        nested.contains("*.md filter=lfs diff=lfs merge=lfs"),
        "{nested}"
    );
    let md = oid_at(&repo, "main", "a.md");
    let nested_md = oid_at(&repo, "main", "b/a.md");
    let txt = oid_at(&repo, "main", "a.txt");

    // b/a.md is tracked by b/.gitattributes alone.
    import(&repo, &["a.txt", "b/a.md"]).ok();

    repo.assert_pointer("refs/heads/main", "b/a.md", &nested_md, 140);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.assert_local_object(&nested_md, 140);
    repo.assert_local_object(&txt, 120);
    repo.refute_local_object(&md);

    // a.md isn't tracked anywhere.
    let head = repo.rev_parse("HEAD");
    import(&repo, &["a.md"])
        .fails()
        .has("a.md did not match any Git LFS filters in '.gitattributes'");
    assert_eq!(repo.rev_parse("HEAD"), head, "HEAD moved");
    repo.refute_pointer("refs/heads/main", "a.md");
}

/// Check that `--no-rewrite` with `--message` set to `message` makes a new
/// commit with that subject.
fn assert_message(message: &str) {
    let t = Test::new();
    let repo = fixtures::local_branch_with_gitattrs(&t, Attrs::Plain);
    let prev = repo.rev_parse("HEAD");

    import(&repo, &["--message", message, "a.txt"]).ok();

    assert_new_commit(&repo, &prev);
    assert_eq!(subject(&repo), message);
}

#[test]
fn migrate_import_no_rewrite_with_commit_message() {
    assert_message("run git-lfs migrate import --no-rewrite");
}

#[test]
fn migrate_import_no_rewrite_with_empty_commit_message() {
    assert_message("");
}

#[test]
fn migrate_import_no_rewrite_strict_gitattributes() {
    let t = Test::new();
    let repo = t.init("repo-strict-match");
    let foo = "major-oak/mainst/.yarn-offline-mirror/typescript-3.4.3.tgz";
    let bar = "major-oak/major-oak/frontend/.yarn-offline-mirror/typescript-2.9.2.tgz";
    repo.write(foo, "foo");
    repo.write(bar, "bar");
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", "Initial import"]).ok();
    repo.write(
        ".gitattributes",
        format!(
            "{foo} filter=lfs diff=lfs merge=lfs -text\n\
             {bar} filter=lfs diff=lfs merge=lfs -text\n"
        ),
    );
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", ".gitattributes"]).ok();

    import(&repo, &[foo, bar]).ok();

    repo.assert_pointer("refs/heads/main", foo, &oid("foo"), 3);
    repo.assert_pointer("refs/heads/main", bar, &oid("bar"), 3);
    repo.assert_local_object(&oid("foo"), 3);
    repo.assert_local_object(&oid("bar"), 3);
}
