//! `t-multiple-remotes.sh`.
//!
//! The shell suite skips itself with Git older than 2.27, which doesn't
//! send the treeish to the filter. These assume a newer Git.
//!
//! The remotes are `file://` URLs, so objects move through the standalone
//! transfer agent. Where the shell tests that are meant to succeed only
//! check the exit status, these also check that `a.bin` has its contents.

use crate::harness::{Repo, Test};

const A_BIN: &str = "1234";

/// A repository `dir` with remotes `mr` and `fr`, the remote
/// repositories `main` and `fork`, as `file://` URLs, both fetched.
fn consumer<'a>(t: &'a Test, dir: &str, main: &str, fork: &str) -> Repo<'a> {
    let repo = t.init(dir);
    for (remote, name) in [("mr", main), ("fr", fork)] {
        let path = t.dir().join("remote").join(format!("{name}.git"));
        let url = format!("file://{}", path.display());
        repo.git(["remote", "add", remote, &url]).ok();
        repo.git(["fetch", remote]).ok();
    }
    repo
}

/// Two remote repositories for `case`: on `main`, `a.bin` is a commit
/// ahead of `fork`. Returns a fresh repository with both as remotes.
fn forks<'a>(t: &'a Test, case: &str) -> Repo<'a> {
    let main = format!("t-multiple-remotes-{case}-main-remote");
    let fork = format!("t-multiple-remotes-{case}-fork-remote");
    t.remote(&main);
    t.remote(&fork);
    let repo = consumer(t, &format!("{case}-main-repo"), &main, &fork);
    repo.git(["checkout", "-b", "main"]).ok();
    repo.lfs(["track", "*.bin"]).ok();
    repo.git(["add", "--all"]).ok();
    repo.git(["commit", "-m", "Initial commit"]).ok();
    repo.git(["push", "-u", "mr", "main"]).ok();
    repo.git(["push", "-u", "fr", "main"]).ok();
    repo.write("a.bin", A_BIN);
    repo.git(["add", "--all"]).ok();
    repo.git(["commit", "-m", "Add Bin file"]).ok();
    repo.git(["push", "mr", "main"]).ok();
    consumer(t, &format!("{case}-fork-repo"), &main, &fork)
}

/// Set `lfs.remote.searchall` and `lfs.remote.autodetect`.
fn remote_config(repo: &Repo, searchall: bool, autodetect: bool) {
    repo.git(["config", "lfs.remote.searchall", &searchall.to_string()])
        .ok();
    repo.git(["config", "lfs.remote.autodetect", &autodetect.to_string()])
        .ok();
}

/// [`forks`] with `fr/main` checked out and the remote settings given.
fn on_fork<'a>(t: &'a Test, case: &str, searchall: bool, autodetect: bool) -> Repo<'a> {
    let repo = forks(t, case);
    repo.git(["checkout", "fr/main"]).ok();
    remote_config(&repo, searchall, autodetect);
    repo
}

/// Check that `args` brings in `a.bin` with its contents.
#[track_caller]
fn assert_accepts(repo: &Repo, args: &[&str]) {
    repo.git(args).ok();
    assert_eq!(repo.read("a.bin"), A_BIN);
}

/// Check that `args` fails, as `a.bin` can't be downloaded.
#[track_caller]
fn assert_rejects(repo: &Repo, args: &[&str]) {
    repo.git(args).fails();
}

/// [`forks`] with a sparse checkout of `.gitignore` only and `mr/main`
/// checked out, then the remote settings given.
fn sparse<'a>(t: &'a Test, case: &str, autodetect: bool) -> Repo<'a> {
    let repo = forks(t, case);
    repo.git(["sparse-checkout", "init", "--no-cone"]).ok();
    repo.git(["sparse-checkout", "set", "/.gitignore"]).ok();
    repo.git(["checkout", "mr/main"]).ok();
    remote_config(&repo, false, autodetect);
    repo
}

/// [`forks`] on a branch `main` that tracks `fr/main`, with the remote
/// settings given.
fn tracking_fork<'a>(t: &'a Test, case: &str, searchall: bool) -> Repo<'a> {
    let repo = forks(t, case);
    repo.git(["checkout", "-b", "main", "--track", "fr/main"])
        .ok();
    remote_config(&repo, searchall, false);
    repo
}

#[test]
fn accept_reset_to_different_remote() {
    let t = Test::new();
    let repo = on_fork(&t, "a-reset", false, true);
    assert_accepts(&repo, &["reset", "--hard", "mr/main"]);
}

#[test]
fn accept_pull_from_different_remote() {
    let t = Test::new();
    let repo = on_fork(&t, "a-pull", false, true);
    assert_accepts(&repo, &["pull", "mr", "main"]);
}

#[test]
fn accept_checkout_different_remote() {
    let t = Test::new();
    let repo = on_fork(&t, "a-checkout", false, true);
    assert_accepts(&repo, &["checkout", "mr/main"]);
}

#[test]
fn accept_rebase_different_remote() {
    let t = Test::new();
    let repo = on_fork(&t, "a-rebase", false, true);
    assert_accepts(&repo, &["rebase", "mr/main"]);
}

#[test]
fn accept_add_bin_file_with_sparsecheckout() {
    let t = Test::new();
    let repo = sparse(&t, "a-sparsecheckout", true);
    assert_accepts(&repo, &["sparse-checkout", "add", "a.bin"]);
}

#[test]
fn accept_cherry_pick_head_different_remote() {
    let t = Test::new();
    let repo = tracking_fork(&t, "a-cherrypick", true);
    assert_accepts(&repo, &["cherry-pick", "mr/main"]);
}

#[test]
fn reject_reset_to_different_remote() {
    let t = Test::new();
    let repo = on_fork(&t, "r-reset", false, false);
    assert_rejects(&repo, &["reset", "--hard", "mr/main"]);
}

#[test]
fn reject_pull_from_different_remote() {
    let t = Test::new();
    let repo = on_fork(&t, "r-pull", false, false);
    assert_rejects(&repo, &["pull", "mr", "main"]);
}

#[test]
fn reject_checkout_different_remote() {
    let t = Test::new();
    let repo = on_fork(&t, "r-checkout", false, false);
    assert_rejects(&repo, &["checkout", "mr/main"]);
}

#[test]
fn reject_rebase_different_remote() {
    let t = Test::new();
    let repo = on_fork(&t, "r-rebase", false, false);
    assert_rejects(&repo, &["rebase", "mr/main"]);
}

#[test]
fn reject_add_bin_file_with_sparsecheckout() {
    let t = Test::new();
    let repo = sparse(&t, "r-sparsecheckout", false);
    assert_rejects(&repo, &["sparse-checkout", "add", "a.bin"]);
}

#[test]
fn reject_cherry_pick_head_different_remote() {
    let t = Test::new();
    let repo = tracking_fork(&t, "r-cherrypick", false);
    assert_rejects(&repo, &["cherry-pick", "mr/main"]);
}
