//! `t-migrate-export.sh`.
//!
//! Where the shell tests grep `.gitattributes` for part of a line, these
//! check for the whole line. Where they check that an object isn't stored
//! with some size, these check that it isn't stored at all.

use crate::harness::{Repo, Run, Test, oid, sized};
use crate::migrate_fixtures::{self as fixtures, Attrs};

const MD_OFF: &str = "*.md !text !filter !merge !diff";
const TXT_OFF: &str = "*.txt !text !filter !merge !diff";

/// Run `git lfs migrate export` with `args`, with stderr merged into
/// stdout.
fn export(repo: &Repo, args: &[&str]) -> Run {
    repo.lfs_command()
        .args(["migrate", "export"])
        .args(args)
        .merged()
        .run()
}

/// The object IDs of `a.md` and `a.txt` in the working tree with `rev`
/// checked out, then `main` checked out again.
fn worktree_oids(repo: &Repo, rev: &str) -> (String, String) {
    repo.git(["checkout", rev]).ok();
    let oids = (oid(repo.read("a.md")), oid(repo.read("a.txt")));
    repo.git(["checkout", "main"]).ok();
    oids
}

/// Whether `.gitattributes` at `rev` has the line `line`.
fn has_attr(repo: &Repo, rev: &str, line: &str) -> bool {
    let attrs = repo.blob(rev, ".gitattributes").unwrap_or_default();
    attrs.lines().any(|l| l == line)
}

/// Check that `.gitattributes` at `rev` has each of `lines`.
#[track_caller]
fn assert_attrs(repo: &Repo, rev: &str, lines: &[&str]) {
    for line in lines {
        assert!(has_attr(repo, rev, line), "{rev} lacks {line:?}");
    }
}

/// Check that `.gitattributes` at `rev` has none of `lines`.
#[track_caller]
fn refute_attrs(repo: &Repo, rev: &str, lines: &[&str]) {
    for line in lines {
        assert!(!has_attr(repo, rev, line), "{rev} has {line:?}");
    }
}

#[test]
fn migrate_export_default_branch() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_tracked(&t);
    // b.md, a pointer only on main.
    repo.write("b.md", sized("b.md", 160));
    repo.git(["add", "b.md"]).ok();
    repo.git(["commit", "-m", "add b.md"]).ok();
    let (md, txt) = worktree_oids(&repo, "main");
    let b_md = oid(repo.read("b.md"));
    let (md_feature, _) = worktree_oids(&repo, "my-feature");
    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.assert_pointer("refs/heads/main", "b.md", &b_md, 160);
    repo.assert_pointer("refs/heads/my-feature", "a.md", &md_feature, 30);

    export(&repo, &["--include=*.md, *.txt"]).ok();

    repo.refute_pointer("refs/heads/main", "a.md");
    repo.refute_pointer("refs/heads/main", "a.txt");
    repo.refute_pointer("refs/heads/main", "b.md");
    repo.assert_pointer("refs/heads/my-feature", "a.md", &md_feature, 30);
    // No pointer refers to b.md any more, so it's pruned. The others are
    // still pointers on my-feature.
    repo.refute_local_object(&b_md);
    repo.assert_local_object(&md, 140);
    repo.assert_local_object(&txt, 120);
    repo.assert_local_object(&md_feature, 30);
    assert_attrs(&repo, "refs/heads/main", &[MD_OFF, TXT_OFF]);
    refute_attrs(&repo, "refs/heads/my-feature", &[MD_OFF, TXT_OFF]);
}

/// Delete the local LFS objects, so export has to download them.
///
/// First the working tree files get an old timestamp and the index is
/// refreshed. Otherwise Git may find them racily clean and run the clean
/// filter on them again, which stores their objects again.
fn remove_local_objects(repo: &Repo) {
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for path in ["a.md", "a.txt"] {
        let file = std::fs::File::options()
            .write(true)
            .open(repo.path().join(path))
            .unwrap();
        file.set_modified(old).unwrap();
    }
    repo.git(["update-index", "--refresh"]).ok();
    repo.remove_local_objects();
}

#[test]
fn migrate_export_with_remote() {
    let t = Test::new();
    let repo = fixtures::single_remote_branch_tracked(&t);
    repo.git(["push", "origin", "main"]).ok();
    let (md, txt) = worktree_oids(&repo, "main");
    repo.assert_pointer("refs/heads/main", "a.md", &md, 50);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 30);
    repo.assert_pointer("refs/remotes/origin/main", "a.md", &md, 50);
    repo.assert_pointer("refs/remotes/origin/main", "a.txt", &txt, 30);
    // Every object has to be downloaded.
    remove_local_objects(&repo);

    export(&repo, &["--everything", "--include=*.md, *.txt"]).ok();

    repo.refute_pointer("refs/heads/main", "a.md");
    repo.refute_pointer("refs/heads/main", "a.txt");
    // Every pointer was exported, so every object is pruned.
    repo.refute_local_object(&md);
    repo.refute_local_object(&txt);
    assert_attrs(&repo, "refs/heads/main", &[MD_OFF, TXT_OFF]);
}

#[test]
fn migrate_export_include_exclude_args() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked(&t, Attrs::Plain);
    let (md, txt) = worktree_oids(&repo, "main");
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);

    export(&repo, &["--include=*", "--exclude=a.md"]).ok();

    repo.refute_pointer("refs/heads/main", "a.txt");
    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);
    repo.refute_local_object(&txt);
    repo.assert_local_object(&md, 140);
    assert_attrs(
        &repo,
        "refs/heads/main",
        &[
            "* !text !filter !merge !diff",
            "a.md filter=lfs diff=lfs merge=lfs -text",
        ],
    );
}

#[test]
fn migrate_export_bare_repository() {
    let t = Test::new();
    let repo = fixtures::single_remote_branch_tracked(&t);
    repo.git(["push", "origin", "main"]).ok();
    let (md, txt) = worktree_oids(&repo, "main");
    let repo = fixtures::make_bare(&t, repo);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 30);
    repo.assert_pointer("refs/heads/main", "a.md", &md, 50);

    export(&repo, &["--everything", "--include=*"]).ok();

    repo.refute_pointer("refs/heads/main", "a.md");
    repo.refute_pointer("refs/heads/main", "a.txt");
    // Every pointer was exported, so every object is pruned.
    repo.refute_local_object(&md);
    repo.refute_local_object(&txt);
}

#[test]
fn migrate_export_given_branch() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_tracked(&t);
    let (md, txt) = worktree_oids(&repo, "main");
    let (md_feature, _) = worktree_oids(&repo, "my-feature");
    repo.assert_pointer("refs/heads/my-feature", "a.md", &md_feature, 30);
    repo.assert_pointer("refs/heads/my-feature", "a.txt", &txt, 120);
    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);

    export(&repo, &["--include=*.md,*.txt", "my-feature"]).ok();

    repo.refute_pointer("refs/heads/my-feature", "a.md");
    repo.refute_pointer("refs/heads/my-feature", "a.txt");
    repo.refute_pointer("refs/heads/main", "a.md");
    repo.refute_pointer("refs/heads/main", "a.txt");
    // No pointers are left, so every object is pruned.
    repo.refute_local_object(&md_feature);
    repo.refute_local_object(&txt);
    repo.refute_local_object(&md);
    assert_attrs(&repo, "refs/heads/main", &[MD_OFF, TXT_OFF]);
    assert_attrs(&repo, "refs/heads/my-feature", &[MD_OFF, TXT_OFF]);
}

#[test]
fn migrate_export_no_filter() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_tracked(&t);
    let main = repo.rev_parse("main");

    export(&repo, &["--yes"])
        .fails()
        .has("One or more files must be specified with --include");

    assert_eq!(repo.rev_parse("main"), main, "main moved");
}

#[test]
fn migrate_export_exclude_remote_refs() {
    let t = Test::new();
    let repo = fixtures::single_remote_branch_tracked(&t);
    let remote = "refs/remotes/origin/main";
    let (md, txt) = worktree_oids(&repo, "main");
    let (md_remote, txt_remote) = worktree_oids(&repo, remote);
    let remote_before = repo.rev_parse(remote);
    repo.assert_pointer("refs/heads/main", "a.md", &md, 50);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 30);
    repo.assert_pointer(remote, "a.md", &md_remote, 140);
    repo.assert_pointer(remote, "a.txt", &txt_remote, 120);

    export(&repo, &["--include=*.md,*.txt"]).ok();

    repo.refute_pointer("refs/heads/main", "a.md");
    repo.refute_pointer("refs/heads/main", "a.txt");
    repo.refute_local_object(&md);
    repo.refute_local_object(&txt);
    repo.assert_pointer(remote, "a.md", &md_remote, 140);
    repo.assert_pointer(remote, "a.txt", &txt_remote, 120);
    assert_eq!(repo.rev_parse(remote), remote_before, "{remote} moved");
    // The pushed commit isn't rewritten.
    assert_eq!(
        repo.rev_parse("main~1"),
        remote_before,
        "pushed commit rewritten"
    );
    // The remote has these objects, so they're pruned.
    repo.refute_local_object(&md_remote);
    repo.refute_local_object(&txt_remote);
    assert_attrs(&repo, "refs/heads/main", &[MD_OFF, TXT_OFF]);
    refute_attrs(&repo, remote, &[MD_OFF, TXT_OFF]);
}

#[test]
fn migrate_export_skip_fetch() {
    let t = Test::new();
    let repo = fixtures::single_remote_branch_tracked(&t);
    let remote = "refs/remotes/origin/main";
    let (md_main, txt_main) = worktree_oids(&repo, "main");
    let (md_remote, txt_remote) = worktree_oids(&repo, remote);
    repo.git(["tag", "pseudo-remote", &repo.rev_parse(remote)])
        .ok();
    // Without the remote ref, and without fetching it again, nothing is
    // excluded.
    repo.git(["update-ref", "-d", remote]).ok();
    repo.assert_pointer("refs/heads/main", "a.md", &md_main, 50);
    repo.assert_pointer("pseudo-remote", "a.md", &md_remote, 140);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt_main, 30);
    repo.assert_pointer("pseudo-remote", "a.txt", &txt_remote, 120);

    export(&repo, &["--skip-fetch", "--include=*.md,*.txt"]).ok();

    repo.refute_pointer("refs/heads/main", "a.md");
    repo.refute_pointer("pseudo-remote", "a.md");
    repo.refute_pointer("refs/heads/main", "a.txt");
    repo.refute_pointer("pseudo-remote", "a.txt");
    repo.refute_local_object(&md_main);
    repo.refute_local_object(&md_remote);
    repo.refute_local_object(&txt_main);
    repo.refute_local_object(&txt_remote);
    assert_attrs(&repo, "refs/heads/main", &[MD_OFF, TXT_OFF]);
    assert_attrs(&repo, "pseudo-remote", &[MD_OFF, TXT_OFF]);
}

#[test]
fn migrate_export_include_exclude_ref() {
    let t = Test::new();
    let repo = fixtures::multiple_remote_branches_gitattrs(&t);
    let remote = "refs/remotes/origin/main";
    let (md_main, txt_main) = worktree_oids(&repo, "main");
    let (md_remote, txt_remote) = worktree_oids(&repo, remote);
    let (md_feature, txt_feature) = worktree_oids(&repo, "my-feature");
    let main = repo.rev_parse("main");

    export(
        &repo,
        &[
            "--include=*.txt",
            "--include-ref=refs/heads/my-feature",
            "--exclude-ref=refs/heads/main",
        ],
    )
    .ok();

    assert_eq!(repo.rev_parse("main"), main, "main moved");
    repo.assert_pointer("refs/heads/main", "a.md", &md_main, 21);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt_main, 20);
    repo.assert_pointer(remote, "a.md", &md_remote, 11);
    repo.assert_pointer(remote, "a.txt", &txt_remote, 10);
    repo.assert_pointer("refs/heads/my-feature", "a.md", &md_feature, 31);
    repo.refute_pointer("refs/heads/my-feature", "a.txt");
    // main's objects stay, as unpushed commits refer to them. The remote
    // has its own, so they're pruned.
    repo.assert_local_object(&md_main, 21);
    repo.assert_local_object(&txt_main, 20);
    repo.refute_local_object(&md_remote);
    repo.refute_local_object(&txt_remote);
    // a.txt on my-feature isn't a pointer any more, so it's pruned. a.md
    // still is.
    repo.assert_local_object(&md_feature, 31);
    repo.refute_local_object(&txt_feature);
    refute_attrs(&repo, "refs/heads/main", &[TXT_OFF]);
    refute_attrs(&repo, remote, &[TXT_OFF]);
    assert_attrs(&repo, "refs/heads/my-feature", &[TXT_OFF]);
    refute_attrs(&repo, "refs/heads/my-feature", &[MD_OFF]);
}

#[test]
fn migrate_export_invalid_ref() {
    let t = Test::new();
    let repo = t.init("migrate-export-invalid-ref");
    repo.git(["commit", "--allow-empty", "-m", "initial commit"])
        .ok();
    export(&repo, &["--yes", "--include=*", "jibberish"])
        .fails()
        .has("can't resolve ref");
}

#[test]
fn migrate_export_gitattributes_with_different_permissions() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked(&t, Attrs::Executable);
    let (md, txt) = worktree_oids(&repo, "main");
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 120);
    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);
    assert!(fixtures::is_executable(&repo, ".gitattributes"));

    export(&repo, &["--include=*.txt"]).ok();

    assert!(!fixtures::is_executable(&repo, ".gitattributes"));
    repo.refute_pointer("refs/heads/main", "a.txt");
    repo.assert_pointer("refs/heads/main", "a.md", &md, 140);
    repo.refute_local_object(&txt);
    repo.assert_local_object(&md, 140);
    assert_attrs(&repo, "refs/heads/main", &[TXT_OFF]);
    let tree = repo.git(["ls-tree", "refs/heads/main"]).ok().stdout();
    let entries: Vec<(&str, &str)> = tree
        .lines()
        .map(|l| (&l[..6], l.split('\t').nth(1).unwrap()))
        .collect();
    assert_eq!(
        entries,
        [
            ("100644", ".gitattributes"),
            ("100644", "a.md"),
            ("100644", "a.txt")
        ],
        "{tree}"
    );
}

#[test]
fn migrate_export_gitattributes_symlink() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked(&t, Attrs::Link);
    let main = repo.rev_parse("main");

    export(&repo, &["--yes", "--include=*.txt"])
        .fails()
        .has("expected '.gitattributes' to be a file, got a symbolic link");

    assert_eq!(repo.rev_parse("main"), main, "main moved");
    let tree = repo
        .git(["ls-tree", "main", "--", ".gitattributes"])
        .ok()
        .stdout();
    assert!(tree.starts_with("120000 blob "), "{tree}");
}

/// Every commit reachable from a ref, in `git log` order.
fn all_commits(repo: &Repo) -> Vec<String> {
    let log = repo.git(["log", "--all", "--pretty=format:%H"]).ok();
    log.stdout().lines().map(str::to_owned).collect()
}

#[test]
fn migrate_export_object_map() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_tracked(&t);
    let map = t.dir().join("object-map.txt");
    let old = all_commits(&repo);

    export(
        &repo,
        &[
            "--everything",
            "--include=*",
            "--object-map",
            map.to_str().unwrap(),
        ],
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
fn migrate_export_verbose() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_tracked(&t);
    let head = repo.rev_parse("HEAD");

    export(
        &repo,
        &["--everything", "--include=*", "--verbose", "--yes"],
    )
    .ok()
    .has(&format!("  commit {head}: a.md"));
}

#[test]
fn migrate_export_remote() {
    let t = Test::new();
    let repo = fixtures::single_remote_branch_tracked(&t);
    repo.git(["push", "origin", "main"]).ok();
    let (md, txt) = worktree_oids(&repo, "main");
    repo.assert_pointer("refs/heads/main", "a.md", &md, 50);
    repo.assert_pointer("refs/heads/main", "a.txt", &txt, 30);
    // Every object has to be downloaded, from zeta, as origin has no URL.
    remove_local_objects(&repo);
    let url = repo
        .git(["config", "--get", "remote.origin.url"])
        .ok()
        .stdout();
    repo.git(["remote", "add", "zeta", url.trim()]).ok();
    repo.git(["remote", "set-url", "origin", ""]).ok();

    export(
        &repo,
        &["--everything", "--remote=zeta", "--include=*.md, *.txt"],
    )
    .ok();

    repo.refute_pointer("refs/heads/main", "a.md");
    repo.refute_pointer("refs/heads/main", "a.txt");
    repo.refute_local_object(&md);
    repo.refute_local_object(&txt);
    assert_eq!(repo.blob("main", "a.md"), Some(repo.read("a.md")));
}

#[test]
fn migrate_export_invalid_remote() {
    let t = Test::new();
    let repo = fixtures::single_remote_branch_tracked(&t);
    let main = repo.rev_parse("main");

    export(&repo, &["--include=*", "--remote=zz", "--yes"])
        .fails()
        .has("Invalid remote zz provided");

    assert_eq!(repo.rev_parse("main"), main, "main moved");
}

/// A file that isn't a pointer stays as it is, whatever its contents.
#[test]
fn migrate_export_invalid_pointer() {
    let t = Test::new();
    for (dir, contents) in [("repo1", "git-lfs\n"), ("repo2", "not git-lfs\n")] {
        let repo = t.init(dir);
        repo.write("problematic_file", contents);
        repo.git(["add", "."]).ok();
        repo.git(["commit", "-m", "create repo"]).ok();

        export(&repo, &["--include=*", "--everything", "--yes"]).ok();

        assert_eq!(
            repo.blob("main", "problematic_file").as_deref(),
            Some(contents)
        );
    }
}

/// Not from the shell suite: the patterns are escaped in `.gitattributes`
/// as upstream does it, so a space doesn't split the line.
#[test]
fn migrate_export_escapes_patterns() {
    let t = Test::new();
    let repo = t.init("repo");
    repo.lfs(["track", "*.txt", "*.md"]).ok();
    repo.write("a b#c.txt", "a b#c.txt contents");
    repo.write("x\\y.md", "x\\y.md contents");
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", "initial"]).ok();

    export(
        &repo,
        &["--include=a b#c.txt", "--exclude=x\\y.md", "--yes"],
    )
    .ok();

    assert_attrs(
        &repo,
        "main",
        &[
            r"a[[:space:]]b\#c.txt !text !filter !merge !diff",
            r"x\\y.md filter=lfs diff=lfs merge=lfs -text",
        ],
    );
    assert_eq!(
        repo.blob("main", "a b#c.txt").as_deref(),
        Some("a b#c.txt contents")
    );
    let run = repo
        .git(["check-attr", "filter", "--", "a b#c.txt"])
        .ok()
        .stdout();
    assert_eq!(run, "a b#c.txt: filter: unspecified\n");
}
