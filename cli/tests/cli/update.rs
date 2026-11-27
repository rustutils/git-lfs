//! `t-update.sh`.
//!
//! The shell's "update lfs.{url}.access" sets up the settings and its
//! expected output, but never runs `git lfs update`. This one does.

use crate::harness::{Repo, Test};
use crate::install::{HOOKS, hook};

const UPDATED: &str = "Updated Git hooks.\n";

/// Run `git lfs update` with `args` in `repo` and check that it prints
/// that it updated the hooks.
#[track_caller]
fn assert_updates(repo: &Repo, args: &[&str]) {
    let run = repo.lfs_command().args(["update"]).args(args).run().ok();
    assert_eq!(run.stdout(), UPDATED, "{run}");
}

/// Check that each hook in `dir` is the current one.
#[track_caller]
fn assert_hooks(repo: &Repo, dir: &str) {
    for name in HOOKS {
        assert_eq!(repo.read(&format!("{dir}/{name}")), hook(name), "{name}");
    }
}

/// Write `contents` to each hook in `dir`.
fn write_hooks(repo: &Repo, dir: &str, contents: &str) {
    for name in HOOKS {
        repo.write(&format!("{dir}/{name}"), contents);
    }
}

/// Check that each hook in `dir` is still `test`.
#[track_caller]
fn assert_hooks_untouched(repo: &Repo, dir: &str) {
    for name in HOOKS {
        assert_eq!(repo.read(&format!("{dir}/{name}")), "test\n", "{name}");
    }
}

/// The `--manual` instructions for the hooks in `dir`.
fn manual(dir: &str) -> String {
    HOOKS
        .iter()
        .map(|name| {
            let body: String = hook(name).lines().map(|l| format!("\t{l}\n")).collect();
            format!("Add the following to '{dir}/{name}':\n\n{body}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Earlier pre-push hooks that `update` replaces.
const OLD_PRE_PUSH: [&str; 7] = [
    "#!/bin/sh\ngit lfs push --stdin $*\n",
    "#!/bin/sh\ngit lfs push --stdin \"$@\"\n",
    "#!/bin/sh\ngit lfs pre-push \"$@\"\n",
    "#!/bin/sh\ncommand -v git-lfs >/dev/null 2>&1 || { echo >&2 \"\\nThis repository has been set up with Git LFS but Git LFS is not installed.\\n\"; exit 0; }\ngit lfs pre-push \"$@\"\n",
    "#!/bin/sh\ncommand -v git-lfs >/dev/null 2>&1 || { echo >&2 \"\\nThis repository has been set up with Git LFS but Git LFS is not installed.\\n\"; exit 2; }\ngit lfs pre-push \"$@\"\n",
    "#!/bin/sh\ncommand -v git-lfs >/dev/null 2>&1 || { echo >&2 \"\\nThis repository is configured for Git LFS but 'git-lfs' was not found on your path. If you no longer wish to use Git LFS, remove this hook by deleting '.git/hooks/pre-push'.\\n\"; exit 2; }\ngit lfs pre-push \"$@\"\n",
    "#!/bin/sh\ncommand -v git-lfs >/dev/null 2>&1 || { echo >&2 \"\\nThis repository is configured for Git LFS but 'git-lfs' was not found on your path. If you no longer wish to use Git LFS, remove this hook by deleting the 'pre-push' file in the hooks directory (set by 'core.hookspath'; usually '.git/hooks').\\n\"; exit 2; }\ngit lfs pre-push \"$@\"\n",
];

#[test]
fn update() {
    let t = Test::new();
    let repo = t.init("without-pre-push");

    // Twice.
    for _ in 0..2 {
        assert_updates(&repo, &[]);
        assert_hooks(&repo, ".git/hooks");
    }

    // Old hooks are replaced.
    for old in &OLD_PRE_PUSH[..3] {
        repo.write(".git/hooks/pre-push", old);
        assert_updates(&repo, &[]);
        assert_eq!(repo.read(".git/hooks/pre-push"), hook("pre-push"), "{old}");
    }

    // So are blank ones.
    for name in ["pre-push", "post-checkout", "post-merge"] {
        repo.write(&format!(".git/hooks/{name}"), "");
    }
    assert_updates(&repo, &[]);
    assert_hooks(&repo, ".git/hooks");

    for old in &OLD_PRE_PUSH[3..] {
        repo.write(".git/hooks/pre-push", old);
        assert_updates(&repo, &[]);
        assert_eq!(repo.read(".git/hooks/pre-push"), hook("pre-push"), "{old}");
    }

    // Unexpected hooks aren't.
    write_hooks(&repo, ".git/hooks", "test\n");
    let run = repo.lfs_command().args(["update"]).merged().run().fails();
    assert_eq!(
        run.stdout(),
        "Hook already exists: pre-push\n\
         \n\
         \ttest\n\
         \n\
         To resolve this, either:\n  \
         1: run `git lfs update --manual` for instructions on how to merge hooks.\n  \
         2: run `git lfs update --force` to overwrite your hook.\n"
    );
    assert_hooks_untouched(&repo, ".git/hooks");

    // `--manual` prints what to add.
    let run = repo
        .lfs_command()
        .args(["update", "--manual"])
        .merged()
        .run();
    assert_eq!(run.stdout().trim_end(), manual(".git/hooks").trim_end());
    assert_hooks_untouched(&repo, ".git/hooks");

    // `--force` replaces them.
    assert_updates(&repo, &["--force"]);
    assert_hooks(&repo, ".git/hooks");

    // The same with `core.hookspath`.
    std::fs::remove_dir_all(repo.path().join(".git/hooks")).unwrap();
    repo.git(["config", "core.hookspath", "hooks"]).ok();
    write_hooks(&repo, "hooks", "test\n");
    let run = repo
        .lfs_command()
        .args(["update", "--manual"])
        .merged()
        .run();
    assert_eq!(run.stdout().trim_end(), manual("hooks").trim_end());
    assert_hooks_untouched(&repo, "hooks");
    assert_updates(&repo, &["--force"]);
    assert_hooks(&repo, "hooks");
    assert!(!repo.path().join(".git/hooks").exists());

    // A bare repository has its hooks in `hooks`.
    t.repo("")
        .git(["clone", "--mirror", "without-pre-push", "bare"])
        .ok();
    let bare = t.repo("bare");
    bare.lfs(["env"]).ok();
    bare.lfs(["update"]).ok();
    assert_eq!(bare.read("hooks/pre-push"), hook("pre-push"));
}

/// An old hook indented with tabs is still recognized.
#[test]
fn update_with_leading_spaces() {
    let t = Test::new();
    let repo = t.init("update-leading-spaces");
    assert_updates(&repo, &[]);
    repo.write(
        ".git/hooks/pre-push",
        "#!/bin/sh\n\tcommand -v git-lfs >/dev/null 2>&1 || { echo >&2 \"\\nThis repository is configured for Git LFS but 'git-lfs' was not found on your path. If you no longer wish to use Git LFS, remove this hook by deleting '.git/hooks/pre-push'.\\n\"; exit 2; }\n\tgit lfs pre-push \"$@\"",
    );

    assert_updates(&repo, &[]);
}

/// The old access values `private` become `basic`, and invalid ones are
/// removed. The shell test never checks this.
#[test]
fn update_lfs_url_access() {
    let t = Test::new();
    let repo = t.init("update-access");
    for (url, value) in [
        ("http://example.com", "private"),
        ("https://example.com", "private"),
        ("https://example2.com", "basic"),
        ("https://example3.com", "other"),
    ] {
        repo.git(["config", &format!("lfs.{url}.access"), value])
            .ok();
    }

    let run = repo.lfs(["update"]).ok();

    // In any order: upstream's comes from a Go map.
    let mut lines: Vec<String> = run.stdout().lines().map(str::to_owned).collect();
    lines.sort();
    let mut expected = vec![
        "Updated Git hooks.",
        "Updated http://example.com access from private to basic.",
        "Updated https://example.com access from private to basic.",
        "Removed invalid https://example3.com access of other.",
    ];
    expected.sort();
    assert_eq!(lines, expected);
    for (url, value) in [
        ("http://example.com", "basic\n"),
        ("https://example.com", "basic\n"),
        ("https://example2.com", "basic\n"),
        ("https://example3.com", ""),
    ] {
        let run = repo.git(["config", &format!("lfs.{url}.access")]);
        assert_eq!(run.stdout(), value, "{url}");
    }
}

#[test]
fn update_outside_git_repository() {
    let t = Test::new();
    let dir = t.repo("");

    let run = dir.lfs(["update"]);

    assert_eq!(run.code(), Some(128), "{run}");
    assert!(run.stdout().contains("Not in a Git repository"), "{run}");
    assert!(!dir.path().join("hooks").exists(), "hooks directory made");
}
