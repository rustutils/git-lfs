//! `t-clean.sh`.

use crate::harness::{Repo, Run, Test, oid, pointer, sized};

/// Upstream's largest pointer, the read buffer size the shell tests get
/// from `lfstest-getlimit --max-pointer-size`.
const MAX_POINTER_SIZE: usize = 1024;

/// Run `git lfs clean` with `args` in `repo`, with `stdin`.
fn clean(repo: &Repo, args: &[&str], stdin: impl Into<Vec<u8>>) -> Run {
    repo.lfs_command()
        .args(["clean"])
        .args(args)
        .stdin(stdin)
        .run()
        .ok()
}

/// Check that cleaning `contents`, which looks like a pointer but isn't
/// one, gives a pointer to it.
#[track_caller]
fn assert_cleans_to_pointer(repo: &Repo, contents: &str) {
    let run = clean(repo, &[], contents);
    assert_eq!(run.stdout(), pointer(&oid(contents), contents.len()));
}

const WHATEVER_OID: &str = "cd293be6cea034bd45a0352775a219ef5dc7825ce55d1f7dae9762d80ce64411";

#[test]
fn clean_simple_file() {
    let t = Test::new();
    let repo = t.init("simple");

    let run = clean(&repo, &[], "whatever\n");

    assert_eq!(run.stdout(), pointer(WHATEVER_OID, 9));
    repo.assert_local_object(WHATEVER_OID, 9);
}

#[test]
fn clean_a_pointer() {
    let t = Test::new();
    let repo = t.init("pointer");

    let run = clean(&repo, &[], pointer(WHATEVER_OID, 9));

    assert_eq!(run.stdout(), pointer(WHATEVER_OID, 9));
}

#[test]
fn clean_pseudo_pointer() {
    let t = Test::new();
    let repo = t.init("pseudo");
    let contents = "version https://git-lfs.github.com/spec/v1
oid sha256:7cd8be1d2cd0dd22cd9d229bb6b5785009a05e8b39d405615d882caac56562b5
size 9999

This is my test pointer.  There are many like it, but this one is mine.";

    assert_cleans_to_pointer(&repo, contents);
}

/// The pointer-like start is followed by more than a read buffer of
/// newlines (upstream's PR 271).
#[test]
fn clean_pseudo_pointer_with_extra_data() {
    let t = Test::new();
    let repo = t.init("extra-data");
    let contents = format!(
        "version https://git-lfs.github.com/spec/v1
oid sha256:7cd8be1d2cd0dd22cd9d229bb6b5785009a05e8b39d405615d882caac56562b5
size 9999
{}
This is my test pointer.  There are many like it, but this one is mine.",
        "\n".repeat(MAX_POINTER_SIZE)
    );

    assert_cleans_to_pointer(&repo, &contents);
}

#[test]
fn clean_with_pointer_extension() {
    let t = Test::new();
    let repo = t.init("pointer-extension");
    let log = repo.case_inverter_extension();
    let contents = "abc\ndef";
    let inverted = "ABC\nDEF";

    let run = clean(&repo, &["--", "dir1/abc.dat"], contents);

    repo.assert_local_object(&oid(inverted), 7);
    assert_eq!(
        run.stdout(),
        format!(
            "version https://git-lfs.github.com/spec/v1\n\
             ext-0-caseinverter sha256:{}\n\
             oid sha256:{}\n\
             size 7\n",
            oid(contents),
            oid(inverted)
        )
    );
    let log = std::fs::read_to_string(log).unwrap();
    assert!(log.contains("clean: dir1/abc.dat"), "{log}");
}

/// Input as large as a read buffer, and larger (upstream's issue 2487).
#[test]
fn clean_stdin() {
    let t = Test::new();
    let repo = t.init("clean-over-stdin");

    for size in [MAX_POINTER_SIZE, MAX_POINTER_SIZE * 2] {
        let contents = sized("clean-over-stdin", size);
        let run = clean(&repo, &[], contents.as_str());
        assert_eq!(run.stdout(), pointer(&oid(&contents), size), "{size}");
    }
}

/// The file on disk is smaller than what Git passes in, and smaller than a
/// read buffer (upstream's PR 6011).
#[test]
fn clean_ignore_file_size_on_disk_when_cleaning() {
    let t = Test::new();
    let repo = t.init("clean-ignore-file-size");
    let contents = vec![0u8; MAX_POINTER_SIZE * 2];
    repo.write("large.dat", "");

    let run = repo
        .lfs_command()
        .args(["clean", "--", "large.dat"])
        .stdin(contents.clone())
        .merged()
        .run()
        .ok();

    assert_eq!(run.stdout(), pointer(&oid(&contents), contents.len()));
}
