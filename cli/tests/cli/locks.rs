//! `t-locks.sh`.
//!
//! The two SSH tests, "list a single lock (SSH; git-lfs-authenticate)" and
//! "list a single lock (SSH; git-lfs-transfer)", aren't ported yet: they
//! need the shell suites' `lfs-ssh-echo`, and the second scutiger's
//! `git-lfs-transfer`, which come with the SSH suites.

use crate::harness::{Repo, Run, Test};

/// Lock `path`, check the server has the lock, and return its ID.
fn lock(t: &Test, repo: &Repo, name: &str, path: &str) -> String {
    t.assert_lock(name, &repo.lfs(["lock", "--json", path]).ok(), path)
}

/// Check that `run` printed `n` lines on stdout.
#[track_caller]
fn assert_lines(run: &Run, n: usize) {
    assert_eq!(run.stdout().lines().count(), n, "{run}");
}

/// A clone of the new remote `name`, in `dir`, with `files` tracked,
/// committed, and pushed, each holding its own name.
fn files_setup<'a>(t: &'a Test, name: &str, dir: &str, files: &[&str]) -> Repo<'a> {
    t.remote(name);
    let repo = t.clone(name, dir);
    repo.lfs(["track", "*.dat"]).ok();
    for file in files {
        repo.write(file, format!("{file}\n"));
    }
    repo.git(["add", "."]).ok();
    let mut run = repo
        .git(["commit", "-m", "add files"])
        .ok()
        .has(&format!("{} files changed", files.len() + 1))
        .has("create mode 100644 .gitattributes");
    for file in files {
        run = run.has(&format!("create mode 100644 {file}"));
    }
    repo.git(["push", "origin", "main"])
        .ok()
        .has("main -> main");
    repo
}

#[test]
fn list_a_single_lock_with_bad_ref() {
    let t = Test::new();
    let name = "locks-list-other-branch-required";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("f.dat", "f\n");
    repo.git(["add", ".gitattributes", "f.dat"]).ok();
    repo.git(["commit", "-m", "add f.dat"]).ok();
    repo.git(["push", "origin", "main:other"]).ok();
    repo.git(["checkout", "-b", "other"]).ok();
    lock(&t, &repo, name, "f.dat");

    repo.git(["checkout", "main"]).ok();
    repo.lfs(["locks", "--path", "f.dat"])
        .fails()
        .has(r#"Expected ref "refs/heads/other", got "refs/heads/main""#);
}

#[test]
fn list_a_single_lock() {
    let t = Test::new();
    let name = "locks-list-main-branch-required";
    t.remote_with_file(name, "f.dat");
    let repo = t.clone(name, name);
    lock(&t, &repo, name, "f.dat");

    let run = repo
        .lfs(["locks", "--path", "f.dat"])
        .ok()
        .has("f.dat")
        .has("Git LFS Tests");
    assert_lines(&run, 1);
}

#[test]
fn list_a_single_lock_remote_overrides_push_default() {
    let t = Test::new();
    let name = "locks-list-remote";
    t.remote_with_file(name, "a.dat");
    let repo = t.clone(name, name);
    let id = lock(&t, &repo, name, "a.dat");
    repo.git(["remote", "add", "bad-remote", "invalid-url"])
        .ok();

    for (key, unset) in [
        ("remote.pushDefault", true),
        ("branch.main.pushRemote", false),
    ] {
        repo.git(["config", key, "bad-remote"]).ok();
        repo.lfs(["locks"]).fails();
        let run = repo
            .lfs(["locks", "--remote", "origin", "--path", "a.dat"])
            .ok()
            .has("a.dat")
            .has(&format!("ID:{id}"));
        assert_lines(&run, 1);
        if unset {
            repo.git(["config", "--unset", key]).ok();
        }
    }
}

/// The `.lfsconfig` names a default push remote that doesn't exist, whose
/// URL is a repository without the locking API. Both are ignored, and the
/// locks come from `origin`.
#[test]
fn list_a_single_lock_with_invalid_default_push_remote_in_lfsconfig() {
    let t = Test::new();
    let name = "locks-list-invalid-remote-push-default";
    let repo = t.remote_and_clone(name);
    let id = lock(&t, &repo, name, "test.bin");

    let unsupported = format!("{}/locksunsupported.git", t.server().url());
    let lfsconfig = |key: &str, value: &str| {
        repo.git(["config", "--file", ".lfsconfig", key, value])
            .ok();
    };
    lfsconfig("remote.other.origin.url", &unsupported);
    lfsconfig("remote.pushDefault", "other.origin");

    let run = repo
        .lfs_command()
        .args(["locks", "--path", "test.bin"])
        .env("GIT_CURL_VERBOSE", "1")
        .run()
        .ok()
        .has("test.bin")
        .has(&format!("ID:{id}"))
        .lacks("locksunsupported")
        .lacks("404 Not Found");
    assert_lines(&run, 1);
}

#[test]
fn list_a_single_lock_json() {
    let t = Test::new();
    let name = "locks_list_single_json";
    let repo = t.remote_with_file(name, "f_json.dat");
    lock(&t, &repo, name, "f_json.dat");
    repo.lfs(["locks", "--json", "--path", "f_json.dat"])
        .ok()
        .has(r#""path":"f_json.dat""#)
        .has(r#""owner":{"name":"Git LFS Tests"}"#);
}

#[test]
fn list_locks_with_a_limit() {
    let t = Test::new();
    let name = "locks_list_limit";
    let files = ["g_1.dat", "g_2.dat"];
    let repo = files_setup(&t, name, "clone_locks_list_limit", &files);
    for file in files {
        lock(&t, &repo, name, file);
    }
    assert_lines(&repo.lfs(["locks", "--limit", "1"]).ok(), 1);
}

/// The server returns at most three locks at a time, so four take two
/// pages.
#[test]
fn list_locks_with_pagination() {
    let t = Test::new();
    let name = "locks_list_paginate";
    let files = ["h_1.dat", "h_2.dat", "h_3.dat", "h_4.dat", "h_5.dat"];
    let repo = files_setup(&t, name, "clone_locks_list_paginate", &files);
    for file in files {
        lock(&t, &repo, name, file);
    }
    assert_lines(&repo.lfs(["locks", "--limit", "4"]).ok(), 4);
}

const CACHED: [&str; 2] = ["cached1.dat", "cached2.dat"];

#[test]
fn cached_locks() {
    let t = Test::new();
    let name = "remote_cached_locks";
    let repo = files_setup(&t, name, "clone_cached_locks", &CACHED);
    for file in CACHED {
        lock(&t, &repo, name, file);
    }
    assert_lines(&repo.lfs(["locks", "--local"]).ok(), 2);

    // Without the remote, the locks must come from the local records.
    repo.git(["remote", "remove", "origin"]).ok();
    let run = repo
        .lfs(["locks", "--local", "--path", "cached1.dat"])
        .ok()
        .has("cached1.dat");
    assert_lines(&run, 1);
    assert_lines(&repo.lfs(["locks", "--local", "--limit", "1"]).ok(), 1);
}

/// A lock or unlock that fails for one of its files still records the
/// others.
#[test]
fn cached_locks_with_failed_lock() {
    let t = Test::new();
    let name = "remote_cached-locks-failed-lock";
    let repo = files_setup(&t, name, "clone_cached-locks-failed-lock", &CACHED);
    lock(&t, &repo, name, "cached1.dat");
    // cached1.dat is already locked.
    let run = repo.lfs(["lock", "--json", "cached1.dat", "cached2.dat"]);
    t.assert_lock(name, &run, "cached2.dat");
    assert_lines(&repo.lfs(["locks", "--local"]).ok(), 2);

    repo.lfs(["unlock", "--json", "cached1.dat"]).ok();
    // cached1.dat is already unlocked.
    repo.lfs(["unlock", "--json", "cached1.dat", "cached2.dat"]);
    assert_lines(&repo.lfs(["locks", "--local"]).ok(), 0);
}
