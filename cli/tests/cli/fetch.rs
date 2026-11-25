//! `t-fetch.sh`.

use serde_json::{Value, json};

use crate::harness::{Commit, Repo, Test, oid};

const NAME: &str = "t-fetch";

/// The shell suite's "init for fetch tests": the remote `t-fetch` with
/// `a.dat` and `empty.dat` on main and `b.dat` on newbranch, the
/// repository `repo` that pushed them, a clone in `clone`, and a shared
/// clone of that in `shared`.
fn setup(t: &Test) {
    let a = oid("a");
    let b = oid("b");
    t.remote(NAME);
    let repo = t.clone(NAME, "repo");
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    repo.write("a.dat", "a");
    repo.git(["add", "a.dat", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "add a.dat"])
        .ok()
        .has("main (root-commit)")
        .has("2 files changed")
        .has("create mode 100644 a.dat")
        .has("create mode 100644 .gitattributes");
    assert_eq!(repo.read("a.dat"), "a");
    repo.assert_local_object(&a, 1);
    assert!(t.server().object(NAME, &a).is_none());

    repo.git(["push", "origin", "main"])
        .ok()
        .has("Uploading LFS objects: 100% (1/1), 1 B")
        .has("main -> main");
    assert!(t.server().object(NAME, &a).is_some());

    repo.write("empty.dat", "");
    repo.git(["add", "empty.dat"]).ok();
    repo.git(["commit", "-m", "empty"]).ok();

    repo.git(["checkout", "-b", "newbranch"]).ok();
    repo.write("b.dat", "b");
    repo.git(["add", "b.dat"]).ok();
    repo.git(["commit", "-m", "add b.dat"]).ok();
    repo.assert_local_object(&b, 1);
    repo.git(["push", "origin", "newbranch"]).ok();
    assert!(t.server().object(NAME, &b).is_some());

    t.clone(NAME, "clone");
    t.command(t.dir(), "git")
        .args(["clone", "--shared", "clone", "shared"])
        .run()
        .ok();
}

/// The clone from [`setup`], with its LFS objects removed.
fn clone(t: &Test) -> Repo<'_> {
    setup(t);
    let clone = t.repo("clone");
    clone.remove_local_objects();
    clone
}

/// [`clone`], with a local newbranch, as the shell tests have after "fetch
/// with remote and branches".
fn clone_with_newbranch(t: &Test) -> Repo<'_> {
    let clone = clone(t);
    clone.git(["checkout", "newbranch"]).ok();
    clone.git(["checkout", "main"]).ok();
    clone.remove_local_objects();
    clone
}

#[track_caller]
fn assert_fsck_ok(repo: &Repo) {
    repo.lfs(["fsck"]).ok().has("Git LFS fsck OK");
}

/// The `--json` output for the object `contents`, in `file`.
fn transfer(t: &Test, repo: &Repo, file: &str, contents: &str) -> Value {
    let oid = oid(contents);
    json!({
        "name": file,
        "oid": oid,
        "size": contents.len(),
        "actions": {
            "download": {
                "href": format!("{}/storage/{oid}?r={NAME}", t.server().url()),
                "expires_at": "0001-01-01T00:00:00Z",
            },
        },
        "path": repo.local_object_path(&oid),
    })
}

/// The transfers in `--json` output, sorted by name.
fn transfers(output: &str) -> Vec<Value> {
    let value: Value = serde_json::from_str(output).unwrap();
    let mut transfers = value["transfers"].as_array().unwrap().clone();
    transfers.sort_by_key(|t| t["name"].as_str().unwrap().to_owned());
    transfers
}

#[test]
fn init_for_fetch_tests() {
    let t = Test::new();
    setup(&t);
}

#[test]
fn fetch() {
    let t = Test::new();
    let clone = clone(&t);
    let a = oid("a");

    clone
        .lfs(["fetch", "--dry-run"])
        .ok()
        .has(&format!("fetch {a} => a.dat"));
    clone.refute_local_object(&a);

    clone.lfs(["fetch"]).ok();
    clone.assert_local_object(&a, 1);
    assert_fsck_ok(&clone);

    clone.lfs(["fetch", "--dry-run"]).ok().lacks("=> a.dat");
}

#[test]
fn fetch_json() {
    let t = Test::new();
    let clone = clone(&t);

    let dry_run = clone.lfs(["fetch", "--dry-run", "--json"]).ok();
    let fetch = clone.lfs(["fetch", "--json"]).ok();
    clone.assert_local_object(&oid("a"), 1);
    let expected = [transfer(&t, &clone, "a.dat", "a")];
    assert_eq!(transfers(&dry_run.stdout()), expected);
    assert_eq!(transfers(&fetch.stdout()), expected);

    let fetch = clone.lfs(["fetch", "--json"]).ok();
    assert_eq!(
        serde_json::from_str::<Value>(&fetch.stdout()).unwrap(),
        json!({ "transfers": [] })
    );
}

#[test]
fn fetch_refetch() {
    let t = Test::new();
    let clone = clone(&t);
    let a = oid("a");

    clone.lfs(["fetch"]).ok();
    clone.assert_local_object(&a, 1);

    clone.corrupt_local_object(&a);
    clone.lfs(["fetch", "--refetch"]).ok();
    clone.assert_local_object(&a, 1);
    assert_fsck_ok(&clone);
}

#[test]
fn fetch_shared_repository() {
    let t = Test::new();
    setup(&t);
    let shared = t.repo("shared");
    shared.remove_local_objects();

    shared.lfs(["fetch"]).lacks("Could not scan");
    shared.assert_local_object(&oid("a"), 1);
    assert_fsck_ok(&shared);
}

#[test]
fn fetch_with_remote() {
    let t = Test::new();
    let clone = clone(&t);

    clone.lfs(["fetch", "origin"]).ok();
    clone.assert_local_object(&oid("a"), 1);
    clone.refute_local_object(&oid("b"));
    assert_fsck_ok(&clone);
}

#[test]
fn fetch_with_remote_and_branches() {
    let t = Test::new();
    let clone = clone_with_newbranch(&t);
    let (a, b) = (oid("a"), oid("b"));
    let dry_run = || {
        clone
            .lfs(["fetch", "origin", "main", "newbranch", "--dry-run"])
            .ok()
    };
    let stdin_fetch = |extra: &[&str]| {
        clone
            .lfs_command()
            .args(["fetch", "origin", "--stdin"])
            .args(extra)
            .stdin("main\nnewbranch\n")
            .run()
            .ok()
    };

    dry_run()
        .has(&format!("fetch {a} => a.dat"))
        .has(&format!("fetch {b} => b.dat"));
    clone.refute_local_object(&a);
    clone.refute_local_object(&b);

    clone.lfs(["fetch", "origin", "main", "newbranch"]).ok();
    clone.assert_local_object(&a, 1);
    clone.assert_local_object(&b, 1);
    assert_fsck_ok(&clone);
    dry_run().lacks("=> a.dat").lacks("=> b.dat");

    clone.remove_local_objects();

    stdin_fetch(&["--dry-run"])
        .has(&format!("fetch {a} => a.dat"))
        .has(&format!("fetch {b} => b.dat"));
    clone.refute_local_object(&a);
    clone.refute_local_object(&b);

    stdin_fetch(&[]);
    clone.assert_local_object(&a, 1);
    clone.assert_local_object(&b, 1);
    assert_fsck_ok(&clone);
    dry_run().lacks("=> a.dat").lacks("=> b.dat");
}

#[test]
fn fetch_json_with_remote_and_branches() {
    let t = Test::new();
    let clone = clone_with_newbranch(&t);

    let dry_run = clone
        .lfs([
            "fetch",
            "origin",
            "main",
            "newbranch",
            "--json",
            "--dry-run",
        ])
        .ok();
    clone.refute_local_object(&oid("a"));
    clone.refute_local_object(&oid("b"));

    let fetch = clone
        .lfs(["fetch", "origin", "main", "newbranch", "--json"])
        .ok();
    clone.assert_local_object(&oid("a"), 1);
    clone.assert_local_object(&oid("b"), 1);

    // In either order.
    let expected = [
        transfer(&t, &clone, "a.dat", "a"),
        transfer(&t, &clone, "b.dat", "b"),
    ];
    assert_eq!(transfers(&dry_run.stdout()), expected);
    assert_eq!(transfers(&fetch.stdout()), expected);
}

#[test]
fn fetch_refetch_with_remote_and_branches() {
    let t = Test::new();
    let clone = clone_with_newbranch(&t);
    let (a, b) = (oid("a"), oid("b"));

    clone.lfs(["fetch", "origin", "main", "newbranch"]).ok();
    clone.assert_local_object(&a, 1);
    clone.assert_local_object(&b, 1);

    clone.corrupt_local_object(&a);
    clone.corrupt_local_object(&b);

    let run = clone
        .lfs([
            "fetch",
            "--refetch",
            "--json",
            "origin",
            "main",
            "newbranch",
        ])
        .ok();
    clone.assert_local_object(&a, 1);
    clone.assert_local_object(&b, 1);

    // a.dat is fetched only once.
    assert_eq!(run.count(r#""name": "a.dat""#), 1, "{run}");
    assert_fsck_ok(&clone);
}

#[test]
fn fetch_with_main_commit_sha1() {
    let t = Test::new();
    let clone = clone_with_newbranch(&t);

    let main = clone.git(["rev-parse", "main"]).ok().stdout();
    clone.lfs(["fetch", "origin", main.trim()]).ok();
    clone.assert_local_object(&oid("a"), 1);
    clone.refute_local_object(&oid("b"));
    assert_fsck_ok(&clone);
}

#[test]
fn fetch_with_newbranch_commit_sha1() {
    let t = Test::new();
    let clone = clone_with_newbranch(&t);

    let newbranch = clone.git(["rev-parse", "newbranch"]).ok().stdout();
    clone.lfs(["fetch", "origin", newbranch.trim()]).ok();
    clone.assert_local_object(&oid("a"), 1);
    clone.assert_local_object(&oid("b"), 1);
    assert_fsck_ok(&clone);
}

/// Fetch main and newbranch with `args` and the config `config`, and
/// check which of a.dat and b.dat were fetched.
#[track_caller]
fn filtered_fetch<'a>(
    t: &'a Test,
    config: &[(&str, &str)],
    args: &[&str],
    a: bool,
    b: bool,
) -> Repo<'a> {
    let clone = clone_with_newbranch(t);
    for (key, value) in config {
        clone.git(["config", key, value]).ok();
    }
    let mut fetch = vec!["fetch"];
    fetch.extend(args);
    fetch.extend(["origin", "main", "newbranch"]);
    clone.lfs(fetch).ok();
    for (contents, fetched) in [("a", a), ("b", b)] {
        if fetched {
            clone.assert_local_object(&oid(contents), 1);
        } else {
            clone.refute_local_object(&oid(contents));
        }
    }
    clone
}

#[test]
fn fetch_with_include_filters_in_gitconfig() {
    let t = Test::new();
    let clone = filtered_fetch(&t, &[("lfs.fetchinclude", "a*")], &[], true, false);
    assert_fsck_ok(&clone);
}

#[test]
fn fetch_with_exclude_filters_in_gitconfig() {
    let t = Test::new();
    let clone = filtered_fetch(&t, &[("lfs.fetchexclude", "a*")], &[], false, true);
    assert_fsck_ok(&clone);
}

#[test]
fn fetch_with_include_exclude_filters_in_gitconfig() {
    let t = Test::new();
    let config = [("lfs.fetchinclude", "a*,b*"), ("lfs.fetchexclude", "c*,d*")];
    let clone = filtered_fetch(&t, &config, &[], true, true);

    clone.remove_local_objects();
    clone.git(["config", "lfs.fetchinclude", "c*,d*"]).ok();
    clone.git(["config", "lfs.fetchexclude", "a*,b*"]).ok();
    clone.lfs(["fetch", "origin", "main", "newbranch"]).ok();
    clone.refute_local_object(&oid("a"));
    clone.refute_local_object(&oid("b"));
}

#[test]
fn fetch_with_include_filter_in_cli() {
    let t = Test::new();
    filtered_fetch(&t, &[], &["--include=a*"], true, false);
}

#[test]
fn fetch_with_exclude_filter_in_cli() {
    let t = Test::new();
    filtered_fetch(&t, &[], &["--exclude=a*"], false, true);
}

#[test]
fn fetch_with_include_exclude_filters_in_cli() {
    let t = Test::new();
    let clone = filtered_fetch(&t, &[], &["-I", "a*,b*", "-X", "c*,d*"], true, true);

    clone.remove_local_objects();
    clone
        .lfs([
            "fetch",
            "--include=c*,d*",
            "--exclude=a*,b*",
            "origin",
            "main",
            "newbranch",
        ])
        .ok();
    clone.refute_local_object(&oid("a"));
    clone.refute_local_object(&oid("b"));
}

#[test]
fn fetch_with_include_filter_overriding_exclude_filter() {
    let t = Test::new();
    let clone = clone_with_newbranch(&t);
    clone.git(["config", "lfs.fetchexclude", "b*"]).ok();
    clone
        .lfs([
            "fetch",
            "-I",
            "b.dat",
            "-X",
            "",
            "origin",
            "main",
            "newbranch",
        ])
        .ok();
    clone.assert_local_object(&oid("b"), 1);
}

#[test]
fn fetch_with_missing_object() {
    let t = Test::new();
    let clone = clone_with_newbranch(&t);
    t.server().delete_object(NAME, &oid("b"));

    // It fails, but fetches the other objects.
    clone.lfs(["fetch", "origin", "main", "newbranch"]).fails();
    clone.assert_local_object(&oid("a"), 1);
    clone.refute_local_object(&oid("b"));
}

#[test]
fn fetch_does_not_crash_on_empty_key_files() {
    let t = Test::new();
    let clone = clone_with_newbranch(&t);
    clone
        .git(["config", "--local", "http.sslKey", "/dev/null"])
        .ok();
    clone
        .git(["config", "--local", "http.sslCert", "/dev/null"])
        .ok();
    clone
        .lfs(["fetch", "origin", "main"])
        .has("Error decoding PEM block");
}

#[test]
fn fetch_all() {
    let t = Test::new();
    let name = "fetch-all";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    let content: Vec<String> = (0..12).map(|i| format!("filecontent{i}")).collect();
    let c = |i: usize| content[i].as_str();
    repo.add_commits(&[
        Commit::new()
            .days_ago(180)
            .file("file1.dat", c(0))
            .file("file2.dat", c(1)),
        Commit::new()
            .new_branch("branch1")
            .days_ago(140)
            .file("file3.dat", c(2)),
        Commit::new()
            .on("main")
            .days_ago(100)
            .file("file1.dat", c(3)),
        Commit::new()
            .new_branch("remote_branch_only")
            .days_ago(80)
            .file("file2.dat", c(4)),
        Commit::new()
            .on("main")
            .days_ago(75)
            .file("file4.dat", c(5)),
        Commit::new()
            .new_branch("tag_only")
            .tag("tag1")
            .days_ago(70)
            .file("file4.dat", c(6)),
        Commit::new()
            .on("main")
            .days_ago(60)
            .file("file1.dat", c(7)),
        Commit::new()
            .new_branch("branch3")
            .days_ago(50)
            .file("file4.dat", c(8)),
        Commit::new()
            .on("main")
            .days_ago(40)
            .file("file1.dat", c(9))
            .file("file2.dat", c(10)),
        Commit::new()
            .on("main")
            .days_ago(30)
            .file("file4.dat", c(11)),
    ]);
    for r in [
        "main",
        "branch1",
        "branch3",
        "remote_branch_only",
        "tag_only",
        "tag1",
    ] {
        repo.git(["push", "origin", r]).ok();
    }
    for contents in &content {
        assert!(t.server().object(name, &oid(contents)).is_some());
    }

    // Objects only on a remote branch, or only reachable from a tag, are
    // fetched too.
    repo.git(["branch", "-D", "remote_branch_only"]).ok();
    repo.git(["branch", "-D", "tag_only"]).ok();

    // Fetch `args`, with `stdin`, and check exactly the objects `fetched`
    // are local.
    let check = |repo: &Repo, args: &[&str], stdin: &str, fetched: &[usize]| {
        repo.remove_local_objects();
        repo.lfs_command().args(args).stdin(stdin).run().ok();
        for (i, contents) in content.iter().enumerate() {
            if fetched.contains(&i) {
                repo.assert_local_object(&oid(contents), contents.len());
            } else {
                repo.refute_local_object(&oid(contents));
            }
        }
    };
    let all: Vec<usize> = (0..12).collect();
    let main = [0, 1, 3, 5, 7, 9, 10, 11];
    let branch1_tag1 = [0, 1, 2, 3, 5, 6];

    check(&repo, &["fetch", "--all", "origin"], "", &all);
    check(&repo, &["fetch", "--all", "origin", "main"], "", &main);
    let args = ["fetch", "--all", "origin", "branch1", "tag1"];
    check(&repo, &args, "", &branch1_tag1);

    // The same, in a bare clone.
    t.command(t.dir(), "git")
        .args(["clone", "--bare", &t.remote_url(name), "fetch-all-bare"])
        .run()
        .ok();
    let bare = t.repo("fetch-all-bare");
    check(&bare, &["fetch", "--all", "origin"], "", &all);
    check(&bare, &["fetch", "--all", "origin", "main"], "", &main);
    check(
        &bare,
        &["fetch", "--all", "--stdin", "origin"],
        "main\n",
        &main,
    );
    check(&bare, &args, "", &branch1_tag1);
    let stdin = "branch1\ntag1\n\n";
    check(
        &bare,
        &["fetch", "--all", "--stdin", "origin"],
        stdin,
        &branch1_tag1,
    );
}

#[test]
fn fetch_outside_git_repository() {
    let t = Test::new();
    let outside = t.repo("outside");
    std::fs::create_dir(outside.path()).unwrap();
    let run = outside.lfs(["fetch"]);
    assert_eq!(run.code(), Some(128), "{run}");
    run.has("Not in a Git repository");
}

#[test]
fn fetch_with_no_origin_remote() {
    let t = Test::new();
    let name = "fetch-no-remote";
    t.remote(name);
    let clone = t.clone(name, "no-remote-clone");
    let repo = t.clone(name, "no-remote-repo");
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    let a = oid("a");
    repo.write("a.dat", "a");
    repo.git(["add", "a.dat", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "add a.dat"])
        .ok()
        .has("main (root-commit)");
    repo.assert_local_object(&a, 1);
    assert!(t.server().object(name, &a).is_none());
    repo.git(["push", "origin", "main"])
        .ok()
        .has("Uploading LFS objects: 100% (1/1), 1 B")
        .has("main -> main");

    clone.git(["pull", "origin", "main"]).ok();
    clone.assert_local_object(&a, 1);

    // Track nothing on the remote, and rename the only remote.
    clone.git(["checkout", "--detach"]).ok();
    std::fs::remove_dir_all(clone.git_dir().join("lfs")).unwrap();
    clone.git(["remote", "rename", "origin", "something"]).ok();

    // Without a tracked remote or origin, the only remote is the default.
    clone.lfs(["fetch"]).ok();
    clone.assert_local_object(&a, 1);
}

#[test]
fn fetch_with_invalid_default_remote_in_lfsconfig() {
    let t = Test::new();
    let name = "fetch-invalid-default-remote";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.bin"]).ok();
    repo.write("test.bin", "test");
    repo.git(["add", ".gitattributes", "test.bin"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo.git(["push", "origin", "main"]).ok();

    // The branch's remote would take precedence over remote.lfsDefault.
    repo.git(["config", "--local", "--unset", "branch.main.remote"])
        .ok();
    // Both are invalid, so origin is used. The server answers 404 to every
    // batch request for this repository name.
    let url = format!("{}/batchunsupported.git", t.server().url());
    repo.git([
        "config",
        "--file",
        ".lfsconfig",
        "remote.other.origin.url",
        &url,
    ])
    .ok();
    repo.git([
        "config",
        "--file",
        ".lfsconfig",
        "remote.lfsDefault",
        "other.origin",
    ])
    .ok();
    repo.remove_local_objects();

    repo.lfs_command()
        .args(["fetch"])
        .env("GIT_CURL_VERBOSE", "1")
        .run()
        .ok()
        .lacks("batchunsupported")
        .lacks("404 Not Found");
    repo.assert_local_object(&oid("test"), 4);
}

#[test]
fn fetch_prune() {
    let t = Test::new();
    let repo = t.remote_and_clone("remote_fetch_prune");
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    let head = "HEAD content";
    let commit2 = "Content for commit 2 (prune)";
    let commit1 = "Content for commit 1 (prune)";
    repo.add_commits(&[
        Commit::new().days_ago(50).file("file.dat", commit1),
        Commit::new().days_ago(35).file("file.dat", commit2),
        Commit::new().days_ago(25).file("file.dat", head),
    ]);

    // Everything is pushed, so nothing is kept for being unpushed.
    repo.git(["push", "origin", "main"]).ok();
    // Nothing is recent, so as much as possible is pruned.
    repo.git(["config", "lfs.fetchrecentrefsdays", "0"]).ok();
    repo.git(["config", "lfs.fetchrecentcommitsdays", "0"]).ok();

    // Without the HEAD object, the fetch downloads something, and the
    // prune removes everything else.
    repo.delete_local_object(&oid(head));
    repo.lfs(["fetch", "--prune"]).ok();
    repo.assert_local_object(&oid(head), head.len());
    repo.refute_local_object(&oid(commit1));
    repo.refute_local_object(&oid(commit2));
}

/// Not in t-fetch: the prune after a fetch stops, and exits 2, when an
/// object to prune is missing on the remote.
#[test]
fn fetch_prune_missing_on_remote() {
    let t = Test::new();
    let name = "remote_fetch_prune_missing";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    let old = "Old content (prune)";
    let head = "HEAD content (prune)";
    repo.add_commits(&[
        Commit::new().days_ago(50).file("file.dat", old),
        Commit::new().days_ago(25).file("file.dat", head),
    ]);
    repo.git(["push", "origin", "main"]).ok();
    repo.git(["config", "lfs.fetchrecentrefsdays", "0"]).ok();
    repo.git(["config", "lfs.fetchrecentcommitsdays", "0"]).ok();
    repo.git(["config", "lfs.pruneverifyremotealways", "true"])
        .ok();
    t.server().delete_object(name, &oid(old));

    let run = repo.lfs_command().args(["fetch", "--prune"]).run();
    assert_eq!(run.code(), Some(2), "{run}");
    run.has("missing on remote:").has(&oid(old));
    repo.assert_local_object(&oid(old), old.len());
}

#[test]
fn fetch_raw_remote_url() {
    let t = Test::new();
    setup(&t);
    let raw = t.repo("raw");
    std::fs::create_dir(raw.path()).unwrap();
    let a = oid("a");
    raw.git(["init"]).ok();
    raw.lfs(["install", "--local", "--skip-smudge"]).ok();
    raw.git(["remote", "add", "origin", &t.remote_url(NAME)])
        .ok();
    raw.git(["pull", "origin", "main"]).ok();

    // The working tree has the pointer, and the object isn't downloaded.
    raw.refute_local_object(&a);
    assert!(raw.read("a.dat").contains(&a));

    raw.lfs(["fetch", &t.remote_url(NAME)]).ok();
    raw.assert_local_object(&a, 1);
    assert!(raw.read("a.dat").contains(&a));
}

#[test]
fn fetch_with_invalid_ref() {
    let t = Test::new();
    setup(&t);
    t.repo("repo")
        .lfs(["fetch", "origin", "jibberish"])
        .fails()
        .has("Invalid ref argument");
}

#[test]
fn fetch_with_invalid_remote() {
    let t = Test::new();
    setup(&t);
    t.repo("repo")
        .lfs(["fetch", "not-a-remote"])
        .has("Invalid remote name");
}

#[test]
fn fetch_fails_when_lfs_directory_has_wrong_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let t = Test::new();
    setup(&t);
    let shared = t.repo("shared");
    // Root ignores permissions.
    if shared.command("id").args(["-u"]).run().ok().stdout().trim() == "0" {
        return;
    }
    let objects = shared.git_dir().join("lfs/objects");
    shared.remove_local_objects();
    std::fs::create_dir(&objects).unwrap();
    std::fs::set_permissions(&objects, std::fs::Permissions::from_mode(0o400)).unwrap();

    shared
        .lfs(["fetch"])
        .has("error trying to create local storage directory");
    // So the test's directory can be removed.
    std::fs::set_permissions(&objects, std::fs::Permissions::from_mode(0o700)).unwrap();
}
