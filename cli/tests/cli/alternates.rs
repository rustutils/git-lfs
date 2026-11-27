//! `t-alternates.sh`.
//!
//! Each repository's objects are in another clone, named as a Git
//! alternate, so fetch has nothing to download. A stale alternate is a
//! clone without its LFS objects.

use std::path::PathBuf;

use crate::harness::{Repo, Run, Test};

const BATCH: &str = "sending batch of size 1";

/// The remote repository `name` with `a.txt` pushed, with its clone's
/// local objects removed, and a clone in `{name}_alternate`, and in
/// `{name}_alternate_stale` without objects if `stale`. Returns the first
/// clone and the alternates' object directories, the good one first.
fn with_alternates<'a>(t: &'a Test, name: &str, stale: bool) -> (Repo<'a>, Vec<PathBuf>) {
    let repo = t.remote_with_file(name, "a.txt");
    let mut alternates = Vec::new();
    for (suffix, wanted) in [("_alternate", true), ("_alternate_stale", stale)] {
        if !wanted {
            continue;
        }
        let dir = format!("{name}{suffix}");
        let clone = t.clone(name, &dir);
        if suffix.ends_with("stale") {
            clone.remove_local_objects();
        }
        alternates.push(clone.path().join(".git/objects"));
    }
    repo.remove_local_objects();
    (repo, alternates)
}

/// Run `git lfs fetch origin main` in `repo`, traced, with `env` set.
fn fetch(repo: &Repo, env: &[(&str, String)]) -> Run {
    let mut cmd = repo
        .lfs_command()
        .args(["fetch", "origin", "main"])
        .env("GIT_TRACE", "1")
        .merged();
    for (key, value) in env {
        cmd = cmd.env(key, value);
    }
    cmd.run().ok()
}

/// Write `lines` to `repo`'s `.git/objects/info/alternates`.
fn write_alternates(repo: &Repo, lines: &[String]) {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    repo.write(".git/objects/info/alternates", text);
}

fn display(path: &std::path::Path) -> String {
    path.display().to_string()
}

#[test]
fn alternates_single() {
    let t = Test::new();
    let (repo, alternates) = with_alternates(&t, "alternates-single-alternate", false);
    write_alternates(&repo, &[display(&alternates[0])]);

    assert_eq!(fetch(&repo, &[]).count(BATCH), 0);
}

#[test]
fn alternates_multiple() {
    let t = Test::new();
    let (repo, alternates) = with_alternates(&t, "alternates-multiple-alternates", true);
    write_alternates(&repo, &[display(&alternates[0]), display(&alternates[1])]);

    assert_eq!(fetch(&repo, &[]).count(BATCH), 0);
}

/// A commented-out alternate isn't used, so the object is downloaded.
#[test]
fn alternates_commented() {
    let t = Test::new();
    let (repo, alternates) = with_alternates(&t, "alternates-commented-alternate", false);
    write_alternates(&repo, &[format!("# {}", display(&alternates[0]))]);

    assert_eq!(fetch(&repo, &[]).count(BATCH), 1);
}

#[test]
fn alternates_quoted() {
    let t = Test::new();
    let (repo, alternates) = with_alternates(&t, "alternates-quoted-alternate", false);
    write_alternates(&repo, &[format!("\"{}\"", display(&alternates[0]))]);

    assert_eq!(fetch(&repo, &[]).count(BATCH), 0);
}

/// Check that with `GIT_ALTERNATE_OBJECT_DIRECTORIES` set to `dirs`,
/// `repo`, emptied of Git objects, fetches nothing and can push.
#[track_caller]
fn assert_environment_alternates(repo: &Repo, dirs: &[PathBuf]) {
    let objects = repo.path().join(".git/objects");
    for entry in std::fs::read_dir(&objects).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
    }
    repo.git(["init"]).ok();
    let value = std::env::join_paths(dirs).unwrap();
    let value = value.to_string_lossy().into_owned();
    let env = [("GIT_ALTERNATE_OBJECT_DIRECTORIES", value.clone())];

    assert_eq!(fetch(repo, &env).count(BATCH), 0);

    let url = repo
        .git(["config", "remote.origin.url"])
        .ok()
        .stdout()
        .trim_end()
        .to_owned();
    repo.lfs_command()
        .args(["push", &url, "main"])
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &value)
        .run()
        .ok();
}

#[test]
fn alternates_os_environment_single() {
    let t = Test::new();
    let (repo, alternates) = with_alternates(&t, "alternates-environment-single-alternate", false);
    assert_environment_alternates(&repo, &alternates);
}

/// The stale alternate comes first.
#[test]
fn alternates_os_environment_multiple() {
    let t = Test::new();
    let (repo, alternates) =
        with_alternates(&t, "alternates-environment-multiple-alternates", true);
    let dirs = [alternates[1].clone(), alternates[0].clone()];
    assert_environment_alternates(&repo, &dirs);
}
