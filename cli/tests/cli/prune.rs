//! `t-prune.sh`.

use crate::harness::{Commit, Repo, Run, Test, oid};

/// The clone `clone_<name>` of the new remote `remote_<name>`, with `*.dat`
/// tracked.
fn prune_setup<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    t.remote(&format!("remote_{name}"));
    let repo = t.clone(&format!("remote_{name}"), &format!("clone_{name}"));
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    repo
}

/// Set `lfs.<key>` for each of `settings`.
fn configure(repo: &Repo, settings: &[(&str, &str)]) {
    for (key, value) in settings {
        repo.git(["config", &format!("lfs.{key}"), value]).ok();
    }
}

/// Check the local store has each of `contents`.
#[track_caller]
fn assert_kept(repo: &Repo, contents: &[&str]) {
    for c in contents {
        repo.assert_local_object(&oid(c), c.len());
    }
}

/// Check the local store has none of `contents`.
#[track_caller]
fn assert_pruned(repo: &Repo, contents: &[&str]) {
    for c in contents {
        repo.refute_local_object(&oid(c));
    }
}

/// Run `git lfs prune` with `args`, check it succeeds, and check its output
/// has each of `lines` and the object IDs of each of `listed`.
#[track_caller]
fn prune(repo: &Repo, args: &[&str], lines: &[&str], listed: &[&str]) {
    check_output(prune_run(repo, args).ok(), lines, listed);
}

/// [`prune`], for a run that stops because objects are missing on the
/// remote, which it lists.
#[track_caller]
fn prune_halted(repo: &Repo, args: &[&str], lines: &[&str], listed: &[&str]) {
    let run = prune_run(repo, args).fails().has("missing on remote:");
    check_output(run, lines, listed);
}

fn prune_run(repo: &Repo, args: &[&str]) -> Run {
    repo.lfs_command().args(["prune"]).args(args).run()
}

#[track_caller]
fn check_output(mut run: Run, lines: &[&str], listed: &[&str]) {
    for line in lines {
        run = run.has(line);
    }
    for c in listed {
        run = run.has(&oid(c));
    }
}

#[test]
fn prune_unreferenced_and_old() {
    let t = Test::new();
    let repo = prune_setup(&t, "prune_unref_old");
    let unreferenced = "To delete: unreferenced";
    let old_and_pushed = "To delete: pushed and too old";
    let old_and_unchanged = "Keep: pushed and created a while ago, but still current";
    let retain1 = "Retained content 1";
    let retain2 = "Retained content 2";

    // Content is too old when it was replaced outside the prune window, not
    // when it was added. old.dat from 7 days ago was current until a commit
    // inside the window, so it's kept. The last commit on main is today, so
    // recent commits are counted back from there.
    repo.add_commits(&[
        Commit::new()
            .days_ago(20)
            .file("old.dat", old_and_pushed)
            .file("stillcurrent.dat", old_and_unchanged),
        Commit::new().days_ago(7).file("old.dat", retain1),
        Commit::new()
            .days_ago(4)
            .new_branch("branch_to_delete")
            .file("unreferenced.dat", unreferenced),
        Commit::new().on("main").file("old.dat", retain2),
    ]);
    repo.git(["push", "origin", "main"]).ok();
    repo.git(["branch", "-D", "branch_to_delete"]).ok();
    configure(
        &repo,
        &[
            ("fetchrecentrefsdays", "5"),
            ("fetchrecentremoterefs", "true"),
            ("fetchrecentcommitsdays", "3"),
            ("pruneoffsetdays", "2"),
        ],
    );

    prune(
        &repo,
        &["--dry-run", "--verbose"],
        &["5 local objects, 3 retained", "2 files would be pruned"],
        &[old_and_pushed, unreferenced],
    );
    assert_kept(&repo, &[old_and_pushed, unreferenced]);
    prune(&repo, &[], &[], &[]);
    assert_pruned(&repo, &[old_and_pushed, unreferenced]);
    assert_kept(&repo, &[old_and_unchanged, retain1, retain2]);

    // Keep only what the refs point at, nothing recent.
    configure(&repo, &[("fetchrecentcommitsdays", "0")]);
    prune(
        &repo,
        &["--verbose"],
        &[
            "3 local objects, 2 retained",
            "Deleting objects: 100% (1/1), done.",
        ],
        &[retain1],
    );
    assert_pruned(&repo, &[retain1]);
    assert_kept(&repo, &[retain2]);
}

#[test]
fn prune_all_excluded_paths() {
    let t = Test::new();
    let repo = prune_setup(&t, "prune_unref_old_exclude");
    let old_and_excluded = "To delete: pushed and too old and excluded by filter";
    let old_and_unchanged = "Keep: pushed and created a while ago, but still current";
    let prev_and_excluded =
        "To delete: pushed and in previous commit to HEAD but excluded by filter";
    let unreferenced_and_excluded =
        "To delete: unreferenced in deleted branch and pushed and excluded by filter";
    let included_and_excluded =
        "Keep: pushed and both excluded by filter and included by another path";
    let excluded = "To delete: pushed and in HEAD but excluded by filter";

    repo.add_commits(&[
        Commit::new()
            .days_ago(20)
            .file("foo/oldandexcluded.dat", old_and_excluded)
            .file("stillcurrent.dat", old_and_unchanged),
        Commit::new()
            .days_ago(7)
            .file("foo/oldandexcluded.dat", prev_and_excluded),
        Commit::new()
            .days_ago(4)
            .new_branch("branch_to_delete")
            .file("unreferenced.dat", unreferenced_and_excluded),
        Commit::new()
            .on("main")
            .file("foo/unreferencedandexcluded.dat", unreferenced_and_excluded)
            .file("foo/includedandexcluded.dat", included_and_excluded)
            .file("included.dat", included_and_excluded)
            .file("foo/oldandexcluded.dat", excluded),
    ]);
    repo.git(["push", "origin", "main"]).ok();
    repo.git(["branch", "-D", "branch_to_delete"]).ok();
    configure(
        &repo,
        &[
            ("fetchrecentrefsdays", "5"),
            ("fetchrecentremoterefs", "true"),
            ("fetchrecentcommitsdays", "3"),
            ("pruneoffsetdays", "2"),
            ("fetchexclude", "/foo"),
        ],
    );

    let pruned = [
        old_and_excluded,
        prev_and_excluded,
        unreferenced_and_excluded,
        excluded,
    ];
    prune(
        &repo,
        &["--dry-run", "--verbose"],
        &["6 local objects, 2 retained", "4 files would be pruned"],
        &pruned,
    );
    assert_kept(&repo, &pruned);
    prune(&repo, &[], &[], &[]);
    assert_pruned(&repo, &pruned);
    assert_kept(&repo, &[old_and_unchanged, included_and_excluded]);
}

#[test]
fn prune_keep_unpushed() {
    let t = Test::new();
    let name = "prune_keep_unpushed";
    let repo = prune_setup(&t, name);
    // Each branch has old commits, which would be pruned if they were
    // pushed. Tips are never pruned.
    let head1 = "Keep: unpushed HEAD 1";
    let head2 = "Keep: unpushed HEAD 2";
    let head3 = "Keep: unpushed HEAD 3";
    let branch1 = "Keep: unpushed second branch 1";
    let branch2 = "Keep: unpushed second branch 2";
    let branch3 = "Keep: unpushed second branch 3";
    let excluded_branch1 = "Keep: unpushed second branch 1 excluded by filter";
    let excluded_branch2 = "Keep: unpushed second branch 2 excluded by filter";
    let excluded_branch3 = "Keep: unpushed second branch 3 excluded by filter";
    let tag1 = "Keep: unpushed tag only 1";
    let tag2 = "Keep: unpushed tag only 2";
    let tag3 = "Keep: unpushed tag only 3";

    repo.add_commits(&[
        Commit::new().days_ago(40).file("file.dat", head1),
        Commit::new()
            .days_ago(31)
            .on("main")
            .new_branch("branch_unpushed")
            .file("file.dat", branch1)
            .file("foo/file.dat", excluded_branch1),
        Commit::new()
            .days_ago(16)
            .file("file.dat", branch2)
            .file("foo/file.dat", excluded_branch2),
        Commit::new()
            .days_ago(2)
            .file("file.dat", branch3)
            .file("foo/file.dat", excluded_branch3),
        Commit::new()
            .days_ago(31)
            .on("main")
            .new_branch("branch_unpushed_tagged_only")
            .file("file2.dat", tag1),
        Commit::new().days_ago(16).file("file2.dat", tag2),
        Commit::new()
            .days_ago(2)
            .tag("tag_unpushed")
            .file("file2.dat", tag3),
        Commit::new()
            .days_ago(21)
            .on("main")
            .file("file.dat", head2),
        Commit::new().days_ago(0).file("file.dat", head3),
    ]);
    configure(
        &repo,
        &[
            ("fetchrecentrefsdays", "5"),
            ("fetchrecentremoterefs", "true"),
            // Only what the refs point at, nothing recent.
            ("fetchrecentcommitsdays", "0"),
            ("pruneoffsetdays", "2"),
            ("fetchexclude", "/foo"),
        ],
    );
    // Color codes in git diff's meta lines.
    repo.git(["config", "color.diff", "always"]).ok();
    repo.git(["branch", "-D", "branch_unpushed_tagged_only"])
        .ok();

    prune(&repo, &[], &[], &[]);

    // With main pushed, only its older versions are removed.
    repo.git(["push", "origin", "main"]).ok();
    prune(
        &repo,
        &["--verbose"],
        &[
            "12 local objects, 10 retained",
            "Deleting objects: 100% (2/2), done.",
        ],
        &[head1, head2],
    );
    assert_pruned(&repo, &[head1, head2]);

    // Merge the branch, taking its side of conflicts, and the tag, delete
    // both, and push main. Everything in between is pruned, and on the
    // remote.
    repo.git(["merge", "-Xtheirs", "branch_unpushed"]).ok();
    repo.git(["merge", "tag_unpushed"]).ok();
    repo.git(["branch", "-D", "branch_unpushed"]).ok();
    repo.git(["tag", "-d", "tag_unpushed"]).ok();
    repo.git(["push", "origin", "main"]).ok();

    // excluded_branch3 is in the new HEAD, but excluded and pushed. head3
    // was replaced by the merge.
    let pruned = [
        branch1,
        branch2,
        excluded_branch1,
        excluded_branch2,
        excluded_branch3,
        tag1,
        tag2,
        head3,
    ];
    prune(
        &repo,
        &["--verbose"],
        &[
            "10 local objects, 2 retained",
            "Deleting objects: 100% (8/8), done.",
        ],
        &pruned,
    );
    assert_pruned(&repo, &pruned);
    for c in pruned {
        assert!(
            t.server()
                .object(&format!("remote_{name}"), &oid(c))
                .is_some()
        );
    }
}

#[test]
fn prune_keep_recent() {
    let t = Test::new();
    let repo = prune_setup(&t, "prune_recent");
    let keep_head = "Keep: HEAD";
    let keep_branch1_tip = "Keep: Recent branch 1 tip";
    let keep_branch2_tip = "Keep: Recent branch 2 tip";
    let keep_recent_head = "Keep: Recent commit on HEAD";
    let keep_recent_branch1 = "Keep: Recent commit on recent branch 1";
    let keep_recent_branch2 = "Keep: Recent commit on recent branch 2";
    let prune_old_branch1 = "Prune: old commit on old branch";
    let prune_old_branch2 = "Prune: old branch tip";
    let prune_branch1 = "Prune: old commit on recent branch 1";
    let prune_branch2 = "Prune: old commit on recent branch 2";
    let prune_head = "Prune: old commit on HEAD";

    // One file, so each commit replaces the last. A recent day keeps the
    // last commit before it, whose contents were current then.
    repo.add_commits(&[
        Commit::new().days_ago(50).file("file.dat", prune_head),
        Commit::new()
            .days_ago(30)
            .file("file.dat", keep_recent_head),
        Commit::new()
            .days_ago(8)
            .new_branch("branch_old")
            .file("file.dat", prune_old_branch1),
        Commit::new()
            .days_ago(7)
            .file("file.dat", prune_old_branch2),
        Commit::new()
            .days_ago(9)
            .on("main")
            .new_branch("branch1")
            .file("file.dat", prune_branch1),
        Commit::new()
            .days_ago(8)
            .file("file.dat", keep_recent_branch1),
        Commit::new().days_ago(5).file("file.dat", keep_branch1_tip),
        Commit::new()
            .days_ago(17)
            .on("main")
            .new_branch("branch2")
            .file("file.dat", prune_branch2),
        Commit::new()
            .days_ago(10)
            .file("file.dat", keep_recent_branch2),
        Commit::new().days_ago(2).file("file.dat", keep_branch2_tip),
        Commit::new()
            .days_ago(1)
            .on("main")
            .file("file.dat", keep_head),
    ]);
    // Keep refs from the last 6 days, and commits that overlap the 2 days
    // before each tip (recent plus offset).
    configure(
        &repo,
        &[
            ("fetchrecentrefsdays", "5"),
            ("fetchrecentremoterefs", "true"),
            ("fetchrecentcommitsdays", "1"),
            ("pruneoffsetdays", "1"),
        ],
    );
    repo.git(["config", "color.diff", "always"]).ok();
    // Everything is pushed, so nothing is kept for being unpushed.
    repo.git([
        "push",
        "origin",
        "main:main",
        "branch_old:branch_old",
        "branch1:branch1",
        "branch2:branch2",
    ])
    .ok();

    let pruned = [
        prune_old_branch1,
        prune_old_branch2,
        prune_branch1,
        prune_branch2,
        prune_head,
    ];
    prune(
        &repo,
        &["--verbose"],
        &[
            "11 local objects, 6 retained, done.",
            "Deleting objects: 100% (5/5), done.",
        ],
        &pruned,
    );
    assert_pruned(&repo, &pruned);
    assert_kept(
        &repo,
        &[
            keep_head,
            keep_branch1_tip,
            keep_branch2_tip,
            keep_recent_head,
            keep_recent_branch1,
            keep_recent_branch2,
        ],
    );

    // Without recent commits, only the branch tips are kept.
    configure(&repo, &[("fetchrecentcommitsdays", "0")]);
    prune(
        &repo,
        &["--verbose"],
        &[
            "6 local objects, 3 retained, done.",
            "Deleting objects: 100% (3/3), done.",
        ],
        &[],
    );
    assert_kept(&repo, &[keep_head, keep_branch1_tip, keep_branch2_tip]);
    assert_pruned(
        &repo,
        &[keep_recent_head, keep_recent_branch1, keep_recent_branch2],
    );

    // Without recent refs, only HEAD is kept.
    configure(&repo, &[("fetchrecentrefsdays", "0")]);
    prune(
        &repo,
        &["--verbose"],
        &[
            "3 local objects, 1 retained, done.",
            "Deleting objects: 100% (2/2), done.",
        ],
        &[],
    );
    assert_kept(&repo, &[keep_head]);
    assert_pruned(&repo, &[keep_branch1_tip, keep_branch2_tip]);
}

/// Settings that let prune remove as much as possible.
const NO_RECENTS: [(&str, &str); 4] = [
    ("fetchrecentrefsdays", "0"),
    ("fetchrecentremoterefs", "true"),
    ("fetchrecentcommitsdays", "0"),
    ("pruneoffsetdays", "1"),
];

#[test]
fn prune_remote_tests() {
    let t = Test::new();
    let name = "prune_no_or_nonorigin_remote";
    t.command(t.dir(), "git").args(["init", name]).run().ok();
    let repo = t.repo(name);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    // Contents of different sizes, so different objects.
    let contents = |size: usize| "x".repeat(size);
    repo.add_commits(&[
        Commit::new().days_ago(50).file("file.dat", contents(30)),
        Commit::new().days_ago(40).file("file.dat", contents(28)),
        Commit::new().days_ago(35).file("file.dat", contents(37)),
        Commit::new().days_ago(25).file("file.dat", contents(42)),
    ]);
    configure(&repo, &NO_RECENTS);

    // Nothing is pruned without a remote.
    let all_retained = ["4 local objects, 4 retained, done."];
    prune(&repo, &["--verbose"], &all_retained, &[]);

    // Nor when the remote isn't origin.
    t.remote(&format!("remote1_{name}"));
    t.remote(&format!("remote2_{name}"));
    let url = t.remote_url(&format!("remote1_{name}"));
    repo.git(["remote", "add", "not_origin", &url]).ok();
    repo.git(["push", "not_origin", "main"]).ok();
    prune(&repo, &["--verbose"], &all_retained, &[]);

    // Unless prune checks that remote.
    configure(&repo, &[("pruneremotetocheck", "not_origin")]);
    prune(
        &repo,
        &["--verbose", "--dry-run"],
        &[
            "4 local objects, 1 retained, done.",
            "3 files would be pruned",
        ],
        &[],
    );
}

#[test]
fn prune_verify() {
    let t = Test::new();
    let name = "prune_verify";
    let repo = prune_setup(&t, name);
    let head = "HEAD content";
    let commit3 = "Content for commit 3 (prune)";
    let commit2_fail_verify = "Content for commit 2 (prune - fail verify)";
    let commit1 = "Content for commit 1 (prune)";
    repo.add_commits(&[
        Commit::new().days_ago(50).file("file.dat", commit1),
        Commit::new()
            .days_ago(40)
            .file("file.dat", commit2_fail_verify),
        Commit::new().days_ago(35).file("file.dat", commit3),
        Commit::new().days_ago(25).file("file.dat", head),
    ]);
    // Everything is pushed, so nothing is kept for being unpushed.
    repo.git(["push", "origin", "main"]).ok();
    configure(&repo, &NO_RECENTS);
    let old = [commit1, commit2_fail_verify, commit3];

    // With every object on the remote, verifying would prune.
    prune(
        &repo,
        &["--dry-run", "--verify-remote", "--verbose"],
        &[
            "4 local objects, 1 retained, 3 verified with remote, done.",
            "3 files would be pruned",
        ],
        &old,
    );

    // With one missing from the remote, nothing is pruned.
    t.server()
        .delete_object(&format!("remote_{name}"), &oid(commit2_fail_verify));
    let not_on_remote =
        "4 local objects, 1 retained, 2 verified with remote, 1 not on remote, done.";
    prune_halted(
        &repo,
        &["--verify-remote"],
        &[not_on_remote],
        &[commit2_fail_verify],
    );
    assert_kept(&repo, &old);

    // The same, with the setting instead of the option.
    configure(&repo, &[("pruneverifyremotealways", "true")]);
    prune_halted(&repo, &[], &[not_on_remote], &[commit2_fail_verify]);
    assert_kept(&repo, &old);

    // With --when-unverified=continue, the verified objects are pruned.
    prune(
        &repo,
        &["--when-unverified=continue", "--dry-run", "--verbose"],
        &[not_on_remote, "2 files would be pruned"],
        &[commit1, commit3],
    );

    // The option overrides the setting.
    prune(
        &repo,
        &["--no-verify-remote"],
        &[
            "4 local objects, 1 retained, done.",
            "Deleting objects: 100% (3/3), done.",
        ],
        &[],
    );
    assert_pruned(&repo, &old);
}

#[test]
fn prune_verify_large_numbers_of_refs() {
    let t = Test::new();
    let repo = prune_setup(&t, "prune_verify_large");
    repo.add_commits(&[
        Commit::new()
            .days_ago(50)
            .file("file.dat", "Old content 1, unique"),
        Commit::new()
            .days_ago(45)
            .file("file.dat", "Old content 2, unique"),
        Commit::new().days_ago(2).file("file.dat", "Recent commit"),
        Commit::new().days_ago(1).file("file.dat", "HEAD content"),
    ]);

    // Many refs to an old commit, so prune has a lot to read.
    let log = repo.git(["log", "--format=%H", "main"]).ok().stdout();
    let old = log.lines().nth(2).unwrap();
    let tags: String = (0..=1000)
        .map(|i| format!("create refs/tags/v{i} {old}\n"))
        .collect();
    repo.command("git")
        .args(["update-ref", "--stdin"])
        .stdin(tags)
        .run()
        .ok();
    configure(
        &repo,
        &[
            ("fetchrecentrefsdays", "3"),
            ("fetchrecentremoterefs", "true"),
            ("fetchrecentcommitsdays", "3"),
            ("pruneoffsetdays", "3"),
        ],
    );

    // It doesn't hang.
    prune(
        &repo,
        &["--dry-run", "--verify-remote", "--verbose"],
        &[],
        &[],
    );
}

#[test]
fn prune_unreachable() {
    let t = Test::new();
    let repo = prune_setup(&t, "prune_unreachable");
    let head = "HEAD content";
    let orphan = "orphan content (not pruned)";
    let commit3 = "Content for commit 3 (prune)";
    let commit2 = "Content for commit 2";
    let commit1 = "Content for commit 1 (prune)";
    repo.add_commits(&[
        Commit::new().days_ago(50).file("file.dat", commit1),
        Commit::new().days_ago(40).file("file.dat", commit2),
        Commit::new().days_ago(35).file("file.dat", commit3),
        Commit::new().days_ago(25).file("file.dat", head),
    ]);
    repo.git(["push", "origin", "main"]).ok();
    configure(&repo, &NO_RECENTS);

    // An object only an unstaged change ever had.
    repo.write("file.dat", orphan);
    repo.git(["add", "file.dat"]).ok();
    repo.git(["reset", "file.dat"]).ok();

    let verified = "5 local objects, 1 retained, 3 verified with remote, done.";
    let not_on_remote =
        "5 local objects, 1 retained, 3 verified with remote, 1 not on remote, done.";

    // Without --verify-unreachable, it would be pruned.
    prune(
        &repo,
        &["--dry-run", "--verify-remote", "--verbose"],
        &[verified, "4 files would be pruned"],
        &[commit3, commit2, commit1, orphan],
    );

    // With it, it can't be verified, so nothing is pruned.
    prune_halted(
        &repo,
        &["--verify-remote", "--verify-unreachable"],
        &[not_on_remote],
        &[orphan],
    );
    assert_kept(&repo, &[head, commit1, commit2, commit3, orphan]);

    // The same, with the setting.
    configure(&repo, &[("pruneverifyunreachablealways", "true")]);
    prune_halted(&repo, &["--verify-remote"], &[not_on_remote], &[orphan]);

    // The option overrides the setting.
    prune(
        &repo,
        &["--verify-remote", "--no-verify-unreachable", "--dry-run"],
        &[verified, "4 files would be pruned"],
        &[],
    );

    // With --when-unverified=continue, the verified objects are pruned,
    // and the orphan is kept.
    prune(
        &repo,
        &["--verify-remote", "--when-unverified=continue"],
        &[not_on_remote, "Deleting objects: 100% (3/3), done."],
        &[],
    );
    assert_pruned(&repo, &[commit1, commit2, commit3]);
    assert_kept(&repo, &[orphan]);
}

const OLD_AND_PUSHED: &str = "To delete: pushed and too old";
const UNREFERENCED: &str = "To delete: unreferenced";
const RETAIN1: &str = "Retained content 1";

/// The history the stash tests share: old.dat replaced, a branch to delete
/// with `unreferenced.dat` and `foo/unreferenced.dat`, and
/// `stashedfile.dat` and `foo/stashedfile.dat` on main, all pushed.
fn stash_setup<'a>(t: &'a Test, name: &str, commit_attributes: bool) -> Repo<'a> {
    let repo = prune_setup(t, name);
    if commit_attributes {
        repo.git(["add", ".gitattributes"]).ok();
        repo.git(["commit", "-m", "attribs"]).ok();
    }
    let in_repo = "This is the original committed data";
    repo.add_commits(&[
        Commit::new().days_ago(20).file("old.dat", OLD_AND_PUSHED),
        Commit::new().days_ago(7).file("old.dat", RETAIN1),
        Commit::new()
            .days_ago(4)
            .new_branch("branch_to_delete")
            .file("unreferenced.dat", UNREFERENCED)
            .file("foo/unreferenced.dat", UNREFERENCED),
        Commit::new()
            .days_ago(1)
            .on("main")
            .file("stashedfile.dat", in_repo)
            .file("foo/stashedfile.dat", in_repo),
    ]);
    repo.git(["push", "origin", "main"]).ok();
    assert_kept(&repo, &[OLD_AND_PUSHED, UNREFERENCED, RETAIN1]);
    repo
}

/// Exclude `/foo`, turn on diff colors, prune, and check `stashed` is kept.
fn prune_stash(repo: &Repo, stashed: &[&str]) {
    // The stashes went through the clean filter.
    assert_kept(repo, stashed);
    configure(repo, &[("fetchexclude", "/foo")]);
    repo.git(["config", "color.diff", "always"]).ok();
    prune(repo, &[], &[], &[]);
    assert_pruned(repo, &[OLD_AND_PUSHED, UNREFERENCED]);
    assert_kept(repo, &[RETAIN1]);
    assert_kept(repo, stashed);
}

#[test]
fn prune_keep_stashed_changes() {
    let t = Test::new();
    let repo = stash_setup(&t, "prune_keep_stashed", false);
    let stashed = "This data will be stashed and should not be deleted";
    let stashed_excluded =
        "This data will be stashed and should not be deleted despite being excluded";
    let branch = "This data will be stashed on a branch and should not be deleted";
    let branch_excluded =
        "This data will be stashed on a branch and should not be deleted despite being excluded";

    repo.write("stashedfile.dat", stashed);
    repo.write("foo/stashedfile.dat", stashed_excluded);
    repo.git(["stash"]).ok();

    // Stash changes on a branch, then delete it.
    repo.git(["checkout", "branch_to_delete"]).ok();
    repo.write("unreferenced.dat", branch);
    repo.write("foo/unreferenced.dat", branch_excluded);
    repo.git(["stash"]).ok();
    repo.git(["checkout", "main"]).ok();
    repo.git(["branch", "-D", "branch_to_delete"]).ok();

    prune_stash(&repo, &[stashed, stashed_excluded, branch, branch_excluded]);
}

const INDEX: &str = "This data will be stashed from the index and should not be deleted";
const INDEX_EXCLUDED: &str =
    "This data will be stashed from the index and should not be deleted despite being excluded";
const STASHED: &str = "This data will be stashed and should not be deleted";
const STASHED_EXCLUDED: &str =
    "This data will be stashed and should not be deleted despite being excluded";
const INDEX_BRANCH: &str =
    "This data will be stashed on a branch from the index and should not be deleted";
const INDEX_EXCLUDED_BRANCH: &str = "This data will be stashed on a branch from the index and should not be deleted despite being excluded";
const STASHED_BRANCH: &str = "This data will be stashed on a branch and should not be deleted";
const STASHED_EXCLUDED_BRANCH: &str =
    "This data will be stashed on a branch and should not be deleted despite being excluded";

#[test]
fn prune_keep_stashed_changes_in_index() {
    let t = Test::new();
    let repo = stash_setup(&t, "prune_keep_stashed_index", false);

    // Changes in the index, then more in the working tree, stashed.
    repo.write("stashedfile.dat", INDEX);
    repo.write("foo/stashedfile.dat", INDEX_EXCLUDED);
    repo.git(["add", "stashedfile.dat", "foo/stashedfile.dat"])
        .ok();
    repo.write("stashedfile.dat", STASHED);
    repo.write("foo/stashedfile.dat", STASHED_EXCLUDED);
    repo.git(["stash"]).ok();

    // The same on a branch, then delete it.
    repo.git(["checkout", "branch_to_delete"]).ok();
    repo.write("unreferenced.dat", INDEX_BRANCH);
    repo.write("foo/unreferenced.dat", INDEX_EXCLUDED_BRANCH);
    repo.git(["add", "unreferenced.dat", "foo/unreferenced.dat"])
        .ok();
    repo.write("unreferenced.dat", STASHED_BRANCH);
    repo.write("foo/unreferenced.dat", STASHED_EXCLUDED_BRANCH);
    repo.git(["stash"]).ok();
    repo.git(["checkout", "main"]).ok();
    repo.git(["branch", "-D", "branch_to_delete"]).ok();

    prune_stash(
        &repo,
        &[
            INDEX,
            INDEX_EXCLUDED,
            STASHED,
            STASHED_EXCLUDED,
            INDEX_BRANCH,
            INDEX_EXCLUDED_BRANCH,
            STASHED_BRANCH,
            STASHED_EXCLUDED_BRANCH,
        ],
    );
}

#[test]
fn prune_keep_stashed_untracked_files() {
    let t = Test::new();
    // .gitattributes is committed, so git stash -u doesn't take it away
    // before the branch's files are changed.
    let repo = stash_setup(&t, "prune_keep_stashed_untracked", true);
    let untracked = "This UNTRACKED FILE data will be stashed and should not be deleted";
    let untracked_excluded =
        "This UNTRACKED FILE data will be stashed and should not be deleted despite being excluded";
    let untracked_branch =
        "This UNTRACKED FILE data will be stashed on a branch and should not be deleted";
    let untracked_excluded_branch = "This UNTRACKED FILE data will be stashed on a branch and should not be deleted despite being excluded";

    // Changes in the index, more in the working tree, and untracked files,
    // stashed.
    repo.write("stashedfile.dat", INDEX);
    repo.write("foo/stashedfile.dat", INDEX_EXCLUDED);
    repo.git(["add", "stashedfile.dat", "foo/stashedfile.dat"])
        .ok();
    repo.write("stashedfile.dat", STASHED);
    repo.write("foo/stashedfile.dat", STASHED_EXCLUDED);
    repo.write("untrackedfile.dat", untracked);
    repo.write("foo/untrackedfile.dat", untracked_excluded);
    repo.git(["stash", "-u"]).ok();

    // The same on a branch, then delete it.
    repo.git(["checkout", "branch_to_delete"]).ok();
    repo.write("unreferenced.dat", INDEX_BRANCH);
    repo.write("foo/unreferenced.dat", INDEX_EXCLUDED_BRANCH);
    repo.git(["add", "unreferenced.dat", "foo/unreferenced.dat"])
        .ok();
    repo.write("unreferenced.dat", STASHED_BRANCH);
    repo.write("foo/unreferenced.dat", STASHED_EXCLUDED_BRANCH);
    repo.write("untrackedfile.dat", untracked_branch);
    repo.write("foo/untrackedfile.dat", untracked_excluded_branch);
    repo.git(["stash", "-u"]).ok();
    repo.git(["checkout", "main"]).ok();
    repo.git(["branch", "-D", "branch_to_delete"]).ok();

    prune_stash(
        &repo,
        &[
            INDEX,
            INDEX_EXCLUDED,
            STASHED,
            STASHED_EXCLUDED,
            untracked,
            untracked_excluded,
            INDEX_BRANCH,
            INDEX_EXCLUDED_BRANCH,
            STASHED_BRANCH,
            STASHED_EXCLUDED_BRANCH,
            untracked_branch,
            untracked_excluded_branch,
        ],
    );
}

const IN_REPO: &str = "this is the original committed data";

/// A commit from yesterday with `file.dat` containing [`IN_REPO`], then
/// one replacing it with `new`, and a stashed change to `stashed`.
fn recent_setup<'a>(t: &'a Test, name: &str, new: &str, stashed: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(&format!("remote_{name}"));
    repo.lfs(["track", "*.dat"]).ok();
    repo.add_commits(&[Commit::new().days_ago(1).file("file.dat", IN_REPO)]);
    repo.write("file.dat", new);
    repo.commit_all("Update file.dat");
    repo.write("file.dat", stashed);
    repo.git(["stash"]).ok();
    configure(
        &repo,
        &[
            ("fetchrecentrefsdays", "5"),
            ("fetchrecentremoterefs", "true"),
            ("fetchrecentcommitsdays", "3"),
        ],
    );
    assert_kept(&repo, &[new, IN_REPO, stashed]);
    repo
}

#[test]
fn prune_recent_changes_with_recent() {
    let t = Test::new();
    let new = "this data will be recent";
    let stashed = "This data will be stashed and should not be deleted";
    let repo = recent_setup(&t, "prune_recent_arg", new, stashed);

    // Nothing is pushed, so nothing is pruned.
    prune(&repo, &["--recent"], &[], &[]);
    assert_kept(&repo, &[new, IN_REPO, stashed]);

    repo.git(["push", "origin", "HEAD"]).ok();
    prune(&repo, &["--recent"], &[], &[]);
    assert_kept(&repo, &[new, stashed]);
    assert_pruned(&repo, &[IN_REPO]);
}

#[test]
fn prune_keep_files_in_index() {
    let t = Test::new();
    let repo = t.remote_and_clone("remote_prune_index");
    repo.lfs(["track", "*.dat"]).ok();
    let new = "this data will be indexed";
    let untracked = "This data will be untracked and added to the index and should not be deleted";
    repo.add_commits(&[Commit::new().days_ago(1).file("file.dat", IN_REPO)]);

    // A change and a new file, staged.
    repo.write("file.dat", new);
    repo.git(["add", "."]).ok();
    repo.write("untracked.dat", untracked);
    repo.git(["add", "."]).ok();
    assert_kept(&repo, &[new, IN_REPO, untracked]);

    // Nothing is pushed, so nothing is pruned.
    prune(&repo, &[], &[], &[]);
    assert_kept(&repo, &[new, IN_REPO, untracked]);

    // --force keeps what's in the index.
    repo.git(["push", "origin", "HEAD"]).ok();
    prune(&repo, &["--force"], &[], &[]);
    assert_kept(&repo, &[new, untracked]);
    assert_pruned(&repo, &[IN_REPO]);
}

#[test]
fn prune_force() {
    let t = Test::new();
    let new = "this data will be recent";
    let stashed = "This data will be stashed and should not be deleted";
    let repo = recent_setup(&t, "prune_force", new, stashed);

    // Nothing is pushed, so nothing is pruned.
    prune(&repo, &["--force"], &[], &[]);
    assert_kept(&repo, &[new, IN_REPO, stashed]);

    // --force prunes even HEAD's object, but not stashes.
    repo.git(["push", "origin", "HEAD"]).ok();
    prune(&repo, &["--force"], &[], &[]);
    assert_pruned(&repo, &[new, IN_REPO]);
    assert_kept(&repo, &[stashed]);
}

#[test]
fn prune_does_not_fail_on_empty_files() {
    let t = Test::new();
    let repo = t.remote_and_clone("remote-prune-empty-file");
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    repo.write("empty.dat", "");
    repo.git(["add", ".gitattributes", "empty.dat"]).ok();
    repo.git(["commit", "-m", "Add empty"]).ok();
    repo.git(["push", "origin", "main"]).ok();
    prune(&repo, &["--force"], &[], &[]);
}

#[test]
fn prune_does_not_invoke_external_diff_programs() {
    let t = Test::new();
    let repo = t.remote_and_clone("remote-prune-external-diff");
    // A textconv that fails.
    repo.git(["config", "diff.word.textconv", "false"]).ok();
    repo.write(".git/info/attributes", "*.dot diff=word\n");
    for n in 1..=1000 {
        repo.write(&format!("{n}.dot"), format!("{n}\n"));
    }
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-am", "initial"]).ok();
    prune(&repo, &[], &[], &[]);
}

/// Go's `bufio.MaxScanTokenSize`, which upstream's diff parsing once
/// couldn't read lines longer than.
const MAX_SCAN_TOKEN_SIZE: usize = 64 * 1024;

/// A repository where `test.dat` was committed as an LFS file, then
/// untracked and replaced with one long line.
fn long_line_setup<'a>(t: &'a Test) -> Repo<'a> {
    t.command(t.dir(), "git")
        .args(["init", "prune_longlines"])
        .run()
        .ok();
    let repo = t.repo("prune_longlines");
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    repo.write("test.dat", "test data\n");
    repo.git(["add", ".gitattributes", "test.dat"]).ok();
    repo.git(["commit", "-m", "tracked"]).ok();
    repo.lfs(["untrack", "*.dat"])
        .ok()
        .has(r#"Untracking "*.dat""#);
    repo.write("test.dat", "A".repeat(MAX_SCAN_TOKEN_SIZE * 2));
    repo
}

#[test]
fn prune_doesnt_hang_on_long_lines_in_diff() {
    let t = Test::new();
    let repo = long_line_setup(&t);
    repo.git(["add", ".gitattributes", "test.dat"]).ok();
    repo.git(["commit", "-m", "untracked"]).ok();
    prune(&repo, &[], &[], &[]);
}

#[test]
fn prune_doesnt_hang_on_long_lines_in_stash_diff() {
    let t = Test::new();
    let repo = long_line_setup(&t);
    repo.git(["stash"]).ok();
    prune(&repo, &[], &[], &[]);
}
