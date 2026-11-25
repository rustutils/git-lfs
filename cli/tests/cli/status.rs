//! `t-status.sh`.

use crate::harness::{Repo, Run, Test, git_lfs, oid};

/// The first 7 characters of the OID of `contents`, as status shows them.
fn short(contents: &str) -> String {
    oid(contents)[..7].to_owned()
}

/// Check that stdout is `expected`, give or take a final newline, as the
/// shell's `$(...)` compares it.
#[track_caller]
fn assert_output(run: &Run, expected: &str) {
    assert_eq!(run.stdout().trim_end_matches('\n'), expected, "{run}");
}

#[track_caller]
fn assert_status(repo: &Repo, expected: &str) {
    assert_output(&repo.lfs(["status"]).ok(), expected);
}

/// The default output, with `pushed` naming the objects to push, if
/// there's an upstream branch, and `staged` and `unstaged` the lines of
/// the other two sections.
fn expected(pushed: Option<&[String]>, staged: &[String], unstaged: &[String]) -> String {
    let mut out = String::from("On branch main\n");
    if let Some(pushed) = pushed {
        out += "Objects to be pushed to origin/main:\n\n";
        for line in pushed {
            out += &format!("\t{line}\n");
        }
    }
    out += "\nObjects to be committed:\n\n";
    for line in staged {
        out += &format!("\t{line}\n");
    }
    out += "\nObjects not staged for commit:";
    if !unstaged.is_empty() {
        out += "\n\n";
        out += &unstaged
            .iter()
            .map(|l| format!("\t{l}"))
            .collect::<Vec<_>>()
            .join("\n");
    }
    out
}

/// A new repository `name` with `*.dat` tracked, uncommitted.
fn tracked<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo
}

/// `file1.dat` committed and changed, `file2.dat` added, and `file3.dat`
/// added and changed.
fn changes_setup<'a>(t: &'a Test, name: &str, newline: &str) -> Repo<'a> {
    let repo = tracked(t, name);
    repo.write("file1.dat", format!("some data{newline}"));
    repo.git(["add", "file1.dat"]).ok();
    repo.git(["commit", "-m", "file1.dat"]).ok();
    repo.write("file1.dat", format!("other data{newline}"));
    repo.write("file2.dat", format!("file2 data{newline}"));
    repo.git(["add", "file2.dat"]).ok();
    repo.write("file3.dat", format!("file3 data{newline}"));
    repo.git(["add", "file3.dat"]).ok();
    repo.write("file3.dat", format!("file3 other data{newline}"));
    repo
}

#[test]
fn status() {
    let t = Test::new();
    let repo = changes_setup(&t, "repo-1", "");
    assert_status(
        &repo,
        &expected(
            None,
            &[
                format!("file2.dat (LFS: {})", short("file2 data")),
                format!("file3.dat (LFS: {})", short("file3 data")),
            ],
            &[
                format!(
                    "file1.dat (LFS: {} -> File: {})",
                    short("some data"),
                    short("other data")
                ),
                format!("file3.dat (File: {})", short("file3 other data")),
            ],
        ),
    );
}

#[test]
fn status_porcelain() {
    let t = Test::new();
    let repo = changes_setup(&t, "repo-2", "\n");
    let run = repo.lfs(["status", "--porcelain"]).ok();
    assert_output(&run, " M file1.dat\nA  file3.dat\nA  file2.dat");
}

#[test]
fn status_json() {
    let t = Test::new();
    let repo = tracked(&t, "repo-3");
    repo.write("file1.dat", "some data\n");
    repo.git(["add", "file1.dat"]).ok();
    repo.git(["commit", "-m", "file1.dat"]).ok();
    let json = |expected: &str| {
        assert_output(&repo.lfs(["status", "--json"]).ok(), expected);
    };

    repo.write("file1.dat", "other data\n");
    json(r#"{"files":{"file1.dat":{"status":"M"}}}"#);

    repo.git(["add", "file1.dat"]).ok();
    repo.git(["commit", "-m", "file1.dat changed"]).ok();
    repo.git(["mv", "file1.dat", "file2.dat"]).ok();
    json(r#"{"files":{"file2.dat":{"status":"R","from":"file1.dat"}}}"#);

    // Files outside LFS aren't listed.
    repo.git(["commit", "-m", "file1.dat -> file2.dat"]).ok();
    repo.write("test1.txt", "hi\n");
    repo.git(["add", "test1.txt"]).ok();
    json(r#"{"files":{}}"#);
}

#[test]
fn status_in_a_sub_directory() {
    let t = Test::new();
    let repo = tracked(&t, "status-sub-directory");
    repo.write("file.dat", "asdf");
    std::fs::create_dir(repo.path().join("dir")).unwrap();
    repo.git(["add", ".gitattributes", "file.dat"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.write("file.dat", "ASDF");

    let run = repo.at("dir").lfs(["status"]).ok();
    assert_output(
        &run,
        &expected(
            None,
            &[],
            &["../file.dat (LFS: f0e4c2f -> File: 99b3bcf)".into()],
        ),
    );
}

#[test]
fn status_outside_git_repository() {
    let t = Test::new();
    let run = t.command(t.dir(), git_lfs()).args(["status"]).run();
    assert_eq!(run.code(), Some(128), "{run}");
    assert!(run.stdout().contains("Not in a Git repository"), "{run}");
}

/// Without a commit, there's no branch line.
#[test]
fn status_before_initial_commit() {
    let t = Test::new();
    let repo = tracked(&t, "repo-initial");
    // Nothing to show isn't an error.
    repo.lfs(["status"]).ok();

    repo.write("file1.dat", "some data");
    repo.git(["add", "file1.dat"]).ok();
    let expected = expected(
        None,
        &[format!("file1.dat (LFS: {})", short("some data"))],
        &[],
    );
    let expected = expected.strip_prefix("On branch main\n").unwrap();
    assert_status(&repo, expected);
}

#[test]
fn status_shows_multiple_files_with_identical_contents() {
    let t = Test::new();
    let repo = tracked(&t, "uniq-status");
    repo.write("a.dat", "contents");
    repo.write("b.dat", "contents");
    repo.git(["add", "--all", "."]).ok();
    let run = repo.lfs(["status"]).ok();
    assert_eq!(run.count("a.dat"), 1, "{run}");
    assert_eq!(run.count("b.dat"), 1, "{run}");
}

/// A new repository `name` with `*.dat` tracked and committed.
fn committed<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = tracked(t, name);
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo
}

#[test]
fn status_shows_multiple_copies_of_partially_staged_files() {
    let t = Test::new();
    let repo = committed(&t, "status-partially-staged");
    repo.write("a.dat", "part 1");
    repo.git(["add", "a.dat"]).ok();
    repo.write("a.dat", "part 2");
    assert_status(
        &repo,
        &expected(
            None,
            &[format!("a.dat (LFS: {})", short("part 1"))],
            &[format!("a.dat (File: {})", short("part 2"))],
        ),
    );
}

const CONTENTS: &str = "contents";
const CONTENTS_NEW: &str = "contents +extra";

#[test]
fn status_lfs_to_lfs_change() {
    let t = Test::new();
    let repo = t.remote_and_clone("status-lfs-to-lfs-change");
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "track *.dat files"]).ok();
    repo.write("a.dat", CONTENTS);
    repo.git(["add", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();
    repo.write("a.dat", CONTENTS_NEW);
    repo.git(["add", "a.dat"]).ok();

    let line = format!(
        "a.dat (LFS: {} -> LFS: {})",
        short(CONTENTS),
        short(CONTENTS_NEW)
    );
    assert_status(&repo, &expected(None, &[line], &[]));
}

/// A clone of the new remote `name` with `a.dat` committed outside LFS,
/// then `*.dat` tracked.
fn git_then_lfs<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.write("a.dat", CONTENTS);
    repo.git(["add", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "track *.dat files"]).ok();
    repo
}

#[test]
fn status_git_to_lfs_change() {
    let t = Test::new();
    let repo = git_then_lfs(&t, "status-git-to-lfs-change");
    repo.write("a.dat", CONTENTS_NEW);
    repo.git(["add", "a.dat"]).ok();

    let line = format!(
        "a.dat (Git: {} -> LFS: {})",
        short(CONTENTS),
        short(CONTENTS_NEW)
    );
    assert_status(&repo, &expected(None, &[line], &[]));
}

/// Adding the file again in a clone converts it to LFS, as the clean
/// filter now applies.
#[test]
fn status_git_to_lfs_conversion() {
    let t = Test::new();
    let name = "status-git-to-lfs-conversion";
    let repo = git_then_lfs(&t, name);
    repo.git(["push", "origin", "main"]).ok();

    let clone = t.clone(name, &format!("{name}-2"));
    clone.git(["add", "a.dat"]).ok();
    let run = clone.lfs_command().args(["status"]).merged().run().ok();
    let line = format!(
        "a.dat (Git: {} -> LFS: {})",
        short(CONTENTS),
        short(CONTENTS)
    );
    assert_output(&run, &expected(Some(&[]), &[line], &[]));
}

#[test]
fn status_missing_objects() {
    let t = Test::new();
    let repo = tracked(&t, "status-missing-objects");
    repo.write("a.dat", "a");
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();

    // Remove the Git object for the file's contents, not its pointer.
    let blob = repo
        .git(["hash-object", "-t", "blob", "--", "a.dat"])
        .ok()
        .stdout();
    let blob = blob.trim();
    let path = repo
        .git_dir()
        .join("objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    let _ = std::fs::remove_file(path);

    // An unstaged change against a source that doesn't exist.
    repo.write("a.dat", "b");
    repo.git(["add", "a.dat"]).ok();
    repo.lfs(["status"])
        .ok()
        .has(&format!("a.dat (?: <missing> -> LFS: {})", short("b")));
}

/// A clone of the new remote `name`, with `*.dat` tracked and pushed, and
/// then `a.dat` committed.
fn unpushed<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.git(["push", "-u", "origin", "main"]).ok();
    repo.write("a.dat", "a");
    repo.git(["add", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a large file"]).ok();
    repo
}

#[test]
fn status_unpushed_objects() {
    let t = Test::new();
    let repo = unpushed(&t, "status-unpushed-objects");
    let pushed = [format!("a.dat ({})", oid("a"))];
    assert_status(&repo, &expected(Some(&pushed), &[], &[]));
}

/// We exit 1 here where upstream exits 128 (see NOTES.md). The shell test
/// only checks for failure.
#[test]
fn status_without_a_working_copy() {
    let t = Test::new();
    let name = "status-no-working-copy.git";
    t.command(t.dir(), "git")
        .args(["init", "--bare", name])
        .run()
        .ok();
    let run = t
        .repo(name)
        .lfs_command()
        .args(["status"])
        .merged()
        .run()
        .fails();
    assert_output(&run, "This operation must be run in a work tree.");
}

#[test]
fn status_deleted_files() {
    let t = Test::new();
    let repo = unpushed(&t, "status-deleted-files");
    repo.git(["rm", "a.dat"]).ok();
    let pushed = [format!("a.dat ({})", oid("a"))];
    let staged = [format!("a.dat (LFS: {} -> File: deleted)", short("a"))];
    assert_status(&repo, &expected(Some(&pushed), &staged, &[]));
}

#[test]
fn status_file_to_dir() {
    let t = Test::new();
    let repo = t.remote_and_clone("status-file-to-dir");
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.write("test", "file");
    repo.git(["add", "test"]).ok();
    repo.git(["commit", "-m", "add test"]).ok();

    repo.git(["rm", "test"]).ok();
    repo.write("test/a.dat", "a");
    repo.git(["add", "test"]).ok();
    repo.git(["commit", "-m", "add files"]).ok();
    repo.git(["reset", "HEAD~"]).ok();
    repo.git(["add", "test"]).ok();

    let staged = [
        format!("test (Git: {} -> File: deleted)", short("file")),
        format!("test/a.dat (LFS: {})", short("a")),
    ];
    assert_status(&repo, &expected(None, &staged, &[]));
}

/// A permission change isn't listed.
#[test]
fn status_permission_change() {
    let t = Test::new();
    let repo = t.remote_and_clone("status-permission-change");
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "track *.dat"]).ok();
    repo.write("a.dat", CONTENTS);
    repo.git(["add", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();

    let path = repo.path().join("a.dat");
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o400);
    std::fs::set_permissions(&path, perms).unwrap();

    let run = repo.lfs_command().args(["status"]).merged().run().ok();
    assert_output(&run, &expected(None, &[], &[]));
}
