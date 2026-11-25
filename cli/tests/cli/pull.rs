//! `t-pull.sh`.

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

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
/// `cwd`, and check it succeeds, prints each of `messages`, and fetches
/// the object.
#[track_caller]
fn pull_with_conflicts(repo: &Repo, cwd: &str, messages: &[&str]) -> Run {
    repo.remove_local_objects();
    let mut run = repo.at(cwd).lfs(["pull"]).ok();
    for message in messages {
        run = run.has(message);
    }
    repo.assert_local_object(&oid("a"), 1);
    repo.assert_clean_index();
    run
}

/// Replace `path`, a file, directory, or symlink, with nothing.
fn remove(path: &Path) {
    if path.is_dir() && !path.is_symlink() {
        std::fs::remove_dir_all(path).unwrap();
    } else if path.symlink_metadata().is_ok() {
        std::fs::remove_file(path).unwrap();
    }
}

fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
}

const NOT_A_DIRECTORY: [&str; 2] = [
    r#""dir1/a.dat": not a directory"#,
    r#""dir2/dir3/dir4/a.dat": not a directory"#,
];

#[test]
fn pull_skip_directory_file_conflicts() {
    let t = Test::new();
    let files = ["dir1/a.dat", "dir2/dir3/dir4/a.dat"];
    let repo = conflict_setup(&t, "pull-skip-dir-file-conflicts", &files);

    remove(&repo.path().join("dir1"));
    remove(&repo.path().join("dir2/dir3"));
    repo.write("dir1", "");
    repo.write("dir2/dir3", "");

    for cwd in ["", "dir2"] {
        pull_with_conflicts(&repo, cwd, &NOT_A_DIRECTORY);
        assert!(repo.path().join("dir1").is_file());
        assert!(repo.path().join("dir2/dir3").is_file());
    }
}

#[test]
fn pull_skip_directory_symlink_conflicts() {
    let t = Test::new();
    let files = ["dir1/a.dat", "dir2/dir3/dir4/a.dat"];
    let repo = conflict_setup(&t, "pull-skip-dir-symlink-conflicts", &files);
    let root = repo.path();
    let outside = t.dir();
    let not_beyond = |run: Run| {
        assert_eq!(run.count("is beyond a symbolic link"), 0, "{run}");
    };
    let relink = |link1: &str, link2: &str| {
        remove(&root.join("dir1"));
        remove(&root.join("dir2/dir3"));
        symlink(link1, root.join("dir1")).unwrap();
        symlink(link2, root.join("dir2/dir3")).unwrap();
    };

    // Symlinks to directories outside the working tree.
    std::fs::create_dir(outside.join("link1")).unwrap();
    std::fs::create_dir(outside.join("link2")).unwrap();
    relink("../link1", "../../link2");
    not_beyond(pull_with_conflicts(&repo, "", &NOT_A_DIRECTORY));
    assert!(is_symlink(&root.join("dir1")));
    assert!(is_symlink(&root.join("dir2/dir3")));
    assert!(!outside.join("link1/a.dat").exists());
    assert!(!outside.join("link2/dir4").exists());

    // Symlinks to directories inside it.
    std::fs::create_dir(root.join("link1")).unwrap();
    std::fs::create_dir(root.join("link2")).unwrap();
    relink("link1", "../link2");
    for cwd in ["", "dir2"] {
        not_beyond(pull_with_conflicts(&repo, cwd, &NOT_A_DIRECTORY));
        assert!(is_symlink(&root.join("dir1")));
        assert!(is_symlink(&root.join("dir2/dir3")));
        assert!(!root.join("link1/a.dat").exists());
        assert!(!root.join("link2/dir4").exists());
    }

    // A symlink to a file outside, and a dangling one.
    remove(&outside.join("link1"));
    remove(&outside.join("link2"));
    std::fs::write(outside.join("link1"), "").unwrap();
    relink("../link1", "../../link2");
    pull_with_conflicts(&repo, "", &NOT_A_DIRECTORY);
    assert!(is_symlink(&root.join("dir1")));
    assert!(is_symlink(&root.join("dir2/dir3")));
    assert!(outside.join("link1").is_file());
    assert!(!outside.join("link2").exists());

    // The same, inside.
    remove(&root.join("link1"));
    remove(&root.join("link2"));
    std::fs::write(root.join("link1"), "").unwrap();
    relink("link1", "../link2");
    for cwd in ["", "dir2"] {
        pull_with_conflicts(&repo, cwd, &NOT_A_DIRECTORY);
        assert!(is_symlink(&root.join("dir1")));
        assert!(is_symlink(&root.join("dir2/dir3")));
        assert!(root.join("link1").is_file());
        assert!(!root.join("link2").exists());
    }
}

const NOT_A_REGULAR_FILE: [&str; 2] = [
    r#""a.dat": not a regular file"#,
    r#""dir1/dir2/dir3/a.dat": not a regular file"#,
];

#[test]
fn pull_skip_file_symlink_conflicts() {
    let t = Test::new();
    let files = ["a.dat", "dir1/dir2/dir3/a.dat"];
    let repo = conflict_setup(&t, "pull-skip-file-symlink-conflicts", &files);
    let root = repo.path();
    let outside = t.dir();
    let pointer = repo.git(["cat-file", "-p", ":a.dat"]).ok().stdout();
    let relink = |link1: &str, link2: &str| {
        remove(&root.join("a.dat"));
        remove(&root.join("dir1/dir2/dir3/a.dat"));
        symlink(link1, root.join("a.dat")).unwrap();
        symlink(link2, root.join("dir1/dir2/dir3/a.dat")).unwrap();
    };
    let assert_links = || {
        assert!(is_symlink(&root.join("a.dat")));
        assert!(is_symlink(&root.join("dir1/dir2/dir3/a.dat")));
    };

    // Symlinks to pointer files outside the working tree.
    std::fs::write(outside.join("link1"), &pointer).unwrap();
    std::fs::write(outside.join("link2"), &pointer).unwrap();
    relink("../link1", "../../../../link2");
    pull_with_conflicts(&repo, "", &NOT_A_REGULAR_FILE);
    assert_links();
    assert_eq!(
        std::fs::read_to_string(outside.join("link1")).unwrap(),
        pointer
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("link2")).unwrap(),
        pointer
    );

    // Symlinks to pointer files inside it.
    std::fs::write(root.join("link1"), &pointer).unwrap();
    std::fs::write(root.join("link2"), &pointer).unwrap();
    relink("link1", "../../../link2");
    for cwd in ["", "dir1/dir2"] {
        pull_with_conflicts(&repo, cwd, &NOT_A_REGULAR_FILE);
        assert_links();
        assert_eq!(repo.read("link1"), pointer);
        assert_eq!(repo.read("link2"), pointer);
    }

    // A symlink to a directory outside, and a dangling one.
    remove(&outside.join("link1"));
    remove(&outside.join("link2"));
    std::fs::create_dir(outside.join("link1")).unwrap();
    relink("../link1", "../../../../link2");
    pull_with_conflicts(&repo, "", &NOT_A_REGULAR_FILE);
    assert_links();
    assert!(outside.join("link1").is_dir());
    assert!(!outside.join("link2").exists());

    // The same, inside.
    remove(&root.join("link1"));
    remove(&root.join("link2"));
    std::fs::create_dir(root.join("link1")).unwrap();
    relink("link1", "../../../link2");
    for cwd in ["", "dir1/dir2"] {
        pull_with_conflicts(&repo, cwd, &NOT_A_REGULAR_FILE);
        assert_links();
        assert!(root.join("link1").is_dir());
        assert!(!root.join("link2").exists());
    }
}

/// On a case-insensitive file system, files whose names differ only in case
/// from symlinks must not be written through them. On a case-sensitive one,
/// they're all just checked out.
#[test]
fn pull_skip_case_based_symlink_conflicts() {
    let t = Test::new();
    let name = "pull-skip-case-symlink-conflicts";
    let repo = t.remote_and_clone(name);
    let root = repo.path();
    std::fs::create_dir(root.join("dir1")).unwrap();
    symlink("../link1", root.join("A.dat")).unwrap();
    symlink("../../link2", root.join("dir1/a.dat")).unwrap();
    symlink("../link3", root.join("DIR3")).unwrap();
    symlink("../../link4", root.join("dir1/dir2")).unwrap();
    repo.git(["add", "A.dat", "dir1", "DIR3"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    for link in ["A.dat", "dir1/a.dat", "DIR3", "dir1/dir2"] {
        remove(&root.join(link));
    }

    repo.write(
        ".gitattributes",
        "*.dat filter=lfs diff=lfs merge=lfs -text\n",
    );
    let files = ["a.dat", "dir1/A.dat", "dir3/a.dat", "dir1/DIR2/a.dat"];
    for file in files {
        repo.write(file, "a");
    }
    let mut add = vec!["-c", "core.ignoreCase=false", "add", ".gitattributes"];
    add.extend(files);
    repo.git(add).ok();
    repo.git(["commit", "-m", "case-conflicting commit"]).ok();
    repo.git(["push", "origin", "main"]).ok();
    assert!(t.server().object(name, &oid("a")).is_some());

    let clone_run = t
        .command(t.dir(), "git")
        .args(["clone", &t.remote_url(name), &format!("{name}-assert")])
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .run()
        .ok();
    let case_insensitive = clone_run.count("collided") > 0;
    let clone = t.repo(&format!("{name}-assert"));
    let root = clone.path();
    let outside = t.dir();
    clone.refute_local_object(&oid("a"));

    for path in ["a.dat", "A.dat", "dir1", "dir3", "DIR3"] {
        remove(&root.join(path));
    }
    std::fs::create_dir(outside.join("link3")).unwrap();
    std::fs::create_dir(outside.join("link4")).unwrap();

    clone.lfs(["pull"]).ok();
    clone.assert_local_object(&oid("a"), 1);
    for file in files {
        assert_eq!(clone.read(file), "a", "{file}");
    }
    let assert_outside_untouched = || {
        assert!(!outside.join("link1").exists());
        assert!(!outside.join("link2").exists());
        assert!(!outside.join("link3/a.dat").exists());
        assert!(!outside.join("link4/a.dat").exists());
    };
    assert_outside_untouched();
    clone.assert_clean_index();

    for path in ["a.dat", "dir1/A.dat", "dir3", "dir1/DIR2"] {
        remove(&root.join(path));
    }
    clone
        .git(["checkout", "--", "A.dat", "dir1/a.dat", "DIR3", "dir1/dir2"])
        .ok();

    let run = clone.lfs(["pull"]).ok();
    if case_insensitive {
        run.has(r#""a.dat": not a regular file"#)
            .has(r#""dir1/A.dat": not a regular file"#)
            .has(r#""dir3/a.dat": not a directory"#)
            .has(r#""dir1/DIR2/a.dat": not a directory"#)
            .lacks("is beyond a symbolic link");
        for path in ["a.dat", "dir1/A.dat", "dir3", "dir1/DIR2"] {
            assert!(is_symlink(&root.join(path)), "{path}");
        }
    } else {
        for file in files {
            assert_eq!(clone.read(file), "a", "{file}");
        }
    }
    assert_outside_untouched();
    clone.assert_clean_index();
}

#[test]
fn pull_skip_changed_files() {
    let t = Test::new();
    let repo = conflict_setup(&t, "pull-skip-changed-files", &["a.dat"]);

    repo.write("a.dat", "a +extra");
    pull_with_conflicts(&repo, "", &[]);
    assert_eq!(repo.read("a.dat"), "a +extra");

    remove(&repo.path().join("a.dat"));
    std::fs::create_dir(repo.path().join("a.dat")).unwrap();
    for cwd in ["", "a.dat"] {
        pull_with_conflicts(&repo, cwd, &[]);
        assert!(repo.path().join("a.dat").is_dir());
    }
}

#[test]
fn pull_break_hard_links_to_existing_files() {
    let t = Test::new();
    let files = ["a.dat", "dir1/dir2/dir3/a.dat"];
    let repo = conflict_setup(&t, "pull-break-file-hardlinks", &files);
    let link = t.dir().join("link");
    let pointer = repo.git(["cat-file", "-p", ":a.dat"]).ok().stdout();
    std::fs::write(&link, &pointer).unwrap();

    for cwd in ["", "dir1/dir2"] {
        for file in files {
            remove(&repo.path().join(file));
            std::fs::hard_link(&link, repo.path().join(file)).unwrap();
        }
        repo.remove_local_objects();
        repo.at(cwd).lfs(["pull"]).ok();
        repo.assert_local_object(&oid("a"), 1);
        for file in files {
            assert_eq!(repo.read(file), "a", "{file}");
        }
        assert_eq!(std::fs::read_to_string(&link).unwrap(), pointer);
        repo.assert_clean_status();
    }
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
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", file]).ok();
    repo.write(file, format!("{file}\n"));
    repo.git(["add", ".gitattributes", file]).ok();
    repo.git(["commit", "-m", &format!("add {file}")])
        .ok()
        .has("main (root-commit)");
    repo.git(["push", "origin", "main"])
        .ok()
        .has("main -> main");
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
