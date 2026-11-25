//! `t-push.sh`.

use crate::harness::{Commit, Repo, Test, oid};

/// A clone of a new remote repository `name`, with `a.dat` tracked and
/// committed but not pushed.
fn push_repo_setup<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    set_locksverify(t, &repo, name, false);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "push a\n");
    repo.commit_all("add a.dat");
    repo
}

fn set_locksverify(t: &Test, repo: &Repo, name: &str, value: bool) {
    let key = format!("lfs.{}.locksverify", t.endpoint(name));
    repo.git(["config", &key, &value.to_string()]).ok();
}

#[test]
fn push_with_good_ref() {
    let t = Test::new();
    let repo = push_repo_setup(&t, "push-main-branch-required");
    repo.lfs(["push", "origin", "main"]).ok();
}

#[test]
fn push_with_tracked_ref() {
    let t = Test::new();
    let repo = push_repo_setup(&t, "push-tracked-branch-required");
    repo.git(["config", "push.default", "upstream"]).ok();
    repo.git(["config", "branch.main.merge", "refs/heads/tracked"])
        .ok();
    repo.lfs(["push", "origin", "main"]).ok();
}

#[test]
fn push_with_invalid_ref() {
    let t = Test::new();
    let repo = push_repo_setup(&t, "push-invalid-branch-required");
    repo.lfs(["push", "origin", "jibberish"])
        .fails()
        .has("Invalid ref argument");
}

#[test]
fn push_with_bad_ref() {
    let t = Test::new();
    let repo = push_repo_setup(&t, "push-other-branch-required");
    repo.lfs(["push", "origin", "main"])
        .fails()
        .has(r#"batch response: Expected ref "refs/heads/other", got "refs/heads/main""#);
}

#[test]
fn push_with_nothing() {
    let t = Test::new();
    let repo = push_repo_setup(&t, "push-nothing");
    repo.lfs(["push", "origin"])
        .has("At least one ref must be supplied without --all");
}

#[test]
fn push_with_given_remote_configured_push_remote() {
    let t = Test::new();
    let repo = push_repo_setup(&t, "push-given-and-config");
    repo.git(["remote", "add", "bad-remote", "invalid-url"])
        .ok();
    repo.git(["config", "branch.main.pushRemote", "bad-remote"])
        .ok();
    repo.lfs(["push", "--all", "origin"]).ok();
}

#[test]
fn push_via_stdin_with_extra_arguments() {
    let t = Test::new();
    let repo = push_repo_setup(&t, "push-stdin-extra-args");
    repo.lfs_command()
        .args(["push", "origin", "--stdin", "--dry-run", "another-ref"])
        .stdin("main\n")
        .run()
        .has("Further command line arguments are ignored with --stdin");
}

#[test]
fn push() {
    let t = Test::new();
    let repo = t.remote_and_clone("t-push");
    set_locksverify(&t, &repo, "t-push", true);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "push a\n");
    repo.commit_all("add a.dat");
    let a = format!("push {} => a.dat", oid("push a\n"));
    let b = format!("push {} => b.dat", oid("push b\n"));

    let run = repo
        .lfs(["push", "--dry-run", "origin", "main"])
        .ok()
        .has(&a);
    assert_eq!(run.count_lines("push "), 1);

    repo.lfs(["push", "origin", "main"])
        .ok()
        .has("Uploading LFS objects: 100% (1/1), 7 B");

    repo.git(["checkout", "-b", "push-b"]).ok();
    repo.write("b.dat", "push b\n");
    repo.commit_all("add b.dat");

    let run = repo
        .lfs(["push", "--dry-run", "origin", "push-b"])
        .ok()
        .has(&a)
        .has(&b);
    assert_eq!(run.count_lines("push "), 2);

    let run = repo
        .lfs_command()
        .args(["push", "--dry-run", "origin", "--stdin"])
        .stdin("push-b\n\n")
        .run()
        .ok()
        .has(&a)
        .has(&b);
    assert_eq!(run.count_lines("push "), 2);

    // Simulate a remote ref.
    let head = repo.git(["rev-parse", "HEAD"]).ok().stdout();
    repo.write(".git/refs/remotes/origin/HEAD", head);
    let run = repo.lfs(["push", "--dry-run", "origin", "push-b"]).ok();
    assert_eq!(run.count_lines("push "), 0);
    std::fs::remove_dir_all(repo.path().join(".git/refs/remotes")).unwrap();

    repo.lfs(["push", "origin", "push-b"])
        .ok()
        .has("Uploading LFS objects: 100% (2/2), 14 B");
}

/// The objects in the `push --all` tests' history.
struct Oids {
    initial: String,
    update: String,
    branch: String,
    tagged: String,
    main: String,
    /// In `file2.dat`, which the last commit deletes.
    extra: String,
}

impl Oids {
    fn new() -> Self {
        Self {
            initial: oid("initial"),
            update: oid("update"),
            branch: oid("branch"),
            tagged: oid("tagged"),
            main: oid("main"),
            extra: oid("extra"),
        }
    }

    fn all(&self) -> [&str; 6] {
        [
            &self.initial,
            &self.update,
            &self.branch,
            &self.tagged,
            &self.main,
            &self.extra,
        ]
    }
}

/// The setup the `push --all` tests share: a history with a branch, a tag,
/// and a deleted file, and `origin` pointing at a new empty repository.
struct PushAll<'a> {
    t: &'a Test,
    repo: Repo<'a>,
    /// The repository `origin` points at.
    remote: String,
    extra: String,
}

impl<'a> PushAll<'a> {
    fn setup(t: &'a Test, suffix: &str) -> (Self, Oids) {
        let name = format!("t-push-all-{suffix}");
        t.remote(&name);
        let repo = t.clone(&name, &format!("push-all-{suffix}"));
        set_locksverify(t, &repo, &name, true);
        repo.lfs(["track", "*.dat"]).ok();
        repo.add_commits(&[
            Commit::new().file("file1.dat", "initial"),
            Commit::new().file("file1.dat", "update"),
            Commit::new()
                .new_branch("branch")
                .file("file1.dat", "branch"),
            Commit::new()
                .on("main")
                .tag("tag")
                .file("file1.dat", "tagged"),
            Commit::new()
                .file("file1.dat", "main")
                .file("file2.dat", "extra"),
        ]);
        repo.git(["rm", "file2.dat"]).ok();
        repo.git(["commit", "-m", "remove file2.dat"]).ok();

        // Simulate a remote ref.
        let head = repo.git(["rev-parse", "HEAD"]).ok().stdout();
        repo.write(".git/refs/remotes/origin/HEAD", head);

        let remote = format!("{name}-{suffix}");
        repo.alternate_remote(&remote);
        set_locksverify(t, &repo, &remote, true);
        let oids = Oids::new();
        let p = Self {
            t,
            repo,
            remote,
            extra: oids.extra.clone(),
        };
        (p, oids)
    }

    /// Check that a dry run lists exactly `oids`, the extra one as
    /// `file2.dat` and the rest as `file1.dat`.
    #[track_caller]
    fn assert_dry_run(&self, args: &[&str], stdin: &str, oids: &[&str]) {
        let run = self.repo.lfs_command().args(args).stdin(stdin).run().ok();
        for oid in oids {
            let file = if *oid == self.extra {
                "file2.dat"
            } else {
                "file1.dat"
            };
            let line = format!("push {oid} => {file}");
            assert!(run.output().contains(&line), "no {line:?}\n{run}");
        }
        assert_eq!(run.count_lines("push "), oids.len(), "{run}");
    }

    /// Point `origin` at another new repository, push only the first
    /// object there, and delete it locally.
    fn push_first_object_to_new_remote(&mut self, oids: &Oids) {
        self.remote = format!("{}-2", self.remote);
        self.repo.alternate_remote(&self.remote);
        set_locksverify(self.t, &self.repo, &self.remote, true);
        self.repo
            .lfs(["push", "--object-id", "origin", &oids.initial])
            .ok();
        let all = oids.all();
        self.t
            .assert_server_objects(&self.remote, &all[..1], &all[1..]);
        self.repo.delete_local_object(&oids.initial);
    }
}

#[test]
fn push_all_no_ref_args() {
    let t = Test::new();
    let (mut p, oids) = PushAll::setup(&t, "everything");
    let all = oids.all();
    let dry_run = ["push", "--dry-run", "--all", "origin"];

    p.assert_dry_run(&dry_run, "", &all);
    let run = p.repo.git(["push", "--all", "origin"]).ok();
    assert_eq!(run.count("Uploading LFS objects: 100% (6/6)"), 1, "{run}");
    t.assert_server_objects(&p.remote, &all, &[]);

    // Push while missing old objects locally.
    p.push_first_object_to_new_remote(&oids);

    // A dry run lists the missing local object, which the server has.
    p.assert_dry_run(&dry_run, "", &all);
    p.repo
        .git(["push", "--all", "origin"])
        .ok()
        .has("Uploading LFS objects: 100% (6/6)");
    t.assert_server_objects(&p.remote, &all, &[]);
}

#[test]
fn push_all_1_ref_arg() {
    let t = Test::new();
    let (mut p, oids) = PushAll::setup(&t, "ref");
    let all = oids.all();
    // The tagged object is in main and the tag.
    let (branch, rest) = all.split_at(3);
    let dry_run = ["push", "--dry-run", "--all", "origin", "branch"];

    p.assert_dry_run(&dry_run, "", branch);
    let run = p.repo.lfs(["push", "--all", "origin", "branch"]).ok();
    assert_eq!(run.count("Uploading LFS objects: 100% (3/3)"), 1, "{run}");
    t.assert_server_objects(&p.remote, branch, rest);

    p.push_first_object_to_new_remote(&oids);

    // The dry run doesn't change.
    p.assert_dry_run(&dry_run, "", branch);
    let run = p.repo.git(["push", "origin", "branch"]).ok();
    assert_eq!(run.count("Uploading LFS objects: 100% (3/3)"), 1, "{run}");
    t.assert_server_objects(&p.remote, &branch[1..], rest);
}

#[test]
fn push_all_multiple_ref_args() {
    let t = Test::new();
    let (mut p, oids) = PushAll::setup(&t, "multiple-refs");
    let all = oids.all();
    // The main object is only in main.
    let (refs, rest) = all.split_at(4);
    let dry_run = ["push", "--dry-run", "--all", "origin", "branch", "tag"];

    p.assert_dry_run(&dry_run, "", refs);
    p.assert_dry_run(
        &["push", "--dry-run", "--all", "origin", "--stdin"],
        "branch\ntag",
        refs,
    );

    let run = p
        .repo
        .lfs(["push", "--all", "origin", "branch", "tag"])
        .ok();
    assert_eq!(run.count("Uploading LFS objects: 100% (4/4)"), 1, "{run}");
    t.assert_server_objects(&p.remote, refs, rest);

    p.push_first_object_to_new_remote(&oids);

    // The dry run doesn't change.
    p.assert_dry_run(&dry_run, "", refs);
    let run = p
        .repo
        .git(["push", "origin", "branch", "refs/tags/tag"])
        .ok();
    assert_eq!(run.count("Uploading LFS objects: 100% (4/4)"), 1, "{run}");
    t.assert_server_objects(&p.remote, &refs[1..], rest);
}

#[test]
fn push_all_ref_with_deleted_files() {
    let t = Test::new();
    let (mut p, oids) = PushAll::setup(&t, "ref-with-deleted");
    let main = [
        oids.initial.as_str(),
        &oids.update,
        &oids.tagged,
        &oids.main,
        &oids.extra,
    ];
    // Only in the branch.
    let branch = [oids.branch.as_str()];
    let dry_run = ["push", "--dry-run", "--all", "origin", "main"];

    p.assert_dry_run(&dry_run, "", &main);
    let run = p.repo.lfs(["push", "--all", "origin", "main"]).ok();
    assert_eq!(run.count("Uploading LFS objects: 100% (5/5)"), 1, "{run}");
    t.assert_server_objects(&p.remote, &main, &branch);

    p.push_first_object_to_new_remote(&oids);

    // The dry run doesn't change.
    p.assert_dry_run(&dry_run, "", &main);
    let run = p.repo.git(["push", "origin", "main"]).ok();
    assert_eq!(run.count("Uploading LFS objects: 100% (5/5)"), 1, "{run}");
    t.assert_server_objects(&p.remote, &main[1..], &branch);
}

/// A clone of the new remote repository `name` in `dir`, with `a.dat`
/// tracked and committed.
fn object_id_setup<'a>(t: &'a Test, name: &str, dir: &str) -> Repo<'a> {
    t.remote(name);
    let repo = t.clone(name, dir);
    set_locksverify(t, &repo, name, true);
    repo.lfs(["track", "*.dat"]).ok();
    repo.write("a.dat", "push a\n");
    repo.commit_all("add a.dat");
    repo
}

#[test]
fn push_object_ids() {
    let t = Test::new();
    let repo = object_id_setup(&t, "t-push", "repo2");
    let a = oid("push a\n");
    let b = oid("push b\n");

    repo.lfs(["push", "--object-id", "origin", "--dry-run"])
        .has("At least one object ID must be supplied with --object-id");

    repo.lfs(["push", "--object-id", "origin", &a])
        .ok()
        .has("Uploading LFS objects: 100% (1/1), 7 B");

    repo.write("b.dat", "push b\n");
    repo.commit_all("add b.dat");

    repo.lfs(["push", "--object-id", "origin", &a, &b])
        .ok()
        .has("Uploading LFS objects: 100% (2/2), 14 B");
}

#[test]
fn push_object_ids_via_stdin() {
    let t = Test::new();
    let repo = object_id_setup(&t, "t-push", "repo3");
    let a = oid("push a\n");
    let b = oid("push b\n");
    let push = |stdin: String, extra: &[&str]| {
        repo.lfs_command()
            .args(["push", "--object-id", "origin", "--stdin", "--dry-run"])
            .args(extra)
            .stdin(stdin)
            .run()
    };

    push(String::new(), &[]).lacks("At least one object ID must be supplied with --object-id");
    push(format!("{a}\n"), &["c0ffee"])
        .has("Further command line arguments are ignored with --stdin");
    push(format!("{a}\n"), &[]).has(&format!("push {a} =>"));

    repo.write("b.dat", "push b\n");
    repo.commit_all("add b.dat");

    push(format!("{a}\n{b}\n\n"), &[])
        .has(&format!("push {a} =>"))
        .has(&format!("push {b} =>"));
}

#[test]
fn push_modified_files() {
    let t = Test::new();
    let name = "t-push-modified";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.add_commits(&[
        Commit::new().file("file1.dat", "filecontent1"),
        Commit::new().file("file1.dat", "filecontent2"),
        Commit::new()
            .new_branch("other_branch")
            .file("file1.dat", "filecontent5"),
        Commit::new()
            .on("main")
            .file("file1.dat", "filecontent3")
            .file("file2.dat", "filecontent4"),
    ]);

    repo.lfs(["push", "origin", "main"]).ok();
    repo.lfs(["push", "origin", "other_branch"]).ok();
    let oids: Vec<String> = (1..=5).map(|i| oid(format!("filecontent{i}"))).collect();
    let oids: Vec<&str> = oids.iter().map(String::as_str).collect();
    t.assert_server_objects(name, &oids, &[]);
}

#[test]
fn push_with_invalid_remote() {
    let t = Test::new();
    let repo = push_repo_setup(&t, "t-push");
    repo.lfs(["push", "not-a-remote"])
        .has("Invalid remote name");
}

#[test]
fn push_ambiguous_branch_name() {
    let t = Test::new();
    let repo = t.remote_and_clone("t-push-ambiguous-branch");
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    repo.add_commits(&[
        Commit::new()
            .file("file1.dat", "filecontent0")
            .file("file2.dat", "filecontent1"),
        Commit::new()
            .new_branch("ambiguous")
            .file("file3.dat", "filecontent2"),
        Commit::new().file("file4.dat", "filecontent3"),
        Commit::new().on("main").file("file1.dat", "filecontent4"),
    ]);

    // A tag with the same name as the branch.
    repo.git(["tag", "ambiguous"]).ok();

    repo.lfs(["push", "origin", "main"]).ok();
    // This works, because the scan uses the commit, not the ref name.
    repo.lfs(["push", "origin", "ambiguous"]).ok();
}

#[test]
fn push_retry_with_expired_actions() {
    let t = Test::new();
    let repo = t.remote_and_clone("push_retry_expired_action");
    repo.lfs(["track", "*.dat"]).ok();
    let contents = "return-expired-action";
    repo.write("a.dat", contents);
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "add a.dat, .gitattributes"])
        .ok()
        .has("main (root-commit)")
        .has("2 files changed")
        .has("create mode 100644 a.dat")
        .has("create mode 100644 .gitattributes");

    let expected = format!(
        r#"enqueue retry #1 after 0.25s for "{}" (size: {}): LFS: action "upload" expires at"#,
        oid(contents),
        contents.len()
    );
    repo.command("git")
        .args(["push", "origin", "main"])
        .env("GIT_TRACE", "1")
        .run()
        .ok()
        .has(&expected)
        .has("Uploading LFS objects: 100% (1/1), 21 B");
}

#[test]
fn push_to_raw_remote_url() {
    let t = Test::new();
    t.remote("push-raw");
    let repo = t.repo("push-raw");
    std::fs::create_dir(repo.path()).unwrap();
    repo.git(["init"]).ok();
    repo.lfs(["track", "*.dat"]).ok();
    let contents = "raw";
    repo.write("raw.dat", contents);
    repo.git(["add", "raw.dat", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "add"])
        .ok()
        .has("main (root-commit)")
        .has("2 files changed")
        .has("create mode 100644 raw.dat")
        .has("create mode 100644 .gitattributes");

    t.assert_server_objects("push-raw", &[], &[&oid(contents)]);
    repo.lfs(["push", &t.remote_url("push-raw"), "main"]).ok();
    t.assert_server_objects("push-raw", &[&oid(contents)], &[]);
}

#[test]
fn push_with_invalid_object_size() {
    let t = Test::new();
    let name = "push-invalid-object-size";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    let contents = "return-invalid-size";
    repo.write("a.dat", contents);
    repo.git(["add", "a.dat", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "add a.dat, .gitattributes"])
        .ok()
        .has("main (root-commit)")
        .has("2 files changed")
        .has("create mode 100644 a.dat")
        .has("create mode 100644 .gitattributes");

    let run = repo.git(["push", "origin", "main"]).fails();
    assert!(run.stderr().contains("invalid size (got: -1)"), "{run}");
    assert!(!run.stderr().contains("panic"), "{run}");
    t.assert_server_objects(name, &[], &[&oid(contents)]);
}

#[test]
fn push_with_deprecated_links() {
    let t = Test::new();
    let name = "t-push-deprecated";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.commit_all("initial commit");
    let contents = "send-deprecated-links";
    repo.write("a.dat", contents);
    repo.commit_all("add a.dat");

    repo.git(["push", "origin", "main"]).ok();
    t.assert_server_objects(name, &[&oid(contents)], &[]);
}

#[test]
fn push_with_invalid_push_insteadof() {
    let t = Test::new();
    let repo = push_repo_setup(&t, "push-invalid-pushinsteadof");
    let storage = format!("{}/storage/", t.server().url());
    // Rewrite the upload href, and turn rewriting on.
    let key = format!("url.{}/storage/invalid.pushInsteadOf", t.server().url());
    repo.git(["config", &key, &storage]).ok();
    repo.git(["config", "lfs.transfer.enablehrefrewrite", "true"])
        .ok();

    let run = repo.lfs(["push", "origin", "main"]);
    assert_eq!(run.code(), Some(2), "{run}");
    // The rewritten href is used.
    run.has(&format!(
        "LFS: Authorization error: {}/storage/invalid",
        t.server().url()
    ));

    // Pushing works without the rewriting.
    repo.git(["config", "--unset", "lfs.transfer.enablehrefrewrite"])
        .ok();
    repo.lfs(["push", "origin", "main"]).ok();
}

/// A clone of the new remote repository `name`, with `*.dat` tracked, and
/// `a.dat` committed and pushed.
fn server_data_setup<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.commit_all("initial commit");
    repo.write("a.dat", "abc123");
    repo.commit_all("add a.dat");
    repo.git(["push", "origin", "main"]).ok();
    t.assert_server_objects(name, &[&oid("abc123")], &[]);
    repo
}

#[test]
fn push_with_data_the_server_already_has() {
    let t = Test::new();
    let name = "push-server-data";
    let repo = server_data_setup(&t, name);

    repo.git(["checkout", "-b", "side"]).ok();
    // A different file name, so the test fails if the push runs the wrong
    // object through the clean filter.
    repo.write("b.dat", "def456");
    repo.commit_all("add b.dat");

    // The server already has this one.
    repo.delete_local_object(&oid("abc123"));

    // Push to the URL, so there are no "origin/*" refs that show what the
    // server has. The push walks the whole history, and mustn't fail on
    // the deleted object.
    repo.git(["push", &t.remote_url(name), "side"]).ok();
    t.assert_server_objects(name, &[&oid("def456")], &[]);
}

#[test]
fn push_with_multiple_refs_and_data_the_server_already_has() {
    let t = Test::new();
    let name = "push-multi-ref-server-data";
    let repo = server_data_setup(&t, name);

    repo.write("b.dat", "def456");
    repo.commit_all("add b.dat");
    // A new ref would normally mean walking the whole history, but not
    // when pushing more than one ref.
    repo.git(["tag", "-m", "v1.0.0", "-a", "v1.0.0"]).ok();
    // The server already has this one.
    repo.delete_local_object(&oid("abc123"));

    // Push to the URL, so there are no "origin/*" refs that show what the
    // server has.
    let run = repo
        .command("git")
        .args(["push", &t.remote_url(name), "main", "v1.0.0"])
        .env("GIT_TRACE", "1")
        .env("GIT_TRANSFER_TRACE", "1")
        .env("GIT_CURL_VERBOSE", "1")
        .run()
        .ok();
    // No batch request for the object in the earlier main, which the
    // remote has.
    assert_eq!(run.count(&oid("abc123")), 0, "{run}");
    t.assert_server_objects(name, &[&oid("def456")], &[]);
}

#[test]
fn push_with_multiple_tag_refs() {
    let t = Test::new();
    let name = "push-multi-ref-tags";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.commit_all("initial commit");
    repo.write("a.dat", "abc123");
    repo.commit_all("add a.dat");
    repo.git(["tag", "v1.0.0"]).ok();

    repo.git(["push", "origin", "main", "v1.0.0"]).ok();
    t.assert_server_objects(name, &[&oid("abc123")], &[]);

    repo.write("b.dat", "def456");
    repo.commit_all("add b.dat");
    repo.git(["tag", "v1.0.1"]).ok();

    repo.lfs(["push", "origin", "v1.0.1"]).ok();
    t.assert_server_objects(name, &[&oid("def456")], &[]);
}

#[test]
fn push_custom_reference() {
    let t = Test::new();
    let name = "push-custom-reference";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok();
    repo.add_commits(&[Commit::new().file("file.dat", "filecontent")]);

    // A ref outside refs/heads, refs/tags, and refs/remotes.
    repo.git([
        "update-ref",
        "refs/custom/remote/heads/main",
        "refs/heads/main",
    ])
    .ok();
    repo.lfs(["push", "origin", "refs/custom/remote/heads/main"])
        .ok();
    t.assert_server_objects(name, &[&oid("filecontent")], &[]);
}

#[test]
fn push_object_id_invalid_value() {
    let t = Test::new();
    let (p, oids) = PushAll::setup(&t, "push-invalid-oid");
    p.repo
        .lfs(["push", "--object-id", "origin", ""])
        .has("too short object ID");
    p.repo
        .lfs(["push", "--object-id", "origin", &oids.initial[..3]])
        .has("too short object ID");
}
