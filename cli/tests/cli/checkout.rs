//! `t-checkout.sh`.

use std::os::unix::fs::{PermissionsExt, symlink};

use crate::conflicts::{self, CaseSymlinks, is_symlink, remove};
use crate::harness::{Repo, Run, Test, git_lfs, oid, pointer};

const NAME: &str = "t-checkout";
const CONTENTS: &str = "something something";
const FILES: [&str; 5] = [
    "file1.dat",
    "file2.dat",
    "file3.dat",
    "folder1/nested.dat",
    "folder2/folder3/folder4/nested.dat",
];

/// The first part of the shell suite's "checkout" test, which later tests
/// use: the remote `t-checkout`, and its clone `repo` with five files of
/// the same contents committed and pushed.
fn setup(t: &Test) -> Repo<'_> {
    let repo = t.remote_and_clone(NAME);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    // The same contents everywhere is one object.
    for file in FILES {
        repo.write(file, CONTENTS);
    }
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", "add files"]).ok();
    assert_files(&repo, &FILES);
    repo.assert_pointer("main", "file1.dat", &oid(CONTENTS), CONTENTS.len());
    repo.git(["push", "origin", "main"]).ok();
    repo
}

/// Check that each of `files` is checked out.
#[track_caller]
fn assert_files(repo: &Repo, files: &[&str]) {
    for file in files {
        assert_eq!(repo.read(file), CONTENTS, "{file}");
    }
}

/// Check that none of `files` exists.
#[track_caller]
fn assert_missing(repo: &Repo, files: &[&str]) {
    for file in files {
        assert!(!repo.path().join(file).exists(), "{file} exists");
    }
}

/// Remove the files from [`setup`] from the working tree.
fn remove_files(repo: &Repo) {
    for file in [
        "file1.dat",
        "file2.dat",
        "file3.dat",
        "folder1/nested.dat",
        "folder2",
    ] {
        remove(&repo.path().join(file));
    }
}

#[test]
fn checkout() {
    let t = Test::new();
    let repo = setup(&t);
    remove_files(&repo);

    // It replaces everything.
    repo.lfs_command()
        .args(["checkout"])
        .env("GIT_TRACE", "1")
        .run()
        .ok()
        .has("Checking out LFS objects: 100% (5/5), 95 B")
        .has(r#"accepting "file1.dat""#)
        .lacks(r#"rejecting "file1.dat""#);
    assert_files(&repo, &FILES);
    repo.assert_clean_status();

    // It skips files deleted in the index.
    repo.git(["rm", "file1.dat"]).ok();
    repo.lfs(["checkout"]).ok();
    assert_missing(&repo, &["file1.dat"]);
    repo.assert_clean_worktree_except(&["file1.dat"]);

    repo.git(["reset", "--hard"]).ok();
    remove_files(&repo);

    // With a path.
    repo.lfs(["checkout", "file2.dat"]).ok();
    assert_files(&repo, &["file2.dat"]);
    assert_missing(
        &repo,
        &["file1.dat", "file3.dat", "folder1/nested.dat", "folder2"],
    );
    repo.assert_clean_worktree_except(&[
        "file1.dat",
        "file3.dat",
        "folder1/nested.dat",
        "folder2/folder3/folder4/nested.dat",
    ]);

    // With a pattern.
    repo.lfs(["checkout", "file*.dat"]).ok();
    assert_files(&repo, &["file1.dat", "file3.dat"]);
    assert_missing(&repo, &["folder1/nested.dat", "folder2"]);
    repo.assert_clean_worktree_except(&[
        "folder1/nested.dat",
        "folder2/folder3/folder4/nested.dat",
    ]);

    // In a subdirectory, paths are relative to it.
    remove(&repo.path().join("file1.dat"));
    let folder1 = repo.at("folder1");
    let others_missing = || {
        assert_missing(&repo, &["file1.dat", "folder2"]);
        repo.assert_clean_worktree_except(&["file1.dat", "folder2/folder3/folder4/nested.dat"]);
    };
    folder1.lfs(["checkout", "nested.dat"]).ok();
    assert_files(&repo, &["folder1/nested.dat"]);
    others_missing();

    remove(&repo.path().join("folder1/nested.dat"));
    folder1.lfs(["checkout", "."]).ok();
    assert_files(&repo, &["folder1/nested.dat"]);
    others_missing();

    folder1.lfs(["checkout", ".."]).ok();
    assert_files(&repo, &["file1.dat", "folder2/folder3/folder4/nested.dat"]);
    repo.assert_clean_status();

    remove(&repo.path().join("folder2"));
    folder1.lfs(["checkout", "../folder2/**"]).ok();
    assert_files(&repo, &["folder2/folder3/folder4/nested.dat"]);
    repo.assert_clean_status();

    // A directory, missing, existing, and as a pattern.
    for (args, mkdir) in [
        (["checkout", "folder2"], false),
        (["checkout", "folder2"], true),
        (["checkout", "folder2/**"], false),
    ] {
        remove(&repo.path().join("folder2"));
        if mkdir {
            std::fs::create_dir(repo.path().join("folder2")).unwrap();
        }
        repo.lfs(args).ok();
        assert_files(&repo, &["folder2/folder3/folder4/nested.dat"]);
        repo.assert_clean_status();
    }

    // "." at the root.
    remove_files(&repo);
    remove(&repo.path().join("folder1"));
    repo.lfs(["checkout", "."]).ok();
    assert_files(&repo, &FILES);
    repo.assert_clean_status();

    // Into existing directories.
    remove(&repo.path().join("folder1/nested.dat"));
    remove(&repo.path().join("folder2/folder3/folder4"));
    repo.lfs(["checkout"]).ok();
    assert_files(
        &repo,
        &["folder1/nested.dat", "folder2/folder3/folder4/nested.dat"],
    );
    repo.assert_clean_status();

    // Missing objects don't make it fail, and leave pointers.
    repo.git(["push", "origin", "main"]).ok();
    repo.remove_local_objects();
    for file in ["file1.dat", "file2.dat", "file3.dat"] {
        remove(&repo.path().join(file));
    }
    repo.lfs(["checkout"]).ok();
    let pointer = pointer(&oid(CONTENTS), CONTENTS.len());
    for file in ["file1.dat", "file2.dat", "file3.dat"] {
        assert_eq!(repo.read(file), pointer, "{file}");
    }
    assert_files(&repo, &FILES[3..]);
    repo.assert_clean_worktree_except(&["file1.dat", "file2.dat", "file3.dat"]);
}

/// The repository `name` with `files` containing "a" committed.
fn conflict_setup<'a>(t: &'a Test, name: &str, files: &[&str]) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    for file in files {
        repo.write(file, "a");
    }
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo
}

/// Run `git lfs checkout` from the subdirectory `cwd`, and check it
/// succeeds.
fn checkout_in<'a>(repo: &'a Repo) -> impl Fn(&str) -> Run + 'a {
    move |cwd| repo.at(cwd).lfs(["checkout"]).ok()
}

#[test]
fn checkout_skip_directory_file_conflicts() {
    let t = Test::new();
    let files = ["dir1/a.dat", "dir2/dir3/dir4/a.dat"];
    let repo = conflict_setup(&t, "checkout-skip-dir-file-conflicts", &files);
    conflicts::directory_file(&repo, checkout_in(&repo));
}

#[test]
fn checkout_skip_directory_symlink_conflicts() {
    let t = Test::new();
    let files = ["dir1/a.dat", "dir2/dir3/dir4/a.dat"];
    let repo = conflict_setup(&t, "checkout-skip-dir-symlink-conflicts", &files);
    conflicts::directory_symlink(&t, &repo, checkout_in(&repo));
}

#[test]
fn checkout_skip_file_symlink_conflicts() {
    let t = Test::new();
    let files = ["a.dat", "dir1/dir2/dir3/a.dat"];
    let repo = conflict_setup(&t, "checkout-skip-file-symlink-conflicts", &files);
    conflicts::file_symlink(&t, &repo, checkout_in(&repo));
}

#[test]
fn checkout_skip_case_based_symlink_conflicts() {
    let t = Test::new();
    conflicts::case_symlinks(
        &t,
        CaseSymlinks {
            name: "checkout-skip-case-symlink-conflicts",
            prepare: &|clone| {
                clone.lfs(["fetch", "origin", "main"]).ok();
                clone.assert_local_object(&oid("a"), 1);
            },
            run: &|clone| clone.lfs(["checkout"]).ok(),
            progress: Some("Checking out LFS objects: 100% (4/4), 4 B"),
        },
    );
}

#[test]
fn checkout_skip_changed_files() {
    let t = Test::new();
    let repo = conflict_setup(&t, "checkout-skip-changed-files", &["a.dat"]);
    conflicts::changed_files(&repo, checkout_in(&repo));
}

#[test]
fn checkout_break_hard_links_to_existing_files() {
    let t = Test::new();
    let name = "checkout-break-file-hardlinks";
    let repo = conflict_setup(&t, name, &["a.dat", "dir1/dir2/dir3/a.dat"]);
    repo.git(["push", "origin", "main"]).ok();
    assert!(t.server().object(name, &oid("a")).is_some());

    let clone = t.clone_skip_smudge(name, &format!("{name}-assert"));
    clone.lfs(["fetch", "origin", "main"]).ok();
    clone.assert_local_object(&oid("a"), 1);
    conflicts::hard_links(&t, &clone, checkout_in(&clone));
}

#[test]
fn checkout_without_clean_filter() {
    let t = Test::new();
    setup(&t);
    t.command(t.dir(), git_lfs()).args(["uninstall"]).run().ok();

    t.command(t.dir(), "git")
        .args(["clone", &t.remote_url(NAME), "checkout-without-clean"])
        .run()
        .ok();
    let repo = t.repo("checkout-without-clean");
    repo.lfs(["uninstall"]).ok();
    repo.git(["config", "--list"])
        .ok()
        .lacks("filter.lfs.clean");

    repo.lfs(["checkout"]).ok().has("Git LFS is not installed");
    let pointer = pointer(&oid(CONTENTS), CONTENTS.len());
    for file in FILES {
        assert_eq!(repo.read(file), pointer, "{file}");
    }
}

#[test]
fn checkout_outside_git_repository() {
    let t = Test::new();
    let outside = t.repo("outside");
    std::fs::create_dir(outside.path()).unwrap();
    let run = outside.lfs(["checkout"]);
    assert_eq!(run.code(), Some(128), "{run}");
    run.has("Not in a Git repository");
}

/// A new repository in `dir`, with `*.bin` tracked.
fn local_repo<'a>(t: &'a Test, dir: &str) -> Repo<'a> {
    t.command(t.dir(), "git").args(["init", dir]).run().ok();
    let repo = t.repo(dir);
    repo.lfs(["track", "*.bin"]).ok();
    repo
}

#[test]
fn checkout_read_only_directory() {
    let t = Test::new();
    if t.is_root() {
        return;
    }
    let repo = local_repo(&t, "checkout-read-only");
    repo.write("dir/a.bin", "a");
    repo.git(["add", ".gitattributes", "dir/a.bin"]).ok();
    repo.git(["commit", "-m", "add dir/a.bin"]).ok();

    remove(&repo.path().join("dir/a.bin"));
    let dir = repo.path().join("dir");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    // It reports an error, but exits 0.
    let run = repo.lfs(["checkout"]).ok();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    repo.assert_local_object(&oid("a"), 1);
    assert!(!repo.path().join("dir/a.bin").exists());
    run.has(r#"could not check out "dir/a.bin""#)
        .has("could not create working directory file")
        .has("permission denied");
}

#[test]
fn checkout_read_only_file() {
    let t = Test::new();
    let name = "checkout-locked";
    let file = "a.txt";
    t.remote_with_file(name, file);

    let clone = t.clone_skip_smudge(name, "checkout-locked-assert");
    let path = clone.path().join(file);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    let writable = || std::fs::metadata(&path).unwrap().permissions().mode() & 0o200 != 0;

    assert!(!writable());
    clone.assert_pointer("refs/heads/main", file, &oid(format!("{file}\n")), 6);
    clone.lfs(["fetch"]).ok();
    clone.lfs(["checkout", file]).ok();
    assert!(!writable());
    assert_eq!(clone.read(file), format!("{file}\n"));
}

#[test]
fn checkout_with_empty_file_doesnt_modify_mtime() {
    let t = Test::new();
    let repo = local_repo(&t, "checkout-empty-file");
    repo.commit_all("gitattributes");
    repo.write("abc.bin", "abc");
    repo.commit_all("abc");

    let path = repo.path().join("foo.bin");
    repo.write("foo.bin", "");
    let mtime = || std::fs::metadata(&path).unwrap().modified().unwrap();
    let before = mtime();
    repo.commit_all("foo");

    repo.lfs(["checkout"]).ok();
    assert_eq!(mtime(), before);
}

#[test]
fn checkout_conflicts() {
    let t = Test::new();
    let name = "checkout-conflicts";
    t.remote_with_file(name, "file1.dat");
    let repo = t.clone(name, &format!("{name}_checkout"));
    let read = |path: &std::path::Path| std::fs::read_to_string(path).unwrap();

    repo.git(["tag", "base"]).ok();
    repo.git(["checkout", "-b", "first"]).ok();
    repo.write("file1.dat", "abc123\n");
    repo.git(["add", "-u"]).ok();
    repo.write("other.txt", "first\n");
    repo.git(["add", "other.txt"]).ok();
    repo.git(["commit", "-m", "first"]).ok();

    let together = "--to and exactly one of --theirs, --ours, and --base must be used together";
    let one_path = "--to requires exactly one Git LFS object file path";
    for (args, message) in [
        (&["--to", "base.txt"][..], together),
        (&["--base"], together),
        (
            &["--to", "base.txt", "--ours", "--theirs"],
            "at most one of --base, --theirs, and --ours is allowed",
        ),
        (&["--to", "base.txt", "--base"], one_path),
        (&["--to", "base.txt", "--base", "abc", "def"], one_path),
        (
            &["--to", "base.txt", "--base", "file1.dat"],
            "not in the middle of a merge",
        ),
    ] {
        repo.lfs_command()
            .args(["checkout"])
            .args(args)
            .run()
            .has(message);
    }

    repo.git(["checkout", "-b", "second", "main"]).ok();
    repo.write("file1.dat", "def456\n");
    repo.git(["add", "-u"]).ok();
    repo.write("other.txt", "second\n");
    repo.git(["add", "other.txt"]).ok();
    repo.git(["commit", "-m", "second"]).ok();

    // The merge conflicts.
    repo.git(["merge", "first"]).fails();

    let assert_dir = t.dir().join(format!("{name}-assert"));
    let theirs = assert_dir.join("dir1/dir2/theirs.txt");
    let theirs_str = theirs.to_str().unwrap();
    let base = repo.path().join("base.txt");
    let ours = t.dir().join("ours.txt");

    repo.lfs(["checkout", "--to", "base.txt", "--base", "file1.dat"])
        .ok();
    repo.lfs(["checkout", "--to", "../ours.txt", "--ours", "file1.dat"])
        .ok();
    repo.lfs(["checkout", "--to", theirs_str, "--theirs", "file1.dat"])
        .ok();
    assert_eq!(read(&base), "file1.dat\n");
    assert_eq!(read(&ours), "def456\n");
    assert_eq!(read(&theirs), "abc123\n");

    // From a subdirectory, paths are relative to it.
    remove(&base);
    remove(&ours);
    remove(&assert_dir);
    std::fs::create_dir_all(repo.path().join("dir1/dir2")).unwrap();
    let sub = repo.at("dir1/dir2");
    sub.lfs(["checkout", "--to", "base.txt", "--base", "../../file1.dat"])
        .ok();
    sub.lfs([
        "checkout",
        "--to",
        "../../../ours.txt",
        "--ours",
        "../../file1.dat",
    ])
    .ok();
    sub.lfs([
        "checkout",
        "--to",
        theirs_str,
        "--theirs",
        "../../file1.dat",
    ])
    .ok();
    assert_eq!(repo.read("dir1/dir2/base.txt"), "file1.dat\n");
    assert_eq!(read(&ours), "def456\n");
    assert_eq!(read(&theirs), "abc123\n");

    // A symlink in the --to path's directories is followed.
    remove(&assert_dir);
    std::fs::create_dir_all(assert_dir.join("link1")).unwrap();
    symlink("link1", assert_dir.join("dir1")).unwrap();
    repo.lfs(["checkout", "--to", theirs_str, "--theirs", "file1.dat"])
        .ok();
    assert!(is_symlink(&assert_dir.join("dir1")));
    assert_eq!(read(&assert_dir.join("link1/dir2/theirs.txt")), "abc123\n");

    // A symlink at the --to path is replaced, not written through.
    let link1 = repo.path().join("link1");
    let link2 = t.dir().join("link2");
    for path in [&base, &link1, &ours, &link2] {
        remove(path);
    }
    symlink("link1", &base).unwrap();
    symlink("link2", &ours).unwrap();
    repo.lfs(["checkout", "--to", "base.txt", "--base", "file1.dat"])
        .ok();
    repo.lfs(["checkout", "--to", "../ours.txt", "--ours", "file1.dat"])
        .ok();
    assert!(!is_symlink(&base));
    assert!(!is_symlink(&ours));
    assert!(!link1.exists());
    assert!(!link2.exists());
    assert_eq!(read(&base), "file1.dat\n");
    assert_eq!(read(&ours), "def456\n");

    // A hard link at the --to path is broken, not written through.
    for path in [&base, &link1, &ours, &link2] {
        remove(path);
    }
    std::fs::write(&link1, "link1").unwrap();
    std::fs::write(&link2, "link2").unwrap();
    std::fs::hard_link(&link1, &base).unwrap();
    std::fs::hard_link(&link2, &ours).unwrap();
    repo.lfs(["checkout", "--to", "base.txt", "--base", "file1.dat"])
        .ok();
    repo.lfs(["checkout", "--to", "../ours.txt", "--ours", "file1.dat"])
        .ok();
    assert_eq!(read(&link1), "link1");
    assert_eq!(read(&link2), "link2");
    assert_eq!(read(&base), "file1.dat\n");
    assert_eq!(read(&ours), "def456\n");

    repo.lfs(["checkout", "--to", "base.txt", "--ours", "other.txt"])
        .has("Could not find decoder pointer for object");
    repo.lfs(["checkout", "--to", "base.txt", "--ours", "."])
        .has("Could not checkout")
        .has(r#"Git can't resolve ref: ":2:.""#);
}

#[test]
fn checkout_git_work_tree() {
    let t = Test::new();
    setup(&t);
    let work_tree = "checkout-work-tree";
    let git_dir = "checkout-work-tree-git";
    std::fs::create_dir(t.dir().join(work_tree)).unwrap();
    std::fs::create_dir(t.dir().join(git_dir)).unwrap();
    let run = |program: std::path::PathBuf, args: &[&str]| {
        t.command(t.dir(), program)
            .args(args)
            .env("GIT_WORK_TREE", work_tree)
            .env("GIT_DIR", git_dir)
            .run()
            .ok();
    };

    run("git".into(), &["init"]);
    run(
        "git".into(),
        &["remote", "add", "origin", &t.remote_url(NAME)],
    );
    run(git_lfs(), &["uninstall", "--skip-repo"]);
    run("git".into(), &["fetch", "origin"]);
    run("git".into(), &["checkout", "-B", "main", "origin/main"]);
    run(git_lfs(), &["install"]);
    run(git_lfs(), &["fetch"]);
    run(git_lfs(), &["checkout"]);

    let file = t.dir().join(work_tree).join("file1.dat");
    assert_eq!(std::fs::read_to_string(file).unwrap(), CONTENTS);
}

#[test]
fn checkout_bare_repository() {
    let t = Test::new();
    t.command(t.dir(), "git")
        .args(["init", "--bare", "checkout-bare"])
        .run()
        .ok();
    let run = t.repo("checkout-bare").lfs(["checkout"]).ok();
    assert_eq!(
        run.output().trim(),
        "This operation must be run in a work tree."
    );
}

#[test]
fn checkout_sparse_with_partial_clone_and_sparse_index() {
    let t = Test::new();
    let name = "checkout-sparse";
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
            "checkout-sparse-partial",
        ])
        .run()
        .ok();
    let partial = t.repo("checkout-sparse-partial");
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

    // With Git 2.42 or later, checkout uses git ls-files, and skips files
    // outside the sparse cone. Before that, it checks out everything.
    let ls_files = t.git_version() >= (2, 42);
    let skipped = r#"Skipped checkout for "out-dir/c.dat""#;
    let c = partial.path().join("out-dir/c.dat");
    let run = partial.lfs(["checkout"]).ok();
    if ls_files {
        run.lacks(skipped);
        assert!(!c.exists());
    } else {
        run.has(skipped);
        assert_eq!(partial.read("out-dir/c.dat"), pointer(&oid("c"), 1));
    }

    // Fetch everything, including objects outside the cone.
    partial.lfs(["fetch", "origin", "main"]).ok();
    partial.assert_local_object(&oid("c"), 1);

    let progress = "Checking out LFS objects: 100% (3/3), 3 B";
    let run = partial.lfs(["checkout"]).ok();
    if ls_files {
        run.lacks(progress);
        assert!(!c.exists());
    } else {
        run.has(progress);
        assert_eq!(partial.read("out-dir/c.dat"), "c");
    }
}

/// The repository `name` with `*.dat` tracked, the case-inverting
/// extension, and `dir1/abc.dat` committed. Returns the extension's log.
fn extension_setup<'a>(t: &'a Test, name: &str) -> (Repo<'a>, std::path::PathBuf) {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    let log = repo.case_inverter_extension();
    repo.write("dir1/abc.dat", "abc");
    repo.git(["add", ".gitattributes", "dir1"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.assert_local_object(&oid("ABC"), 3);
    (repo, log)
}

/// Check the extension smudged `dir1/abc.dat`, then clear its log.
#[track_caller]
fn assert_smudged(log: &std::path::Path) {
    let text = std::fs::read_to_string(log).unwrap();
    assert!(text.contains("smudge: dir1/abc.dat"), "{text}");
    std::fs::remove_file(log).unwrap();
}

#[test]
fn checkout_pointer_extension() {
    let t = Test::new();
    let (repo, log) = extension_setup(&t, "checkout-pointer-extension");

    remove(&repo.path().join("dir1"));
    remove(&log);
    repo.lfs(["checkout"]).ok();
    assert_eq!(repo.read("dir1/abc.dat"), "abc");
    assert_smudged(&log);

    // From a subdirectory, the extension still runs at the root.
    remove(&repo.path().join("dir1"));
    std::fs::create_dir(repo.path().join("dir2")).unwrap();
    repo.at("dir2").lfs(["checkout"]).ok();
    assert_eq!(repo.read("dir1/abc.dat"), "abc");
    assert_smudged(&log);
}

#[test]
fn checkout_pointer_extension_with_conflict() {
    let t = Test::new();
    let name = "checkout-pointer-extension-conflict";
    let (repo, log) = extension_setup(&t, name);

    repo.git(["checkout", "-b", "theirs"]).ok();
    repo.write("dir1/abc.dat", "Abc");
    repo.git(["add", "dir1"]).ok();
    repo.git(["commit", "-m", "theirs"]).ok();

    repo.git(["checkout", "main"]).ok();
    repo.write("dir1/abc.dat", "aBc");
    repo.git(["add", "dir1"]).ok();
    repo.git(["commit", "-m", "ours"]).ok();

    repo.git(["merge", "theirs"]).fails();
    remove(&log);

    repo.lfs(["checkout", "--to", "base.txt", "--base", "dir1/abc.dat"])
        .ok();
    assert_eq!(repo.read("base.txt"), "abc");
    assert_smudged(&log);

    repo.at("dir1")
        .lfs(["checkout", "--to", "../ours.txt", "--ours", "abc.dat"])
        .ok();
    assert_eq!(repo.read("ours.txt"), "aBc");
    assert_smudged(&log);

    let theirs = t.dir().join(format!("{name}-assert/dir1/dir2/theirs.txt"));
    std::fs::create_dir(repo.path().join("dir2")).unwrap();
    repo.at("dir2")
        .lfs([
            "checkout",
            "--to",
            theirs.to_str().unwrap(),
            "--theirs",
            "../dir1/abc.dat",
        ])
        .ok();
    assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "Abc");
    assert_smudged(&log);
}
