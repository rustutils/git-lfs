//! Setup shared by the transfer suites, such as the batch encoding, retry,
//! and rate limit ones.
//!
//! Where the shell tests set `credential.helper lfstest` in a clone, these
//! don't: the harness's credential store already answers for the server.

use crate::harness::{Repo, Run, Test};

/// Run `git` with `args` in `repo`, traced, with stderr merged into
/// stdout.
pub fn traced(repo: &Repo, args: &[&str]) -> Run {
    repo.command("git")
        .args(args)
        .env("GIT_TRACE", "1")
        .env("GIT_CURL_VERBOSE", "1")
        .merged()
        .run()
}

/// The remote repository `name`, cloned into `dir`, with `files` committed
/// under `*.dat` tracking.
pub fn committed<'a>(t: &'a Test, name: &str, dir: &str, files: &[(&str, &str)]) -> Repo<'a> {
    t.remote(name);
    let repo = t.clone(name, dir);
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    for (path, contents) in files {
        repo.write(path, contents);
        repo.git(["add", path]).ok();
    }
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo
}

/// [`committed`], pushed.
pub fn pushed<'a>(t: &'a Test, name: &str, dir: &str, files: &[(&str, &str)]) -> Repo<'a> {
    let repo = committed(t, name, dir, files);
    repo.git(["push", "origin", "main"]).ok();
    repo
}

/// Clone the remote repository `name` into `{name}-assert` without
/// running the LFS filters, so the clone has pointers and no objects.
pub fn clone_without_filters<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let dir = format!("{name}-assert");
    t.repo("")
        .git([
            "-c",
            "filter.lfs.process=",
            "-c",
            "filter.lfs.smudge=cat",
            "-c",
            "filter.lfs.required=false",
            "clone",
            &t.remote_url(name),
            &dir,
        ])
        .ok();
    t.repo(&dir)
}

/// Run `git lfs clone` of the remote repository `name` into
/// `{name}-assert`, traced. Returns the run and the clone.
pub fn lfs_clone_traced<'a>(t: &'a Test, name: &str) -> (Run, Repo<'a>) {
    let dir = format!("{name}-assert");
    let run = traced(&t.repo(""), &["lfs", "clone", &t.remote_url(name), &dir]);
    (run, t.repo(&dir))
}

/// Check that `run`'s output has `count` lines containing each of `lines`.
#[track_caller]
pub fn assert_counts(run: &Run, lines: &[(&str, usize)]) {
    for (text, count) in lines {
        assert_eq!(run.count(text), *count, "{text:?}\n{run}");
    }
}

/// Check that `run`'s output has a line containing all of `parts`, like
/// a shell `grep` for `a.*b`.
#[track_caller]
pub fn assert_line_with(run: &Run, parts: &[&str]) {
    let found = run
        .output()
        .lines()
        .any(|l| parts.iter().all(|p| l.contains(p)));
    assert!(found, "no line with {parts:?}\n{run}");
}
