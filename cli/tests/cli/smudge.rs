//! `t-smudge.sh`.
//!
//! "smudge with temp file" runs in the first test's clone. Here it sets up
//! its own.

use crate::harness::{Repo, Run, Test, oid, pointer, sized};

/// The object ID of `smudge a\n`.
const A_OID: &str = "fcf5015df7a9089a7aa7fe74139d4b8f7d62e52d5a34f9a87aeffc8e8c668254";

/// Upstream's in-memory spool buffer, which the shell test gets from
/// `lfstest-getlimit --max-spool-mem-buffer-size`.
const MAX_SPOOL_MEM_BUFFER_SIZE: usize = 1024;

/// The pointer for `smudge a\n`.
fn a_pointer() -> String {
    pointer(A_OID, 9)
}

/// Run `git lfs smudge` with `args` in `repo`, with `stdin`.
fn run_smudge(repo: &Repo, args: &[&str], stdin: impl Into<Vec<u8>>) -> Run {
    repo.lfs_command()
        .args(["smudge"])
        .args(args)
        .stdin(stdin)
        .run()
}

/// The remote repository `name`, cloned into `dir`, with `a.dat` holding
/// `smudge a\n` committed. With `push`, it's pushed and the local objects
/// removed.
///
/// Before the push, smudge reads the object from the local store.
fn a_dat<'a>(t: &'a Test, name: &str, dir: &str, push: bool) -> Repo<'a> {
    t.remote(name);
    let repo = t.clone(name, dir);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "smudge a\n");
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat"]).ok();
    assert_eq!(
        run_smudge(&repo, &[], a_pointer()).ok().stdout(),
        "smudge a\n"
    );
    if push {
        repo.git(["push", "origin", "main"]).ok();
        repo.remove_local_objects();
    }
    repo
}

#[test]
fn smudge() {
    let t = Test::new();

    // Downloads it from the server.
    let repo = a_dat(&t, "t-smudge", "repo", true);

    assert_eq!(
        run_smudge(&repo, &[], a_pointer()).ok().stdout(),
        "smudge a\n"
    );
}

/// A leftover temporary file for the object doesn't get in the way.
#[test]
fn smudge_with_temp_file() {
    let t = Test::new();
    let repo = a_dat(&t, "t-smudge", "repo", true);
    repo.write(&format!(".git/lfs/tmp/objects/{A_OID}-1"), "");

    let run = repo
        .lfs_command()
        .args(["smudge"])
        .stdin(a_pointer())
        .env("GIT_TRACE", "5")
        .run()
        .ok();

    assert_eq!(run.stdout(), "smudge a\n");
}

/// Input that isn't a pointer comes out as it went in, also when it's more
/// than the in-memory spool buffer (upstream's PRs 1922, 1932, and 5617).
#[test]
fn smudge_with_invalid_pointer() {
    let t = Test::new();
    let repo = t.init("repo");
    let spool = sized("spool", MAX_SPOOL_MEM_BUFFER_SIZE * 2);

    for input in ["wat\n", "not a git-lfs file\n", "version \n", &spool] {
        let run = run_smudge(&repo, &[], input).ok();
        assert_eq!(run.stdout(), input);
    }
}

#[test]
fn smudge_with_pointer_extension() {
    let t = Test::new();
    let repo = t.init("smudge-pointer-extension");
    let log = repo.case_inverter_extension();
    repo.lfs(["track", "*.dat"]).ok();
    let contents = "abc\ndef";
    let inverted = "ABC\nDEF";
    repo.write("dir1/abc.dat", contents);
    repo.git(["add", ".gitattributes", "dir1"]).ok();
    repo.assert_local_object(&oid(inverted), 7);
    let pointer = format!(
        "version https://git-lfs.github.com/spec/v1\n\
         ext-0-caseinverter sha256:{}\n\
         oid sha256:{}\n\
         size 7\n",
        oid(contents),
        oid(inverted)
    );

    let run = run_smudge(&repo, &["--", "dir1/abc.dat"], pointer).ok();

    assert_eq!(run.stdout(), contents);
    let log = std::fs::read_to_string(log).unwrap();
    assert!(log.contains("smudge: dir1/abc.dat"), "{log}");
}

/// Check that smudging `path` in `repo` downloads the object, or gives
/// back the pointer if `downloads` is false.
#[track_caller]
fn assert_smudges(repo: &Repo, path: &str, downloads: bool) {
    let run = run_smudge(repo, &[path], a_pointer()).ok();
    let expected = if downloads {
        "smudge a\n".to_owned()
    } else {
        a_pointer()
    };
    assert_eq!(run.stdout(), expected, "{path}");
}

#[test]
fn smudge_include_exclude() {
    let t = Test::new();
    let repo = a_dat(&t, "t-smudge-includeexclude", "includeexclude", true);

    repo.git(["config", "lfs.fetchexclude", "a*"]).ok();
    assert_smudges(&repo, "a.dat", false);

    repo.write("foo/a.dat", "smudge a\n");
    repo.write("foo/bar/a.dat", "smudge a\n");
    repo.git(["add", "foo"]).ok();
    repo.git(["commit", "-m", "add foo"]).ok();
    repo.git(["push", "origin", "main"]).ok();
    repo.remove_local_objects();
    repo.git(["config", "lfs.fetchinclude", "/foo"]).ok();
    repo.git(["config", "lfs.fetchexclude", "/foo/bar"]).ok();

    assert_smudges(&repo, "a.dat", false);
    assert_smudges(&repo, "foo/a.dat", true);
    assert_smudges(&repo, "foo/bar/a.dat", false);
}

/// Clone the remote repository `name` into `dir`, with `env` set.
fn clone_with<'a>(t: &'a Test, name: &str, dir: &str, env: &[(&str, &str)]) -> Repo<'a> {
    let mut cmd = t
        .command(t.dir(), "git")
        .args(["clone", &t.remote_url(name), dir]);
    for (key, value) in env {
        cmd = cmd.env(key, value);
    }
    cmd.run().ok();
    t.repo(dir)
}

#[test]
fn smudge_with_skip() {
    let t = Test::new();
    let name = "t-smudge-skip";
    let repo = a_dat(&t, name, "skip", true);

    // Skipping only stops downloads, so the local objects are gone.
    let run = repo
        .lfs_command()
        .args(["smudge"])
        .stdin(a_pointer())
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .run()
        .ok();
    assert_eq!(run.stdout(), a_pointer());

    // A clone with the variable set.
    let clone = clone_with(&t, name, "skip-clone-env", &[("GIT_LFS_SKIP_SMUDGE", "1")]);
    assert_eq!(clone.read("a.dat"), a_pointer());
    clone.lfs(["pull"]).ok();
    assert_eq!(clone.read("a.dat"), "smudge a\n");

    // Without it.
    let clone = clone_with(&t, name, "no-skip", &[]);
    assert_eq!(clone.read("a.dat"), "smudge a\n");

    // With `install --skip-smudge`.
    clone.lfs(["install", "--skip-smudge"]).ok();
    let skipped = clone_with(&t, name, "skip-clone-init", &[]);
    assert_eq!(skipped.read("a.dat"), a_pointer());
    skipped.lfs(["install", "--force"]).ok();
}

#[test]
fn smudge_clone_with_include_exclude() {
    let t = Test::new();
    let name = "smudge_include_exclude";
    t.remote(name);
    let repo = t.clone(name, &format!("repo_{name}"));
    repo.lfs_command()
        .args(["track", "*.dat"])
        .merged()
        .run()
        .ok()
        .has("Tracking \"*.dat\"");
    let a = oid("a");
    repo.write("a.dat", "a");
    repo.git(["add", "a.dat"]).ok();
    repo.git(["add", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "add a.dat"])
        .ok()
        .has("main (root-commit)")
        .has("2 files changed")
        .has("create mode 100644 a.dat")
        .has("create mode 100644 .gitattributes");
    assert_eq!(repo.read("a.dat"), "a");
    repo.assert_local_object(&a, 1);
    repo.command("git")
        .args(["push", "origin", "main"])
        .merged()
        .run()
        .ok()
        .has("Uploading LFS objects: 100% (1/1), 1 B")
        .has("main -> main");
    t.assert_server_objects(name, &[&a], &[]);

    // The clone succeeds without downloading.
    let clone_dir = format!("clone_{name}");
    t.repo("")
        .git([
            "-c",
            "lfs.fetchexclude=a*",
            "clone",
            &t.remote_url(name),
            &clone_dir,
        ])
        .ok();
    t.repo(&clone_dir).refute_local_object(&a);
    std::fs::remove_dir_all(t.dir().join(&clone_dir)).unwrap();

    let b = oid("b");
    let c = oid("c");
    repo.write("foo/b.dat", "b");
    repo.write("foo/bar/c.dat", "c");
    repo.git(["add", "foo"]).ok();
    repo.git(["commit", "-m", "add foo"]).ok();
    repo.assert_local_object(&b, 1);
    repo.assert_local_object(&c, 1);
    repo.git(["push", "origin", "main"]).ok();
    t.assert_server_objects(name, &[&b, &c], &[]);

    t.git_config_global("lfs.fetchinclude", "/foo");
    t.git_config_global("lfs.fetchexclude", "/foo/bar");
    let clone = t.clone(name, &clone_dir);
    clone.refute_local_object(&a);
    clone.assert_local_object(&b, 1);
    clone.refute_local_object(&c);
}

#[test]
fn smudge_skip_download_failure() {
    let t = Test::new();
    let repo = a_dat(&t, "t-smudge-skipdownloadfail", "skipdownloadfail", true);
    repo.git(["remote", "set-url", "origin", "httpnope://nope.com/nope"])
        .ok();
    // Git LFS reads this URL as SSH to the host `httpnope`. Fail at once,
    // instead of waiting for that name to not resolve.
    repo.git(["config", "core.sshCommand", "false"]).ok();

    run_smudge(&repo, &["a.dat"], a_pointer()).fails();

    repo.git(["config", "lfs.skipdownloaderrors", "true"]).ok();
    let run = run_smudge(&repo, &["a.dat"], a_pointer()).ok();
    assert_eq!(run.stdout(), a_pointer());

    repo.git(["config", "--unset", "lfs.skipdownloaderrors"])
        .ok();
    repo.lfs_command()
        .args(["smudge", "a.dat"])
        .stdin(a_pointer())
        .env("GIT_LFS_SKIP_DOWNLOAD_ERRORS", "1")
        .run()
        .ok();
}

/// The only remote isn't `origin`, and there's no ref to say which to
/// use, so smudge uses that one.
#[test]
fn smudge_no_ref_non_origin() {
    let t = Test::new();
    let name = "t-smudge-no-ref-non-origin";
    let first = a_dat(&t, name, &format!("{name}-1"), false);
    first.git(["push", "origin", "main"]).ok();
    let main = first.rev_parse("main");

    let repo = t.init(name);
    repo.git(["config", "remote.random.url", &t.remote_url(name)])
        .ok();
    repo.git(["fetch", &t.remote_url(name)]).ok();
    repo.git(["checkout", &main]).ok();

    assert_eq!(repo.read("a.dat"), "smudge a\n");
}
