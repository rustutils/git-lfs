//! `t-untrack.sh`.

use crate::harness::{Repo, Test};

const A_DAT: &str = "a.dat filter=lfs diff=lfs merge=lfs\n";

/// The number of lines in `repo`'s `.gitattributes` with both `pattern`
/// and `filter=lfs`, like the shell suites' `assert_attributes_count`.
fn lfs_lines_with(repo: &Repo, pattern: &str) -> usize {
    repo.read(".gitattributes")
        .lines()
        .filter(|l| l.contains(pattern) && l.contains("filter=lfs"))
        .count()
}

#[test]
fn untrack() {
    let t = Test::new();
    let repo = t.init("untrack");
    repo.lfs(["track", "*.jpg"]).ok().has("Tracking \"*.jpg\"");
    repo.append(".gitattributes", "* annex.backend=SHA512E\n");

    repo.lfs(["untrack", "*.jpg"]).ok();

    assert_eq!(repo.read(".gitattributes"), "* annex.backend=SHA512E\n");
}

/// Like Git, it exits 128 outside a repository.
#[test]
fn untrack_outside_git_repo() {
    let t = Test::new();
    std::fs::create_dir(t.dir().join("outside")).unwrap();

    let run = t.repo("outside").lfs(["untrack", "*.foo"]);

    assert_eq!(run.code(), Some(128), "{run}");
}

#[test]
fn untrack_removes_escape_sequences() {
    let t = Test::new();
    let repo = t.init("untrack-remove-escape-sequence");

    for (pattern, escaped) in [(" ", "[[:space:]]"), ("#", "\\#")] {
        repo.lfs(["track", pattern])
            .ok()
            .has(&format!("Tracking \"{pattern}\""));
        assert_eq!(lfs_lines_with(&repo, escaped), 1, "{pattern:?}");

        repo.lfs(["untrack", pattern])
            .ok()
            .has(&format!("Untracking \"{pattern}\""));
        assert_eq!(lfs_lines_with(&repo, escaped), 0, "{pattern:?}");
    }
}

/// Check that `untrack` with `./a.dat` and with `a.dat` each empty
/// `.gitattributes`, which holds `line`.
fn assert_untracks_with_and_without_prefix(name: &str, line: &str) {
    let t = Test::new();
    let repo = t.init(name);
    repo.write(".gitattributes", line);
    repo.write("a.dat", "a");
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();

    for pattern in ["./a.dat", "a.dat"] {
        repo.lfs(["untrack", pattern]).ok();
        assert_eq!(repo.read(".gitattributes").trim(), "", "{pattern}");
        repo.git(["checkout", "--", ".gitattributes"]).ok();
    }
}

#[test]
fn untrack_removes_prefixed_patterns_legacy() {
    assert_untracks_with_and_without_prefix(
        "untrack-removes-prefix-patterns-legacy",
        &format!("./{A_DAT}"),
    );
}

#[test]
fn untrack_removes_prefixed_patterns_modern() {
    assert_untracks_with_and_without_prefix("untrack-removes-prefix-patterns-modern", A_DAT);
}

/// Every copy of the escaped line goes.
#[test]
fn untrack_removes_escaped_pattern_in_gitattributes() {
    let t = Test::new();
    let repo = t.init("untrack-escaped");
    let line = "file[[:space:]]with[[:space:]]spaces.\\# filter=lfs diff=lfs merge=lfs -text\n";
    repo.write(".gitattributes", line.repeat(3));

    repo.lfs(["untrack", "file with spaces.#"]).ok();

    assert_eq!(repo.read(".gitattributes").trim(), "");
}

#[test]
fn untrack_works_with_git_work_tree() {
    let t = Test::new();
    let dir = t.repo("");
    for d in ["untrack-work-tree", "untrack-work-tree-git"] {
        std::fs::create_dir(t.dir().join(d)).unwrap();
    }
    let run = |program: &str, args: &[&str]| {
        t.command(t.dir(), program)
            .args(args)
            .env("GIT_WORK_TREE", "untrack-work-tree")
            .env("GIT_DIR", "untrack-work-tree-git")
            .run()
            .ok()
    };
    let git_lfs = crate::harness::git_lfs();
    let git_lfs = git_lfs.to_str().unwrap();

    run("git", &["init"]);
    run(git_lfs, &["track", "*.bin"]);
    let attributes = dir.read("untrack-work-tree/.gitattributes");
    assert!(
        attributes.contains("*.bin filter=lfs diff=lfs merge=lfs -text"),
        "{attributes}"
    );

    run(git_lfs, &["untrack", "*.bin"]);

    let path = t.dir().join("untrack-work-tree/.gitattributes");
    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
    assert_eq!(size, 0, "{}", dir.read("untrack-work-tree/.gitattributes"));
}

/// Not from the shell suite: a line for the same pattern without
/// `filter=lfs` stays.
#[test]
fn untrack_keeps_other_lines_for_the_pattern() {
    let t = Test::new();
    let repo = t.init("untrack-keeps-other-lines");
    repo.write(
        ".gitattributes",
        "*.jpg -text\n*.jpg filter=lfs diff=lfs merge=lfs -text\n",
    );

    repo.lfs(["untrack", "*.jpg"]).ok();

    assert_eq!(repo.read(".gitattributes"), "*.jpg -text\n");
}
