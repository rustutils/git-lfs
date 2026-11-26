//! `t-migrate-import.sh`.
//!
//! Where the shell tests check that a `.gitattributes` line matches, these
//! check the whole line. Where they check that an object isn't stored with
//! some size, these check that it isn't stored at all.

use crate::harness::{Repo, Run, Test, oid};
use crate::migrate_fixtures::{self as fixtures, Attrs};

const MD: &str = "*.md filter=lfs diff=lfs merge=lfs -text";
const TXT: &str = "*.txt filter=lfs diff=lfs merge=lfs -text";

/// Run `git lfs migrate import` with `args`, with stderr merged into
/// stdout.
fn import(repo: &Repo, args: &[&str]) -> Run {
    repo.lfs_command()
        .args(["migrate", "import"])
        .args(args)
        .merged()
        .run()
}

/// The object ID of the contents of `path` at `rev`.
fn oid_at(repo: &Repo, rev: &str, path: &str) -> String {
    oid(repo.blob(rev, path).expect("no such blob"))
}

/// The lines of `.gitattributes` at `rev`, sorted, or `None` if there's
/// no such file.
fn attrs(repo: &Repo, rev: &str) -> Option<Vec<String>> {
    let blob = repo.blob(rev, ".gitattributes")?;
    let mut lines: Vec<String> = blob.lines().map(str::to_owned).collect();
    lines.sort();
    Some(lines)
}

/// Check that `.gitattributes` at `rev` has exactly `lines`, in any order.
#[track_caller]
fn assert_attrs(repo: &Repo, rev: &str, lines: &[&str]) {
    let mut expected: Vec<&str> = lines.to_vec();
    expected.sort();
    assert_eq!(
        attrs(repo, rev).expect("no .gitattributes"),
        expected,
        "{rev}"
    );
}

/// Check that there's no `.gitattributes` at `rev`.
#[track_caller]
fn refute_attrs(repo: &Repo, rev: &str) {
    assert_eq!(attrs(repo, rev), None, "{rev}");
}

/// Check that `ref` points where it did, at `before`.
#[track_caller]
fn assert_ref_unmoved(repo: &Repo, rev: &str, before: &str) {
    assert_eq!(repo.rev_parse(rev), before, "{rev} moved");
}

#[test]
fn migrate_import_default_branch() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let md = oid_at(&repo, "main", "a.md");
    let txt = oid_at(&repo, "main", "a.txt");
    let md_feature = oid_at(&repo, "my-feature", "a.md");

    import(&repo, &[]).ok();

    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.assert_local_object(&md, 140);
    repo.assert_local_object(&txt, 120);
    repo.refute_local_object(&md_feature);
    assert_attrs(&repo, "refs/heads/main", &[MD, TXT]);
    refute_attrs(&repo, "refs/heads/my-feature");
    let hook = std::fs::read_to_string(repo.git_dir().join("hooks/pre-push")).unwrap();
    assert!(hook.contains("git-lfs"), "{hook}");
}

/// The shell test's fixture isn't bare: it just runs with `--everything`.
#[test]
fn migrate_import_bare_repository() {
    let t = Test::new();
    let repo = fixtures::multiple_remote_branches(&t);
    import(&repo, &["--everything"]).ok();
}

#[test]
fn migrate_import_given_branch() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let md = oid_at(&repo, "main", "a.md");
    let txt = oid_at(&repo, "main", "a.txt");
    let md_feature = oid_at(&repo, "my-feature", "a.md");

    import(&repo, &["my-feature"]).ok();

    repo.assert_pointer("refs/heads/my-feature", "a.md", &md_feature, 30);
    repo.assert_pointer("refs/heads/my-feature", "a.txt", &txt, 120);
    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.assert_local_object(&md, 140);
    repo.assert_local_object(&md_feature, 30);
    repo.assert_local_object(&txt, 120);
    assert_attrs(&repo, "refs/heads/main", &[MD, TXT]);
    assert_attrs(&repo, "refs/heads/my-feature", &[MD, TXT]);
}

#[test]
fn migrate_import_default_branch_with_filter() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let md = oid_at(&repo, "main", "a.md");
    let txt = oid_at(&repo, "main", "a.txt");
    let md_feature = oid_at(&repo, "my-feature", "a.md");

    import(&repo, &["--include", "*.md"]).ok();

    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);
    repo.refute_pointer("refs/heads/main", "a.txt");
    repo.assert_local_object(&md, 140);
    repo.refute_local_object(&txt);
    repo.refute_local_object(&md_feature);
    assert_attrs(&repo, "refs/heads/main", &[MD]);
    refute_attrs(&repo, "refs/heads/my-feature");
}

#[test]
fn migrate_import_given_branch_with_filter() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let md = oid_at(&repo, "main", "a.md");
    let txt = oid_at(&repo, "main", "a.txt");
    let md_feature = oid_at(&repo, "my-feature", "a.md");

    import(&repo, &["--include", "*.md", "my-feature"]).ok();

    repo.assert_pointer("refs/heads/my-feature", "a.md", &md_feature, 30);
    repo.assert_pointer("refs/heads/my-feature~1", "a.md", &md, 140);
    repo.refute_pointer("refs/heads/my-feature", "a.txt");
    repo.assert_local_object(&md, 140);
    repo.assert_local_object(&md_feature, 30);
    repo.refute_local_object(&txt);
    assert_attrs(&repo, "refs/heads/main", &[MD]);
    assert_attrs(&repo, "refs/heads/my-feature", &[MD]);
}

#[test]
fn migrate_import_git_objects_symlink() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let other = t.dir().join("other");
    std::fs::create_dir(&other).unwrap();
    let objects = repo.path().join(".git/objects");
    std::fs::rename(&objects, other.join("objects")).unwrap();
    std::os::unix::fs::symlink("../../other/objects", &objects).unwrap();
    let md = oid_at(&repo, "main", "a.md");
    let txt = oid_at(&repo, "main", "a.txt");
    let md_feature = oid_at(&repo, "my-feature", "a.md");

    import(&repo, &[]).ok();

    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.assert_local_object(&md, 140);
    repo.assert_local_object(&txt, 120);
    repo.refute_local_object(&md_feature);
}

#[test]
fn migrate_import_default_branch_exclude_remote_refs() {
    let t = Test::new();
    let repo = fixtures::single_remote_branch(&t);
    let remote = "refs/remotes/origin/main";
    let md_remote = oid_at(&repo, remote, "a.md");
    let txt_remote = oid_at(&repo, remote, "a.txt");
    let md = oid_at(&repo, "main", "a.md");
    let txt = oid_at(&repo, "main", "a.txt");
    let remote_before = repo.rev_parse(remote);

    import(&repo, &[]).ok();

    repo.assert_pointer("refs/heads/main", "a.md", &md, 50);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 30);
    repo.assert_local_object(&md, 50);
    repo.assert_local_object(&txt, 30);
    repo.refute_local_object(&md_remote);
    repo.refute_local_object(&txt_remote);
    assert_attrs(&repo, "refs/heads/main", &[MD, TXT]);
    refute_attrs(&repo, remote);
    assert_ref_unmoved(&repo, remote, &remote_before);
}

#[test]
fn migrate_import_given_branch_exclude_remote_refs() {
    let t = Test::new();
    let repo = fixtures::multiple_remote_branches(&t);
    let remote = "refs/remotes/origin/main";
    let md_main = oid_at(&repo, "main", "a.md");
    let md_remote = oid_at(&repo, remote, "a.md");
    let md_feature = oid_at(&repo, "my-feature", "a.md");
    let txt_main = oid_at(&repo, "main", "a.txt");
    let txt_remote = oid_at(&repo, remote, "a.txt");
    let txt_feature = oid_at(&repo, "my-feature", "a.txt");

    import(&repo, &["my-feature"]).ok();

    repo.assert_pointer("refs/heads/main", "a.md", &md_main, 21);
    repo.assert_pointer("refs/heads/my-feature", "a.md", &md_feature, 31);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt_main, 20);
    repo.assert_pointer("refs/heads/my-feature", "a.txt", &txt_feature, 30);
    repo.assert_local_object(&md_feature, 31);
    repo.assert_local_object(&md_main, 21);
    repo.assert_local_object(&txt_feature, 30);
    repo.assert_local_object(&txt_main, 20);
    repo.refute_local_object(&md_remote);
    repo.refute_local_object(&txt_remote);
    assert_attrs(&repo, "refs/heads/main", &[MD, TXT]);
    refute_attrs(&repo, remote);
    assert_attrs(&repo, "refs/heads/my-feature", &[MD, TXT]);
}

#[test]
fn migrate_import_given_ref_skip_fetch() {
    let t = Test::new();
    let repo = fixtures::single_remote_branch(&t);
    let remote = "refs/remotes/origin/main";
    let md_main = oid_at(&repo, "main", "a.md");
    let md_remote = oid_at(&repo, remote, "a.md");
    let txt_main = oid_at(&repo, "main", "a.txt");
    let txt_remote = oid_at(&repo, remote, "a.txt");
    repo.git(["tag", "pseudo-remote", &repo.rev_parse(remote)])
        .ok();
    // Without the remote ref, and without fetching it again, nothing is
    // excluded.
    repo.git(["update-ref", "-d", remote]).ok();

    import(&repo, &["--skip-fetch"]).ok();

    repo.assert_pointer("refs/heads/main", "a.md", &md_main, 50);
    repo.assert_pointer("pseudo-remote", "a.md", &md_remote, 140);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt_main, 30);
    repo.assert_pointer("pseudo-remote", "a.txt", &txt_remote, 120);
    repo.assert_local_object(&md_main, 50);
    repo.assert_local_object(&txt_main, 30);
    repo.assert_local_object(&md_remote, 140);
    repo.assert_local_object(&txt_remote, 120);
    assert_attrs(&repo, "refs/heads/main", &[MD, TXT]);
    assert_attrs(&repo, "pseudo-remote", &[MD, TXT]);
}

/// Check that `migrate import --everything` follows the tag `v1.0.0` from
/// [`fixtures::single_local_branch_with_tags`].
fn assert_tag_follows(annotated: bool) {
    let t = Test::new();
    let repo = fixtures::single_local_branch_with_tags(&t, annotated);
    let txt = oid_at(&repo, "main", "a.txt");

    import(&repo, &["--everything"]).ok();

    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 2);
    repo.assert_local_object(&txt, 2);
    let tags = repo.git(["tag", "--points-at", "HEAD"]).ok();
    assert_eq!(tags.stdout(), "v1.0.0\n", "{tags}");
}

#[test]
fn migrate_import_un_annotated_tags() {
    assert_tag_follows(false);
}

#[test]
fn migrate_import_annotated_tags() {
    assert_tag_follows(true);
}

/// The oids of `a.md` and `a.txt` on `main`, `origin/main`, and
/// `my-feature` of [`fixtures::multiple_remote_branches`].
struct RemoteBranchOids {
    md_main: String,
    md_remote: String,
    md_feature: String,
    txt_main: String,
    txt_remote: String,
    txt_feature: String,
}

impl RemoteBranchOids {
    fn new(repo: &Repo) -> Self {
        let remote = "refs/remotes/origin/main";
        Self {
            md_main: oid_at(repo, "main", "a.md"),
            md_remote: oid_at(repo, remote, "a.md"),
            md_feature: oid_at(repo, "my-feature", "a.md"),
            txt_main: oid_at(repo, "main", "a.txt"),
            txt_remote: oid_at(repo, remote, "a.txt"),
            txt_feature: oid_at(repo, "my-feature", "a.txt"),
        }
    }
}

/// Check that migrating `my-feature` but not `main`, with `args`, migrated
/// only the commit on `my-feature`.
fn assert_feature_only(args: &[&str]) {
    let t = Test::new();
    let repo = fixtures::multiple_remote_branches(&t);
    let oids = RemoteBranchOids::new(&repo);
    let main = repo.rev_parse("main");

    import(&repo, args).ok();

    repo.assert_pointer("refs/heads/my-feature", "a.md", &oids.md_feature, 31);
    repo.assert_pointer("refs/heads/my-feature", "a.txt", &oids.txt_feature, 30);
    repo.assert_local_object(&oids.md_feature, 31);
    repo.refute_local_object(&oids.md_main);
    repo.assert_local_object(&oids.txt_feature, 30);
    repo.refute_local_object(&oids.txt_main);
    repo.refute_local_object(&oids.md_remote);
    repo.refute_local_object(&oids.txt_remote);
    assert_ref_unmoved(&repo, "main", &main);
    refute_attrs(&repo, "refs/heads/main");
    refute_attrs(&repo, "refs/remotes/origin/main");
    assert_attrs(&repo, "refs/heads/my-feature", &[MD, TXT]);
}

#[test]
fn migrate_import_include_exclude_ref() {
    assert_feature_only(&[
        "--include-ref=refs/heads/my-feature",
        "--exclude-ref=refs/heads/main",
    ]);
}

#[test]
fn migrate_import_include_exclude_ref_args() {
    assert_feature_only(&["my-feature", "^main"]);
}

#[test]
fn migrate_import_include_exclude_ref_with_filter() {
    let t = Test::new();
    let repo = fixtures::multiple_remote_branches(&t);
    let oids = RemoteBranchOids::new(&repo);

    import(
        &repo,
        &[
            "--include=*.txt",
            "--include-ref=refs/heads/my-feature",
            "--exclude-ref=refs/heads/main",
        ],
    )
    .ok();

    repo.assert_pointer("refs/heads/my-feature", "a.txt", &oids.txt_feature, 30);
    repo.refute_pointer("refs/heads/my-feature", "a.md");
    repo.refute_local_object(&oids.md_feature);
    repo.refute_local_object(&oids.md_main);
    repo.assert_local_object(&oids.txt_feature, 30);
    repo.refute_local_object(&oids.txt_main);
    repo.refute_local_object(&oids.md_remote);
    repo.refute_local_object(&oids.txt_remote);
    refute_attrs(&repo, "refs/heads/main");
    refute_attrs(&repo, "refs/remotes/origin/main");
    assert_attrs(&repo, "refs/heads/my-feature", &[TXT]);
}

#[test]
fn migrate_import_invalid_ref() {
    let t = Test::new();
    let repo = t.init("migrate-import-invalid-ref");
    repo.git(["commit", "--allow-empty", "-m", "initial commit"])
        .ok();
    import(&repo, &["--yes", "jibberish"])
        .fails()
        .has("can't resolve ref");
}

/// Check that `--above 121B` migrates `name`, 140 bytes, but not `a.txt`,
/// 120 bytes, from [`fixtures::single_local_branch_untracked`], and tracks
/// it by its path.
fn assert_above(name: &str) {
    let t = Test::new();
    let repo = fixtures::single_local_branch_untracked(&t, name);
    let big = oid_at(&repo, "main", name);
    let txt = oid_at(&repo, "main", "a.txt");

    import(&repo, &["--above", "121B"]).ok();

    repo.assert_local_object(&big, 140);
    repo.assert_pointer("refs/heads/main", name, &big, 140);
    repo.refute_pointer("refs/heads/main", "a.txt");
    repo.refute_local_object(&txt);
    let line = format!("/{name} filter=lfs diff=lfs merge=lfs -text");
    assert_attrs(&repo, "refs/heads/main", &[&line]);
    repo.git(["check-attr", "filter", "--", "a.txt"])
        .ok()
        .has("filter: unspecified");
}

#[test]
fn migrate_import_above() {
    assert_above("a.md");
}

#[test]
fn migrate_import_above_without_extension() {
    assert_above("just-b");
}

/// `b.txt` sorts after `a.txt`, so a pattern from it mustn't match `a.txt`
/// too.
#[test]
fn migrate_import_above_with_multiple_files() {
    assert_above("b.txt");
}

#[test]
fn migrate_import_above_with_include_or_exclude() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_untracked(&t, "a.md");
    let main = repo.rev_parse("main");
    for args in [
        &["--above", "121B", "--include", "*.md"][..],
        &["--above", "121B", "--exclude", "*.txt"],
        &["--above", "121B", "--fixup"],
    ] {
        import(&repo, args).fails();
    }
    assert_ref_unmoved(&repo, "main", &main);
}

#[test]
fn migrate_import_existing_gitattributes() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_gitattrs(&t, Attrs::Plain);
    let txt = oid_at(&repo, "main", "a.txt");

    import(
        &repo,
        &["--yes", "--include-ref=refs/heads/main", "--include=*.txt"],
    )
    .ok();

    repo.assert_local_object(&txt, 120);
    let main = repo.blob("refs/heads/main", ".gitattributes");
    assert_eq!(
        main.as_deref(),
        Some(
            "*.txt filter=lfs diff=lfs merge=lfs -text\n\
             *.other filter=lfs diff=lfs merge=lfs -text\n"
        )
    );
    let prev = repo.blob("refs/heads/main^1", ".gitattributes");
    assert_eq!(
        prev.as_deref(),
        Some("*.txt filter=lfs diff=lfs merge=lfs -text\n")
    );
}

#[test]
fn migrate_import_exclude_with_existing_gitattributes() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_gitattrs(&t, Attrs::Plain);
    let txt = oid_at(&repo, "main", "a.txt");

    import(
        &repo,
        &[
            "--yes",
            "--include-ref=refs/heads/main",
            "--include=*.txt",
            "--exclude=*.bin",
        ],
    )
    .ok();

    repo.assert_local_object(&txt, 120);
    let main = repo.blob("refs/heads/main", ".gitattributes");
    assert_eq!(
        main.as_deref(),
        Some(
            "*.txt filter=lfs diff=lfs merge=lfs -text\n\
             *.other filter=lfs diff=lfs merge=lfs -text\n\
             *.bin !text -filter -merge -diff\n"
        )
    );
    let prev = repo.blob("refs/heads/main^1", ".gitattributes");
    assert_eq!(
        prev.as_deref(),
        Some(
            "*.txt filter=lfs diff=lfs merge=lfs -text\n\
             *.bin !text -filter -merge -diff\n"
        )
    );
}

#[test]
fn migrate_import_existing_gitattributes_with_different_permissions() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_gitattrs(&t, Attrs::Executable);
    let txt = oid_at(&repo, "main", "a.txt");
    assert!(fixtures::is_executable(&repo, ".gitattributes"));

    import(
        &repo,
        &["--yes", "--include-ref=refs/heads/main", "--include=*.txt"],
    )
    .ok();

    assert!(!fixtures::is_executable(&repo, ".gitattributes"));
    repo.assert_local_object(&txt, 120);
    let main = repo.blob("refs/heads/main", ".gitattributes");
    assert_eq!(
        main.as_deref(),
        Some(
            "*.txt filter=lfs diff=lfs merge=lfs -text\n\
             *.other filter=lfs diff=lfs merge=lfs -text\n"
        )
    );
    let prev = repo.blob("refs/heads/main^1", ".gitattributes");
    assert_eq!(
        prev.as_deref(),
        Some("*.txt filter=lfs diff=lfs merge=lfs -text\n")
    );
    let tree = repo.git(["ls-tree", "refs/heads/main"]).ok().stdout();
    let modes: Vec<&str> = tree.lines().map(|l| &l[..6]).collect();
    assert_eq!(modes, ["100644", "100644"], "{tree}");
}

#[test]
fn migrate_import_existing_gitattributes_symlink() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_gitattrs(&t, Attrs::Link);
    let main = repo.rev_parse("main");

    import(
        &repo,
        &["--yes", "--include-ref=refs/heads/main", "--include=*.txt"],
    )
    .fails()
    .has("expected '.gitattributes' to be a file, got a symbolic link");

    assert_ref_unmoved(&repo, "main", &main);
    let tree = repo
        .git(["ls-tree", "main", "--", ".gitattributes"])
        .ok()
        .stdout();
    assert!(tree.starts_with("120000 blob "), "{tree}");
}

#[test]
fn migrate_import_identical_contents_different_permissions() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    repo.write("foo.dat", "foo\n");
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", "add file"]).ok();
    fixtures::make_executable(&repo, "foo.dat");
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", "make file executable"]).ok();
    assert!(fixtures::is_executable(&repo, "foo.dat"));

    import(&repo, &["--everything", "--include=*.dat"]).ok();

    assert!(fixtures::is_executable(&repo, "foo.dat"));
    let tree = repo.git(["ls-tree", "main", "--", "foo.dat"]).ok().stdout();
    assert!(tree.starts_with("100755 blob "), "{tree}");
    let before = repo
        .git(["ls-tree", "main~1", "--", "foo.dat"])
        .ok()
        .stdout();
    assert!(before.starts_with("100644 blob "), "{before}");
}

#[test]
fn migrate_import_tags_with_same_name_as_branches() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    repo.write("hello.dat", "hello");
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", "add file"]).ok();
    repo.git(["branch", "foo"]).ok();
    repo.git(["tag", "foo"]).ok();
    repo.git(["tag", "bar"]).ok();

    import(&repo, &["--everything", "--include=*.dat"]).ok();

    let branch = repo.rev_parse("refs/heads/foo");
    assert_eq!(branch, repo.rev_parse("refs/tags/foo"));
    assert_eq!(branch, repo.rev_parse("refs/tags/bar"));
    repo.assert_pointer("refs/heads/foo", "hello.dat", &oid("hello"), 5);
}

/// The fixture made bare, unlike the other test of this name.
#[test]
fn migrate_import_bare_repository_made_bare() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let md = oid_at(&repo, "main", "a.md");
    let repo = fixtures::make_bare(&t, repo);

    import(&repo, &["--include-ref=main"]).ok();

    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);
    repo.assert_local_object(&md, 140);
}

#[test]
fn migrate_import_nested_sub_trees_no_filter() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_deep_trees(&t);
    let txt = oid_at(&repo, "main", "foo/bar/baz/a.txt");

    import(&repo, &["--everything"]).ok();

    repo.assert_pointer("main", "foo/bar/baz/a.txt", &txt, 120);
    repo.assert_local_object(&txt, 120);
}

#[test]
fn migrate_import_prefix_includes() {
    for include in ["foo/bar/baz/**", "foo/**/baz/a.txt", "*.txt"] {
        let t = Test::new();
        let repo = fixtures::single_local_branch_deep_trees(&t);
        let txt = oid_at(&repo, "main", "foo/bar/baz/a.txt");

        import(&repo, &[&format!("--include={include}")]).ok();

        repo.assert_local_object(&txt, 120);
    }
}

#[test]
fn migrate_import_everything() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let txt = oid_at(&repo, "main", "a.txt");
    let md = oid_at(&repo, "main", "a.md");
    let md_feature = oid_at(&repo, "my-feature", "a.md");

    import(&repo, &["--everything"]).ok();

    repo.assert_pointer("main", "a.txt", &txt, 120);
    repo.assert_pointer("main", "a.md", &md, 140);
    repo.assert_pointer("my-feature", "a.md", &md_feature, 30);
}

#[test]
fn migrate_import_ambiguous_reference() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    // A tag named like the object ID of HEAD.
    let head = repo.rev_parse("HEAD");
    repo.git(["tag", &head]).ok();

    import(&repo, &["--everything"]).ok();
}

#[test]
fn migrate_import_everything_with_args() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let run = import(&repo, &["--everything", "main"]).fails();
    assert_eq!(
        run.stdout().trim_end(),
        "Cannot use --everything with explicit reference arguments",
        "{run}"
    );
}

#[test]
fn migrate_import_everything_with_include_ref() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let run = import(&repo, &["--everything", "--include-ref=refs/heads/main"]).fails();
    assert_eq!(
        run.stdout().trim_end(),
        "Cannot use --everything with --include-ref or --exclude-ref",
        "{run}"
    );
}

#[test]
fn migrate_import_everything_with_exclude_ref() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let run = import(&repo, &["--everything", "--exclude-ref=refs/heads/main"]).fails();
    assert_eq!(
        run.stdout().trim_end(),
        "Cannot use --everything with --include-ref or --exclude-ref",
        "{run}"
    );
}

#[test]
fn migrate_import_everything_and_include_with_glob_pattern() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let md_main = oid_at(&repo, "main", "a.md");
    let txt_main = oid_at(&repo, "main", "a.txt");
    let md_feature = oid_at(&repo, "my-feature", "a.md");
    let txt_feature = oid_at(&repo, "my-feature", "a.txt");
    let head = repo.rev_parse("HEAD");

    import(
        &repo,
        &["--verbose", "--everything", "--include=*.[mM][dD]", "--yes"],
    )
    .ok()
    .has(&format!("  commit {head}: a.md"));

    repo.assert_pointer("refs/heads/main", "a.md", &md_main, 140);
    repo.assert_pointer("refs/heads/my-feature", "a.md", &md_feature, 30);
    repo.assert_local_object(&md_main, 140);
    repo.assert_local_object(&md_feature, 30);
    repo.refute_local_object(&txt_main);
    repo.refute_local_object(&txt_feature);
}

#[test]
fn migrate_import_everything_with_tag_pointing_to_tag() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let md_main = oid_at(&repo, "main", "a.md");
    let txt_main = oid_at(&repo, "main", "a.txt");
    let md_feature = oid_at(&repo, "my-feature", "a.md");
    let txt_feature = oid_at(&repo, "my-feature", "a.txt");
    repo.git(["tag", "-a", "-m", "abc", "abc", "refs/heads/main"])
        .ok();
    repo.git(["tag", "-a", "-m", "def", "def", "refs/tags/abc"])
        .ok();
    let head = repo.rev_parse("HEAD");

    import(
        &repo,
        &["--verbose", "--everything", "--include=*.[mM][dD]", "--yes"],
    )
    .ok()
    .has(&format!("  commit {head}: a.md"));

    repo.assert_pointer("refs/heads/main", "a.md", &md_main, 140);
    repo.assert_pointer("refs/tags/abc", "a.md", &md_main, 140);
    repo.assert_pointer("refs/tags/def", "a.md", &md_main, 140);
    repo.assert_pointer("refs/heads/my-feature", "a.md", &md_feature, 30);
    // `--points-at` lists the tag itself too.
    let at_abc = repo.git(["tag", "--points-at", "refs/tags/abc"]).ok();
    assert!(at_abc.stdout().lines().any(|l| l == "def"), "{at_abc}");
    let at_def = repo.git(["tag", "--points-at", "refs/tags/def"]).ok();
    assert!(!at_def.stdout().lines().any(|l| l == "abc"), "{at_def}");
    repo.assert_local_object(&md_main, 140);
    repo.assert_local_object(&md_feature, 30);
    repo.refute_local_object(&txt_main);
    repo.refute_local_object(&txt_feature);
}

#[test]
fn migrate_import_nested_sub_trees_and_include_with_wildcard() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_deep_trees(&t);
    let txt = oid_at(&repo, "main", "foo/bar/baz/a.txt");

    import(&repo, &["--include=**/*ar/**"]).ok();

    repo.assert_pointer("refs/heads/main", "foo/bar/baz/a.txt", &txt, 120);
    repo.assert_local_object(&txt, 120);
}

#[test]
fn migrate_import_handle_copies_of_files() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_deep_trees(&t);
    std::fs::copy(
        repo.path().join("foo/bar/baz/a.txt"),
        repo.path().join("a.txt"),
    )
    .unwrap();
    repo.git(["add", "a.txt"]).ok();
    repo.git(["commit", "-m", "duplicated file"]).ok();
    let txt = oid_at(&repo, "main", "foo/bar/baz/a.txt");
    let root = repo.blob("main", "a.txt");

    import(&repo, &["--include=foo/**"]).ok();

    repo.assert_pointer("refs/heads/main", "foo/bar/baz/a.txt", &txt, 120);
    repo.assert_local_object(&txt, 120);
    // a.txt isn't under foo, so it stays out of LFS.
    assert_eq!(repo.blob("main", "a.txt"), root);
}

/// Check that `--include=*.txt` migrates the files that end in `.txt`, but
/// not `bar.txt/b.md`, with `lfs.pathFilterCacheSize` set to `cache`.
fn assert_filter_matches_files_only(cache: Option<&str>) {
    let t = Test::new();
    let repo = fixtures::single_local_branch_same_file_tree_ext(&t);
    let txt_root = oid_at(&repo, "main", "a.txt");
    let txt_foo = oid_at(&repo, "main", "foo/a.txt");
    let md_bar = oid_at(&repo, "main", "bar.txt/b.md");
    let txt_bar = oid_at(&repo, "main", "bar.txt/b.txt");
    if let Some(cache) = cache {
        repo.git(["config", "lfs.pathFilterCacheSize", cache]).ok();
    }

    import(&repo, &["--include=*.txt"]).ok();

    repo.assert_local_object(&txt_root, 120);
    repo.assert_local_object(&txt_foo, 120);
    repo.assert_local_object(&txt_bar, 120);
    repo.refute_local_object(&md_bar);
    repo.refute_pointer("main", "bar.txt/b.md");
}

#[test]
fn migrate_import_filter_matches_files_only_path_cache_settings() {
    assert_filter_matches_files_only(None);
    for cache in ["none", "1", "2", "unlimited", "not valid"] {
        assert_filter_matches_files_only(Some(cache));
    }
}

/// Every commit reachable from a ref, in `git log` order.
fn all_commits(repo: &Repo) -> Vec<String> {
    let log = repo.git(["log", "--all", "--pretty=format:%H"]).ok();
    log.stdout().lines().map(str::to_owned).collect()
}

#[test]
fn migrate_import_object_map() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let map = t.dir().join("object-map.txt");
    let old = all_commits(&repo);

    import(
        &repo,
        &["--everything", "--object-map", map.to_str().unwrap()],
    )
    .ok();

    let new = all_commits(&repo);
    let mut expected: Vec<String> = old
        .iter()
        .zip(&new)
        .map(|(old, new)| format!("{old},{new}"))
        .collect();
    expected.sort();
    let contents = std::fs::read_to_string(&map).unwrap();
    let mut actual: Vec<&str> = contents.lines().collect();
    actual.sort();
    assert_eq!(actual, expected);
    assert_ne!(old, new);
}

#[test]
fn migrate_import_include_with_space() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_space(&t);
    let txt = oid_at(&repo, "main", "a file.txt");

    import(&repo, &["--include", "a file.txt"]).ok();

    repo.assert_pointer("refs/heads/main", "a file.txt", &txt, 50);
    let attrs = repo.read(".gitattributes");
    assert_eq!(attrs.matches("a[[:space:]]file.txt").count(), 1, "{attrs}");
}

#[test]
fn migrate_import_handle_symbolic_link() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_symlink(&t);
    let txt = oid_at(&repo, "main", "a.txt");
    let link = oid_at(&repo, "main", "link.txt");

    import(&repo, &["--include=*.txt"]).ok();

    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.assert_local_object(&txt, 120);
    // link.txt is a symbolic link, so it stays out of LFS.
    repo.refute_local_object(&link);
    let tree = repo
        .git(["ls-tree", "main", "--", "link.txt"])
        .ok()
        .stdout();
    assert!(tree.starts_with("120000 blob "), "{tree}");
    assert_eq!(repo.blob("main", "link.txt").as_deref(), Some("a.txt"));
}

#[test]
fn migrate_import_commit_allow_empty() {
    let t = Test::new();
    let repo = t.init("migrate---allow-empty");
    repo.git(["commit", "--allow-empty", "-m", "initial commit"])
        .ok();
    let head = repo.rev_parse("HEAD");

    import(&repo, &["--everything"]).ok();

    assert_ref_unmoved(&repo, "HEAD", &head);
}

#[test]
fn migrate_import_multiple_remotes() {
    let t = Test::new();
    let repo = fixtures::multiple_remotes(&t);
    let main = repo.rev_parse("main");

    import(&repo, &[]).ok();

    assert_ref_unmoved(&repo, "main", &main);
}

/// Check that `migrate import --everything` with `answer` on stdin refuses
/// to migrate the dirty working tree of
/// [`fixtures::local_branch_with_dirty_copy`], asking `prompts` times.
fn assert_dirty_refused(answer: &str, prompts: usize) {
    let t = Test::new();
    let repo = fixtures::local_branch_with_dirty_copy(&t);
    let main = repo.rev_parse("main");

    let run = repo
        .lfs_command()
        .args(["migrate", "import", "--everything"])
        .stdin(answer)
        .merged()
        .run()
        .has("working copy must not be dirty");
    let asked = run
        .output()
        .matches("override changes in your working copy")
        .count();
    assert_eq!(asked, prompts, "{run}");

    assert_ref_unmoved(&repo, "main", &main);
}

#[test]
fn migrate_import_dirty_copy_default_negative_answer() {
    assert_dirty_refused("\n", 1);
}

#[test]
fn migrate_import_dirty_copy_negative_answer() {
    assert_dirty_refused("n\n", 1);
}

#[test]
fn migrate_import_dirty_copy_unknown_then_negative_answer() {
    assert_dirty_refused("x\nn\n", 2);
}

#[test]
fn migrate_import_dirty_copy_positive_answer() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_dirty_copy(&t);
    let txt = oid("a.txt");

    repo.lfs_command()
        .args(["migrate", "import", "--everything"])
        .stdin("y\n")
        .merged()
        .run()
        .ok()
        .has("changes in your working copy will be overridden ...");

    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 5);
    repo.assert_local_object(&txt, 5);
}

#[test]
fn migrate_import_non_standard_refs() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_non_standard(&t);
    let md = oid_at(&repo, "main", "a.md");
    let txt = oid_at(&repo, "main", "a.txt");
    let md_feature = oid_at(&repo, "my-feature", "a.md");

    import(&repo, &["--everything"]).ok();

    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.assert_pointer("refs/pull/1/base", "a.md", &md, 140);
    repo.assert_pointer("refs/pull/1/base", "a.txt", &txt, 120);
    repo.assert_pointer("refs/heads/my-feature", "a.md", &md_feature, 30);
    repo.assert_pointer("refs/heads/my-feature", "a.txt", &txt, 120);
    repo.assert_pointer("refs/pull/1/head", "a.md", &md_feature, 30);
    repo.assert_pointer("refs/pull/1/head", "a.txt", &txt, 120);
    repo.assert_local_object(&md, 140);
    repo.assert_local_object(&txt, 120);
    repo.assert_local_object(&md_feature, 30);
}

/// The lines of `.gitattributes` in the working tree that start with
/// `prefix`.
fn attrs_starting(repo: &Repo, prefix: &str) -> usize {
    let attrs = repo.read(".gitattributes");
    attrs.lines().filter(|l| l.starts_with(prefix)).count()
}

#[test]
fn migrate_import_copied_file() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_copied_file(&t);

    import(&repo, &["--above=1b"]).ok();

    assert_eq!(attrs_starting(&repo, "/dir/a.txt "), 1);
    assert_eq!(attrs_starting(&repo, "/a.txt "), 1);
}

#[test]
fn migrate_import_copied_file_with_only_a_single_path() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_copied_file(&t);
    let txt = oid("a.txt");

    import(&repo, &["--include=/a.txt", "--everything"]).ok();

    assert_eq!(attrs_starting(&repo, "/dir/a.txt"), 0);
    assert_eq!(attrs_starting(&repo, "/a.txt "), 1);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 5);
    repo.refute_pointer("refs/heads/main", "dir/a.txt");
}

/// Not from the shell suite: a pattern without a slash matches the file
/// name in any directory, like a `.gitattributes` pattern.
#[test]
fn migrate_import_include_without_slash_matches_any_directory() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_copied_file(&t);
    let txt = oid("a.txt");

    import(&repo, &["--include=a.txt"]).ok();

    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 5);
    repo.assert_pointer("refs/heads/main", "dir/a.txt", &txt, 5);
}

/// Not from the shell suite: with `core.ignorecase`, a pattern matches
/// regardless of case.
#[test]
fn migrate_import_include_ignores_case_with_core_ignorecase() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let txt = oid_at(&repo, "main", "a.txt");
    repo.git(["config", "core.ignorecase", "true"]).ok();

    import(&repo, &["--include=*.TXT"]).ok();

    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.refute_pointer("refs/heads/main", "a.md");
}

#[test]
fn migrate_import_filename_special_characters() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_special_character_files(&t);

    import(&repo, &["--above=1b"]).ok();

    let run = repo
        .git([
            "check-attr",
            "filter",
            "--",
            "test - special.bin",
            "test (test2) special.bin",
            "test * ** special.bin",
        ])
        .ok();
    assert_eq!(run.count("filter: lfs"), 3, "{run}");
}
