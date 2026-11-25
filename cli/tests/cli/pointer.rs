//! `t-pointer.sh`.
//!
//! Most of these run in the test's directory, which isn't a repository, as
//! the shell tests run in theirs.

use crate::harness::{Repo, Run, Test, oid};

/// The pointer for `simple\n`, without a final newline.
const POINTER: &str = "version https://git-lfs.github.com/spec/v1
oid sha256:6c17f2007cbe934aee6e309b28b2dba3c119c5dff2ef813ed124699efe319868
size 7";

/// [`POINTER`], with the wrong size.
const WRONG_SIZE: &str = "version https://git-lfs.github.com/spec/v1
oid sha256:6c17f2007cbe934aee6e309b28b2dba3c119c5dff2ef813ed124699efe319868
size 123";

const BLOB: &str = "e18acd45d7e3ce0451d1d637f9697aa508e07dee";
const WRONG_SIZE_BLOB: &str = "905bcc24b5dc074ab870f9944178e398eec3b470";

/// The test's directory, with `some-file` holding `simple\n`.
fn with_some_file(t: &Test) -> Repo<'_> {
    let dir = t.repo("");
    dir.write("some-file", "simple\n");
    dir
}

/// Run `git lfs pointer` with `args` and `stdin`, with stderr merged into
/// stdout.
fn pointer(dir: &Repo, args: &[&str], stdin: &str) -> Run {
    dir.lfs_command()
        .args(["pointer"])
        .args(args)
        .stdin(stdin)
        .merged()
        .run()
}

/// Check that the merged output is `expected`, give or take a final
/// newline, as the shell's `$(...)` compares it.
#[track_caller]
fn assert_output(run: &Run, expected: &str) {
    assert_eq!(run.stdout().trim_end_matches('\n'), expected, "{run}");
}

/// The section of the output for a pointer from `source`.
fn section(source: &str, pointer: &str, blob: &str) -> String {
    format!("{source}\n\n{pointer}\n\nGit blob OID: {blob}")
}

fn file_section() -> String {
    section("Git LFS pointer for some-file", POINTER, BLOB)
}

#[test]
fn pointer_file_stdin() {
    let t = Test::new();
    let dir = with_some_file(&t);
    let run = pointer(
        &dir,
        &["--file=some-file", "--stdin"],
        &format!("{POINTER}\n"),
    )
    .ok();
    let expected = format!(
        "{}\n\n{}",
        file_section(),
        section("Pointer from STDIN", POINTER, BLOB)
    );
    assert_output(&run, &expected);
}

#[test]
fn pointer_file_stdin_mismatch() {
    let t = Test::new();
    let dir = with_some_file(&t);
    let run = pointer(
        &dir,
        &["--file=some-file", "--stdin"],
        &format!("{WRONG_SIZE}\n"),
    );
    assert_eq!(run.code(), Some(1), "{run}");
    let expected = format!(
        "{}\n\n{}\n\nPointers do not match",
        file_section(),
        section("Pointer from STDIN", WRONG_SIZE, WRONG_SIZE_BLOB)
    );
    assert_output(&run, &expected);
}

#[test]
fn pointer_stdin() {
    let t = Test::new();
    let dir = t.repo("");
    let run = pointer(&dir, &["--stdin"], &format!("{POINTER}\n")).ok();
    assert_output(&run, &format!("Pointer from STDIN\n\n{POINTER}"));
}

/// Stdin that's a character device, such as a terminal or `/dev/null`,
/// can't hold a pointer. The shell test pipes in a newline instead, which
/// gives an invalid header, so it only passes when it skips itself, which
/// it does when stdin is a terminal.
#[test]
fn pointer_stdin_without_stdin() {
    let t = Test::new();
    let run = t
        .repo("")
        .lfs_command()
        .args(["pointer", "--stdin"])
        .null_stdin()
        .merged()
        .run();
    assert_eq!(run.code(), Some(1), "{run}");
    assert_output(
        &run,
        "Cannot read from STDIN: The --stdin flag expects a pointer file from STDIN.",
    );
}

#[test]
fn pointer_stdin_with_bad_pointer() {
    let t = Test::new();
    let run = pointer(&t.repo(""), &["--stdin"], "not a pointer\n");
    assert_eq!(run.code(), Some(1), "{run}");
    assert_output(
        &run,
        "Pointer from STDIN\n\nPointer file error: invalid header",
    );
}

#[test]
fn pointer_file_pointer_mismatch() {
    let t = Test::new();
    let dir = with_some_file(&t);
    dir.write("invalid-pointer", format!("{WRONG_SIZE}\n"));
    let run = pointer(&dir, &["--file=some-file", "--pointer=invalid-pointer"], "");
    assert_eq!(run.code(), Some(1), "{run}");
    let expected = format!(
        "{}\n\n{}\n\nPointers do not match",
        file_section(),
        section("Pointer from invalid-pointer", WRONG_SIZE, WRONG_SIZE_BLOB)
    );
    assert_output(&run, &expected);
}

#[test]
fn pointer_file_pointer() {
    let t = Test::new();
    let dir = with_some_file(&t);
    dir.write("valid-pointer", format!("{POINTER}\n"));
    let run = pointer(&dir, &["--file=some-file", "--pointer=valid-pointer"], "").ok();
    let expected = format!(
        "{}\n\n{}",
        file_section(),
        section("Pointer from valid-pointer", POINTER, BLOB)
    );
    assert_output(&run, &expected);
}

#[test]
fn pointer_pointer() {
    let t = Test::new();
    let dir = t.repo("");
    dir.write("valid-pointer", format!("{POINTER}\n"));
    let run = pointer(&dir, &["--pointer=valid-pointer"], "").ok();
    assert_output(&run, &format!("Pointer from valid-pointer\n\n{POINTER}"));
}

#[test]
fn pointer_missing_pointer() {
    let t = Test::new();
    let run = pointer(&t.repo(""), &["--pointer=missing-pointer"], "");
    assert_eq!(run.code(), Some(1), "{run}");
    run.has("open missing-pointer:");
}

#[test]
fn pointer_invalid_pointer() {
    let t = Test::new();
    let dir = t.repo("");
    dir.write("some-pointer", "not a pointer\n");
    let run = pointer(&dir, &["--pointer=some-pointer"], "");
    assert_eq!(run.code(), Some(1), "{run}");
    assert_output(
        &run,
        "Pointer from some-pointer\n\nPointer file error: invalid header",
    );
}

#[test]
fn pointer_file() {
    let t = Test::new();
    let dir = with_some_file(&t);
    let run = pointer(&dir, &["--file=some-file"], "").ok();
    assert_output(&run, &format!("Git LFS pointer for some-file\n\n{POINTER}"));
}

#[test]
fn pointer_without_args() {
    let t = Test::new();
    let run = pointer(&t.repo(""), &[], "");
    assert_eq!(run.code(), Some(1), "{run}");
    assert_output(&run, "Nothing to do!");
}

const STDOUT_TEST_OID: &str = "e96ec1bd71eea8df78b24c64a7ab9d42dd7f821c4e503f0e2288273b9bff6c16";

/// The pointer goes to stdout, and the header to stderr.
#[test]
fn pointer_stdout_stderr() {
    let t = Test::new();
    let dir = t.repo("");
    dir.write("pointer-stdout-test.txt", "pointer-stdout-test\n");
    let run = dir.lfs(["pointer", "--file=pointer-stdout-test.txt"]).ok();
    let stdout = run.stdout();
    assert_eq!(stdout.lines().count(), 3, "{run}");
    assert!(
        stdout.contains(&format!("oid sha256:{STDOUT_TEST_OID}")),
        "{run}"
    );
    assert!(!stdout.contains("Git LFS pointer"), "{run}");
    let stderr = run.stderr();
    assert!(stderr.contains("Git LFS pointer"), "{run}");
    assert!(!stderr.contains("oid sha256:"), "{run}");
}

#[test]
fn pointer_to_console() {
    let t = Test::new();
    let dir = t.repo("");
    dir.write("pointer-stdout-test.txt", "pointer-stdout-test\n");
    pointer(&dir, &["--file=pointer-stdout-test.txt"], "")
        .ok()
        .has("Git LFS pointer")
        .has(&format!("oid sha256:{STDOUT_TEST_OID}"));
}

/// Check `git lfs pointer --check` on `file`, through `--file` and
/// `--stdin`, by default and with `--no-strict` and `--strict`. It should
/// pass all but `strict` checks, and pass those if `strict_passes`.
#[track_caller]
fn assert_check(repo: &Repo, file: &str, passes: bool, strict_passes: bool) {
    let contents = std::fs::read(repo.path().join(file)).unwrap();
    for (flag, should_pass) in [
        (None, passes),
        (Some("--no-strict"), passes),
        (Some("--strict"), strict_passes),
    ] {
        let mut args = vec!["pointer", "--check"];
        args.extend(flag);
        let by_file = repo.lfs(args.iter().chain(&["--file", file]));
        let by_stdin = repo
            .lfs_command()
            .args(args.iter().chain(&["--stdin"]))
            .stdin(contents.clone())
            .run();
        for run in [by_file, by_stdin] {
            assert_eq!(run.code() == Some(0), should_pass, "{run}");
        }
    }
}

#[test]
fn pointer_check_with_valid_pointer() {
    let t = Test::new();
    let repo = t.init("pointer---check-valid-pointer");
    repo.write("good.txt", "contents\n");
    let run = repo.lfs(["pointer", "--file", "good.txt"]).ok();
    repo.write("good.ptr", run.stdout());
    assert_check(&repo, "good.ptr", true, true);
}

#[test]
fn pointer_check_with_invalid_pointer() {
    let t = Test::new();
    let repo = t.init("pointer---check-invalid-pointer");
    repo.write("bad.ptr", "not-a-pointer\n");
    assert_check(&repo, "bad.ptr", false, false);
}

#[test]
fn pointer_check_with_empty_file() {
    let t = Test::new();
    let repo = t.init("pointer---check-empty-file");
    repo.write("empty.ptr", "");
    assert_check(&repo, "empty.ptr", true, true);
}

/// A pointer for an empty file is valid, but not canonical: that's an
/// empty file.
#[test]
fn pointer_check_with_size_0_pointer() {
    let t = Test::new();
    let repo = t.init("pointer---check-size-0");
    repo.write(
        "zero.ptr",
        "version https://git-lfs.github.com/spec/v1\n\
         oid sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n\
         size 0\n",
    );
    assert_check(&repo, "zero.ptr", true, false);
}

#[test]
fn pointer_check_with_crlf_endings() {
    let t = Test::new();
    let repo = t.init("pointer---check-crlf");
    repo.write(
        "crlf.ptr",
        "version https://git-lfs.github.com/spec/v1\r\n\
         oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\r\n\
         size 12345\r\n",
    );
    assert_check(&repo, "crlf.ptr", true, false);
}

#[test]
fn pointer_check_with_invalid_arguments() {
    let t = Test::new();
    let repo = t.init("pointer---check-invalid-pointer");
    repo.write("a.txt", "");
    // With --compare, without --file or --stdin, and with both.
    repo.lfs(["pointer", "--check", "--compare"]).fails();
    repo.lfs(["pointer", "--check"]).fails();
    repo.lfs(["pointer", "--check", "--file", "a.txt", "--stdin"])
        .fails();
}

/// A repository `name` with `*.dat` tracked through the case-inverting
/// extension. Returns the extension's log.
fn extension_setup<'a>(t: &'a Test, name: &str) -> (Repo<'a>, std::path::PathBuf) {
    let repo = t.init(name);
    repo.lfs(["track", "*.dat"]).ok();
    let log = repo.case_inverter_extension();
    (repo, log)
}

/// The lines of `run`'s stdout that are part of the pointer, without the
/// header or warning.
fn pointer_lines(run: &Run) -> String {
    run.stdout()
        .lines()
        .filter(|l| !l.contains("Git LFS pointer") && !l.contains("warning:"))
        .map(|l| format!("{l}\n"))
        .collect()
}

#[test]
fn pointer_with_extension() {
    let t = Test::new();
    let (repo, log) = extension_setup(&t, "pointer-extension");
    repo.write("file.dat", "abc");
    pointer(&repo, &["--file=file.dat"], "")
        .ok()
        .has("warning: Using LFS extensions")
        .has(&format!("ext-0-caseinverter sha256:{}", oid("abc")))
        .has(&format!("oid sha256:{}", oid("ABC")))
        .has("size 3");
    let log = std::fs::read_to_string(log).unwrap();
    assert!(log.contains("clean: file.dat"), "{log}");
}

#[test]
fn pointer_with_no_extensions_flag() {
    let t = Test::new();
    let (repo, log) = extension_setup(&t, "pointer-no-extensions");
    repo.write("file.dat", "abc");
    pointer(&repo, &["--file=file.dat", "--no-extensions"], "")
        .ok()
        .lacks("warning: Using LFS extensions")
        .lacks("ext-0-caseinverter")
        .has(&format!("oid sha256:{}", oid("abc")))
        .has("size 3");
    assert!(!log.exists(), "the extension ran");
}

#[test]
fn pointer_extension_vs_no_extension_comparison() {
    let t = Test::new();
    let (repo, _) = extension_setup(&t, "pointer-extension-comparison");
    repo.write("file.dat", "abc");
    let with = pointer_lines(&repo.lfs(["pointer", "--file=file.dat"]).ok());
    let without = pointer_lines(
        &repo
            .lfs(["pointer", "--file=file.dat", "--no-extensions"])
            .ok(),
    );
    assert_ne!(with, without);
    assert!(with.contains(&format!("ext-0-caseinverter sha256:{}", oid("abc"))));
    assert!(with.contains(&format!("oid sha256:{}", oid("ABC"))));
    assert!(!without.contains("ext-0-caseinverter"));
    assert!(without.contains(&format!("oid sha256:{}", oid("abc"))));
}

#[test]
fn pointer_file_stdin_with_extension() {
    let t = Test::new();
    let (repo, _) = extension_setup(&t, "pointer-extension-file-stdin");
    repo.write("file.dat", "abc");
    let expected = pointer_lines(&repo.lfs(["pointer", "--file=file.dat"]).ok());
    pointer(&repo, &["--file=file.dat", "--stdin"], &expected)
        .ok()
        .has("Git LFS pointer for file.dat")
        .has(&format!("ext-0-caseinverter sha256:{}", oid("abc")))
        .has("Pointer from STDIN")
        .lacks("Pointers do not match");
}

#[test]
fn pointer_file_stdin_with_extension_mismatch() {
    let t = Test::new();
    let (repo, _) = extension_setup(&t, "pointer-extension-file-stdin-mismatch");
    repo.write("file.dat", "abc");
    // Without the extension, so it doesn't match.
    let standard = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize 3",
        oid("abc")
    );
    let run = pointer(&repo, &["--file=file.dat", "--stdin"], &standard);
    assert_eq!(run.code(), Some(1), "{run}");
    run.has("Git LFS pointer for file.dat")
        .has("ext-0-caseinverter")
        .has("Pointer from STDIN")
        .has("Pointers do not match")
        .has("note: Mismatch may be due to differing LFS extensions");
}

#[test]
fn pointer_extension_with_multiple_files() {
    let t = Test::new();
    let (repo, log) = extension_setup(&t, "pointer-extension-multiple-files");
    let files = ["file1.dat", "file2.dat", "file3.dat"];
    for (file, contents) in files.iter().zip(["abc", "xyz", "Test123"]) {
        repo.write(file, contents);
    }
    for file in files {
        pointer(&repo, &[&format!("--file={file}")], "")
            .ok()
            .has("ext-0-caseinverter");
    }
    let log = std::fs::read_to_string(log).unwrap();
    for file in files {
        assert!(log.contains(&format!("clean: {file}")), "{log}");
    }
    assert_eq!(log.matches("clean:").count(), 3, "{log}");
}
