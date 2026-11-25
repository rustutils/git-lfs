//! `t-pre-push.sh`.

use crate::harness::{Commit, Repo, Run, Test, oid, pointer};

/// The line Git gives the hook to push `main` to a new `main`.
const MAIN: &str =
    "refs/heads/main main refs/heads/main 0000000000000000000000000000000000000000\n";

/// A clone of a new remote repository `name`, without lock verification,
/// with `a.dat` tracked and committed but not pushed.
fn pre_push_setup<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    set_locksverify(t, &repo, name, Some(false));
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "hi\n");
    repo.commit_all("add a.dat");
    repo
}

fn locksverify_key(t: &Test, name: &str) -> String {
    format!("lfs.{}.locksverify", t.endpoint(name))
}

fn set_locksverify(t: &Test, repo: &Repo, name: &str, value: Option<bool>) {
    if let Some(value) = value {
        repo.git(["config", &locksverify_key(t, name), &value.to_string()])
            .ok();
    }
}

/// The `locksverify` setting for the remote `name`, or `""` when unset.
fn locksverify(t: &Test, repo: &Repo, name: &str) -> String {
    let run = repo.git(["config", &locksverify_key(t, name)]);
    run.stdout().trim().to_owned()
}

/// Run `git lfs pre-push` with `args`, given `stdin` as Git would.
fn run_pre_push(repo: &Repo, args: &[&str], stdin: &str) -> Run {
    repo.lfs_command()
        .args(["pre-push"])
        .args(args)
        .stdin(stdin)
        .run()
}

#[test]
fn pre_push_with_good_ref() {
    let t = Test::new();
    let name = "pre-push-main-branch-required";
    let repo = pre_push_setup(&t, name);
    let url = t.remote_url(name);
    t.assert_server_objects(name, &[], &[&oid("hi\n")]);
    run_pre_push(&repo, &["origin", &url], MAIN).ok();
    t.assert_server_objects(name, &[&oid("hi\n")], &[]);
}

#[test]
fn pre_push_with_tracked_ref() {
    let t = Test::new();
    let name = "pre-push-tracked-branch-required";
    let repo = pre_push_setup(&t, name);
    let stdin =
        "refs/heads/main main refs/heads/tracked 0000000000000000000000000000000000000000\n";
    run_pre_push(&repo, &["origin", "main"], stdin).ok();
    t.assert_server_objects(name, &[&oid("hi\n")], &[]);
}

#[test]
fn pre_push_with_bad_ref() {
    let t = Test::new();
    let name = "pre-push-other-branch-required";
    let repo = pre_push_setup(&t, name);
    let url = t.remote_url(name);
    run_pre_push(&repo, &["origin", &url], MAIN)
        .fails()
        .has(r#"Expected ref "refs/heads/other", got "refs/heads/main""#);
    t.assert_server_objects(name, &[], &[&oid("hi\n")]);
}

/// A clone of a new remote repository `name`, with `*.dat` tracked and
/// lock verification on.
fn pre_push_tracked<'a>(t: &'a Test, name: &str, dir: &str) -> Repo<'a> {
    t.remote(name);
    let repo = t.clone(name, dir);
    repo.lfs(["track", "*.dat"]).ok();
    repo.commit_all("add git attributes");
    set_locksverify(t, &repo, name, Some(true));
    repo
}

#[test]
fn pre_push() {
    let t = Test::new();
    let name = "t-pre-push";
    let repo = pre_push_tracked(&t, name, "repo");
    let url = t.remote_url(name);

    // No output when there's nothing to push.
    let run = run_pre_push(&repo, &["origin", &url], MAIN).ok();
    assert_eq!(run.stdout(), "", "{run}");

    repo.write("hi.dat", "hi\n");
    repo.commit_all("add hi.dat");
    t.assert_server_objects(name, &[], &[&oid("hi\n")]);
    run_pre_push(&repo, &["origin", &url], MAIN)
        .ok()
        .has("Uploading LFS objects: 100% (1/1), 3 B");
    t.assert_server_objects(name, &[&oid("hi\n")], &[]);
}

#[test]
fn pre_push_dry_run() {
    let t = Test::new();
    let name = "t-pre-push-dry-run";
    let repo = pre_push_tracked(&t, name, "repo-dry-run");
    let url = t.remote_url(name);
    let args = ["--dry-run", "origin", &url];

    let run = run_pre_push(&repo, &args, MAIN).ok();
    assert_eq!(run.stdout(), "", "{run}");

    repo.write("hi.dat", "dry\n");
    repo.commit_all("add hi.dat");
    let dry = oid("dry\n");
    let run = run_pre_push(&repo, &args, MAIN).ok();
    assert_eq!(run.stdout(), format!("push {dry} => hi.dat\n"), "{run}");
    t.assert_server_objects(name, &[], &[&dry]);
}

#[test]
fn pre_push_skip_push() {
    let t = Test::new();
    let name = "t-pre-push-skip-push";
    let repo = pre_push_tracked(&t, name, "repo-skip-push");
    let url = t.remote_url(name);
    let skip = |repo: &Repo| {
        let run = repo
            .lfs_command()
            .args(["pre-push", "origin", &url])
            .env("GIT_LFS_SKIP_PUSH", "true")
            .stdin(MAIN)
            .run()
            .ok();
        assert_eq!(run.stdout(), "", "{run}");
    };

    skip(&repo);
    repo.write("hi.dat", "dry\n");
    repo.commit_all("add hi.dat");
    skip(&repo);
    t.assert_server_objects(name, &[], &[&oid("dry\n")]);
}

#[test]
fn pre_push_307_redirects() {
    let t = Test::new();
    let name = "t-pre-push";
    t.remote(name);
    let repo = t.clone(name, "repo-307");
    repo.lfs(["track", "*.dat"]).ok();
    repo.commit_all("add git attributes");

    for (route, file) in [("rel", "hi.dat"), ("abs", "hi2.dat")] {
        let lfs_url = format!(
            "{}/redirect307/{route}/{name}.git/info/lfs",
            t.server().url()
        );
        repo.git(["config", "remote.origin.lfsurl", &lfs_url]).ok();
        repo.write(file, "hi\n");
        repo.commit_all(&format!("add {file}"));
        run_pre_push(&repo, &["origin", &lfs_url], MAIN)
            .ok()
            .has("Uploading LFS objects: 100% (1/1), 3 B");
        t.assert_server_objects(name, &[&oid("hi\n")], &[]);
    }
}

#[test]
fn pre_push_with_existing_object() {
    let t = Test::new();
    let name = "pre-push-existing-object";
    let repo = t.remote_and_clone(name);
    let url = t.remote_url(name);
    repo.write("existing.dat", "existing\n");
    repo.commit_all("add existing dat");
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("new.dat", "new\n");
    repo.git(["add", "new.dat", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "add new file through git lfs"])
        .ok();

    run_pre_push(&repo, &["origin", &url], MAIN)
        .ok()
        .has("Uploading LFS objects: 100% (1/1), 4 B");
    t.assert_server_objects(name, &[&oid("new\n")], &[]);
}

#[test]
fn pre_push_with_existing_object_untracked() {
    let t = Test::new();
    let name = "pre-push-existing-object-untracked";
    let repo = t.remote_and_clone(name);
    let url = t.remote_url(name);
    let new = oid("new\n");
    repo.write("new.dat", pointer(&new, 4));
    repo.commit_all("add new pointer");
    repo.write(&format!(".git/lfs/objects/7a/a7/{new}"), "new\n");

    run_pre_push(&repo, &["origin", &url], MAIN)
        .ok()
        .has("Uploading LFS objects: 100% (1/1), 4 B");
    t.assert_server_objects(name, &[&new], &[]);
}

#[test]
fn pre_push_reject_missing_object() {
    let t = Test::new();
    let name = "pre-push-reject-missing-object";
    let repo = t.remote_and_clone(name);
    let url = t.remote_url(name);
    let new = oid("new\n");
    repo.write("new.dat", pointer(&new, 4));
    repo.commit_all("add new pointer");

    let run = run_pre_push(&repo, &["origin", &url], MAIN);
    assert_eq!(run.code(), Some(2), "{run}");
    run.has("LFS upload failed:")
        .has(&format!("  (missing) new.dat ({new})"));
    t.assert_server_objects(name, &[], &[&new]);
}

#[test]
fn pre_push_allow_missing_object_found_on_server() {
    let t = Test::new();
    let name = "pre-push-allow-missing-object-found-on-server";
    let repo = t.remote_and_clone(name);
    let url = t.remote_url(name);
    let contents = "common data";
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("common1.dat", contents);
    repo.commit_all("add first file");
    run_pre_push(&repo, &["origin", &url], MAIN)
        .ok()
        .has("Uploading LFS objects: 100% (1/1), 11 B");
    t.assert_server_objects(name, &[&oid(contents)], &[]);

    // Another commit with the same object, which is only on the server.
    repo.write("common2.dat", contents);
    repo.commit_all("add second file, same content");
    repo.remove_local_objects();
    let run = run_pre_push(&repo, &["origin", &url], MAIN).ok();
    assert!(!run.output().to_lowercase().contains("error"), "{run}");
}

/// A clone of a new remote repository `name`, with `present.dat` and
/// `missing.dat` committed and the object for `missing.dat` deleted.
fn missing_object_setup<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.commit_all("initial commit");
    repo.write("present.dat", "present");
    repo.write("missing.dat", "missing");
    repo.commit_all("add objects");
    repo.delete_local_object(&oid("missing"));
    repo
}

#[test]
fn pre_push_allow_missing_object_lfs_allowincompletepush_true() {
    let t = Test::new();
    let name = "pre-push-allow-missing-object";
    let repo = missing_object_setup(&t, name);
    let url = t.remote_url(name);
    repo.git(["config", "lfs.allowincompletepush", "true"]).ok();

    run_pre_push(&repo, &["origin", &url], MAIN)
        .ok()
        .has("LFS upload missing objects")
        .has(&format!("  (missing) missing.dat ({})", oid("missing")));
    t.assert_server_objects(name, &[&oid("present")], &[&oid("missing")]);
}

#[test]
fn pre_push_reject_missing_object_lfs_allowincompletepush_default() {
    let t = Test::new();
    let name = "pre-push-reject-missing-object-default";
    let repo = missing_object_setup(&t, name);
    let url = t.remote_url(name);
    let missing = oid("missing");

    let run = repo
        .lfs_command()
        .args(["pre-push", "origin", &url])
        .env("GIT_TRACE", "1")
        .stdin(MAIN)
        .run();
    assert_eq!(run.code(), Some(2), "{run}");
    run.has(&format!(
        "tq: stopping batched queue, object \"{missing}\" missing locally and on remote"
    ))
    .has("LFS upload failed:")
    .has(&format!("  (missing) missing.dat ({missing})"));
    t.assert_server_objects(name, &[], &[&oid("present"), &missing]);
}

/// The contents of the files in the branch tests, `filecontent0` and on.
fn contents(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("filecontent{i}")).collect()
}

#[test]
fn pre_push_multiple_branches() {
    let t = Test::new();
    let name = "t-pre-push-multiple-branches";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    let c = contents(6);
    repo.add_commits(&[
        Commit::new()
            .days_ago(10)
            .file("file1.dat", &c[0])
            .file("file2.dat", &c[1]),
        Commit::new()
            .days_ago(5)
            .new_branch("branch1")
            .file("file2.dat", &c[2]),
        Commit::new()
            .days_ago(5)
            .on("main")
            .new_branch("branch2")
            .file("file3.dat", &c[3]),
        Commit::new()
            .days_ago(2)
            .on("main")
            .new_branch("branch3")
            .file("file1.dat", &c[4]),
        Commit::new()
            .days_ago(1)
            .on("main")
            .new_branch("branch4")
            .file("file4.dat", &c[5]),
    ]);

    // Through git push, every branch's objects are pushed.
    repo.git([
        "push", "origin", "main", "branch1", "branch2", "branch3", "branch4",
    ])
    .ok();
    let oids: Vec<String> = c.iter().map(oid).collect();
    let oids: Vec<&str> = oids.iter().map(String::as_str).collect();
    t.assert_server_objects(name, &oids, &[]);
}

#[test]
fn pre_push_with_bad_remote() {
    let t = Test::new();
    let name = "t-pre-push-bad-remote";
    let repo = t.remote_and_clone(name);
    let url = t.remote_url(name);
    run_pre_push(&repo, &["not-a-remote", &url], MAIN).has("Invalid remote name");
}

/// The local cache of the remote's branches has a branch that's been
/// deleted on the remote, whose objects the server has since dropped. A
/// later branch that has them must push them again, so pre-push has to
/// check the remote's real refs.
#[test]
fn pre_push_unfetched_deleted_remote_branch_and_server_gc() {
    let t = Test::new();
    let name = "t-pre-push-server-deleted-branch-gc";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    let c = contents(4);
    repo.add_commits(&[
        Commit::new()
            .days_ago(10)
            .file("file1.dat", &c[0])
            .file("file2.dat", &c[1]),
        Commit::new()
            .days_ago(5)
            .new_branch("branch-to-delete")
            .file("file3.dat", &c[2]),
        Commit::new()
            .days_ago(2)
            .new_branch("branch-to-push-after")
            .file("file4.dat", &c[3]),
    ]);
    let oids: Vec<String> = c.iter().map(oid).collect();
    let oids: Vec<&str> = oids.iter().map(String::as_str).collect();

    repo.git(["push", "origin", "main", "branch-to-delete"])
        .ok();
    t.assert_server_objects(name, &oids[..3], &oids[3..]);

    // Delete the branch on the server and drop its object, but keep the
    // cached remote ref, as if someone else deleted it and we haven't
    // fetched since.
    let cached = "refs/remotes/origin/branch-to-delete";
    let commit = repo.git(["rev-parse", cached]).ok().stdout();
    repo.git(["push", "origin", "--delete", "branch-to-delete"])
        .ok();
    t.server().delete_object(name, oids[2]);
    t.assert_server_objects(name, &[], &oids[2..3]);
    repo.git(["update-ref", cached, commit.trim()]).ok();
    repo.git(["branch", "-r"])
        .ok()
        .has("origin/branch-to-delete");

    repo.git(["push", "origin", "branch-to-push-after"]).ok();
    t.assert_server_objects(name, &oids, &[]);
}

#[test]
fn pre_push_delete_branch() {
    let t = Test::new();
    let name = "t-pre-push-delete-branch";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    let c = contents(4);
    repo.add_commits(&[
        Commit::new()
            .days_ago(2)
            .file("file1.dat", &c[0])
            .file("file2.dat", &c[1]),
        Commit::new()
            .days_ago(1)
            .new_branch("branch-to-delete")
            .file("file3.dat", &c[2]),
        Commit::new().on("main").file("file4.dat", &c[3]),
    ]);
    repo.git(["push", "origin", "main", "branch-to-delete"])
        .ok();
    let oids: Vec<String> = c.iter().map(oid).collect();
    let oids: Vec<&str> = oids.iter().map(String::as_str).collect();
    t.assert_server_objects(name, &oids, &[]);

    // The hook gets a zero object ID for the deleted branch.
    repo.git(["push", "origin", "--delete", "branch-to-delete"])
        .ok();
}

/// Lock `path` with `git lfs lock --json`, check the server has the
/// lock, and return its ID.
fn lock(t: &Test, repo: &Repo, name: &str, path: &str) -> String {
    let run = repo.lfs(["lock", "--json", path]).ok();
    let stdout = run.stdout();
    let id = stdout
        .split(r#""id":""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_else(|| panic!("no lock ID\n{run}"))
        .to_owned();
    assert_lock(t, name, &id, path);
    id
}

#[track_caller]
fn assert_lock(t: &Test, name: &str, id: &str, path: &str) {
    assert_eq!(t.server().lock_path(name, id).as_deref(), Some(path));
}

#[test]
fn pre_push_with_our_lock() {
    let t = Test::new();
    let name = "pre_push_owned_locks";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.commit_all("initial commit");
    repo.write("locked.dat", "locked contents");
    repo.commit_all("add locked.dat");
    repo.git(["push", "origin", "main"]).ok();
    let id = lock(&t, &repo, name, "locked.dat");

    repo.write("locked.dat", "locked contentsauthorized changes");
    repo.commit_all("add unauthorized changes");
    repo.git(["push", "origin", "main"])
        .ok()
        .has("Consider unlocking your own locked files")
        .has("* locked.dat");
    assert_lock(&t, name, &id, "locked.dat");
}

/// A second clone of `name`, with lock verification on.
fn other_clone<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.clone(name, &format!("{name}-assert"));
    repo.git(["config", "lfs.locksverify", "true"]).ok();
    repo
}

/// The server's lock verification reports locks on paths with "theirs" in
/// them as someone else's.
#[test]
fn pre_push_with_their_lock_on_lfs_file() {
    let t = Test::new();
    let name = "pre_push_unowned_lock";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.commit_all("initial commit");
    repo.write("locked_theirs.dat", "locked contents");
    repo.commit_all("add locked_theirs.dat");
    repo.git(["push", "origin", "main"]).ok();
    lock(&t, &repo, name, "locked_theirs.dat");

    let other = other_clone(&t, name);
    let changed = "locked contentsunauthorized changes";
    other.write("locked_theirs.dat", changed);
    other.git(["add", "locked_theirs.dat"]).ok();
    // --no-verify skips the pre-commit hook, which isn't under test.
    other
        .git(["commit", "--no-verify", "-m", "add unauthorized changes"])
        .ok();
    other
        .git(["push", "origin", "main"])
        .fails()
        .has("Unable to push locked files")
        .has("* locked_theirs.dat - Git LFS Tests")
        .has("Cannot update locked files.");
    t.assert_server_objects(name, &[], &[&oid(changed)]);
}

#[test]
fn pre_push_with_their_lock_on_non_lfs_lockable_file() {
    let t = Test::new();
    let name = "pre_push_unowned_lock_not_lfs";
    let repo = t.remote_and_clone(name);
    repo.write(".gitattributes", "*.dat lockable\n");
    repo.commit_all("initial commit");
    let large = "large file\n".repeat(1000);
    repo.write("readme.txt", "hi\n");
    repo.write("tiny_locked_theirs.dat", "tiny\n");
    repo.write("large_locked_theirs.dat", &large);
    repo.commit_all("add initial files");
    repo.git(["push", "origin", "main"]).ok();
    lock(&t, &repo, name, "tiny_locked_theirs.dat");
    lock(&t, &repo, name, "large_locked_theirs.dat");

    let other = other_clone(&t, name);
    // Nothing tracked through LFS installs the pre-push hook.
    other.lfs(["update"]).ok();
    let large = format!("{large}unauthorized changes\n");
    let tiny = "tiny\nunauthorized changes\n";
    other.write("readme.txt", "hi\nother changes\n");
    other.write("large_locked_theirs.dat", &large);
    other.write("tiny_locked_theirs.dat", tiny);
    other
        .git(["commit", "--no-verify", "-am", "add unauthorized changes"])
        .ok();
    other
        .git(["push", "origin", "main"])
        .fails()
        .has("Unable to push locked files")
        .has("* large_locked_theirs.dat - Git LFS Tests")
        .has("* tiny_locked_theirs.dat - Git LFS Tests")
        .has("Cannot update locked files.");
    t.assert_server_objects(name, &[], &[&oid(&large), &oid(tiny)]);
}

const EXAMPLE: &str = "example";

/// A clone of a new remote repository `name`, with `a.dat` tracked and
/// committed, and `locksverify` set to `verify`.
fn lock_verify_setup<'a>(t: &'a Test, name: &str, verify: Option<bool>) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.write("a.dat", EXAMPLE);
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "--message", "initial commit"]).ok();
    set_locksverify(t, &repo, name, verify);
    repo
}

#[test]
fn pre_push_locks_verify_5xx_with_verification_enabled() {
    let t = Test::new();
    let name = "lock-enabled-verify-5xx";
    let repo = lock_verify_setup(&t, name, Some(true));
    repo.git(["push", "origin", "main"])
        .fails()
        .has(r#""origin" does not support the Git LFS locking API"#)
        .has(&format!("git config {} false", locksverify_key(&t, name)));
    t.assert_server_objects(name, &[], &[&oid(EXAMPLE)]);
}

#[test]
fn pre_push_disable_locks_verify_on_exact_url() {
    let t = Test::new();
    let name = "lock-disabled-verify-5xx";
    let repo = lock_verify_setup(&t, name, Some(false));
    repo.git(["push", "origin", "main"])
        .ok()
        .lacks(r#""origin" does not support the Git LFS locking API"#);
    t.assert_server_objects(name, &[&oid(EXAMPLE)], &[]);
}

/// The shell test sets the setting for an empty URL, and its repository's
/// lock verification doesn't fail, so it passes either way. This sets it
/// for the server's URL, on a repository whose verification fails.
#[test]
#[ignore = "lfs.<url>.locksverify only matches the exact endpoint"]
fn pre_push_disable_locks_verify_on_partial_url() {
    let t = Test::new();
    let name = "lock-disabled-partial-verify-5xx";
    let repo = lock_verify_setup(&t, name, None);
    let key = format!("lfs.{}.locksverify", t.server().url());
    repo.git(["config", &key, "false"]).ok();
    repo.git(["push", "origin", "main"])
        .ok()
        .lacks(r#""origin" does not support the Git LFS locking API"#);
    t.assert_server_objects(name, &[&oid(EXAMPLE)], &[]);
}

/// A clone of the branch-checking remote `name`, set to verify locks for
/// the remote's URL.
fn lock_verify_ref_setup<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = lock_verify_setup(t, name, None);
    let key = format!("lfs.{}.git.locksverify", t.remote_url(name));
    repo.git(["config", &key, "true"]).ok();
    repo
}

#[test]
fn pre_push_locks_verify_403_with_good_ref() {
    let t = Test::new();
    let name = "lock-verify-main-branch-required";
    let repo = lock_verify_ref_setup(&t, name);
    repo.git(["push", "origin", "main"]).ok();
    t.assert_server_objects(name, &[&oid(EXAMPLE)], &[]);
}

#[test]
fn pre_push_locks_verify_403_with_good_tracked_ref() {
    let t = Test::new();
    let name = "lock-verify-tracked-branch-required";
    let repo = lock_verify_ref_setup(&t, name);
    repo.git(["config", "push.default", "upstream"]).ok();
    repo.git(["config", "branch.main.merge", "refs/heads/tracked"])
        .ok();
    repo.git(["config", "branch.main.remote", "origin"]).ok();
    repo.git(["push"]).ok();
    t.assert_server_objects(name, &[&oid(EXAMPLE)], &[]);
}

#[test]
fn pre_push_locks_verify_403_with_explicit_ref() {
    let t = Test::new();
    let name = "lock-verify-explicit-branch-required";
    let repo = lock_verify_ref_setup(&t, name);
    repo.git(["push", "origin", "main:explicit"]).ok();
    t.assert_server_objects(name, &[&oid(EXAMPLE)], &[]);
}

#[test]
fn pre_push_locks_verify_403_with_bad_ref() {
    let t = Test::new();
    let name = "lock-verify-other-branch-required";
    let repo = lock_verify_ref_setup(&t, name);
    repo.git(["push", "origin", "main"])
        .fails()
        .has("failed to push some refs");
    t.assert_server_objects(name, &[], &[&oid(EXAMPLE)]);
}

#[test]
fn pre_push_locks_verify_5xx_with_verification_unset() {
    let t = Test::new();
    let name = "lock-unset-verify-5xx";
    let repo = lock_verify_setup(&t, name, None);
    assert_eq!(locksverify(&t, &repo, name), "");
    repo.git(["push", "origin", "main"])
        .ok()
        .has(r#""origin" does not support the Git LFS locking API"#);
    t.assert_server_objects(name, &[&oid(EXAMPLE)], &[]);
}

/// A server without lock verification turns it off, whatever it was set
/// to.
#[track_caller]
fn verify_501(name: &str, verify: Option<bool>) {
    let t = Test::new();
    let repo = lock_verify_setup(&t, name, verify);
    repo.git(["push", "origin", "main"]).ok();
    t.assert_server_objects(name, &[&oid(EXAMPLE)], &[]);
    assert_eq!(locksverify(&t, &repo, name), "false");
}

#[test]
fn pre_push_locks_verify_501_with_verification_enabled() {
    verify_501("lock-enabled-verify-501", Some(true));
}

#[test]
fn pre_push_locks_verify_501_with_verification_disabled() {
    verify_501("lock-disabled-verify-501", Some(false));
}

#[test]
fn pre_push_locks_verify_501_with_verification_unset() {
    verify_501("lock-unset-verify-501", None);
}

#[test]
fn pre_push_locks_verify_200() {
    let t = Test::new();
    let name = "lock-verify-200";
    let repo = lock_verify_setup(&t, name, None);
    assert_eq!(locksverify(&t, &repo, name), "");
    repo.git(["push", "origin", "main"])
        .ok()
        .has(r#"Locking support detected on remote "origin"."#)
        .has(&format!("git config {} true", locksverify_key(&t, name)));
    t.assert_server_objects(name, &[&oid(EXAMPLE)], &[]);
}

#[test]
fn pre_push_locks_verify_403_with_verification_enabled() {
    let t = Test::new();
    let name = "lock-enabled-verify-403";
    let repo = lock_verify_setup(&t, name, Some(true));
    repo.git(["push", "origin", "main"])
        .fails()
        .has("error: Authentication error");
    t.assert_server_objects(name, &[], &[&oid(EXAMPLE)]);
    assert_eq!(locksverify(&t, &repo, name), "true");
}

#[test]
fn pre_push_locks_verify_403_with_verification_disabled() {
    let t = Test::new();
    let name = "lock-disabled-verify-403";
    let repo = lock_verify_setup(&t, name, Some(false));
    repo.git(["push", "origin", "main"]).ok();
    t.assert_server_objects(name, &[&oid(EXAMPLE)], &[]);
    assert_eq!(locksverify(&t, &repo, name), "false");
}

#[test]
fn pre_push_locks_verify_403_with_verification_unset() {
    let t = Test::new();
    let name = "lock-unset-verify-403";
    let repo = lock_verify_setup(&t, name, None);
    repo.git(["push", "origin", "main"])
        .ok()
        .has("warning: Authentication error");
    t.assert_server_objects(name, &[&oid(EXAMPLE)], &[]);
    assert_eq!(locksverify(&t, &repo, name), "");
}

/// `git push` with GIT_TRACE and GIT_TRANSFER_TRACE set.
fn traced_push(repo: &Repo, args: &[&str]) -> Run {
    repo.command("git")
        .args(["push"])
        .args(args)
        .env("GIT_TRACE", "1")
        .env("GIT_TRANSFER_TRACE", "1")
        .run()
        .ok()
}

#[test]
fn pre_push_with_push_default_and_explicit_remote() {
    let t = Test::new();
    let name = "pre-push-pushdefault-explicit";
    let repo = pre_push_setup(&t, name);
    repo.git(["remote", "add", "wrong", &t.endpoint("wrong-url")])
        .ok();
    repo.git(["config", "remote.pushDefault", "wrong"]).ok();
    traced_push(&repo, &["origin", "main"]).lacks("wrong-url");
    t.assert_server_objects(name, &[&oid("hi\n")], &[]);
}

/// A clone of `name` with `a.dat` committed, and lock verification off
/// for the remote's URL, which the tests push to by URL.
fn push_by_url_setup<'a>(t: &'a Test, name: &str) -> (Repo<'a>, String) {
    let repo = t.remote_and_clone(name);
    let url = repo.git(["config", "remote.origin.url"]).ok().stdout();
    let url = url.trim().to_owned();
    repo.git(["config", &format!("lfs.{url}.locksverify"), "false"])
        .ok();
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "hi\n");
    repo.commit_all("add a.dat");
    t.assert_server_objects(name, &[], &[&oid("hi\n")]);
    (repo, url)
}

#[test]
fn pre_push_uses_optimization_if_remote_url_matches() {
    let t = Test::new();
    let name = "pre-push-remote-url-optimization";
    let (repo, url) = push_by_url_setup(&t, name);
    let run = traced_push(&repo, &[&url, "main"]);
    let optimized = run.output().lines().any(|line| {
        line.find("rev-list")
            .is_some_and(|i| line[i..].contains("--not --remotes=origin"))
    });
    assert!(optimized, "{run}");
}

#[test]
fn pre_push_does_not_traverse_git_objects_server_has() {
    let t = Test::new();
    let name = "pre-push-traverse-server-objects";
    let (repo, url) = push_by_url_setup(&t, name);
    // A URL that isn't the remote's, so pre-push can't skip what the
    // remote-tracking branches have.
    let url = format!("{url}.git");
    traced_push(&repo, &[&url, "main"]);
    t.assert_server_objects(name, &[&oid("hi\n")], &[]);

    repo.write("b.dat", "hello\n");
    repo.commit_all("add b.dat");
    t.assert_server_objects(name, &[], &[&oid("hello\n")]);
    // The first object's Git objects aren't traversed again, so it's
    // neither pushed nor asked about.
    traced_push(&repo, &[&url, "main"]).lacks(&oid("hi\n"));
    t.assert_server_objects(name, &[&oid("hello\n")], &[]);
}

#[test]
fn pre_push_with_force_pushed_ref() {
    let t = Test::new();
    let name = "pre-push-force-pushed-ref";
    let repo = pre_push_setup(&t, name);
    repo.git(["tag", "-a", "-m", "tagname", "tagname"]).ok();
    t.assert_server_objects(name, &[], &[&oid("hi\n")]);
    repo.git(["push", "origin", "main", "tagname"]).ok();
    t.assert_server_objects(name, &[&oid("hi\n")], &[]);

    // A different message, so the tag object differs even within the same
    // second. Then drop the old one, which pre-push must cope with.
    repo.git(["tag", "-f", "-a", "-m", "tagname2", "tagname"])
        .ok();
    repo.git(["reflog", "expire", "--all", "--expire=now"]).ok();
    repo.git(["gc", "--prune=now"]).ok();
    repo.git(["push", "origin", "+tagname"]).ok();
}

#[test]
fn pre_push_with_local_path() {
    let t = Test::new();
    let name = "pre-push-local-path";
    t.remote(name);
    let other = t.clone(name, &format!("{name}-2"));
    let repo = t.clone(name, name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "hi\n");
    repo.commit_all("add a.dat");

    repo.git(["push", &format!("../{name}-2"), "main:foo"]).ok();
    repo.git(["push", ".", "main:foo"]).ok();
    repo.lfs(["fsck"]).ok();
    other.git(["checkout", "foo"]).ok();
    other.lfs(["fsck"]).ok();
}
