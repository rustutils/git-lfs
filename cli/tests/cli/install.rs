//! `t-install.sh`.
//!
//! Like the shell tests, these start in the test's directory, which isn't a
//! repository, with the global config that `Test::new` installed.

use std::os::unix::fs::{MetadataExt, PermissionsExt};

use crate::harness::{Repo, Test};

const INSTALLED: &str = "Updated Git hooks.\nGit LFS initialized.\n";

/// The hook `git lfs install` writes for `name`.
fn hook(name: &str) -> String {
    format!(
        "#!/bin/sh\n\
         command -v git-lfs >/dev/null 2>&1 || {{ printf >&2 \"\\n%s\\n\\n\" \
         \"This repository is configured for Git LFS but 'git-lfs' was not \
         found on your path. If you no longer wish to use Git LFS, remove \
         this hook by deleting the '{name}' file in the hooks directory (set \
         by 'core.hookspath'; usually '.git/hooks').\"; exit 2; }}\n\
         git lfs {name} \"$@\"\n"
    )
}

const HOOKS: [&str; 4] = ["pre-push", "post-checkout", "post-commit", "post-merge"];

/// The value of `key` that `git config` reads in `dir`, with `args` such
/// as `--global`, or `None` if it's unset.
fn config(dir: &Repo, args: &[&str], key: &str) -> Option<String> {
    let mut all = vec!["config"];
    all.extend(args);
    all.push(key);
    let run = dir.git(all);
    (run.code() == Some(0)).then(|| run.stdout().trim_end().to_owned())
}

/// The directory `name` in the test's directory, made empty.
fn mkdir<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    std::fs::create_dir(t.dir().join(name)).unwrap();
    t.repo(name)
}

/// Check the three filter settings that `git config` reads in `dir` with
/// `args`.
#[track_caller]
fn assert_filters(dir: &Repo, args: &[&str], smudge: &str, clean: &str, process: &str) {
    assert_eq!(
        config(dir, args, "filter.lfs.smudge").as_deref(),
        Some(smudge)
    );
    assert_eq!(
        config(dir, args, "filter.lfs.clean").as_deref(),
        Some(clean)
    );
    assert_eq!(
        config(dir, args, "filter.lfs.process").as_deref(),
        Some(process)
    );
}

/// Check that `dir` has the default filter settings, as read with `args`.
#[track_caller]
fn assert_default_filters(dir: &Repo, args: &[&str]) {
    assert_filters(
        dir,
        args,
        "git-lfs smudge -- %f",
        "git-lfs clean -- %f",
        "git-lfs filter-process",
    );
}

/// Set the three global filter settings to values `install --local` and
/// `--file` must leave alone.
fn set_other_global_filters(t: &Test) {
    t.git_config_global("filter.lfs.smudge", "global smudge");
    t.git_config_global("filter.lfs.clean", "global clean");
    t.git_config_global("filter.lfs.process", "global filter");
}

#[test]
fn install_again() {
    let t = Test::new();
    let dir = t.repo("");
    assert_default_filters(&dir, &[]);
    let gitconfig = t.home().join(".gitconfig");
    let inode = std::fs::metadata(&gitconfig).unwrap().ino();

    // Upstream traces the `git config` commands it runs. Ours doesn't, so
    // also check that the file wasn't replaced, as `git config` does when
    // it writes.
    dir.lfs_command()
        .args(["install", "--skip-repo"])
        .env("GIT_TRACE", "1")
        .merged()
        .run()
        .ok()
        .lacks("--replace-all");

    assert_default_filters(&dir, &[]);
    assert_eq!(
        std::fs::metadata(&gitconfig).unwrap().ino(),
        inode,
        "config rewritten"
    );
}

#[test]
fn install_with_old_non_upgradeable_settings() {
    let t = Test::new();
    let dir = t.repo("");
    t.git_config_global("filter.lfs.smudge", "git-lfs smudge --something %f");
    t.git_config_global("filter.lfs.clean", "git-lfs clean --something %f");

    let run = dir.lfs_command().args(["install"]).run();
    assert_eq!(run.code(), Some(2), "{run}");
    let stdout = run.stdout();
    assert!(
        stdout.contains("clean\" attribute should be")
            || stdout.contains("smudge\" attribute should be"),
        "{run}"
    );
    assert!(!stdout.contains("(MISSING)"), "{run}");
    assert_eq!(
        config(&dir, &["--global"], "filter.lfs.smudge").as_deref(),
        Some("git-lfs smudge --something %f")
    );
    assert_eq!(
        config(&dir, &["--global"], "filter.lfs.clean").as_deref(),
        Some("git-lfs clean --something %f")
    );

    dir.lfs(["install", "--force"]).ok();

    assert_default_filters(&dir, &["--global"]);
}

#[test]
fn install_with_upgradeable_settings() {
    let t = Test::new();
    let dir = t.repo("");
    t.git_config_global("filter.lfs.smudge", "git-lfs smudge %f");
    t.git_config_global("filter.lfs.clean", "git-lfs clean %f");

    // This old style is upgraded without --force.
    dir.lfs(["install"]).ok();

    assert_default_filters(&dir, &["--global"]);
}

/// Check that each of the four hooks in `hooks` is the one `install`
/// writes.
#[track_caller]
fn assert_hooks(repo: &Repo, hooks: &str) {
    for name in HOOKS {
        assert_eq!(repo.read(&format!("{hooks}/{name}")), hook(name), "{name}");
    }
}

#[test]
fn install_updates_repo_hooks() {
    let t = Test::new();
    let repo = t.init("install-repo-hooks");

    assert_eq!(repo.lfs(["install"]).ok().stdout(), INSTALLED);
    assert_hooks(&repo, ".git/hooks");

    // An old hook is replaced.
    repo.write(
        ".git/hooks/pre-push",
        "#!/bin/sh\ngit lfs push --stdin $*\n",
    );
    assert_eq!(repo.lfs(["install"]).ok().stdout(), INSTALLED);
    assert_eq!(repo.read(".git/hooks/pre-push"), hook("pre-push"));

    // An unexpected one isn't.
    for name in HOOKS {
        repo.write(&format!(".git/hooks/{name}"), "test\n");
    }
    let run = repo.lfs_command().args(["install"]).merged().run().fails();
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
    for name in HOOKS {
        assert_eq!(repo.read(&format!(".git/hooks/{name}")), "test\n", "{name}");
    }

    // Unless forced.
    assert_eq!(repo.lfs(["install", "--force"]).ok().stdout(), INSTALLED);
    assert_hooks(&repo, ".git/hooks");

    // A bare repository has its hooks in `hooks`.
    t.repo("")
        .git([
            "clone",
            "--mirror",
            "install-repo-hooks",
            "bare-install-repo-hooks",
        ])
        .ok();
    let bare = t.repo("bare-install-repo-hooks");
    bare.lfs(["env"]).ok();
    bare.lfs(["install"]).ok();
    assert_eq!(bare.read("hooks/pre-push"), hook("pre-push"));
}

#[test]
fn install_outside_repository_directory() {
    let t = Test::new();
    let dir = t.repo("");

    // Being in a repository isn't needed.
    dir.lfs_command()
        .args(["install"])
        .merged()
        .run()
        .ok()
        .lacks("Not in a Git repository")
        .lacks("Error");

    assert!(!dir.path().join("hooks").exists(), "hooks directory made");
}

#[test]
fn install_skip_smudge() {
    let t = Test::new();
    let dir = mkdir(&t, "install-skip-smudge-test");

    dir.lfs(["install"]).ok();
    assert_default_filters(&dir, &["--global"]);

    dir.lfs(["install", "--skip-smudge"]).ok();
    assert_filters(
        &dir,
        &["--global"],
        "git-lfs smudge --skip -- %f",
        "git-lfs clean -- %f",
        "git-lfs filter-process --skip",
    );

    dir.lfs(["install"]).ok();
    assert_default_filters(&dir, &["--global"]);

    assert!(!dir.path().join("lfs").exists(), "lfs directory made");
}

/// Check that the global filter settings are still the ones
/// [`set_other_global_filters`] set.
#[track_caller]
fn assert_other_global_filters(dir: &Repo) {
    assert_filters(
        dir,
        &["--global"],
        "global smudge",
        "global clean",
        "global filter",
    );
}

#[test]
fn install_local() {
    let t = Test::new();
    set_other_global_filters(&t);
    let repo = t.init("install-local-repo");

    repo.lfs(["install", "--local"]).ok();

    assert_default_filters(&repo, &[]);
    assert_default_filters(&repo, &["--local"]);
    assert_other_global_filters(&repo);
}

#[test]
fn install_file() {
    let t = Test::new();
    set_other_global_filters(&t);
    let repo = t.init("install-file-repo");

    repo.lfs(["install", "--file=test-file"]).ok();

    assert_default_filters(&repo, &["--file", "test-file"]);
    assert_other_global_filters(&repo);
}

#[test]
fn install_local_with_failed_permissions() {
    let t = Test::new();
    // Root is exempt from permissions.
    if t.is_root() {
        return;
    }
    let repo = t.init("install-local-repo-perms");
    let git_dir = repo.path().join(".git");
    // Without a writable .git, the config can't be written.
    std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

    let run = repo.lfs_command().args(["install", "--local"]).run();

    std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(run.code(), Some(2), "{run}");
    let stdout = run.stdout();
    assert!(
        stdout
            .lines()
            .any(|l| l.find("error running").is_some_and(|i| {
                let rest = &l[i..];
                rest.find("git")
                    .is_some_and(|j| rest[j..].contains("config"))
            })),
        "{run}"
    );
}

#[test]
fn install_local_outside_repository() {
    let t = Test::new();
    let dir = t.repo("");

    let run = dir.lfs_command().args(["install", "--local"]).run().fails();

    assert_eq!(run.stdout(), "Not in a Git repository.\n");
}

#[test]
fn install_local_with_conflicting_scope() {
    let t = Test::new();
    let repo = t.init("t-install-scope-conflict");

    let run = repo
        .lfs_command()
        .args(["install", "--local", "--system"])
        .run()
        .fails();

    assert_eq!(
        run.stderr(),
        "Only one of the --local, --system, --worktree, and --file options \
         can be specified.\n"
    );
}

#[test]
fn install_in_directory_without_access_to_git_lfs() {
    let t = Test::new();
    let dir = mkdir(&t, "not-a-repo");
    dir.write(".git/lfs", "");
    dir.write("lfs", "");
    t.git_config_global("filter.lfs.clean", "whatevs");
    assert_eq!(
        config(&dir, &[], "filter.lfs.clean").as_deref(),
        Some("whatevs")
    );

    dir.lfs(["install", "--force"]).ok();

    assert_eq!(
        config(&dir, &[], "filter.lfs.clean").as_deref(),
        Some("git-lfs clean -- %f")
    );
}

#[test]
fn install_in_repo_without_changing_hooks() {
    let t = Test::new();
    let repo = t.init("non-lfs-repo");

    repo.lfs(["install", "--skip-repo"]).ok();

    for name in HOOKS {
        let path = repo.path().join(".git/hooks").join(name);
        assert!(!path.exists(), "{name} installed");
    }
    assert_default_filters(&repo, &[]);
}

#[test]
fn can_install_when_multiple_global_values_registered() {
    let t = Test::new();
    let dir = t.repo("");
    t.git_config_global("filter.lfs.smudge", "git-lfs smudge --something %f");
    dir.git([
        "config",
        "--global",
        "--add",
        "filter.lfs.smudge",
        "git-lfs smudge --something-else %f",
    ])
    .ok();

    dir.lfs(["install", "--force"]).ok();
}
