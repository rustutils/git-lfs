//! `t-fetch-recent.sh`.
//!
//! The shell tests share one clone, each changing its config for the next.
//! Here each test sets up its own clone with the config the shell test
//! would have found, including `color.diff=always` from "fetch-recent
//! commits" onward.

use crate::harness::{Commit, Repo, Test, oid};

const NAME: &str = "fetch-recent";
const CONTENT: [&str; 6] = [
    "filecontent0",
    "filecontent1",
    "filecontent2",
    "filecontent3",
    "filecontent4",
    "filecontent5",
];

/// The object ID of `CONTENT[n]`.
fn oid_of(n: usize) -> String {
    oid(CONTENT[n])
}

/// The remote repository with its history pushed, and the repository it
/// was made in.
///
/// `main` has four commits: 18 days ago (`file1.dat` holding content 0
/// and `file3.dat` content 5), 14 days ago (content 1), and 1 day ago
/// (content 2, and `file2.dat` content 3). `other_branch` branches off the
/// second one with a commit 5 days ago (content 4).
fn remote(t: &Test) -> Repo<'_> {
    t.remote(NAME);
    let repo = t.clone(NAME, NAME);
    repo.lfs_command()
        .args(["track", "*.dat"])
        .merged()
        .run()
        .ok()
        .has("Tracking \"*.dat\"");
    repo.add_commits(&[
        Commit::new()
            .days_ago(18)
            .file("file1.dat", CONTENT[0])
            .file("file3.dat", CONTENT[5]),
        Commit::new().days_ago(14).file("file1.dat", CONTENT[1]),
        Commit::new()
            .days_ago(5)
            .new_branch("other_branch")
            .file("file1.dat", CONTENT[4]),
        Commit::new()
            .days_ago(1)
            .on("main")
            .file("file1.dat", CONTENT[2])
            .file("file2.dat", CONTENT[3]),
    ]);
    repo.git(["push", "origin", "main"]).ok();
    repo.git(["push", "origin", "other_branch"]).ok();
    repo
}

/// A clone of [`remote`], with `other_branch` checked out once so it's a
/// local branch, no local objects, and `config` set.
fn clone<'a>(t: &'a Test, config: &[(&str, &str)]) -> Repo<'a> {
    remote(t);
    let clone = t.clone(NAME, "clone");
    clone.git(["checkout", "other_branch"]).ok();
    clone.git(["checkout", "main"]).ok();
    clone.remove_local_objects();
    for (key, value) in config {
        clone.git(["config", key, value]).ok();
    }
    clone
}

/// Check which of the contents' objects `repo` has.
#[track_caller]
fn assert_objects(repo: &Repo, present: &[usize], absent: &[usize]) {
    for &n in present {
        repo.assert_local_object(&oid_of(n), CONTENT[n].len());
    }
    for &n in absent {
        repo.refute_local_object(&oid_of(n));
    }
}

#[test]
fn init_fetch_recent() {
    let t = Test::new();
    remote(&t);
    let oids: Vec<String> = (0..5).map(oid_of).collect();
    let oids: Vec<&str> = oids.iter().map(String::as_str).collect();
    t.assert_server_objects(NAME, &oids, &[]);
}

/// Without `--recent`, only the tip of `main`.
#[test]
fn fetch_recent_normal() {
    let t = Test::new();
    let clone = clone(
        &t,
        &[
            ("lfs.fetchrecentalways", "false"),
            ("lfs.fetchrecentrefsdays", "0"),
            ("lfs.fetchrecentremoterefs", "false"),
            ("lfs.fetchrecentcommitsdays", "7"),
        ],
    );

    clone.lfs(["fetch", "origin", "main"]).ok();

    assert_objects(&clone, &[2, 3, 5], &[0, 1, 4]);
}

/// The config every `--recent` test starts from, with `refs_days` and
/// `commits_days`.
fn recent_config<'a>(refs_days: &'a str, commits_days: &'a str) -> [(&'a str, &'a str); 5] {
    [
        ("lfs.fetchrecentalways", "false"),
        ("lfs.fetchrecentrefsdays", refs_days),
        ("lfs.fetchrecentremoterefs", "false"),
        ("lfs.fetchrecentcommitsdays", commits_days),
        // Colored diff headers don't confuse the scan.
        ("color.diff", "always"),
    ]
}

/// `main`, and what its commits in the last 7 days replaced.
#[test]
fn fetch_recent_commits() {
    let t = Test::new();
    let clone = clone(&t, &recent_config("0", "7"));

    clone.lfs(["fetch", "--recent", "origin"]).ok();

    assert_objects(&clone, &[2, 3, 1], &[0, 4]);
}

/// Also `other_branch`, whose tip is within 6 days. Content 0 is 13 days
/// before that tip.
#[test]
fn fetch_recent_days() {
    let t = Test::new();
    let clone = clone(&t, &recent_config("6", "7"));

    clone.lfs(["fetch", "--recent", "origin"]).ok();

    assert_objects(&clone, &[2, 3, 1, 4], &[0]);
}

/// 14 days before `other_branch`'s tip reaches content 0.
#[test]
fn fetch_recent_older_commits() {
    let t = Test::new();
    let clone = clone(&t, &recent_config("6", "14"));

    clone.lfs(["fetch", "--recent", "origin"]).ok();

    assert_objects(&clone, &[0], &[]);
}

/// Without the local `other_branch`, and without remote refs, only
/// `main`'s tip.
fn without_local_branch(t: &Test) -> Repo<'_> {
    let clone = clone(t, &recent_config("6", "0"));
    clone.git(["branch", "-D", "other_branch"]).ok();
    clone
}

#[test]
fn fetch_recent_remote_branch() {
    let t = Test::new();
    let clone = without_local_branch(&t);

    clone.lfs(["fetch", "--recent", "origin"]).ok();

    assert_objects(&clone, &[2, 3], &[1, 0, 4]);
}

/// With remote refs, `origin/other_branch`'s tip too.
#[test]
fn fetch_recent_remote_refs() {
    let t = Test::new();
    let clone = without_local_branch(&t);
    clone
        .git(["config", "lfs.fetchrecentremoterefs", "true"])
        .ok();

    clone.lfs(["fetch", "--recent", "origin"]).ok();

    assert_objects(&clone, &[4], &[0, 1]);
}

/// Not from the shell suite: a recent branch other than the one named
/// gets its own window of older commits. In the shell suite's fixture,
/// everything `other_branch`'s window finds is in `main`'s too.
///
/// `c.dat` is `c0` 20 days ago, then `c2` on `main` 19 days ago. A side
/// branch from the first commit sets it to `c1` 4 days ago and `c3` 3
/// days ago, so only that branch's window finds `c1`.
#[test]
fn fetch_recent_window_for_each_recent_branch() {
    let t = Test::new();
    let name = "fetch-recent-each-branch";
    t.remote(name);
    let repo = t.clone(name, name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.add_commits(&[
        Commit::new().days_ago(20).file("c.dat", "c0"),
        Commit::new().days_ago(19).file("c.dat", "c2"),
    ]);
    repo.git(["checkout", "-b", "side", "HEAD~1"]).ok();
    repo.add_commits(&[
        Commit::new().days_ago(4).file("c.dat", "c1"),
        Commit::new().days_ago(3).file("c.dat", "c3"),
    ]);
    repo.git(["push", "origin", "main", "side"]).ok();
    let clone = t.clone(name, "clone");
    clone.git(["checkout", "side"]).ok();
    clone.git(["checkout", "main"]).ok();
    clone.remove_local_objects();
    for (key, value) in recent_config("6", "7") {
        clone.git(["config", key, value]).ok();
    }

    clone.lfs(["fetch", "--recent", "origin"]).ok();

    for contents in ["c0", "c1", "c2", "c3"] {
        clone.assert_local_object(&oid(contents), 2);
    }
}
