//! `t-lock.sh`.
//!
//! "lock with git-lfs-transfer" isn't ported yet: it needs the shell
//! suites' `lfs-ssh-echo` and scutiger's `git-lfs-transfer`, which come
//! with the SSH suites.

use crate::harness::{Repo, Test, oid};

/// A clone of the new remote `name`, with `a.dat` tracked, committed, and
/// pushed to `main`, through `git push` with Git's settings to push `main`
/// to `branch`.
fn tracked_branch_setup<'a>(t: &'a Test, name: &str, branch: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "a\n");
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();
    repo.git(["config", "push.default", "upstream"]).ok();
    let merge = format!("refs/heads/{branch}");
    repo.git(["config", "branch.main.merge", &merge]).ok();
    repo.git(["config", "branch.main.remote", "origin"]).ok();
    repo.git(["push", "origin", "main"]).ok();
    repo
}

/// A clone of the new remote `name`, with `a.dat` and `b.dat` tracked,
/// committed, and pushed to the branch `other`.
fn two_files_setup<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "a\n");
    repo.write("b.dat", "b\n");
    repo.git(["add", ".gitattributes", "a.dat", "b.dat"]).ok();
    repo.git(["commit", "-m", "add dat files"]).ok();
    repo.git(["push", "origin", "main:other"]).ok();
    repo
}

/// Add `bad-remote`, with an invalid URL, for the push remote settings to
/// point at.
fn add_bad_remote(repo: &Repo) {
    repo.git(["remote", "add", "bad-remote", "invalid-url"])
        .ok();
}

#[test]
fn lock_with_good_ref() {
    let t = Test::new();
    let name = "lock-main-branch-required";
    t.remote_with_file(name, "a.dat");
    let repo = t.clone(name, name);
    t.assert_lock(name, &repo.lfs(["lock", "a.dat", "--json"]).ok(), "a.dat");
}

#[test]
fn lock_with_good_tracked_ref() {
    let t = Test::new();
    let name = "lock-tracked-branch-required";
    let repo = tracked_branch_setup(&t, name, "tracked");
    t.assert_lock(name, &repo.lfs(["lock", "a.dat", "--json"]).ok(), "a.dat");
}

#[test]
fn lock_with_good_ref_remote_overrides_push_default() {
    let t = Test::new();
    let name = "lock-remote";
    t.remote_with_file(name, "a.dat");
    let repo = t.clone(name, name);
    add_bad_remote(&repo);
    let lock_through_origin = || {
        let run = repo
            .lfs(["lock", "--remote", "origin", "--json", "a.dat"])
            .ok();
        t.assert_lock(name, &run, "a.dat")
    };

    repo.git(["config", "remote.pushDefault", "bad-remote"])
        .ok();
    repo.lfs(["lock", "a.dat"]).fails();
    let id = lock_through_origin();

    repo.git(["config", "--unset", "remote.pushDefault"]).ok();
    repo.lfs(["unlock", "a.dat"]).ok();
    t.refute_server_lock(name, &id);

    repo.git(["config", "branch.main.pushRemote", "bad-remote"])
        .ok();
    repo.lfs(["lock", "a.dat"]).fails();
    lock_through_origin();
}

/// The `.lfsconfig` names a default push remote that doesn't exist, whose
/// URL is a repository without the locking API. Both are ignored, and the
/// lock goes to `origin`.
#[test]
fn lock_with_invalid_default_push_remote_in_lfsconfig() {
    let t = Test::new();
    let name = "fetch-invalid-remote-push-default";
    let repo = t.remote_and_clone(name);
    let unsupported = format!("{}/locksunsupported.git", t.server().url());
    repo.git([
        "config",
        "--file",
        ".lfsconfig",
        "remote.other.origin.url",
        &unsupported,
    ])
    .ok();
    repo.git([
        "config",
        "--file",
        ".lfsconfig",
        "remote.pushDefault",
        "other.origin",
    ])
    .ok();

    let run = repo
        .lfs_command()
        .args(["lock", "--json", "test.bin"])
        .env("GIT_CURL_VERBOSE", "1")
        .run()
        .ok();
    let id = t.assert_lock(name, &run, "test.bin");
    t.refute_server_lock("locksunsupported", &id);
    run.lacks("locksunsupported").lacks("404 Not Found");
}

#[test]
fn lock_with_bad_ref() {
    let t = Test::new();
    let name = "lock-other-branch-required";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "a\n");
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();
    repo.git(["push", "origin", "main:other"]).ok();

    repo.lfs_command()
        .args(["lock", "a.dat"])
        .env("GIT_CURL_VERBOSE", "1")
        .run()
        .fails()
        .has(r#"Locking a.dat failed: Expected ref "refs/heads/other", got "refs/heads/main""#);
}

#[test]
fn lock_multiple_files() {
    let t = Test::new();
    let repo = two_files_setup(&t, "lock-multiple-files");
    let run = repo
        .lfs_command()
        .args(["lock", "a.dat", "b.dat"])
        .env("GIT_TRACE", "0")
        .run();
    assert_eq!(run.count_lines("Locked a.dat"), 1, "{run}");
    assert_eq!(run.count_lines("Locked b.dat"), 1, "{run}");
    assert_eq!(run.stderr(), "", "{run}");
}

#[test]
fn lock_multiple_files_json() {
    let t = Test::new();
    let repo = two_files_setup(&t, "lock-multiple-files-json");
    let run = repo.lfs(["lock", "--json", "a.dat", "b.dat"]);
    // An array of the two locks, in order.
    let json = run.stdout();
    let locks: Vec<&str> = json
        .trim()
        .strip_prefix(r#"[{"id":""#)
        .and_then(|s| s.strip_suffix(r#""}]"#))
        .unwrap_or_else(|| panic!("not an array of locks\n{run}"))
        .split(r#""},{"id":""#)
        .collect();
    assert_eq!(locks.len(), 2, "{run}");
    for (lock, path) in locks.iter().zip(["a.dat", "b.dat"]) {
        let fields =
            format!(r#"","path":"{path}","owner":{{"name":"Git LFS Tests"}},"locked_at":""#);
        assert!(lock.contains(&fields), "{run}");
    }
}

#[test]
fn lock_absolute_path() {
    let t = Test::new();
    let name = "lock-absolute-path";
    let repo = two_files_setup(&t, name);
    let path = repo.path().join("a.dat");
    let run = repo.lfs(["lock", "--json", path.to_str().unwrap()]).ok();
    t.assert_lock(name, &run, "a.dat");
}

#[test]
fn create_lock_with_server_using_client_cert() {
    let t = Test::new();
    let home = t.home();
    let (cert, key) = (home.join("cert.pem"), home.join("key.pem"));
    std::fs::write(&cert, t.server().client_cert_pem()).unwrap();
    std::fs::write(&key, t.server().client_key_pem()).unwrap();
    let base = t.server().client_cert_url();
    t.git_config_global(&format!("http.{base}/.sslCert"), cert.to_str().unwrap());
    t.git_config_global(&format!("http.{base}/.sslKey"), key.to_str().unwrap());

    let name = "lock_create_client_cert";
    let repo = t.remote_with_file(name, "cc.dat");
    let lfs_url = format!("{base}/{name}.git/info/lfs");
    repo.git(["config", "lfs.url", &lfs_url]).ok();
    t.assert_lock(name, &repo.lfs(["lock", "--json", "cc.dat"]).ok(), "cc.dat");
}

#[test]
fn creating_a_lock_with_output() {
    let t = Test::new();
    let name = "lock_create_simple_output";
    let repo = t.remote_with_file(name, "a_output.dat");
    repo.lfs(["lock", "a_output.dat"])
        .ok()
        .has("Locked a_output.dat");
    assert_eq!(t.server().locked_paths(name), ["a_output.dat"]);
}

#[test]
fn locking_a_file_that_doesnt_exist() {
    let t = Test::new();
    let name = "lock_create_nonexistent";
    let repo = t.remote_with_file(name, "a_output.dat");
    repo.lfs(["lock", "b_output.dat"])
        .ok()
        .has("Locked b_output.dat");
    assert_eq!(t.server().locked_paths(name), ["b_output.dat"]);
}

#[test]
fn locking_a_previously_locked_file() {
    let t = Test::new();
    let name = "lock_create_previously_created";
    let repo = t.remote_with_file(name, "b.dat");
    t.assert_lock(name, &repo.lfs(["lock", "--json", "b.dat"]).ok(), "b.dat");
    repo.lfs(["lock", "b.dat"]).has("lock already created");
}

#[test]
fn locking_a_directory() {
    let t = Test::new();
    let name = "remote_locking_directories";
    t.remote(name);
    let repo = t.clone(name, "clone_locking_directories");
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("dir/a.dat", "a\n");
    repo.git(["add", "dir/a.dat", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "add dir/a.dat"])
        .ok()
        .has("main (root-commit)")
        .has("2 files changed")
        .has("create mode 100644 dir/a.dat")
        .has("create mode 100644 .gitattributes");
    repo.git(["push", "origin", "main"])
        .ok()
        .has("main -> main");

    repo.lfs(["lock", "./dir/"]).has("cannot lock directory");
}

#[test]
fn locking_a_nested_file() {
    let t = Test::new();
    let name = "locking-nested-file";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat", "--lockable"]).ok();
    repo.commit_all("initial commit");
    let contents = "contents";
    repo.write("foo/bar/baz/a.dat", contents);
    repo.commit_all("add a.dat");
    repo.git(["push", "origin", "main"]).ok();
    t.assert_server_objects(name, &[&oid(contents)], &[]);

    repo.lfs(["lock", "foo/bar/baz/a.dat"])
        .ok()
        .has("Locked foo/bar/baz/a.dat");
    repo.lfs(["locks"]).ok().has("foo/bar/baz/a.dat");
}

#[test]
fn creating_a_lock_within_subdirectory() {
    let t = Test::new();
    let name = "lock_create_within_subdirectory";
    let repo = t.remote_with_file(name, "sub/a.dat");
    let run = repo.at("sub").lfs(["lock", "--json", "a.dat"]).ok();
    t.assert_lock(name, &run, "sub/a.dat");
}

#[test]
fn creating_a_lock_symlinked_working_directory() {
    let t = Test::new();
    let name = "lock-in-symlinked-working-directory";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "-l", "*.dat"]).ok();
    repo.write("folder2/a.dat", "hello");
    std::fs::create_dir(repo.path().join("folder1")).unwrap();
    std::os::unix::fs::symlink("../folder2", repo.path().join("folder1/folder2")).unwrap();
    repo.git(["add", "--all", "."]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.git(["push", "origin", "main"]).ok();

    let link = format!("{name}-symlink");
    std::os::unix::fs::symlink(name, t.dir().join(&link)).unwrap();
    let run = t
        .repo(&link)
        .lfs(["lock", "--json", "folder1/folder2/a.dat"])
        .ok();
    t.assert_lock(name, &run, "folder1/folder2/a.dat");
}

/// Mark `a.txt` lockable, then ignore it, checking it out read-only each
/// time. `lfs.lockignoredfiles` is set to `ignored_setting` if given.
fn gitignore_checkouts(name: &str, ignored_setting: Option<&str>) {
    let t = Test::new();
    t.remote_with_file(name, "a.txt");
    let repo = t.clone(name, name);
    if let Some(value) = ignored_setting {
        repo.git(["config", "lfs.lockignoredfiles", value]).ok();
    }
    let checkout = || {
        std::fs::remove_file(repo.path().join("a.txt")).unwrap();
        repo.git(["checkout", "a.txt"]).ok();
        repo.refute_writeable("a.txt");
    };

    repo.write(
        ".gitattributes",
        "*.txt filter=lfs diff=lfs merge=lfs -text lockable\n",
    );
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", ".gitattributes: mark 'a.txt' as lockable"])
        .ok();
    checkout();

    repo.write(".gitignore", "*.txt\n");
    repo.git(["add", ".gitignore"]).ok();
    repo.git(["commit", "-m", ".gitignore: ignore 'a.txt'"])
        .ok();
    checkout();
}

#[test]
fn lock_with_gitignore() {
    gitignore_checkouts("lock-with-gitignore", None);
}

#[test]
fn lock_with_gitignore_and_lfs_lockignoredfiles() {
    gitignore_checkouts("lock-with-gitignore-and-ignoredfiles", Some("true"));
}
