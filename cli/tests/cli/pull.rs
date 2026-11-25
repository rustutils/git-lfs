//! `t-pull.sh`.

use std::os::unix::fs::PermissionsExt;

use crate::conflicts::{self, CaseSymlinks, remove};
use crate::harness::{Repo, Run, Test, oid};

const NAME: &str = "t-pull";

/// The first part of the shell suite's "pull" test, which later tests use:
/// the remote `t-pull` with four files pushed from `repo`, and its clone in
/// `clone`, made before the push.
fn setup(t: &Test) -> Repo<'_> {
    t.remote(NAME);
    let clone = t.clone(NAME, "clone");
    let repo = t.clone(NAME, "repo");
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    repo.write(".gitignore", "*.log\n");
    repo.write("a.dat", "a");
    repo.write("á.dat", "A");
    repo.write("dir1/dir.dat", "dir");
    repo.write("dir2/dir3/dir4/dir.dat", "dir");
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", "add files"])
        .ok()
        .has("main (root-commit)")
        .has("6 files changed")
        .has("create mode 100644 a.dat")
        .has("create mode 100644 .gitattributes");
    assert_files(&repo);
    repo.assert_pointer("main", "a.dat", &oid("a"), 1);
    repo.assert_pointer("main", "á.dat", &oid("A"), 1);
    repo.assert_pointer("main", "dir1/dir.dat", &oid("dir"), 3);
    for contents in ["a", "A", "dir"] {
        assert!(t.server().object(NAME, &oid(contents)).is_none());
    }

    repo.git(["push", "origin", "main"])
        .ok()
        .has("Uploading LFS objects: 100% (3/3), 5 B")
        .has("main -> main");
    for contents in ["a", "A", "dir"] {
        assert!(t.server().object(NAME, &oid(contents)).is_some());
    }
    clone
}

/// Check the files from [`setup`] are checked out.
#[track_caller]
fn assert_files(repo: &Repo) {
    assert_eq!(repo.read("a.dat"), "a");
    assert_eq!(repo.read("á.dat"), "A");
    assert_eq!(repo.read("dir1/dir.dat"), "dir");
    assert_eq!(repo.read("dir2/dir3/dir4/dir.dat"), "dir");
}

/// Check the objects from [`setup`] are in the local store.
#[track_caller]
fn assert_objects(repo: &Repo) {
    repo.assert_local_object(&oid("a"), 1);
    repo.assert_local_object(&oid("A"), 1);
    repo.assert_local_object(&oid("dir"), 3);
}

/// Remove the files from [`setup`] from the working tree.
fn remove_files(repo: &Repo) {
    for file in ["a.dat", "á.dat", "dir1/dir.dat"] {
        let _ = std::fs::remove_file(repo.path().join(file));
    }
    let _ = std::fs::remove_dir_all(repo.path().join("dir2"));
}

#[test]
fn pull() {
    let t = Test::new();
    let clone = setup(&t);

    // A normal pull.
    clone.git(["config", "branch.main.remote", "origin"]).ok();
    clone
        .git(["config", "branch.main.merge", "refs/heads/main"])
        .ok();
    clone.git(["pull"]).ok();
    assert_files(&clone);
    assert_objects(&clone);
    clone.assert_clean_status();

    // git lfs pull, with and without a remote.
    for args in [&["pull"][..], &["pull", "origin"]] {
        remove_files(&clone);
        clone.remove_local_objects();
        clone.lfs(args).ok();
        assert_files(&clone);
        assert_objects(&clone);
        clone.assert_clean_status();
        clone.lfs(["fsck"]).ok();
    }

    // From the local store.
    remove_files(&clone);
    clone.lfs(["pull"]).ok();
    assert_files(&clone);
    clone.assert_clean_status();

    // Into existing directories.
    std::fs::remove_file(clone.path().join("dir1/dir.dat")).unwrap();
    std::fs::remove_dir_all(clone.path().join("dir2/dir3/dir4")).unwrap();
    clone.lfs(["pull"]).ok();
    assert_eq!(clone.read("dir1/dir.dat"), "dir");
    assert_eq!(clone.read("dir2/dir3/dir4/dir.dat"), "dir");
    clone.assert_clean_status();

    // Include and exclude filters in the config.
    clone.remove_local_objects();
    clone.git(["config", "lfs.fetchinclude", "a*"]).ok();
    clone.lfs(["pull"]).ok();
    clone.assert_local_object(&oid("a"), 1);
    clone.assert_clean_status();

    clone.remove_local_objects();
    clone.git(["config", "--unset", "lfs.fetchinclude"]).ok();
    clone.git(["config", "lfs.fetchexclude", "a*"]).ok();
    clone.lfs(["pull"]).ok();
    clone.refute_local_object(&oid("a"));
    clone.assert_clean_status();

    // Include and exclude filters as arguments.
    clone.git(["config", "--unset", "lfs.fetchexclude"]).ok();
    clone.remove_local_objects();
    clone.lfs(["pull", "--include=a*"]).ok();
    clone.assert_local_object(&oid("a"), 1);
    clone.assert_clean_status();

    clone.remove_local_objects();
    clone.lfs(["pull", "--exclude=a*"]).ok();
    clone.refute_local_object(&oid("a"));
    clone.assert_clean_status();

    clone.git(["reset", "--hard"]).ok();
    clone.assert_clean_status();

    // With a clean status.
    clone.lfs(["pull"]).ok();
    assert_files(&clone);
    assert_objects(&clone);
    clone.assert_clean_status();

    // With -I.
    clone.remove_local_objects();
    remove_files(&clone);
    clone.lfs(["pull", "-I", "a.*,dir1/dir.*,dir2/**"]).ok();
    assert_eq!(clone.read("a.dat"), "a");
    assert!(!clone.path().join("á.dat").exists());
    assert_eq!(clone.read("dir1/dir.dat"), "dir");
    assert_eq!(clone.read("dir2/dir3/dir4/dir.dat"), "dir");
    clone.assert_local_object(&oid("a"), 1);
    clone.refute_local_object(&oid("A"));
    clone.assert_local_object(&oid("dir"), 3);
    clone.assert_clean_worktree_except(&["á.dat"]);

    clone.lfs(["pull", "-I", "*.dat"]).ok();
    assert_eq!(clone.read("á.dat"), "A");
    clone.assert_local_object(&oid("A"), 1);
    clone.assert_clean_status();

    // With an empty file.
    clone.write("empty.dat", "");
    clone.git(["add", "empty.dat"]).ok();
    clone.git(["commit", "-m", "empty"]).ok();
    clone.lfs(["pull"]).ok();
    assert_eq!(clone.read("empty.dat"), "");
    clone.assert_clean_status();

    clone.git(["reset", "--hard", "HEAD^"]).ok();
    clone.assert_clean_status();

    // In a subdirectory.
    clone.remove_local_objects();
    remove_files(&clone);
    clone.at("dir1").lfs(["pull"]).ok();
    assert_files(&clone);
    assert_objects(&clone);
    clone.assert_clean_status();

    // In a subdirectory, with -I. The patterns are relative to the root.
    clone.remove_local_objects();
    remove_files(&clone);
    clone
        .at("dir1")
        .lfs(["pull", "-I", "á.*,dir1/dir.dat,dir2/**"])
        .ok();
    assert!(!clone.path().join("a.dat").exists());
    assert_eq!(clone.read("á.dat"), "A");
    assert_eq!(clone.read("dir1/dir.dat"), "dir");
    assert_eq!(clone.read("dir2/dir3/dir4/dir.dat"), "dir");
    clone.refute_local_object(&oid("a"));
    clone.assert_local_object(&oid("A"), 1);
    clone.assert_local_object(&oid("dir"), 3);
    clone.assert_clean_worktree_except(&["a.dat"]);

    clone.at("dir1").lfs(["pull", "-I", "*.dat"]).ok();
    assert_eq!(clone.read("a.dat"), "a");
    clone.assert_local_object(&oid("a"), 1);
    clone.assert_clean_status();
}

/// The remote `name` with `files` containing "a" pushed, and a clone of it
/// in `<name>-assert` without the objects.
fn conflict_setup<'a>(t: &'a Test, name: &str, files: &[&str]) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    for file in files {
        repo.write(file, "a");
    }
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.git(["push", "origin", "main"]).ok();
    assert!(t.server().object(name, &oid("a")).is_some());

    let clone = t.clone_skip_smudge(name, &format!("{name}-assert"));
    clone.refute_local_object(&oid("a"));
    clone
}

/// Remove the local objects, run `git lfs pull` from the subdirectory
/// `cwd`, and check it succeeds and fetches the object.
fn pull_in<'a>(repo: &'a Repo) -> impl Fn(&str) -> Run + 'a {
    move |cwd| {
        repo.remove_local_objects();
        let run = repo.at(cwd).lfs(["pull"]).ok();
        repo.assert_local_object(&oid("a"), 1);
        run
    }
}

#[test]
fn pull_skip_directory_file_conflicts() {
    let t = Test::new();
    let files = ["dir1/a.dat", "dir2/dir3/dir4/a.dat"];
    let repo = conflict_setup(&t, "pull-skip-dir-file-conflicts", &files);
    conflicts::directory_file(&repo, pull_in(&repo));
}

#[test]
fn pull_skip_directory_symlink_conflicts() {
    let t = Test::new();
    let files = ["dir1/a.dat", "dir2/dir3/dir4/a.dat"];
    let repo = conflict_setup(&t, "pull-skip-dir-symlink-conflicts", &files);
    conflicts::directory_symlink(&t, &repo, pull_in(&repo));
}

#[test]
fn pull_skip_file_symlink_conflicts() {
    let t = Test::new();
    let files = ["a.dat", "dir1/dir2/dir3/a.dat"];
    let repo = conflict_setup(&t, "pull-skip-file-symlink-conflicts", &files);
    conflicts::file_symlink(&t, &repo, pull_in(&repo));
}

#[test]
fn pull_skip_case_based_symlink_conflicts() {
    let t = Test::new();
    conflicts::case_symlinks(
        &t,
        CaseSymlinks {
            name: "pull-skip-case-symlink-conflicts",
            prepare: &|clone| clone.refute_local_object(&oid("a")),
            run: &|clone| clone.lfs(["pull"]).ok(),
            progress: None,
        },
    );
}

#[test]
fn pull_skip_changed_files() {
    let t = Test::new();
    let repo = conflict_setup(&t, "pull-skip-changed-files", &["a.dat"]);
    conflicts::changed_files(&repo, pull_in(&repo));
}

#[test]
fn pull_break_hard_links_to_existing_files() {
    let t = Test::new();
    let files = ["a.dat", "dir1/dir2/dir3/a.dat"];
    let repo = conflict_setup(&t, "pull-break-file-hardlinks", &files);
    conflicts::hard_links(&t, &repo, pull_in(&repo));
}

#[test]
fn pull_without_clean_filter() {
    let t = Test::new();
    setup(&t);
    let repo = t.clone_skip_smudge(NAME, "no-clean");
    let a = oid("a");
    repo.lfs(["uninstall"]).ok();
    repo.git(["config", "--list"])
        .ok()
        .lacks("filter.lfs.clean");

    // The working tree has the pointer.
    assert!(repo.read("a.dat").contains(&a));
    repo.lfs(["pull"]).ok().has("Git LFS is not installed");

    // The object is fetched, and the pointer is left alone.
    assert!(repo.read("a.dat").contains(&a));
    repo.assert_local_object(&a, 1);
}

/// A new repository in `dir` with the remote `t-pull` as `origin`, LFS
/// installed without the smudge filter, and main pulled.
fn skip_smudge_init<'a>(t: &'a Test, dir: &str, remotes: &[(&str, &str)]) -> Repo<'a> {
    let repo = t.repo(dir);
    std::fs::create_dir(repo.path()).unwrap();
    repo.git(["init"]).ok();
    repo.lfs(["install", "--local", "--skip-smudge"]).ok();
    repo.git(["remote", "add", "origin", &t.remote_url(NAME)])
        .ok();
    for (name, url) in remotes {
        repo.git(["remote", "add", name, url]).ok();
    }
    repo.git(["pull", "origin", "main"]).ok();
    repo
}

#[test]
fn pull_with_raw_remote_url() {
    let t = Test::new();
    setup(&t);
    let repo = skip_smudge_init(&t, "raw", &[]);
    let a = oid("a");

    // The working tree has the pointer.
    repo.refute_local_object(&a);
    assert!(repo.read("a.dat").contains(&a));

    repo.lfs(["pull", &t.remote_url(NAME)]).ok();
    repo.assert_local_object(&a, 1);
    assert_eq!(repo.read("a.dat"), "a");
}

#[test]
fn pull_with_multiple_remotes() {
    let t = Test::new();
    setup(&t);
    let repo = skip_smudge_init(&t, "multiple", &[("bad-remote", "invalid-url")]);
    let a = oid("a");

    repo.refute_local_object(&a);
    assert!(repo.read("a.dat").contains(&a));

    // origin is the default, not bad-remote.
    repo.lfs(["pull"]).ok();
    repo.assert_local_object(&a, 1);
    assert_eq!(repo.read("a.dat"), "a");
}

#[test]
fn pull_with_invalid_insteadof() {
    let t = Test::new();
    setup(&t);
    let repo = skip_smudge_init(&t, "insteadof", &[]);
    let server = t.server().url();

    // Rewrite the download href, and turn rewriting on.
    let key = format!("url.{server}/storage/invalid.insteadOf");
    repo.git(["config", &key, &format!("{server}/storage/")])
        .ok();
    repo.git(["config", "lfs.transfer.enablehrefrewrite", "true"])
        .ok();

    let run = repo.lfs(["pull"]);
    assert_eq!(run.code(), Some(2), "{run}");
    // The rewritten href is used.
    run.has(&format!(
        "LFS: Repository or object not found: {server}/storage/invalid"
    ));

    repo.git(["config", "--unset", "lfs.transfer.enablehrefrewrite"])
        .ok();
    repo.lfs(["pull"]).ok();
}

/// A new repository in `dir`, with `*.bin` tracked.
fn local_repo<'a>(t: &'a Test, dir: &str) -> Repo<'a> {
    t.command(t.dir(), "git").args(["init", dir]).run().ok();
    let repo = t.repo(dir);
    repo.lfs(["track", "*.bin"]).ok();
    repo.commit_all("gitattributes");
    repo
}

#[test]
fn pull_with_merge_conflict() {
    let t = Test::new();
    let repo = local_repo(&t, "pull-merge-conflict");
    repo.write("abc.bin", "abc");
    repo.commit_all("abc");

    repo.git(["checkout", "-b", "def"]).ok();
    repo.write("abc.bin", "def");
    repo.commit_all("def");

    repo.git(["checkout", "main"]).ok();
    repo.write("abc.bin", "ghi");
    repo.commit_all("ghi");

    // The merge conflicts.
    repo.command("git")
        .args(["merge", "def"])
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .run()
        .fails();
    let run = repo.lfs(["pull"]);
    assert!(run.output().is_empty(), "{run}");
}

#[test]
fn pull_with_missing_object() {
    let t = Test::new();
    let clone = setup(&t);
    clone.git(["pull", "origin", "main"]).ok();
    let a = oid("a");
    clone.remove_local_objects();
    remove(&clone.path().join("a.dat"));
    t.server().delete_object(NAME, &a);

    // It fails, but fetches the other objects.
    clone.git(["config", "branch.main.remote", "origin"]).ok();
    clone
        .git(["config", "branch.main.merge", "refs/heads/main"])
        .ok();
    clone
        .lfs(["pull"])
        .fails()
        .has(&format!("{a} does not exist"));
    clone.assert_local_object(&oid("A"), 1);
    clone.refute_local_object(&a);
}

#[test]
fn pull_outside_git_repository() {
    let t = Test::new();
    let outside = t.repo("outside");
    std::fs::create_dir(outside.path()).unwrap();
    let run = outside.lfs(["pull"]);
    assert_eq!(run.code(), Some(128), "{run}");
    run.has("Not in a Git repository");
}

#[test]
fn pull_read_only_directory() {
    let t = Test::new();
    if t.is_root() {
        return;
    }
    let name = "pull-read-only";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.bin"]).ok();
    repo.write("dir/a.bin", "a");
    repo.git(["add", ".gitattributes", "dir/a.bin"]).ok();
    repo.git(["commit", "-m", "add dir/a.bin"]).ok();
    repo.git(["push", "origin", "main"]).ok();
    assert!(t.server().object(name, &oid("a")).is_some());

    remove(&repo.path().join("dir/a.bin"));
    repo.delete_local_object(&oid("a"));
    let dir = repo.path().join("dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    // It reports an error, but exits 0.
    let run = repo.lfs(["pull"]).ok();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    repo.assert_local_object(&oid("a"), 1);
    assert!(!repo.path().join("dir/a.bin").exists());
    run.has(r#"could not check out "dir/a.bin""#)
        .has("could not create working directory file")
        .has("permission denied");
}

#[test]
fn pull_read_only_file() {
    let t = Test::new();
    let name = "pull-locked";
    let file = "a.txt";
    t.remote_with_file(name, file);
    let contents_oid = oid(format!("{file}\n"));

    let clone = t.clone_skip_smudge(name, "pull-locked-assert");
    let path = clone.path().join(file);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let writable = || std::fs::metadata(&path).unwrap().permissions().mode() & 0o200 != 0;

    assert!(!writable());
    clone.assert_pointer("refs/heads/main", file, &contents_oid, 6);
    clone.lfs(["pull"]).ok();
    assert!(!writable());
    assert_eq!(clone.read(file), format!("{file}\n"));
}

#[test]
fn pull_with_empty_file_doesnt_modify_mtime() {
    let t = Test::new();
    let repo = local_repo(&t, "pull-empty-file");
    repo.write("abc.bin", "abc");
    repo.commit_all("abc");

    let path = repo.path().join("foo.bin");
    repo.write("foo.bin", "");
    let mtime = || std::fs::metadata(&path).unwrap().modified().unwrap();
    let before = mtime();
    repo.commit_all("foo");

    repo.lfs(["pull"]).ok();
    assert_eq!(mtime(), before);
}

#[test]
fn pull_bare_repository() {
    let t = Test::new();
    let name = "pull-bare";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "a");

    // A pointer whose path in the repository, taken as an absolute path,
    // is a writable directory. Pulling in a bare repository must check out
    // neither file, inside the repository or outside it.
    let external = t.dir().join("pull-bare-external");
    let internal = external
        .to_str()
        .unwrap()
        .trim_start_matches('/')
        .to_owned();
    std::fs::create_dir_all(&external).unwrap();
    repo.write(&format!("{internal}/a.dat"), "a");
    repo.git([
        "add",
        ".gitattributes",
        "a.dat",
        &format!("{internal}/a.dat"),
    ])
    .ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.git(["push", "origin", "main"]).ok();
    assert!(t.server().object(name, &oid("a")).is_some());

    t.command(t.dir(), "git")
        .args(["clone", "--bare", &t.remote_url(name), "pull-bare-assert"])
        .run()
        .ok();
    let bare = t.repo("pull-bare-assert");
    assert!(!bare.path().join("lfs").exists());
    bare.refute_local_object(&oid("a"));

    // With Git 2.42 or later, pull lists files with git ls-files, and a
    // bare repository has no index, so nothing is fetched. Before that, it
    // uses git ls-tree.
    let ls_files = t.git_version() >= (2, 42);
    let run = bare.lfs(["pull"]).ok();
    if ls_files {
        run.lacks("Downloading LFS objects");
        bare.refute_local_object(&oid("a"));
    } else {
        run.has("Downloading LFS objects");
        bare.assert_local_object(&oid("a"), 1);
    }
    let assert_not_checked_out = || {
        assert!(!bare.path().join("a.dat").exists());
        assert!(!bare.path().join(&internal).join("a.dat").exists());
        assert!(!external.join("a.dat").exists());
    };
    assert_not_checked_out();

    bare.remove_local_objects();
    bare.refute_local_object(&oid("a"));

    // Put the pointers in the index. git ls-files doesn't read the index's
    // .gitattributes in a bare repository, so they go in info/attributes.
    let tree = bare.git(["ls-tree", "HEAD", "a.dat"]).ok().stdout();
    let blob = tree.split_whitespace().nth(2).unwrap().to_owned();
    for path in ["a.dat".to_owned(), format!("{internal}/a.dat")] {
        bare.git([
            "update-index",
            "--add",
            "--cacheinfo",
            "100644",
            &blob,
            &path,
        ])
        .ok();
    }
    if ls_files {
        let attributes = bare.git(["show", "HEAD:.gitattributes"]).ok().stdout();
        bare.write("info/attributes", attributes);
    }

    bare.lfs(["pull"]).ok().has("Downloading LFS objects");
    bare.assert_local_object(&oid("a"), 1);
    assert_not_checked_out();
}

#[test]
fn pull_with_partial_clone_and_sparse_checkout_and_index() {
    let t = Test::new();
    let name = "pull-sparse";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "a");
    repo.write("in-dir/b.dat", "b");
    repo.write("out-dir/c.dat", "c");
    repo.commit_all("add files");
    repo.git(["push", "origin", "main"]).ok();
    for contents in ["a", "b", "c"] {
        assert!(t.server().object(name, &oid(contents)).is_some());
    }

    // A partial clone with a cone-mode sparse checkout of one directory,
    // and a sparse index, without which git ls-files lists every file.
    t.command(t.dir(), "git")
        .args([
            "clone",
            "--filter=tree:0",
            "--depth=1",
            "--no-checkout",
            &t.remote_url(name),
            "pull-sparse-partial",
        ])
        .run()
        .ok();
    let partial = t.repo("pull-sparse-partial");
    partial
        .git(["sparse-checkout", "init", "--cone", "--sparse-index"])
        .ok();
    partial.git(["sparse-checkout", "set", "in-dir"]).ok();
    partial.git(["checkout", "main"]).ok();
    assert!(partial.path().join("in-dir").is_dir());
    assert!(!partial.path().join("out-dir").exists());
    partial.assert_local_object(&oid("a"), 1);
    partial.assert_local_object(&oid("b"), 1);
    partial.refute_local_object(&oid("c"));

    // With Git 2.42 or later, pull uses git ls-files, and skips files
    // outside the sparse cone. Before that, it pulls everything.
    let run = partial.lfs(["pull"]).ok();
    if t.git_version() >= (2, 42) {
        run.lacks("Downloading LFS objects");
        assert!(!partial.path().join("out-dir").exists());
        partial.refute_local_object(&oid("c"));
    } else {
        run.has("Downloading LFS objects");
        assert_eq!(partial.read("out-dir/c.dat"), "c");
        partial.assert_local_object(&oid("c"), 1);
    }
}

#[test]
fn pull_pointer_extension() {
    let t = Test::new();
    let name = "pull-pointer-extension";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.case_inverter_extension();
    let inverted = oid("ABC");
    repo.write("dir1/abc.dat", "abc");
    repo.git(["add", ".gitattributes", "dir1"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.git(["push", "origin", "main"]).ok();
    assert!(t.server().object(name, &inverted).is_some());

    let clone = t.clone_skip_smudge(name, "pull-pointer-extension-assert");
    clone.refute_local_object(&inverted);
    let log = clone.case_inverter_extension();

    remove(&clone.path().join("dir1"));
    let _ = std::fs::remove_file(&log);
    clone.lfs(["pull"]).ok();
    clone.assert_local_object(&inverted, 3);
    assert_eq!(clone.read("dir1/abc.dat"), "abc");
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains("smudge: dir1/abc.dat")
    );

    // From a subdirectory, the extension still runs at the root.
    clone.remove_local_objects();
    remove(&clone.path().join("dir1"));
    std::fs::remove_file(&log).unwrap();
    std::fs::create_dir(clone.path().join("dir2")).unwrap();
    clone.at("dir2").lfs(["pull"]).ok();
    assert_eq!(clone.read("dir1/abc.dat"), "abc");
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains("smudge: dir1/abc.dat")
    );
    clone.assert_local_object(&inverted, 3);
}
