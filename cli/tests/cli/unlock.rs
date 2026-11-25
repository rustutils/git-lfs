//! `t-unlock.sh`.
//!
//! "unlock with git-lfs-transfer" isn't ported yet: it needs the shell
//! suites' `lfs-ssh-echo` and scutiger's `git-lfs-transfer`, which come
//! with the SSH suites.

use crate::harness::{Repo, Test};

/// The remote `name` with `file` committed, in a clone that marks `*.dat`
/// lockable, like the shell suite's `setup_repo`.
fn setup_repo<'a>(t: &'a Test, name: &str, file: &str) -> Repo<'a> {
    let repo = t.remote_with_file(name, file);
    repo.lfs(["track", "--lockable", "*.dat"]).ok();
    repo.git(["add", "-u"]).ok();
    repo.git(["commit", "-m", "Mark files lockable"]).ok();
    repo
}

/// Lock `path`, check the server has the lock, and return its ID.
fn lock(t: &Test, repo: &Repo, name: &str, path: &str) -> String {
    t.assert_lock(name, &repo.lfs(["lock", "--json", path]).ok(), path)
}

/// A clone of the new remote `name`, with `c.dat` tracked and committed.
fn c_dat_setup<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("c.dat", "c\n");
    repo.git(["add", ".gitattributes", "c.dat"]).ok();
    repo.git(["commit", "-m", "add c.dat"]).ok();
    repo
}

/// Lock and unlock `c.dat` in `name`, with `unlock` giving the arguments
/// to unlock the lock with the given ID.
fn lock_and_unlock(name: &str, unlock: impl Fn(&str) -> Vec<String>) {
    let t = Test::new();
    let repo = setup_repo(&t, name, "c.dat");
    let id = lock(&t, &repo, name, "c.dat");
    repo.lfs(unlock(&id)).ok();
    t.refute_server_lock(name, &id);
}

fn by_path(_: &str) -> Vec<String> {
    vec!["unlock".into(), "c.dat".into()]
}

fn by_id(id: &str) -> Vec<String> {
    vec!["unlock".into(), format!("--id={id}")]
}

#[test]
fn unlocking_a_lock_by_path_without_a_ref_required() {
    lock_and_unlock("unlock-by-path-main-branch-not-required", by_path);
}

#[test]
fn unlocking_a_lock_by_path_with_good_ref() {
    lock_and_unlock("unlock-by-path-main-branch-required", by_path);
}

#[test]
fn unlocking_a_lock_by_id_with_good_ref() {
    lock_and_unlock("unlock-by-id-main-branch-required", by_id);
}

/// Lock and unlock `c.dat` with Git set to push `main` to `tracked`.
fn tracked_ref(name: &str, unlock: impl Fn(&str) -> Vec<String>) {
    let t = Test::new();
    let repo = c_dat_setup(&t, name);
    repo.git(["config", "push.default", "upstream"]).ok();
    repo.git(["config", "branch.main.merge", "refs/heads/tracked"])
        .ok();
    repo.git(["config", "branch.main.remote", "origin"]).ok();
    repo.git(["push", "origin", "main"]).ok();

    let id = lock(&t, &repo, name, "c.dat");
    repo.lfs(unlock(&id)).ok();
    t.refute_server_lock(name, &id);
}

#[test]
fn unlocking_a_lock_by_path_with_tracked_ref() {
    tracked_ref("unlock-by-path-tracked-branch-required", by_path);
}

#[test]
fn unlocking_a_lock_by_id_with_tracked_ref() {
    tracked_ref("unlock-by-id-tracked-branch-required", by_id);
}

/// An invalid push remote setting makes unlocking fail, and `--remote`
/// overrides it. `target` gives the arguments that pick the lock with the
/// given ID, and `json` what `--json` prints for it.
fn remote_overrides_push_default(
    name: &str,
    target: impl Fn(&str) -> Vec<String>,
    json: impl Fn(&str) -> String,
) {
    let t = Test::new();
    t.remote_with_file(name, "a.dat");
    let repo = t.clone(name, name);
    repo.git(["remote", "add", "bad-remote", "invalid-url"])
        .ok();

    for (key, unset) in [
        ("remote.pushDefault", true),
        ("branch.main.pushRemote", false),
    ] {
        let id = lock(&t, &repo, name, "a.dat");
        repo.git(["config", key, "bad-remote"]).ok();
        let mut args = vec!["unlock".to_owned()];
        args.extend(target(&id));
        repo.lfs(&args).fails();

        let mut args = vec!["unlock", "--remote", "origin", "--json"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        args.extend(target(&id));
        repo.lfs(&args).ok().has(&json(&id));
        t.refute_server_lock(name, &id);
        if unset {
            repo.git(["config", "--unset", key]).ok();
        }
    }
}

#[test]
fn unlock_by_id_with_good_ref_remote_overrides_push_default() {
    remote_overrides_push_default(
        "unlock-by-id-remote",
        |id| vec![format!("--id={id}")],
        |id| format!(r#"[{{"id":"{id}","unlocked":true}}]"#),
    );
}

#[test]
fn unlock_by_path_with_good_ref_remote_overrides_push_default() {
    remote_overrides_push_default(
        "unlock-by-path-remote",
        |_| vec!["a.dat".into()],
        |_| r#"[{"path":"a.dat","unlocked":true}]"#.into(),
    );
}

/// The `.lfsconfig` names a default push remote that doesn't exist, whose
/// URL is a repository without the locking API. Both are ignored, and the
/// unlock goes to `origin`.
#[test]
fn unlock_with_invalid_default_push_remote_in_lfsconfig() {
    let t = Test::new();
    let name = "unlock-by-path-invalid-remote-push-default";
    let repo = t.remote_and_clone(name);
    let id = lock(&t, &repo, name, "test.bin");

    let unsupported = format!("{}/locksunsupported.git", t.server().url());
    let lfsconfig = |key: &str, value: &str| {
        repo.git(["config", "--file", ".lfsconfig", key, value])
            .ok();
    };
    lfsconfig("remote.other.origin.url", &unsupported);
    lfsconfig("remote.pushDefault", "other.origin");

    repo.lfs_command()
        .args(["unlock", "--json", "test.bin"])
        .env("GIT_CURL_VERBOSE", "1")
        .run()
        .ok()
        .has(r#"[{"path":"test.bin","unlocked":true}]"#)
        .lacks("locksunsupported")
        .lacks("404 Not Found");
    t.refute_server_lock(name, &id);
}

/// Lock `c.dat` on the branch `other`, then try to unlock it from `main`.
fn bad_ref(name: &str, unlock: impl Fn(&str) -> Vec<String>, required: bool) {
    let t = Test::new();
    let repo = c_dat_setup(&t, name);
    repo.git(["push", "origin", "main:other"]).ok();
    repo.git(["checkout", "-b", "other"]).ok();
    let id = lock(&t, &repo, name, "c.dat");
    repo.git(["checkout", "main"]).ok();

    let run = repo.lfs(unlock(&id));
    if required {
        run.fails()
            .has(r#"Expected ref "refs/heads/other", got "refs/heads/main""#);
        t.assert_server_lock(name, &id, "c.dat");
    } else {
        run.ok();
        t.refute_server_lock(name, &id);
    }
}

#[test]
fn unlocking_a_lock_by_path_with_bad_ref_without_a_ref_required() {
    bad_ref("unlock-by-path-other-branch-not-required", by_path, false);
}

#[test]
fn unlocking_a_lock_by_path_with_bad_ref() {
    bad_ref("unlock-by-path-other-branch-required", by_path, true);
}

#[test]
fn unlocking_a_lock_by_id_with_bad_ref() {
    bad_ref("unlock-by-id-other-branch-required", by_id, true);
}

/// A clone of the new remote `name`, with `a.dat` and `b.dat` committed,
/// pushed to `other`, and locked.
fn two_locks_setup<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "a\n");
    repo.write("b.dat", "b\n");
    repo.git(["add", ".gitattributes", "a.dat", "b.dat"]).ok();
    repo.git(["commit", "-m", "add dat files"]).ok();
    repo.git(["push", "origin", "main:other"]).ok();
    repo.lfs(["lock", "a.dat"]).ok();
    repo.lfs(["lock", "b.dat"]).ok();
    repo
}

#[test]
fn unlock_multiple_files() {
    let t = Test::new();
    let repo = two_locks_setup(&t, "unlock-multiple-files");
    repo.lfs(["unlock", "a.dat", "b.dat"])
        .ok()
        .lacks("Exactly one of --id or a set of paths must be provided");
}

#[test]
fn unlock_multiple_files_json() {
    let t = Test::new();
    let repo = two_locks_setup(&t, "unlock-multiple-files-json");
    repo.lfs(["unlock", "--json", "a.dat", "b.dat"])
        .ok()
        .has(r#"[{"path":"a.dat","unlocked":true},{"path":"b.dat","unlocked":true}]"#);
}

#[test]
fn unlocking_a_file_makes_it_readonly() {
    let t = Test::new();
    let name = "unlock_set_readonly";
    let repo = setup_repo(&t, name, "c.dat");
    lock(&t, &repo, name, "c.dat");
    repo.assert_writeable("c.dat");
    repo.lfs(["unlock", "c.dat"]).ok();
    repo.refute_writeable("c.dat");
}

#[test]
fn unlocking_a_file_ignores_readonly() {
    let t = Test::new();
    let name = "unlock_set_readonly_ignore";
    let repo = setup_repo(&t, name, "c.dat");
    lock(&t, &repo, name, "c.dat");
    repo.assert_writeable("c.dat");
    repo.git([
        "-c",
        "lfs.setlockablereadonly=false",
        "lfs",
        "unlock",
        "c.dat",
    ])
    .ok();
    repo.assert_writeable("c.dat");
}

#[test]
fn unlocking_lock_removed_file() {
    let t = Test::new();
    let name = "unlock-removed-file";
    let repo = setup_repo(&t, name, "a.dat");
    let id = lock(&t, &repo, name, "a.dat");
    repo.git(["rm", "a.dat"]).ok();
    repo.git(["commit", "-m", "a.dat"]).ok();
    repo.assert_clean_status();

    repo.lfs(["unlock", "--force", "a.dat"]).ok();
    t.refute_server_lock(name, &id);
}

#[test]
fn unlocking_nonexistent_file() {
    let t = Test::new();
    let name = "unlock-nonexistent-file";
    let repo = setup_repo(&t, name, "a.dat");
    let id = lock(&t, &repo, name, "b.dat");
    repo.lfs(["unlock", "--force", "b.dat"]).ok();
    t.refute_server_lock(name, &id);
}

/// Lock and force-unlock `README.md`, which isn't lockable, so it stays
/// writeable throughout.
fn unlock_unlockable(t: &Test, repo: &Repo, name: &str) {
    repo.write("README.md", "");
    repo.git(["add", "README.md"]).ok();
    repo.git(["commit", "-m", "Add README"]).ok();
    let id = lock(t, repo, name, "README.md");
    repo.assert_writeable("README.md");
    repo.lfs(["unlock", "--force", "README.md"]).ok();
    t.refute_server_lock(name, &id);
    repo.assert_writeable("README.md");
}

#[test]
fn unlocking_unlockable_file() {
    let t = Test::new();
    let name = "unlock-unlockable-file";
    // With lockable patterns, then without any.
    let repo = setup_repo(&t, name, "a.dat");
    unlock_unlockable(&t, &repo, name);
    let name = "unlock-unlockable-file-2";
    let repo = t.remote_with_file(name, "a.dat");
    unlock_unlockable(&t, &repo, name);
}

#[test]
fn unlocking_a_lock_json() {
    let t = Test::new();
    let name = "unlock_by_path_json";
    let repo = setup_repo(&t, name, "c_json.dat");
    let id = lock(&t, &repo, name, "c_json.dat");
    repo.lfs(["unlock", "--json", "c_json.dat"])
        .ok()
        .has(r#""unlocked":true"#);
    t.refute_server_lock(name, &id);
}

#[test]
fn unlocking_a_missing_lockable_file_json() {
    let t = Test::new();
    let name = "unlock_missing_lockable_path_json";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "--lockable", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "Add lockable pattern"]).ok();
    repo.git(["push", "origin", "main"]).ok();

    repo.write("file1.dat", "file1.dat\n");
    let id = lock(&t, &repo, name, "file1.dat");
    std::fs::remove_file(repo.path().join("file1.dat")).unwrap();
    repo.lfs(["unlock", "--json", "file1.dat"])
        .ok()
        .has(r#"[{"path":"file1.dat","unlocked":true}]"#);
    t.refute_server_lock(name, &id);
}

#[test]
fn unlocking_a_lock_by_id() {
    let t = Test::new();
    let name = "unlock_by_id";
    let repo = setup_repo(&t, name, "d.dat");
    let id = lock(&t, &repo, name, "d.dat");
    repo.assert_writeable("d.dat");
    repo.lfs(["unlock", &format!("--id={id}")]).ok();
    repo.refute_writeable("d.dat");
    t.refute_server_lock(name, &id);
}

#[test]
fn unlocking_a_lock_by_id_json() {
    let t = Test::new();
    let name = "unlock_by_id_json";
    let repo = setup_repo(&t, name, "c_json.dat");
    let id = lock(&t, &repo, name, "c_json.dat");
    repo.lfs(["unlock", "--json", &format!("--id={id}")])
        .ok()
        .has(r#""unlocked":true"#);
    t.refute_server_lock(name, &id);
}

const EXACTLY_ONE: &str = "Exactly one of --id or a set of paths must be provided";

#[test]
fn unlocking_a_lock_without_sufficient_info() {
    let t = Test::new();
    let name = "unlock_ambiguous";
    let repo = setup_repo(&t, name, "e.dat");
    let id = lock(&t, &repo, name, "e.dat");
    repo.lfs(["unlock"]).fails().has(EXACTLY_ONE);
    t.assert_server_lock(name, &id, "e.dat");
}

#[test]
fn unlocking_a_lock_while_uncommitted() {
    let t = Test::new();
    let name = "unlock_modified";
    let repo = setup_repo(&t, name, "mod.dat");
    let id = lock(&t, &repo, name, "mod.dat");
    repo.append("mod.dat", "\nSomething\n");

    repo.lfs(["unlock", "mod.dat"])
        .fails()
        .has("Cannot unlock file with uncommitted changes");
    t.assert_server_lock(name, &id, "mod.dat");

    // After discarding the change, it's allowed.
    repo.git(["checkout", "mod.dat"]).ok();
    repo.lfs(["unlock", "mod.dat"]).ok();
    t.refute_server_lock(name, &id);
}

#[test]
fn unlocking_a_lock_with_ambiguous_arguments() {
    let t = Test::new();
    let name = "unlock_ambiguous_args";
    let repo = setup_repo(&t, name, "a.dat");
    let id = lock(&t, &repo, name, "a.dat");
    repo.lfs(["unlock", "--id", &id, "a.dat"])
        .fails()
        .has(EXACTLY_ONE);
    t.assert_server_lock(name, &id, "a.dat");
}

#[test]
fn unlocking_a_lock_while_uncommitted_with_force() {
    let t = Test::new();
    let name = "unlock_modified_force";
    let repo = setup_repo(&t, name, "modforce.dat");
    let id = lock(&t, &repo, name, "modforce.dat");
    repo.append("modforce.dat", "\nSomething\n");
    repo.lfs(["unlock", "--force", "modforce.dat"])
        .ok()
        .has("warning: unlocking with uncommitted changes");
    t.refute_server_lock(name, &id);
}

#[test]
fn unlocking_a_lock_while_untracked() {
    let t = Test::new();
    let name = "unlock_untracked";
    let repo = setup_repo(&t, name, "notrelevant.dat");
    repo.lfs(["track", "*.dat"]).ok();
    // Not added to Git, so it can't be unlocked.
    repo.write("untracked.dat", "something\n");
    let id = lock(&t, &repo, name, "untracked.dat");

    repo.lfs(["unlock", "untracked.dat"])
        .fails()
        .has("Cannot unlock file with uncommitted changes");
    t.assert_server_lock(name, &id, "untracked.dat");

    // After committing it, it's allowed.
    repo.git(["add", "untracked.dat"]).ok();
    repo.git(["commit", "-m", "Added untracked"]).ok();
    repo.lfs(["unlock", "untracked.dat"]).ok();
    t.refute_server_lock(name, &id);
}
