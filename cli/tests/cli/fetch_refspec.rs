//! `t-fetch-refspec.sh`.
//!
//! These servers need the ref in batch requests: a `*-branch-required`
//! repository wants the branch its name ends with.

use crate::harness::{Repo, Test};

/// The object ID of `a\n`.
const A_OID: &str = "87428fc522803d31065e7bce3cf03fe475096631e5e07bbd7a0fde60c4cf25c7";

/// The remote repository `name` with `a.dat` pushed as `refspec`, and the
/// local objects removed.
fn pushed<'a>(t: &'a Test, name: &str, refspec: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "a\n");
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();
    repo.git(["push", "origin", refspec]).ok();
    repo.assert_local_object(A_OID, 2);
    t.assert_server_objects(name, &[A_OID], &[]);
    repo.remove_local_objects();
    repo
}

#[test]
fn fetch_with_good_ref() {
    let t = Test::new();
    let repo = pushed(&t, "fetch-main-branch-required", "main");

    repo.lfs(["fetch", "--all"]).ok();

    repo.assert_local_object(A_OID, 2);
}

#[test]
fn fetch_with_tracked_ref() {
    let t = Test::new();
    let repo = pushed(&t, "fetch-tracked-branch-required", "main:tracked");
    repo.git(["config", "push.default", "upstream"]).ok();
    repo.git(["config", "branch.main.merge", "refs/heads/tracked"])
        .ok();

    repo.lfs(["fetch", "--all"]).ok();

    repo.assert_local_object(A_OID, 2);
}

/// The fetch sends the tracked branch, not one from the push remote.
#[test]
fn fetch_with_push_remote_configured() {
    let t = Test::new();
    let name = "fetch-tracked-with-push-remote-tracked-branch-required";
    let repo = pushed(&t, name, "main:tracked");
    repo.git(["remote", "add", "push-only", &t.remote_url(name)])
        .ok();
    repo.git(["config", "remote.pushDefault", "push-only"]).ok();
    repo.git(["config", "branch.main.merge", "refs/heads/tracked"])
        .ok();

    repo.lfs(["fetch", "origin", "--all"]).ok();

    repo.assert_local_object(A_OID, 2);
}

#[test]
fn fetch_with_bad_ref() {
    let t = Test::new();
    let repo = pushed(&t, "fetch-other-branch-required", "main:other");

    repo.lfs_command()
        .args(["fetch", "--all"])
        .env("GIT_CURL_VERBOSE", "1")
        .merged()
        .run()
        .fails()
        .has("Expected ref \"refs/heads/other\", got \"refs/heads/main\"");
}
