//! Conflicts between the files `git lfs pull` and `git lfs checkout` write
//! and what's already in the working tree, which `t-pull.sh` and
//! `t-checkout.sh` both test.
//!
//! Each scenario takes `run`, which runs the command from a subdirectory
//! (`""` for the root), checks it succeeded, and returns its output.

use std::os::unix::fs::symlink;
use std::path::Path;

use crate::harness::{Repo, Run, Test, oid};

pub const NOT_A_DIRECTORY: [&str; 2] = [
    r#""dir1/a.dat": not a directory"#,
    r#""dir2/dir3/dir4/a.dat": not a directory"#,
];

pub const NOT_A_REGULAR_FILE: [&str; 2] = [
    r#""a.dat": not a regular file"#,
    r#""dir1/dir2/dir3/a.dat": not a regular file"#,
];

/// Remove `path`, whether it's a file, directory, or symlink.
pub fn remove(path: &Path) {
    if path.is_dir() && !path.is_symlink() {
        std::fs::remove_dir_all(path).unwrap();
    } else if path.symlink_metadata().is_ok() {
        std::fs::remove_file(path).unwrap();
    }
}

pub fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
}

/// Run `run` from `cwd`, check it prints each of `messages`, and check the
/// index is clean.
#[track_caller]
fn run_with_messages(repo: &Repo, run: &impl Fn(&str) -> Run, cwd: &str, messages: &[&str]) -> Run {
    let mut output = run(cwd);
    for message in messages {
        output = output.has(message);
    }
    repo.assert_clean_index();
    output
}

/// `dir1/a.dat` and `dir2/dir3/dir4/a.dat`, with files where `dir1` and
/// `dir2/dir3` should be.
pub fn directory_file(repo: &Repo, run: impl Fn(&str) -> Run) {
    remove(&repo.path().join("dir1"));
    remove(&repo.path().join("dir2/dir3"));
    repo.write("dir1", "");
    repo.write("dir2/dir3", "");

    for cwd in ["", "dir2"] {
        run_with_messages(repo, &run, cwd, &NOT_A_DIRECTORY);
        assert!(repo.path().join("dir1").is_file());
        assert!(repo.path().join("dir2/dir3").is_file());
    }
}

/// `dir1/a.dat` and `dir2/dir3/dir4/a.dat`, with symlinks where `dir1` and
/// `dir2/dir3` should be.
pub fn directory_symlink(t: &Test, repo: &Repo, run: impl Fn(&str) -> Run) {
    let root = repo.path();
    let outside = t.dir();
    let not_beyond = |output: Run| {
        assert_eq!(output.count("is beyond a symbolic link"), 0, "{output}");
    };
    let relink = |link1: &str, link2: &str| {
        remove(&root.join("dir1"));
        remove(&root.join("dir2/dir3"));
        symlink(link1, root.join("dir1")).unwrap();
        symlink(link2, root.join("dir2/dir3")).unwrap();
    };
    let assert_links = || {
        assert!(is_symlink(&root.join("dir1")));
        assert!(is_symlink(&root.join("dir2/dir3")));
    };

    // Symlinks to directories outside the working tree.
    std::fs::create_dir(outside.join("link1")).unwrap();
    std::fs::create_dir(outside.join("link2")).unwrap();
    relink("../link1", "../../link2");
    not_beyond(run_with_messages(repo, &run, "", &NOT_A_DIRECTORY));
    assert_links();
    assert!(!outside.join("link1/a.dat").exists());
    assert!(!outside.join("link2/dir4").exists());

    // Symlinks to directories inside it.
    std::fs::create_dir(root.join("link1")).unwrap();
    std::fs::create_dir(root.join("link2")).unwrap();
    relink("link1", "../link2");
    for cwd in ["", "dir2"] {
        not_beyond(run_with_messages(repo, &run, cwd, &NOT_A_DIRECTORY));
        assert_links();
        assert!(!root.join("link1/a.dat").exists());
        assert!(!root.join("link2/dir4").exists());
    }

    // A symlink to a file outside, and a dangling one.
    remove(&outside.join("link1"));
    remove(&outside.join("link2"));
    std::fs::write(outside.join("link1"), "").unwrap();
    relink("../link1", "../../link2");
    run_with_messages(repo, &run, "", &NOT_A_DIRECTORY);
    assert_links();
    assert!(outside.join("link1").is_file());
    assert!(!outside.join("link2").exists());

    // The same, inside.
    remove(&root.join("link1"));
    remove(&root.join("link2"));
    std::fs::write(root.join("link1"), "").unwrap();
    relink("link1", "../link2");
    for cwd in ["", "dir2"] {
        run_with_messages(repo, &run, cwd, &NOT_A_DIRECTORY);
        assert_links();
        assert!(root.join("link1").is_file());
        assert!(!root.join("link2").exists());
    }
}

/// `a.dat` and `dir1/dir2/dir3/a.dat`, with symlinks in their place.
pub fn file_symlink(t: &Test, repo: &Repo, run: impl Fn(&str) -> Run) {
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
    let read = |path: &Path| std::fs::read_to_string(path).unwrap();

    // Symlinks to pointer files outside the working tree.
    std::fs::write(outside.join("link1"), &pointer).unwrap();
    std::fs::write(outside.join("link2"), &pointer).unwrap();
    relink("../link1", "../../../../link2");
    run_with_messages(repo, &run, "", &NOT_A_REGULAR_FILE);
    assert_links();
    assert_eq!(read(&outside.join("link1")), pointer);
    assert_eq!(read(&outside.join("link2")), pointer);

    // Symlinks to pointer files inside it.
    std::fs::write(root.join("link1"), &pointer).unwrap();
    std::fs::write(root.join("link2"), &pointer).unwrap();
    relink("link1", "../../../link2");
    for cwd in ["", "dir1/dir2"] {
        run_with_messages(repo, &run, cwd, &NOT_A_REGULAR_FILE);
        assert_links();
        assert_eq!(read(&root.join("link1")), pointer);
        assert_eq!(read(&root.join("link2")), pointer);
    }

    // A symlink to a directory outside, and a dangling one.
    remove(&outside.join("link1"));
    remove(&outside.join("link2"));
    std::fs::create_dir(outside.join("link1")).unwrap();
    relink("../link1", "../../../../link2");
    run_with_messages(repo, &run, "", &NOT_A_REGULAR_FILE);
    assert_links();
    assert!(outside.join("link1").is_dir());
    assert!(!outside.join("link2").exists());

    // The same, inside.
    remove(&root.join("link1"));
    remove(&root.join("link2"));
    std::fs::create_dir(root.join("link1")).unwrap();
    relink("link1", "../../../link2");
    for cwd in ["", "dir1/dir2"] {
        run_with_messages(repo, &run, cwd, &NOT_A_REGULAR_FILE);
        assert_links();
        assert!(root.join("link1").is_dir());
        assert!(!root.join("link2").exists());
    }
}

/// How a suite runs [`case_symlinks`].
pub struct CaseSymlinks<'a> {
    /// The remote repository's name.
    pub name: &'a str,
    /// What to do in the skip-smudge clone before the first run, such as
    /// fetching the object.
    pub prepare: &'a dyn Fn(&Repo),
    /// Run the command from the clone's root.
    pub run: &'a dyn Fn(&Repo) -> Run,
    /// The progress line a run that writes all four files prints, if the
    /// suite checks it.
    pub progress: Option<&'a str>,
}

/// Files whose names differ only in case from symlinks in another commit.
/// On a case-insensitive file system, they must not be written through the
/// symlinks. On a case-sensitive one, they're all just written.
pub fn case_symlinks(t: &Test, how: CaseSymlinks) {
    let repo = t.remote_and_clone(how.name);
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
    assert!(t.server().object(how.name, &oid("a")).is_some());

    let dir = format!("{}-assert", how.name);
    let clone_run = t
        .command(t.dir(), "git")
        .args(["clone", &t.remote_url(how.name), &dir])
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .run()
        .ok();
    let case_insensitive = clone_run.count("collided") > 0;
    let clone = t.repo(&dir);
    let root = clone.path();
    let outside = t.dir();
    (how.prepare)(&clone);

    for path in ["a.dat", "A.dat", "dir1", "dir3", "DIR3"] {
        remove(&root.join(path));
    }
    std::fs::create_dir(outside.join("link3")).unwrap();
    std::fs::create_dir(outside.join("link4")).unwrap();
    let assert_outside_untouched = || {
        assert!(!outside.join("link1").exists());
        assert!(!outside.join("link2").exists());
        assert!(!outside.join("link3/a.dat").exists());
        assert!(!outside.join("link4/a.dat").exists());
    };

    let output = (how.run)(&clone);
    if let Some(progress) = how.progress {
        output.has(progress);
    }
    clone.assert_local_object(&oid("a"), 1);
    for file in files {
        assert_eq!(clone.read(file), "a", "{file}");
    }
    assert_outside_untouched();
    clone.assert_clean_index();

    for path in ["a.dat", "dir1/A.dat", "dir3", "dir1/DIR2"] {
        remove(&root.join(path));
    }
    clone
        .git(["checkout", "--", "A.dat", "dir1/a.dat", "DIR3", "dir1/dir2"])
        .ok();

    let output = (how.run)(&clone);
    if case_insensitive {
        output
            .has(r#""a.dat": not a regular file"#)
            .has(r#""dir1/A.dat": not a regular file"#)
            .has(r#""dir3/a.dat": not a directory"#)
            .has(r#""dir1/DIR2/a.dat": not a directory"#)
            .lacks("is beyond a symbolic link");
        for path in ["a.dat", "dir1/A.dat", "dir3", "dir1/DIR2"] {
            assert!(is_symlink(&root.join(path)), "{path}");
        }
    } else {
        if let Some(progress) = how.progress {
            output.has(progress);
        }
        for file in files {
            assert_eq!(clone.read(file), "a", "{file}");
        }
    }
    assert_outside_untouched();
    clone.assert_clean_index();
}

/// `a.dat`, changed in the working tree, then replaced by a directory.
pub fn changed_files(repo: &Repo, run: impl Fn(&str) -> Run) {
    repo.write("a.dat", "a +extra");
    run_with_messages(repo, &run, "", &[]);
    assert_eq!(repo.read("a.dat"), "a +extra");

    remove(&repo.path().join("a.dat"));
    std::fs::create_dir(repo.path().join("a.dat")).unwrap();
    for cwd in ["", "a.dat"] {
        run_with_messages(repo, &run, cwd, &[]);
        assert!(repo.path().join("a.dat").is_dir());
    }
}

/// `a.dat` and `dir1/dir2/dir3/a.dat`, hard links to a pointer file outside
/// the working tree. Writing them mustn't change that file.
pub fn hard_links(t: &Test, repo: &Repo, run: impl Fn(&str) -> Run) {
    let files = ["a.dat", "dir1/dir2/dir3/a.dat"];
    let link = t.dir().join("link");
    let pointer = repo.git(["cat-file", "-p", ":a.dat"]).ok().stdout();
    std::fs::write(&link, &pointer).unwrap();

    for cwd in ["", "dir1/dir2"] {
        for file in files {
            remove(&repo.path().join(file));
            std::fs::hard_link(&link, repo.path().join(file)).unwrap();
        }
        run(cwd);
        for file in files {
            assert_eq!(repo.read(file), "a", "{file}");
        }
        assert_eq!(std::fs::read_to_string(&link).unwrap(), pointer);
        repo.assert_clean_status();
    }
}
