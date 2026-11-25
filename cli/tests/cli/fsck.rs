//! `t-fsck.sh`.

use crate::harness::{Cmd, Repo, Run, Test, git_lfs, oid};

/// Upstream's `lfs.BlobSizeCutoff`, the largest blob read as a possible
/// pointer, which the shell suites get from `lfstest-getlimit`.
const MAX_POINTER_SIZE: usize = 1024;

/// The number of lines in `text` with `start`, then `end` later on, like
/// a grep for `start.*end`.
fn count(text: &str, start: &str, end: &str) -> usize {
    text.lines()
        .filter(|line| {
            line.find(start)
                .is_some_and(|i| line[i + start.len()..].contains(end))
        })
        .count()
}

/// [`count`] for the non-canonical pointer message.
fn non_canonical(text: &str) -> usize {
    count(
        text,
        "pointer: nonCanonicalPointer: Pointer",
        "was not canonical",
    )
}

/// [`count`] for the message that `path` should have been a pointer.
fn not_a_pointer(text: &str, path: &str) -> usize {
    let start = format!("pointer: unexpectedGitObject: \"{path}\"");
    count(text, &start, "should have been a pointer but was not")
}

/// Check that stdout is `expected`, give or take a final newline, as the
/// shell's `$(...)` compares it.
#[track_caller]
fn assert_output(run: &Run, expected: &str) {
    assert_eq!(run.stdout().trim_end_matches('\n'), expected, "{run}");
}

const DATA_A: &str = "test data\n";
const DATA_B: &str = "test data 2\n";

/// A new repository `name` with `a.dat` and `b.dat` tracked and committed.
fn two_files<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", DATA_A);
    repo.write("b.dat", DATA_B);
    repo.git(["add", ".gitattributes", "a.dat", "b.dat"]).ok();
    repo.git(["commit", "-m", "first commit"]).ok();
    repo
}

/// Append to the local object `oid`, so it no longer matches its OID.
fn corrupt(repo: &Repo, oid: &str) {
    let path = repo.local_object_path(oid);
    let mut data = std::fs::read(&path).unwrap();
    data.extend_from_slice(b"CORRUPTION\n");
    std::fs::write(path, data).unwrap();
}

#[test]
fn fsck_default() {
    let t = Test::new();
    let repo = two_files(&t, "fsck-default");
    let (a, b) = (oid(DATA_A), oid(DATA_B));
    assert_output(&repo.lfs(["fsck"]).ok(), "Git LFS fsck OK");
    repo.assert_local_object(&a, DATA_A.len());
    repo.assert_local_object(&b, DATA_B.len());

    corrupt(&repo, &a);
    let run = repo.lfs(["fsck"]);
    let bad = repo.git_dir().join("lfs/bad");
    let moved = bad.canonicalize().unwrap();
    assert_output(
        &run,
        &format!(
            "objects: corruptObject: a.dat ({a}) is corrupt\n\
             objects: repair: moving corrupt objects to {}",
            moved.display()
        ),
    );
    assert!(bad.join(&a).exists());
    repo.refute_local_object(&a);
    repo.assert_local_object(&b, DATA_B.len());
}

#[test]
fn fsck_dry_run() {
    let t = Test::new();
    let repo = two_files(&t, "fsck-dry-run");
    let (a, b) = (oid(DATA_A), oid(DATA_B));
    assert_output(&repo.lfs(["fsck", "--dry-run"]).ok(), "Git LFS fsck OK");

    corrupt(&repo, &a);
    let run = repo.lfs(["fsck", "--dry-run"]);
    assert_output(
        &run,
        &format!("objects: corruptObject: a.dat ({a}) is corrupt"),
    );
    // Nothing is moved.
    let corrupted = repo.local_object(&a).expect("object moved");
    assert_ne!(oid(&corrupted), a);
    repo.assert_local_object(&b, DATA_B.len());
}

#[test]
fn fsck_does_not_fail_with_shell_characters_in_paths() {
    let t = Test::new();
    let repo = two_files(&t, "[[path]]/fsck-shell-paths");
    // Packed, to check the pack code handles glob characters in the path.
    repo.git(["gc", "--aggressive", "--prune=now"]).ok();
    assert_output(&repo.lfs(["fsck"]).ok(), "Git LFS fsck OK");
}

#[test]
fn fsck_outside_git_repository() {
    let t = Test::new();
    let run = t.command(t.dir(), git_lfs()).args(["fsck"]).run();
    assert_eq!(run.code(), Some(128), "{run}");
    assert!(run.stdout().contains("Not in a Git repository"), "{run}");
}

/// Commit invalid pointers next to the pointer for `valid`, in the
/// repository's current directory: `crlf.<ext>`, a copy of the pointer
/// with CRLF line endings, and `large.<ext>`, too large to be a pointer,
/// both bypassing the clean filter.
fn create_invalid_pointers(repo: &Repo, valid: &str, ext: &str) {
    let pointer = repo.git(["cat-file", "blob", &format!(":{valid}")]).ok();
    let crlf = pointer.stdout().replace('\n', "\r\n");
    repo.write(&format!("crlf.{ext}"), crlf);
    // Text, as the shell suite's base64, one byte over the limit.
    let large: String = (0..=MAX_POINTER_SIZE)
        .map(|i| char::from(b'A' + (i % 26) as u8))
        .collect();
    repo.write(&format!("large.{ext}"), large);
    repo.git([
        "-c",
        "filter.lfs.process=",
        "-c",
        "filter.lfs.clean=cat",
        "-c",
        "filter.lfs.required=false",
        "add",
        &format!("crlf.{ext}"),
        &format!("large.{ext}"),
    ])
    .ok();
    repo.git(["commit", "-m", "invalid pointers"]).ok();
}

fn setup_invalid_pointers<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = two_files(t, name);
    create_invalid_pointers(&repo, "a.dat", "dat");
    repo
}

/// Run `fsck` and `fsck --pointers`, check both fail, and return their
/// output together.
fn fsck_both(repo: &Repo) -> String {
    let mut output = String::new();
    for args in [&["fsck"][..], &["fsck", "--pointers"]] {
        let run = repo.lfs(args);
        assert_eq!(run.code(), Some(1), "{run}");
        output += &run.output();
    }
    output
}

#[test]
fn fsck_detects_invalid_pointers() {
    let t = Test::new();
    let repo = setup_invalid_pointers(&t, "fsck-pointers");
    let output = fsck_both(&repo);
    assert_eq!(non_canonical(&output), 2, "{output}");
    assert_eq!(not_a_pointer(&output, "large.dat"), 2, "{output}");
}

#[test]
fn fsck_detects_invalid_pointers_with_macro_patterns() {
    let t = Test::new();
    let repo = t.init("fsck-pointers-macros");
    repo.write(
        ".gitattributes",
        "[attr]lfs filter=lfs diff=lfs merge=lfs -text\n*.dat lfs\n",
    );
    repo.write("a.dat", DATA_A);
    repo.write("dir/.gitattributes", "*.bin lfs\n");
    repo.git(["add", ".gitattributes", "a.dat", "dir"]).ok();
    repo.git(["commit", "-m", "first commit"]).ok();
    create_invalid_pointers(&repo, "a.dat", "dat");
    // `:a.dat` is from the index, so this works from dir too.
    create_invalid_pointers(&repo.at("dir"), "a.dat", "bin");

    // Upstream doesn't resolve a macro used in a .gitattributes that sorts
    // before the top-level one, such as .dir/.gitattributes, so the shell
    // test leaves that out too.
    let output = fsck_both(&repo);
    assert_eq!(non_canonical(&output), 4, "{output}");
    for path in ["large.dat", "dir/large.bin"] {
        assert_eq!(not_a_pointer(&output, path), 2, "{output}");
    }
}

/// The objects come from another repository's object directory.
#[test]
fn fsck_detects_invalid_pointers_with_git_object_directory() {
    let t = Test::new();
    let name = "fsck-pointers-object-directory";
    let repo = setup_invalid_pointers(&t, name);
    let head = repo.git(["rev-parse", "HEAD"]).ok().stdout();
    let objects = repo.git_dir().join("objects").canonicalize().unwrap();
    let other = format!("{name}-2");
    let git_dir = t.init(&other).git_dir().canonicalize().unwrap();
    let in_other = |cmd: Cmd| {
        cmd.env("GIT_WORK_TREE", &other)
            .env("GIT_DIR", &git_dir)
            .env("GIT_OBJECT_DIRECTORY", &objects)
            .run()
    };
    let git = t.command(t.dir(), "git");
    in_other(git.args(["update-ref", "refs/heads/main", head.trim()])).ok();

    let run = in_other(t.command(t.dir(), git_lfs()).args(["fsck", "--pointers"]));
    assert_eq!(run.code(), Some(1), "{run}");
    assert_eq!(non_canonical(&run.output()), 1, "{run}");
    assert_eq!(not_a_pointer(&run.output(), "large.dat"), 1, "{run}");
}

/// Check that `fsck` and `fsck --<mode>` both pass.
#[track_caller]
fn assert_fsck_ok(repo: &Repo, mode: &str) {
    repo.lfs(["fsck"]).ok();
    repo.lfs(["fsck", mode]).ok();
}

/// A new repository `name` with only a README, outside LFS.
fn readme_only<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.write("README.md", "# README\n");
    repo.git(["add", "README.md"]).ok();
    repo.git(["commit", "-m", "Add README"]).ok();
    repo
}

#[test]
fn fsck_does_not_detect_invalid_pointers_with_no_lfs_objects() {
    let t = Test::new();
    let repo = readme_only(&t, "fsck-pointers-none");
    assert_fsck_ok(&repo, "--pointers");
}

#[test]
fn fsck_does_not_detect_invalid_pointers_with_symlinks() {
    let t = Test::new();
    let repo = t.init("fsck-pointers-symlinks");
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "# Test\n");
    std::os::unix::fs::symlink("a.dat", repo.path().join("b.dat")).unwrap();
    repo.git(["add", ".gitattributes", "a.dat", "b.dat"]).ok();
    repo.git(["commit", "-m", "Add files"]).ok();
    assert_fsck_ok(&repo, "--pointers");
}

#[test]
fn fsck_does_not_detect_invalid_pointers_with_negated_patterns() {
    let t = Test::new();
    let repo = t.init("fsck-pointers-none");
    repo.write(
        ".gitattributes",
        "*.dat filter=lfs diff=lfs merge=lfs -text\nb.dat !filter !diff !merge text\n",
    );
    repo.write("a.dat", "# Test\n");
    repo.write("b.dat", "# Test\n");
    repo.git(["add", ".gitattributes", "a.dat", "b.dat"]).ok();
    repo.git(["commit", "-m", "Add files"]).ok();
    assert_fsck_ok(&repo, "--pointers");
}

/// Upstream accepts `.dir/b.dat` for a different reason than `dir/b.dat`:
/// it reads `.dir/.gitattributes` before the top-level one, so it doesn't
/// know the macro yet. Either way, neither is reported.
#[test]
fn fsck_does_not_detect_invalid_pointers_with_negated_macro_patterns() {
    let t = Test::new();
    let repo = t.init("fsck-pointers-macros-none");
    repo.write(
        ".gitattributes",
        "[attr]lfs filter=lfs diff=lfs merge=lfs -text\n*.dat lfs\nb.dat !lfs\n",
    );
    repo.write("a.dat", DATA_A);
    for file in ["b.dat", "dir/b.dat", ".dir/b.dat"] {
        repo.write(file, DATA_A);
    }
    repo.write("dir/.gitattributes", "*.dat !lfs\n");
    repo.write(".dir/.gitattributes", "*.dat !lfs\n");
    repo.git(["add", ".gitattributes", "a.dat", "b.dat", "dir", ".dir"])
        .ok();
    repo.git(["commit", "-m", "first commit"]).ok();
    assert_fsck_ok(&repo, "--pointers");
}

const REPAIR: &str = "objects: repair: moving corrupt objects to ";

const FOO_A: &str = "test test 3\n";
const FOO_B: &str = "test data 4\n";

/// Four files committed, with the objects for `a.dat` and `foo/a.dat`
/// corrupted, and those for `b.dat` and `foo/b.dat` deleted.
fn setup_invalid_objects<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init(name);
    repo.lfs(["track", "*.dat"]).ok();
    for (file, contents) in [
        ("a.dat", DATA_A),
        ("b.dat", DATA_B),
        ("foo/a.dat", FOO_A),
        ("foo/b.dat", FOO_B),
    ] {
        repo.write(file, contents);
    }
    repo.git(["add", ".gitattributes", "a.dat", "b.dat", "foo"])
        .ok();
    repo.git(["commit", "-m", "first commit"]).ok();
    corrupt(&repo, &oid(DATA_A));
    repo.delete_local_object(&oid(DATA_B));
    corrupt(&repo, &oid(FOO_A));
    repo.delete_local_object(&oid(FOO_B));
    repo
}

/// Run `fsck` with `args` on a fresh [`setup_invalid_objects`], and check
/// which files it reports: those under `foo` only unless `exclude_foo`.
fn invalid_objects(name: &str, exclude_foo: bool, args: &[&str]) {
    let t = Test::new();
    let repo = setup_invalid_objects(&t, name);
    if exclude_foo {
        repo.git(["config", "lfs.fetchexclude", "/foo"]).ok();
    }
    let run = repo.lfs(["fsck"].iter().chain(args));
    assert_eq!(run.code(), Some(1), "{run}");
    let output = run.output();
    let in_foo = usize::from(!exclude_foo);
    for (prefix, n) in [("", 1), ("foo/", in_foo)] {
        let corrupt = format!("objects: corruptObject: {prefix}a.dat (");
        assert_eq!(count(&output, &corrupt, ") is corrupt"), n, "{run}");
        let missing = format!("objects: openError: {prefix}b.dat (");
        assert_eq!(
            count(&output, &missing, ") could not be checked: "),
            n,
            "{run}"
        );
    }
    assert_eq!(count(&output, REPAIR, ""), 1, "{run}");
}

#[test]
fn fsck_detects_invalid_objects() {
    invalid_objects("fsck-objects", false, &[]);
    invalid_objects("fsck-objects", false, &["--objects"]);
}

#[test]
fn fsck_detects_invalid_objects_except_in_excluded_paths() {
    invalid_objects("fsck-objects-exclude", true, &[]);
    invalid_objects("fsck-objects-exclude", true, &["--objects"]);
}

#[test]
fn fsck_does_not_detect_invalid_objects_with_no_lfs_objects() {
    let t = Test::new();
    let repo = readme_only(&t, "fsck-objects-none");
    assert_fsck_ok(&repo, "--objects");
}

#[test]
fn fsck_operates_on_specified_refs() {
    let t = Test::new();
    let repo = setup_invalid_pointers(&t, "fsck-refs");
    repo.git(["rm", "-f", "crlf.dat", "large.dat"]).ok();
    repo.write("new.dat", "# Test\n");
    repo.git(["add", "new.dat"]).ok();
    repo.git(["commit", "-m", "third commit"]).ok();
    repo.git(["commit", "--allow-empty", "-m", "fourth commit"])
        .ok();

    // Only HEAD^^, the second commit, has the invalid pointers. Without a
    // ref, HEAD and the index are checked.
    for (args, ok) in [
        (&["fsck"][..], true),
        (&["fsck", "HEAD"], true),
        (&["fsck", "HEAD^^"], false),
        (&["fsck", "HEAD^"], true),
        (&["fsck", "HEAD^..HEAD"], true),
        (&["fsck", "HEAD^^^..HEAD"], false),
        (&["fsck", "HEAD^^^..HEAD^"], false),
    ] {
        let run = repo.lfs(args);
        assert_eq!(run.code() == Some(0), ok, "{run}");
    }

    let run = repo.lfs(["fsck", "--pointers", "HEAD^^^..HEAD^^"]).fails();
    assert_eq!(non_canonical(&run.output()), 1, "{run}");
    assert_eq!(not_a_pointer(&run.output(), "large.dat"), 1, "{run}");

    corrupt(&repo, &oid("# Test\n"));
    let run = repo.lfs(["fsck", "--objects", "HEAD^^..HEAD^"]).fails();
    let output = run.output();
    let corrupt = "objects: corruptObject: new.dat (";
    assert_eq!(count(&output, corrupt, ") is corrupt"), 1, "{run}");
    assert_eq!(count(&output, REPAIR, ""), 1, "{run}");
}

#[test]
fn fsck_detects_invalid_ref() {
    let t = Test::new();
    let repo = t.init("fsck-default");
    repo.lfs(["fsck", "jibberish"])
        .fails()
        .has("can't resolve ref");
}
