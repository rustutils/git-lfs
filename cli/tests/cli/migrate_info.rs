//! `t-migrate-info.sh`.
//!
//! Every run checks that `migrate info` leaves the existing refs alone,
//! where the shell tests check one or two of them.

use crate::harness::{Repo, Run, Test, sized};
use crate::migrate_fixtures::{self as fixtures, Attrs};

/// A row of the table, from its four columns.
fn row(columns: [&str; 4]) -> String {
    columns.join("\t")
}

/// All refs and where they point.
fn refs(repo: &Repo) -> String {
    repo.git(["show-ref", "--head"]).stdout()
}

/// Run `git lfs migrate info` with `args`, with stderr merged into stdout
/// if `merged`, and check it didn't move any ref.
fn run_info(repo: &Repo, args: &[&str], merged: bool) -> Run {
    let before = refs(repo);
    let mut cmd = repo.lfs_command().args(["migrate", "info"]).args(args);
    if merged {
        cmd = cmd.merged();
    }
    let run = cmd.run();
    // A fetch may add refs, such as origin/HEAD, but none may change.
    let after = refs(repo);
    for line in before.lines() {
        assert!(after.lines().any(|l| l == line), "{line} moved\n{run}");
    }
    run
}

/// Check that `migrate info` with `args` succeeds, and that the last lines
/// of its output, with stderr, are `rows`.
#[track_caller]
fn assert_info(repo: &Repo, args: &[&str], rows: &[String]) {
    let run = run_info(repo, args, true).ok();
    let stdout = run.stdout();
    let lines: Vec<&str> = stdout.lines().collect();
    let tail = &lines[lines.len().saturating_sub(rows.len())..];
    assert_eq!(tail, rows, "{run}");
}

/// Check that `migrate info` with `args` succeeds and prints nothing on
/// stdout.
#[track_caller]
fn assert_info_empty(repo: &Repo, args: &[&str]) {
    let run = run_info(repo, args, false).ok();
    assert_eq!(run.stdout(), "", "{run}");
}

/// Check that `migrate info` with `args` exits 2 with `message`.
#[track_caller]
fn assert_info_error(repo: &Repo, args: &[&str], message: &str) {
    let run = run_info(repo, args, true);
    assert_eq!(run.code(), Some(2), "{run}");
    run.has(message);
}

#[test]
fn migrate_info_default_branch() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info(
        &repo,
        &[],
        &[
            row(["*.md ", "140 B", "1/1 file ", "100%"]),
            row(["*.txt", "120 B", "1/1 file ", "100%"]),
        ],
    );
}

/// The shell test's fixture isn't bare either: it just runs with
/// `--everything`.
#[test]
fn migrate_info_bare_repository() {
    let t = Test::new();
    let repo = fixtures::multiple_remote_branches(&t);
    run_info(&repo, &["--everything"], true).ok();
}

#[test]
fn migrate_info_given_branch() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info(
        &repo,
        &["my-feature"],
        &[
            row(["*.md ", "170 B", "2/2 files", "100%"]),
            row(["*.txt", "120 B", "1/1 file ", "100%"]),
        ],
    );
}

#[test]
fn migrate_info_default_branch_with_filter() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info(
        &repo,
        &["--include", "*.md"],
        &[row(["*.md", "140 B", "1/1 file ", "100%"])],
    );
}

#[test]
fn migrate_info_given_branch_with_filter() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info(
        &repo,
        &["--include", "*.md", "my-feature"],
        &[row(["*.md", "170 B", "2/2 files", "100%"])],
    );
}

#[test]
fn migrate_info_default_branch_exclude_remote_refs() {
    let t = Test::new();
    let repo = fixtures::single_remote_branch(&t);
    assert_info(
        &repo,
        &[],
        &[
            row(["*.md ", "50 B", "1/1 file ", "100%"]),
            row(["*.txt", "30 B", "1/1 file ", "100%"]),
        ],
    );
}

#[test]
fn migrate_info_given_branch_exclude_remote_refs() {
    let t = Test::new();
    let repo = fixtures::multiple_remote_branches(&t);
    assert_info(
        &repo,
        &["my-feature"],
        &[
            row(["*.md ", "52 B", "2/2 files", "100%"]),
            row(["*.txt", "50 B", "2/2 files", "100%"]),
        ],
    );
}

/// Without the remote-tracking ref, and told not to fetch it, migrate
/// counts the pushed commit too.
#[test]
fn migrate_info_given_ref_skip_fetch() {
    let t = Test::new();
    let repo = fixtures::single_remote_branch(&t);
    let remote = repo
        .git(["rev-parse", "refs/remotes/origin/main"])
        .ok()
        .stdout();
    repo.git(["tag", "pseudo-remote", remote.trim()]).ok();
    repo.git(["update-ref", "-d", "refs/remotes/origin/main"])
        .ok();
    assert_info(
        &repo,
        &["--skip-fetch"],
        &[
            row(["*.md ", "190 B", "2/2 files", "100%"]),
            row(["*.txt", "150 B", "2/2 files", "100%"]),
        ],
    );
}

/// What's only on `my-feature`.
fn feature_only() -> Vec<String> {
    vec![
        row(["*.md ", "31 B", "1/1 file ", "100%"]),
        row(["*.txt", "30 B", "1/1 file ", "100%"]),
    ]
}

#[test]
fn migrate_info_include_exclude_ref() {
    let t = Test::new();
    let repo = fixtures::multiple_remote_branches(&t);
    let args = [
        "--include-ref=refs/heads/my-feature",
        "--exclude-ref=refs/heads/main",
    ];
    assert_info(&repo, &args, &feature_only());
}

#[test]
fn migrate_info_include_exclude_ref_args() {
    let t = Test::new();
    let repo = fixtures::multiple_remote_branches(&t);
    assert_info(&repo, &["my-feature", "^main"], &feature_only());
}

#[test]
fn migrate_info_include_exclude_ref_with_filter() {
    let t = Test::new();
    let repo = fixtures::multiple_remote_branches(&t);
    let args = [
        "--include=*.txt",
        "--include-ref=refs/heads/my-feature",
        "--exclude-ref=refs/heads/main",
    ];
    assert_info(&repo, &args, &[row(["*.txt", "30 B", "1/1 file ", "100%"])]);
}

#[test]
fn migrate_info_invalid_ref() {
    let t = Test::new();
    let repo = t.init("migrate-info-invalid-ref");
    repo.git(["commit", "--allow-empty", "-m", "initial commit"])
        .ok();
    run_info(&repo, &["jibberish"], true)
        .fails()
        .has("can't resolve ref");
}

#[test]
fn migrate_info_nested_sub_trees_no_filter() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_deep_trees(&t);
    let run = run_info(&repo, &[], false).ok();
    assert_eq!(
        run.stdout(),
        format!("{}\n", row(["*.txt", "120 B", "1/1 file ", "100%"])),
        "{run}"
    );
}

#[test]
fn migrate_info_above_threshold() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info(
        &repo,
        &["--above=130B"],
        &[row(["*.md", "140 B", "1/1 file ", "100%"])],
    );
}

/// [`fixtures::multiple_local_branches`], with `b.bin` committed on
/// `main`.
fn with_bin(t: &Test) -> Repo<'_> {
    let repo = fixtures::multiple_local_branches(t);
    repo.write("b.bin", sized("b.bin", 160));
    repo.git(["add", "b.bin"]).ok();
    repo.git(["commit", "-m", "b.bin"]).ok();
    repo
}

#[test]
fn migrate_info_above_threshold_top() {
    let t = Test::new();
    let repo = with_bin(&t);
    // Only the largest entry, with --top=1.
    assert_info(
        &repo,
        &["--above=130B", "--top=1"],
        &[row(["*.bin", "160 B", "1/1 file ", "100%"])],
    );
}

#[test]
fn migrate_info_top() {
    let t = Test::new();
    let repo = with_bin(&t);
    // Nothing for a --top of zero or less.
    assert_info_empty(&repo, &["--everything", "--top=-1"]);
    assert_info_empty(&repo, &["--everything", "--top=0"]);

    let md = row(["*.md ", "170 B", "2/2 files", "100%"]);
    let bin = row(["*.bin", "160 B", "1/1 file ", "100%"]);
    let txt = row(["*.txt", "120 B", "1/1 file ", "100%"]);
    assert_info(
        &repo,
        &["--everything", "--top=2"],
        &[md.clone(), bin.clone()],
    );
    // More than there are is fine.
    assert_info(&repo, &["--everything", "--top=10"], &[md, bin, txt]);
}

#[test]
fn migrate_info_given_unit() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info(
        &repo,
        &["--unit=kb"],
        &[
            row(["*.md ", "0.1", "1/1 file ", "100%"]),
            row(["*.txt", "0.1", "1/1 file ", "100%"]),
        ],
    );
}

#[test]
fn migrate_info_doesnt_show_empty_info_entries() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info_empty(&repo, &["--above=1mb"]);
}

#[test]
fn migrate_info_empty_set() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info_empty(
        &repo,
        &[
            "--include-ref=refs/heads/main",
            "--exclude-ref=refs/heads/main",
        ],
    );
}

#[test]
fn migrate_info_no_extension_files() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_with_alternate_names(&t);
    assert_info(
        &repo,
        &["--everything"],
        &[
            row(["no_extension", "220 B", "2/2 files", "100%"]),
            row(["*.txt       ", "170 B", "2/2 files", "100%"]),
        ],
    );
}

#[test]
fn migrate_info_everything() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info(
        &repo,
        &["--everything"],
        &[
            row(["*.md ", "170 B", "2/2 files", "100%"]),
            row(["*.txt", "120 B", "1/1 file ", "100%"]),
        ],
    );
}

/// Check that `migrate info` with `args` refuses the `.gitattributes`
/// symlink, which stays a symlink.
#[track_caller]
fn assert_refuses_symlink(repo: &Repo, args: &[&str]) {
    run_info(repo, args, true)
        .fails()
        .has("expected '.gitattributes' to be a file, got a symbolic link");
    let tree = repo
        .git(["ls-tree", "refs/heads/main", "--", ".gitattributes"])
        .ok()
        .stdout();
    assert!(tree.starts_with("120000 blob "), "{tree}");
}

#[test]
fn migrate_info_existing_gitattributes_symlink() {
    let t = Test::new();
    let repo = fixtures::local_branch_with_gitattrs(&t, Attrs::Link);
    assert_refuses_symlink(&repo, &["--everything"]);
}

#[test]
fn migrate_info_potential_fixup_fixup_gitattributes_symlink() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked_corrupt(&t, Attrs::Link);
    assert_refuses_symlink(&repo, &[]);
}

/// With nothing tracked, `--fixup` reports nothing.
#[test]
fn migrate_info_no_potential_fixup_fixup_no_gitattributes() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info_empty(&repo, &["--everything", "--fixup"]);
}

#[test]
fn migrate_info_no_potential_fixup_fixup_gitattributes_with_macro() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    repo.write(".gitattributes", "[attr]foo foo\n");
    repo.write("a.md", sized("a.md macro", 30));
    repo.git(["add", ".gitattributes", "a.md"]).ok();
    repo.git(["commit", "-m", "macro"]).ok();
    assert_info_empty(&repo, &["--everything", "--fixup"]);
}

/// The `.gitattributes`, and the LFS objects behind the pointers, for the
/// fixtures with everything tracked: `objects` of them, `size` bytes in
/// all.
fn tracked_rows(size: &str, objects: &str) -> Vec<String> {
    vec![
        row(["*.gitattributes", "83 B ", "1/1 file ", "100%"]),
        String::new(),
        row(["LFS Objects    ", size, objects, "100%"]),
    ]
}

const ATTRIBUTES_ONLY: [&str; 4] = ["*.gitattributes", "83 B", "1/1 file ", "100%"];

#[test]
fn migrate_info_all_files_tracked() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked(&t, Attrs::Plain);
    assert_info(&repo, &[], &tracked_rows("260 B", "2/2 files"));
}

#[test]
fn migrate_info_all_files_tracked_pointers_follow() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked(&t, Attrs::Plain);
    assert_info(
        &repo,
        &["--pointers=follow"],
        &tracked_rows("260 B", "2/2 files"),
    );
}

/// The pointers themselves are counted.
#[test]
fn migrate_info_all_files_tracked_pointers_no_follow() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked(&t, Attrs::Plain);
    assert_info(
        &repo,
        &["--pointers=no-follow"],
        &[
            row(["*.md           ", "128 B", "1/1 file ", "100%"]),
            row(["*.txt          ", "128 B", "1/1 file ", "100%"]),
            row(["*.gitattributes", "83 B ", "1/1 file ", "100%"]),
        ],
    );
}

#[test]
fn migrate_info_all_files_tracked_pointers_ignore() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked(&t, Attrs::Plain);
    assert_info(&repo, &["--pointers=ignore"], &[row(ATTRIBUTES_ONLY)]);
}

#[test]
fn migrate_info_all_files_tracked_fixup() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked(&t, Attrs::Plain);
    assert_info_empty(&repo, &["--fixup"]);
}

#[test]
fn migrate_info_all_files_tracked_everything() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_tracked(&t);
    assert_info(
        &repo,
        &["--everything"],
        &tracked_rows("290 B", "3/3 files"),
    );
}

#[test]
fn migrate_info_all_files_tracked_everything_and_pointers_follow() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_tracked(&t);
    assert_info(
        &repo,
        &["--everything", "--pointers=follow"],
        &tracked_rows("290 B", "3/3 files"),
    );
}

#[test]
fn migrate_info_all_files_tracked_everything_and_pointers_no_follow() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_tracked(&t);
    assert_info(
        &repo,
        &["--everything", "--pointers=no-follow"],
        &[
            row(["*.md           ", "255 B", "2/2 files", "100%"]),
            row(["*.txt          ", "128 B", "1/1 file ", "100%"]),
            row(["*.gitattributes", "83 B ", "1/1 file ", "100%"]),
        ],
    );
}

#[test]
fn migrate_info_all_files_tracked_everything_and_pointers_ignore() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_tracked(&t);
    assert_info(
        &repo,
        &["--everything", "--pointers=ignore"],
        &[row(ATTRIBUTES_ONLY)],
    );
}

#[test]
fn migrate_info_all_files_tracked_everything_and_fixup() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches_tracked(&t);
    assert_info_empty(&repo, &["--everything", "--fixup"]);
}

/// Files that should be pointers but aren't are reported.
#[test]
fn migrate_info_potential_fixup() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked_corrupt(&t, Attrs::Plain);
    assert_info(
        &repo,
        &[],
        &[
            row(["*.txt          ", "120 B", "1/1 file ", "100%"]),
            row(["*.gitattributes", "42 B ", "1/1 file ", "100%"]),
        ],
    );
}

/// With `--fixup`, only those files, without the .gitattributes.
fn fixup_rows() -> [String; 1] {
    [row(["*.txt", "120 B", "1/1 file ", "100%"])]
}

#[test]
fn migrate_info_potential_fixup_fixup() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked_corrupt(&t, Attrs::Plain);
    assert_info(&repo, &["--fixup"], &fixup_rows());
}

#[test]
fn migrate_info_potential_fixup_fixup_gitattributes_with_macro() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked_corrupt(&t, Attrs::Macro);
    assert_info(&repo, &["--fixup"], &fixup_rows());
}

/// The shell suite's note says migrate doesn't fully handle macros yet, but
/// the test runs and passes.
#[test]
fn migrate_info_potential_fixup_fixup_gitattributes_with_lfs_macro() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked_corrupt(&t, Attrs::LfsMacro);
    assert_info(&repo, &["--fixup"], &fixup_rows());
}

/// `a.txt` should be a pointer and isn't, and `dir/b.txt` isn't tracked.
#[test]
fn migrate_info_potential_fixup_complex_nested() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_complex_tracked(&t);
    assert_info(
        &repo,
        &[],
        &[
            row(["*.gitattributes", "69 B", "2/2 files", "100%"]),
            row(["*.txt          ", "2 B ", "2/2 files", "100%"]),
        ],
    );
}

#[test]
fn migrate_info_potential_fixup_complex_nested_fixup() {
    let t = Test::new();
    let repo = fixtures::single_local_branch_complex_tracked(&t);
    assert_info(
        &repo,
        &["--fixup"],
        &[row(["*.txt", "1 B", "1/1 file ", "100%"])],
    );
}

/// A tag named like HEAD's commit doesn't confuse it.
#[test]
fn migrate_info_ambiguous_reference() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    let head = repo.git(["rev-parse", "HEAD"]).ok().stdout();
    repo.git(["tag", head.trim()]).ok();
    run_info(&repo, &["--everything"], true).ok();
}

#[test]
fn migrate_info_everything_with_args() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info_error(
        &repo,
        &["--everything", "main"],
        "Cannot use --everything with explicit reference arguments",
    );
}

#[test]
fn migrate_info_everything_with_include_ref() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info_error(
        &repo,
        &["--everything", "--include-ref=refs/heads/main"],
        "Cannot use --everything with --include-ref or --exclude-ref",
    );
}

#[test]
fn migrate_info_everything_with_exclude_ref() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info_error(
        &repo,
        &["--everything", "--exclude-ref=refs/heads/main"],
        "Cannot use --everything with --include-ref or --exclude-ref",
    );
}

#[test]
fn migrate_info_pointers_invalid() {
    let t = Test::new();
    let repo = fixtures::multiple_local_branches(&t);
    assert_info_error(
        &repo,
        &["--everything", "--pointers=foo"],
        "Unsupported --pointers option value",
    );
}

/// The four `--fixup` combinations that are refused.
fn fixup_conflict(args: &[&str], message: &str) {
    let t = Test::new();
    let repo = fixtures::single_local_branch_tracked_corrupt(&t, Attrs::Plain);
    let mut all = vec!["--everything", "--fixup"];
    all.extend(args);
    assert_info_error(&repo, &all, message);
}

#[test]
fn migrate_info_fixup_pointers_follow() {
    fixup_conflict(
        &["--pointers=follow"],
        "Cannot use --fixup with --pointers=follow",
    );
}

#[test]
fn migrate_info_fixup_pointers_no_follow() {
    fixup_conflict(
        &["--pointers=no-follow"],
        "Cannot use --fixup with --pointers=no-follow",
    );
}

#[test]
fn migrate_info_fixup_include() {
    fixup_conflict(
        &["--include=*.txt"],
        "Cannot use --fixup with --include, --exclude",
    );
}

#[test]
fn migrate_info_fixup_exclude() {
    fixup_conflict(
        &["--exclude=*.txt"],
        "Cannot use --fixup with --include, --exclude",
    );
}
