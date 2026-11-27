//! `t-merge-driver.sh`.

use crate::harness::{Repo, Test};

/// The object ID of the clean merge, [`merged`].
const RESULT: &str = "07b26d7b3123467282635a68fdf9b59e81269cf9faf12282cedf30f393a55e5b";

/// The driver with Git's merge.
const DRIVER: &str =
    "git lfs merge-driver --ancestor %O --current %A --other %B --marker-size %L --output %A";

/// The driver with `program` passed explicitly.
fn driver_with(program: &str) -> String {
    format!("{DRIVER} --program '{program}'")
}

const GIT_MERGE_FILE: &str = "git merge-file --stdout --marker-size=%%L %%A %%O %%B >%%D";

/// The lines 1 to 10, with the lines in `changes` replaced.
fn lines(changes: &[(usize, &str)]) -> String {
    (1..=10)
        .map(|n| {
            let line = changes
                .iter()
                .find(|(at, _)| *at == n)
                .map_or_else(|| n.to_string(), |(_, s)| (*s).to_owned());
            line + "\n"
        })
        .collect()
}

/// Line 2 from `main` and line 9 from `other`.
fn merged() -> String {
    lines(&[(2, "A"), (9, "B")])
}

/// When `.gitattributes` tracks `*.dat`: from the start, or on each
/// branch after its first commit, so the merge base has no pointer.
#[derive(Clone, Copy, PartialEq)]
enum Tracking {
    First,
    Later,
}

/// A repository `name` with `driver` as the LFS merge driver, where
/// `other` changes `a.dat` with `theirs` and `main` with `ours`.
fn branches<'a>(
    t: &'a Test,
    name: &str,
    driver: &str,
    tracking: Tracking,
    ours: &[(usize, &str)],
    theirs: &[(usize, &str)],
) -> Repo<'a> {
    let repo = t.init(name);
    repo.git(["config", "merge.lfs.driver", driver]).ok();
    if tracking == Tracking::First {
        repo.lfs(["track", "*.dat"]).ok();
    } else {
        repo.write(".gitattributes", "");
    }
    repo.write("a.dat", lines(&[]));
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "Initial import"]).ok();

    repo.git(["checkout", "-b", "other"]).ok();
    repo.write("a.dat", lines(theirs));
    repo.git(["add", "-u"]).ok();
    repo.git(["commit", "-m", "B"]).ok();
    if tracking == Tracking::Later {
        repo.lfs(["track", "*.dat"]).ok();
        repo.git(["add", ".gitattributes"]).ok();
        repo.git(["commit", "-m", "B2"]).ok();
    }

    repo.git(["checkout", "--force", "main"]).ok();
    repo.write("a.dat", lines(ours));
    repo.git(["add", "-u"]).ok();
    repo.git(["commit", "-m", "A"]).ok();
    if tracking == Tracking::Later {
        repo.lfs(["track", "*.dat"]).ok();
        repo.git(["add", "-u"]).ok();
        repo.git(["commit", "-m", "A2"]).ok();
    }
    repo
}

/// Check that merging `other` gives [`merged`], stored as an object.
#[track_caller]
fn assert_merges(repo: &Repo) {
    repo.git(["merge", "other"]).ok();

    assert_eq!(repo.read("a.dat"), merged());
    repo.assert_pointer("main", "a.dat", RESULT, 21);
    repo.assert_local_object(RESULT, 21);
}

/// Check that merging `other` conflicts, with Git's conflict markers in
/// `a.dat`.
#[track_caller]
fn assert_conflicts(repo: &Repo) {
    repo.git(["merge", "other"]).fails();

    let actual: String = repo
        .read("a.dat")
        .lines()
        .map(|l| {
            if l.starts_with("<<<<<<<") {
                "<<<<<<<"
            } else if l.starts_with(">>>>>>>") {
                ">>>>>>>"
            } else {
                l
            }
        })
        .map(|l| format!("{l}\n"))
        .collect();
    let expected = "1\n<<<<<<<\nA\n3\n=======\n2\nB\n>>>>>>>\n4\n5\n6\n7\n8\n9\n10\n";
    assert_eq!(actual, expected);
}

#[test]
fn merge_driver_uses_git_merge_by_default() {
    let t = Test::new();
    let repo = branches(
        &t,
        "merge-driver-basic",
        DRIVER,
        Tracking::First,
        &[(2, "A")],
        &[(9, "B")],
    );
    assert_merges(&repo);
}

#[test]
fn merge_driver_uses_git_merge_when_explicit() {
    let t = Test::new();
    let repo = branches(
        &t,
        "merge-driver-explicit",
        &driver_with(GIT_MERGE_FILE),
        Tracking::First,
        &[(2, "A")],
        &[(9, "B")],
    );
    assert_merges(&repo);
}

/// The custom program takes the first half from `main` and the second
/// from `other`, though both changed lines in each half.
#[test]
fn merge_driver_uses_custom_driver_when_explicit() {
    let t = Test::new();
    let repo = branches(
        &t,
        "merge-driver-custom",
        &driver_with("(sed -n 1,5p %%A; sed -n 6,10p %%B) >%%D"),
        Tracking::First,
        &[(2, "A"), (9, "A")],
        &[(2, "B"), (9, "B")],
    );
    assert_merges(&repo);
}

#[test]
fn merge_driver_reports_conflicts() {
    let t = Test::new();
    let repo = branches(
        &t,
        "merge-driver-conflicts",
        &driver_with(GIT_MERGE_FILE),
        Tracking::First,
        &[(2, "A")],
        &[(3, "B")],
    );
    assert_conflicts(&repo);
}

#[test]
fn merge_driver_gracefully_handles_non_pointer() {
    let t = Test::new();
    let repo = branches(
        &t,
        "merge-driver-non-pointer",
        DRIVER,
        Tracking::Later,
        &[(2, "A")],
        &[(9, "B")],
    );
    assert_merges(&repo);
}

#[test]
fn merge_driver_reports_conflicts_with_non_pointer() {
    let t = Test::new();
    let repo = branches(
        &t,
        "conflicts-non-pointer",
        &driver_with(GIT_MERGE_FILE),
        Tracking::Later,
        &[(2, "A")],
        &[(3, "B")],
    );
    assert_conflicts(&repo);
}
