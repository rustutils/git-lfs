//! `t-migrate-fixup.sh`.
//!
//! Where the shell tests grep `.gitattributes` for a line, these check
//! that `--fixup` left it as it was.

use crate::harness::{Repo, Run, Test, oid, sized};
use crate::migrate_fixtures::{self as fixtures, Attrs};

/// Run `git lfs migrate import --fixup --yes` with `args`, with stderr
/// merged into stdout.
fn fixup(repo: &Repo, args: &[&str]) -> Run {
    repo.lfs_command()
        .args(["migrate", "import", "--fixup", "--yes"])
        .args(args)
        .merged()
        .run()
}

/// The object ID of the contents of `path` at `rev`.
fn oid_at(repo: &Repo, rev: &str, path: &str) -> String {
    oid(repo.blob(rev, path).expect("no such blob"))
}

/// Check that `fixup --everything` on the corrupt fixture changed as
/// `attrs` says converts `a.txt` and leaves `.gitattributes` alone.
fn assert_fixes_a_txt(attrs: Attrs) {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked_corrupt(&t, attrs);
    let txt = oid_at(&repo, "main", "a.txt");
    let attributes = repo.blob("main", ".gitattributes");

    fixup(&repo, &["--everything"]).ok();

    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.assert_local_object(&txt, 120);
    assert_eq!(repo.blob("refs/heads/main", ".gitattributes"), attributes);
}

#[test]
fn migrate_import_fixup() {
    assert_fixes_a_txt(Attrs::Plain);
}

#[test]
fn migrate_import_fixup_special_attributes() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked_corrupt(&t, Attrs::Special);
    let files = ["a.txt", "a.dat", "a.bat", "a.out", "a.bin"];
    let oids: Vec<String> = files.iter().map(|f| oid_at(&repo, "main", f)).collect();

    fixup(&repo, &["--everything"]).ok();

    for (file, oid) in files.iter().zip(&oids) {
        repo.assert_pointer("refs/heads/main", file, oid, 120);
        repo.assert_local_object(oid, 120);
    }
}

#[test]
fn migrate_import_fixup_complex_nested() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_complex_tracked(&t);
    let a = oid_at(&repo, "main", "a.txt");
    let b = oid_at(&repo, "main", "dir/b.txt");
    let attributes = repo.blob("main", ".gitattributes");
    let dir_attributes = repo.blob("main", "dir/.gitattributes");

    fixup(&repo, &["--everything"]).ok();

    repo.assert_pointer("refs/heads/main", "a.txt", &a, 1);
    repo.refute_pointer("refs/heads/main", "dir/b.txt");
    repo.assert_local_object(&a, 1);
    repo.refute_local_object(&b);
    assert_eq!(repo.blob("refs/heads/main", ".gitattributes"), attributes);
    assert_eq!(
        repo.blob("refs/heads/main", "dir/.gitattributes"),
        dir_attributes
    );
}

/// Not from the shell suite: a directory's `.gitattributes` wins over
/// the root's, also when the directory sorts before `.gitattributes`.
#[test]
fn migrate_import_fixup_nested_dir_sorting_first() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_complex_tracked_dash_dir(&t);
    let a = oid_at(&repo, "main", "a.txt");

    fixup(&repo, &["--everything"]).ok();

    repo.assert_pointer("refs/heads/main", "a.txt", &a, 1);
    repo.refute_pointer("refs/heads/main", "-dir/b.txt");
}

/// Check that `fixup --everything` with `flag` fails with `message`, and
/// leaves `main` alone.
fn assert_refused(flag: &str, message: &str) {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked_corrupt(&t, Attrs::Plain);
    let main = repo.rev_parse("main");

    fixup(&repo, &["--everything", flag]).fails().has(message);

    assert_eq!(repo.rev_parse("main"), main, "main moved");
}

#[test]
fn migrate_import_fixup_include() {
    assert_refused(
        "--include=*.txt",
        "Cannot use --fixup with --include, --exclude",
    );
}

#[test]
fn migrate_import_fixup_exclude() {
    assert_refused(
        "--exclude=*.txt",
        "Cannot use --fixup with --include, --exclude",
    );
}

#[test]
fn migrate_import_fixup_no_rewrite() {
    assert_refused(
        "--no-rewrite",
        "--no-rewrite and --fixup cannot be combined",
    );
}

/// A clone has the remote's annotated tag, on a commit no branch has. The
/// fixup succeeds, though there's nothing to fix on `main`.
#[test]
fn migrate_import_fixup_with_remote_tags() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked_corrupt(&t, Attrs::Plain);
    repo.lfs(["uninstall"]).ok();
    repo.write("b.txt", sized("b.txt", 120));
    repo.git(["add", "b.txt"]).ok();
    repo.git(["commit", "-m", "b.txt"]).ok();
    repo.git(["tag", "-m", "tag1", "-a", "tag1"]).ok();
    repo.git(["reset", "--hard", "HEAD^"]).ok();
    repo.lfs(["install"]).ok();
    t.command(t.dir(), "git")
        .args(["clone", repo.path().to_str().unwrap(), "clone"])
        .run()
        .ok();
    let clone = t.repo("clone");

    fixup(&clone, &["main"]).ok();
}

#[test]
fn migrate_import_fixup_gitattributes_symlink() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked_corrupt(&t, Attrs::Link);
    let main = repo.rev_parse("main");

    fixup(&repo, &["--everything"])
        .fails()
        .has("expected '.gitattributes' to be a file, got a symbolic link");

    assert_eq!(repo.rev_parse("main"), main, "main moved");
    let tree = repo
        .git(["ls-tree", "main", "--", ".gitattributes"])
        .ok()
        .stdout();
    assert!(tree.starts_with("120000 blob "), "{tree}");
}

#[test]
fn migrate_import_fixup_gitattributes_with_macro() {
    assert_fixes_a_txt(Attrs::Macro);
}

#[test]
fn migrate_import_fixup_gitattributes_with_lfs_macro() {
    assert_fixes_a_txt(Attrs::LfsMacro);
}

/// Check that `fixup --everything` prints nothing on stdout and leaves
/// HEAD alone.
#[track_caller]
fn assert_nothing_to_fix(repo: &Repo) {
    let head = repo.rev_parse("HEAD");
    let run = repo
        .lfs_command()
        .args(["migrate", "import", "--everything", "--fixup", "--yes"])
        .run()
        .ok();
    assert_eq!(run.stdout(), "", "{run}");
    assert_eq!(repo.rev_parse("HEAD"), head, "HEAD moved");
}

#[test]
fn migrate_import_no_potential_fixup_fixup_no_gitattributes() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_nothing_to_fix(&repo);
}

#[test]
fn migrate_import_no_potential_fixup_fixup_gitattributes_with_macro() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    repo.write(".gitattributes", "[attr]foo foo\n");
    repo.write("a.md", sized("a.md macro", 30));
    repo.git(["add", ".gitattributes", "a.md"]).ok();
    repo.git(["commit", "-m", "macro"]).ok();
    assert_nothing_to_fix(&repo);
}
