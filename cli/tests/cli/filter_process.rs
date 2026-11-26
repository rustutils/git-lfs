//! `t-filter-process.sh`.
//!
//! The shell suite skips itself with Git older than 2.11, which lacks the
//! filter protocol. These assume a newer Git.
//!
//! "git archive does not invoke SSH" needs the pure SSH setup, and is left
//! for the SSH suites. "ignore file size on disk when cleaning" puts
//! upstream's `lfstest-badlocalfile` between Git and the filter, to empty
//! the file while Git sends it. Here the test talks to the filter itself,
//! with the file already empty.

use crate::harness::{Repo, Test, oid, pointer};

/// Upstream's largest pointer, from `lfstest-getlimit --max-pointer-size`.
const MAX_POINTER_SIZE: usize = 1024;

/// Git's largest pkt-line, from `lfstest-getlimit --max-pktline-len`.
const MAX_PKTLINE_LEN: usize = 65520;

/// `git` with the filter settings that make it use `filter-process`, as
/// the shell tests pass with `-c`.
const FILTER_PROCESS_ONLY: [&str; 8] = [
    "-c",
    "filter.lfs.process=git-lfs filter-process",
    "-c",
    "filter.lfs.clean=false",
    "-c",
    "filter.lfs.smudge=false",
    "-c",
    "filter.lfs.required=true",
];

/// Clone the remote repository `name` into `dir` with only
/// `filter-process` configured.
fn clone_with_filter_process<'a>(t: &'a Test, name: &str, dir: &str) -> Repo<'a> {
    t.repo("")
        .git(
            FILTER_PROCESS_ONLY
                .iter()
                .copied()
                .chain(["clone", &t.remote_url(name), dir]),
        )
        .ok();
    t.repo(dir)
}

/// The remote repository `name`, cloned into `dir`, with `*.dat` tracked
/// and `a.dat` on `main` and `b.dat` on `b`, which is checked out.
fn branches_a_and_b<'a>(t: &'a Test, name: &str, dir: &str) -> Repo<'a> {
    t.remote(name);
    let repo = t.clone(name, dir);
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.write("a.dat", "contents_a");
    repo.git(["add", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();
    repo.git(["checkout", "-b", "b"]).ok();
    repo.write("b.dat", "contents_b");
    repo.git(["add", "b.dat"]).ok();
    repo.git(["commit", "-m", "add b.dat"]).ok();
    repo
}

/// The current branch's name.
fn branch(repo: &Repo) -> String {
    repo.git(["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .stdout()
        .trim_end()
        .to_owned()
}

#[test]
fn filter_process_checking_out_a_branch() {
    let t = Test::new();
    let name = "filter_process_checkout";
    let repo = branches_a_and_b(&t, name, "repo");
    repo.git(["push", "origin", "--all"]).ok();

    let clone = clone_with_filter_process(&t, name, &format!("{name}-assert"));

    assert_eq!(branch(&clone), "main");
    assert_eq!(clone.read("a.dat"), "contents_a");
    clone.assert_pointer("main", "a.dat", &oid("contents_a"), 10);
    clone.git(["checkout", "b"]).ok();
    assert_eq!(branch(&clone), "b");
    assert_eq!(clone.read("b.dat"), "contents_b");
    clone.assert_pointer("b", "b.dat", &oid("contents_b"), 10);
}

#[test]
fn filter_process_include_exclude() {
    let t = Test::new();
    let name = "t-filter-process-includeexclude";
    t.remote(name);
    let repo = t.clone(name, name);
    repo.lfs(["track", "*.dat"]).ok();
    for path in ["a.dat", "foo/a.dat", "foo/bar/a.dat"] {
        repo.write(path, "contents_a");
    }
    repo.git(["add", ".gitattributes", "a.dat", "foo"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.git(["push", "origin", "main"]).ok();
    t.git_config_global("lfs.fetchinclude", "/foo");
    t.git_config_global("lfs.fetchexclude", "/foo/bar");

    let clone = clone_with_filter_process(&t, name, &format!("{name}-assert"));

    let a = oid("contents_a");
    assert_eq!(clone.read("a.dat"), pointer(&a, 10));
    assert_eq!(clone.read("foo/a.dat"), "contents_a");
    assert_eq!(clone.read("foo/bar/a.dat"), pointer(&a, 10));
    for path in ["a.dat", "foo/a.dat", "foo/bar/a.dat"] {
        clone.assert_pointer("main", path, &a, 10);
    }
}

#[test]
fn filter_process_adding_a_file() {
    let t = Test::new();
    let name = "filter_process_add";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.write("a.dat", "contents");

    repo.git(["add", "a.dat"]).ok();

    let staged = repo.git(["cat-file", "-p", ":a.dat"]).ok().stdout();
    assert_eq!(staged, pointer(&oid("contents"), 8));
}

#[test]
fn filter_process_pointer_extension() {
    let t = Test::new();
    let repo = t.init("filter-process-pointer-extension");
    // So Git uses `filter.lfs.process`.
    repo.git(["config", "--global", "--unset", "filter.lfs.clean"])
        .ok();
    repo.git(["config", "--global", "--unset", "filter.lfs.smudge"])
        .ok();
    let log = repo.case_inverter_extension();
    repo.lfs(["track", "*.dat"]).ok();
    let contents = "abc\ndef";
    let inverted = "ABC\nDEF";
    repo.write("dir1/abc.dat", contents);
    repo.git(["add", ".gitattributes", "dir1"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();

    let staged = repo.git(["cat-file", "-p", ":dir1/abc.dat"]).ok().stdout();
    assert_eq!(
        staged,
        format!(
            "version https://git-lfs.github.com/spec/v1\n\
             ext-0-caseinverter sha256:{}\n\
             oid sha256:{}\n\
             size 7\n",
            oid(contents),
            oid(inverted)
        )
    );
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("clean: dir1/abc.dat"), "{logged}");
    repo.assert_local_object(&oid(inverted), 7);

    // Checked out from the top, then from a subdirectory.
    for dir in ["", "dir2"] {
        std::fs::remove_dir_all(repo.path().join("dir1")).unwrap();
        std::fs::remove_file(&log).unwrap();
        let cwd = if dir.is_empty() {
            repo.at("")
        } else {
            std::fs::create_dir_all(repo.path().join(dir)).unwrap();
            repo.at(dir)
        };
        let pathspec = if dir.is_empty() { "." } else { ".." };

        cwd.git(["checkout", "--", pathspec]).ok();

        assert_eq!(repo.read("dir1/abc.dat"), contents, "from {dir:?}");
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(logged.contains("smudge: dir1/abc.dat"), "{logged}");
    }
}

/// A file as large as a read buffer (upstream's issue 1697).
#[test]
fn filter_process_non_pointer_file_of_maximum_pointer_size() {
    let t = Test::new();
    let repo = t.init("repo-file-max-pointer-size");
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("first.dat", vec![0u8; MAX_POINTER_SIZE]);
    repo.write("second.dat", "any contents");

    repo.git(["add", "."]).ok();
}

/// `git hash-object --stdin --path` runs the filter on stdin, whatever is
/// on disk at the path (upstream's issue 3884).
#[test]
fn filter_process_hash_object_stdin_path_does_not_hang() {
    let t = Test::new();
    let repo = t.init("repo-hash-object");
    repo.lfs(["track", "*.dat"]).ok();
    let hash_pointer = |contents: &[u8]| {
        repo.command("git")
            .args(["hash-object", "--stdin"])
            .stdin(pointer(&oid(contents), contents.len()))
            .run()
            .ok()
            .stdout()
    };
    let hash_at = |path: &str, contents: &[u8]| {
        repo.command("git")
            .args(["hash-object", "--path", path, "--stdin"])
            .stdin(contents.to_vec())
            .run()
            .ok()
            .stdout()
    };
    repo.write("first.dat", vec![0u8; MAX_POINTER_SIZE * 1000 / 1024]);
    repo.write("second.dat", "a\n");

    // Longer than the input, shorter, and absent.
    let expected = hash_pointer(b"test");
    for path in ["first.dat", "second.dat", "third.dat"] {
        assert_eq!(hash_at(path, b"test"), expected, "{path}");
    }

    // Larger than a pkt-line.
    let large = vec![0u8; MAX_PKTLINE_LEN + MAX_POINTER_SIZE + 1];
    repo.write("large.dat", &large);
    assert_eq!(hash_at("large.dat", &large), hash_pointer(&large));

    repo.git(["add", "."]).ok();
}

#[test]
fn filter_process_checking_out_a_branch_with_skip_smudge_and_checkout_index() {
    let t = Test::new();
    let repo = branches_a_and_b(&t, "filter-process-skip-smudge-checkout-index", "repo");
    repo.lfs(["install", "--local", "--skip-smudge"]).ok();
    repo.git(["checkout", "main"]).ok();

    std::fs::remove_file(repo.path().join("a.dat")).unwrap();
    repo.git(["checkout-index", "-af"]).ok();
    repo.lfs(["pointer", "--check", "--file", "a.dat"]).ok();
    repo.assert_pointer("main", "a.dat", &oid("contents_a"), 10);

    repo.git(["checkout", "b"]).ok();
    for path in ["a.dat", "b.dat"] {
        std::fs::remove_file(repo.path().join(path)).unwrap();
    }
    repo.git(["checkout-index", "-af"]).ok();
    repo.lfs(["pointer", "--check", "--file", "a.dat"]).ok();
    repo.lfs(["pointer", "--check", "--file", "b.dat"]).ok();
    repo.assert_pointer("b", "b.dat", &oid("contents_b"), 10);
}

/// One pkt-line holding `data`.
fn pkt(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", data.len() + 4).into_bytes();
    out.extend_from_slice(data);
    out
}

const FLUSH: &[u8] = b"0000";

/// Split `stream` into pkt-lines, with `None` for each flush.
fn pkts(mut stream: &[u8]) -> Vec<Option<Vec<u8>>> {
    let mut out = Vec::new();
    while !stream.is_empty() {
        let len = usize::from_str_radix(std::str::from_utf8(&stream[..4]).unwrap(), 16).unwrap();
        if len == 0 {
            out.push(None);
            stream = &stream[4..];
        } else {
            out.push(Some(stream[4..len].to_vec()));
            stream = &stream[len..];
        }
    }
    out
}

/// The file on disk is smaller than what Git sends, and smaller than a
/// read buffer (upstream's PR 6011).
#[test]
fn filter_process_ignore_file_size_on_disk_when_cleaning() {
    let t = Test::new();
    let repo = t.remote_and_clone("filter-process-ignore-file-size");
    repo.lfs(["track", "*.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    let contents = vec![0u8; MAX_POINTER_SIZE * 2];
    repo.write("large.dat", "");

    let mut input = Vec::new();
    for line in [&b"git-filter-client\n"[..], b"version=2\n"] {
        input.extend(pkt(line));
    }
    input.extend(FLUSH);
    for line in [&b"capability=clean\n"[..], b"capability=smudge\n"] {
        input.extend(pkt(line));
    }
    input.extend(FLUSH);
    for line in [&b"command=clean\n"[..], b"pathname=large.dat\n"] {
        input.extend(pkt(line));
    }
    input.extend(FLUSH);
    input.extend(pkt(&contents));
    input.extend(FLUSH);
    let run = repo
        .lfs_command()
        .args(["filter-process"])
        .stdin(input)
        .run()
        .ok();

    assert!(!run.stderr().contains("unknown command"), "{run}");
    // After the handshake's two flushes: the status, then the content.
    let lines = pkts(run.stdout().as_bytes());
    let mut flushes = lines.iter().enumerate().filter(|(_, l)| l.is_none());
    let start = flushes.nth(1).expect("no handshake").0 + 1;
    let response = &lines[start..];
    assert_eq!(
        response[0].as_deref(),
        Some(&b"status=success\n"[..]),
        "{run}"
    );
    assert_eq!(response[1], None, "{run}");
    let content: Vec<u8> = response[2..]
        .iter()
        .map_while(|l| l.clone())
        .flatten()
        .collect();
    assert_eq!(
        String::from_utf8(content).unwrap(),
        pointer(&oid(&contents), contents.len())
    );
}
