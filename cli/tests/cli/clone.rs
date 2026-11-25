//! `t-clone.sh`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::harness::{Commit, Repo, Run, Test, git_lfs, oid, pointer, sized};

/// Run `git lfs clone` with `args` in the test's directory.
fn lfs_clone(t: &Test, args: &[&str]) -> Run {
    t.command(t.dir(), git_lfs())
        .args(["clone"])
        .args(args)
        .run()
}

/// Check the clone's hooks are installed, that it has no stray `lfs`
/// directory, and that its status is clean.
#[track_caller]
fn assert_cloned(repo: &Repo) {
    assert_hooks(&repo.git_dir());
    assert!(!repo.path().join("lfs").exists(), "lfs directory");
    repo.assert_clean_status();
}

#[track_caller]
fn assert_hooks(git_dir: &Path) {
    for hook in ["post-checkout", "post-commit", "post-merge", "pre-push"] {
        let path = git_dir.join("hooks").join(hook);
        let mode = std::fs::metadata(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .permissions()
            .mode();
        assert!(mode & 0o111 != 0, "{} isn't executable", path.display());
    }
}

#[track_caller]
fn assert_sizes(repo: &Repo, sizes: &[(&str, usize)]) {
    for (file, size) in sizes {
        assert_eq!(repo.read(file).len(), *size, "{file}");
    }
}

/// The history the clone tests push, the sizes of three of its files
/// after the last commit, and how many objects a checkout downloads.
struct History {
    commits: Vec<Commit>,
    sizes: [(&'static str, usize); 3],
    objects: usize,
}

/// Three commits, with seven objects of which six are checked out.
fn large_history() -> History {
    History {
        commits: vec![
            Commit::new()
                .days_ago(10)
                .file("file1.dat", sized("file1", 100))
                .file("file2.dat", sized("file2", 75)),
            Commit::new()
                .days_ago(7)
                .file("file1.dat", sized("file1", 110))
                .file("file3.dat", sized("file3", 66))
                .file("file4.dat", sized("file4", 23)),
            Commit::new()
                .days_ago(10)
                .file("file5.dat", sized("file5", 120))
                .file("file6.dat", sized("file6", 30)),
        ],
        sizes: [("file1.dat", 110), ("file2.dat", 75), ("file3.dat", 66)],
        objects: 6,
    }
}

/// Two commits with three objects.
fn small_history() -> History {
    History {
        commits: vec![
            Commit::new()
                .days_ago(5)
                .file("file1.dat", sized("file1", 100))
                .file("file2.dat", sized("file2", 75)),
            Commit::new()
                .days_ago(1)
                .file("file3.dat", sized("file3", 30)),
        ],
        sizes: [("file1.dat", 100), ("file2.dat", 75), ("file3.dat", 30)],
        objects: 3,
    }
}

/// Create the remote `name`, and push `history` to it through `base`, one
/// of the server's URLs.
fn push_history(t: &Test, base: &str, name: &str, history: &History) {
    t.remote(name);
    t.command(t.dir(), "git")
        .args(["clone", &format!("{base}/{name}"), name])
        .run()
        .ok();
    let repo = t.repo(name);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    repo.commit_all("Track *.dat");
    repo.add_commits(&history.commits);
    repo.git(["push", "origin", "main"]).ok();
}

/// `git lfs clone` `url` into `dir`, or the directory Git picks, and check
/// the result.
#[track_caller]
fn check_lfs_clone(t: &Test, url: &str, dir: Option<&str>, history: &History) {
    let mut args = vec![url];
    args.extend(dir);
    lfs_clone(t, &args)
        .ok()
        .has("Cloning into")
        .has("Downloading LFS objects:")
        .lacks("filter")
        .lacks("error");
    let dir = dir.unwrap_or_else(|| url.rsplit('/').next().unwrap());
    let repo = t.repo(dir);
    assert_sizes(&repo, &history.sizes);
    assert_cloned(&repo);
}

/// `git clone` `url`, letting the smudge filter download the objects, and
/// check the result.
#[track_caller]
fn check_git_clone(t: &Test, url: &str, history: &History) {
    t.command(t.dir(), "git")
        .args(["clone", url])
        .run()
        .ok()
        .has("Cloning into");
    let repo = t.repo(url.rsplit('/').next().unwrap());
    assert_sizes(&repo, &history.sizes);
    let objects = repo.git_dir().join("lfs/objects");
    assert_eq!(count_files(&objects), history.objects);
    assert_cloned(&repo);
}

fn count_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                count_files(&entry.path())
            } else {
                1
            }
        })
        .sum()
}

fn remove_dir(t: &Test, dir: &str) {
    std::fs::remove_dir_all(t.dir().join(dir)).unwrap();
}

/// The clones `clone` and the cookie test make, from `base`.
fn clone_both_ways(t: &Test, base: &str, name: &str) {
    let history = large_history();
    push_history(t, base, name, &history);
    let url = format!("{base}/{name}");

    check_lfs_clone(t, &url, Some(&format!("{name}1")), &history);
    // The clone the history was pushed from is in the implied directory.
    remove_dir(t, name);
    check_lfs_clone(t, &url, None, &history);
    remove_dir(t, name);
    check_git_clone(t, &url, &history);
}

#[test]
fn clone() {
    let t = Test::new();
    clone_both_ways(&t, t.server().url(), "t-clone");
}

#[test]
fn clone_ssl() {
    let t = Test::new();
    let name = "test-cloneSSL";
    let base = t.server().ssl_url();
    let history = small_history();
    push_history(&t, base, name, &history);
    let url = format!("{base}/{name}");

    check_lfs_clone(&t, &url, Some("testcloneSSL1"), &history);
    remove_dir(&t, name);
    check_git_clone(&t, &url, &history);
}

/// A credential helper that gives the client key's passphrase, `pass`, for
/// `protocol=cert` requests. Git's credential store refuses those, since
/// they have no host.
const CERT_HELPER: &str = "#!/bin/sh
[ \"$1\" = get ] || exit 0
grep -qx protocol=cert && echo password=pass
exit 0
";

/// The client certificate tests, with the certificate and keys at
/// `cert`, `key`, and `encrypted` in the global config, relative to `home`
/// if they start with `~/`.
fn client_cert_clones(t: &Test, name: &str, [cert, key, encrypted]: [&str; 3]) {
    let home = t.home();
    let server = t.server();
    for (path, pem) in [
        (cert, server.client_cert_pem()),
        (key, server.client_key_pem()),
        (encrypted, server.client_key_encrypted_pem()),
    ] {
        let path = path
            .strip_prefix("~/")
            .map_or(path.into(), |p| home.join(p));
        std::fs::write(path, pem).unwrap();
    }
    let helper = home.join("cert-helper");
    std::fs::write(&helper, CERT_HELPER).unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    t.command(&home, "git")
        .args(["config", "--global", "--add", "credential.helper"])
        .args([helper.to_str().unwrap()])
        .run()
        .ok();

    let base = server.client_cert_url();
    t.git_config_global(&format!("http.{base}/.sslCert"), cert);
    t.git_config_global(&format!("http.{base}/.sslKey"), key);

    let history = small_history();
    push_history(t, base, name, &history);
    let url = format!("{base}/{name}");

    // With the plain key, then the encrypted one.
    for encrypted_key in [false, true] {
        if encrypted_key {
            t.git_config_global(&format!("http.{base}/.sslKey"), encrypted);
            t.git_config_global("http.sslCertPasswordProtected", "true");
        }
        check_lfs_clone(t, &url, Some(&format!("{name}-{encrypted_key}")), &history);
        remove_dir(t, name);
        check_git_clone(t, &url, &history);
    }
}

#[test]
fn clone_client_cert() {
    let t = Test::new();
    let home = t.home();
    let paths = ["cert.pem", "key.pem", "key-encrypted.pem"].map(|f| home.join(f));
    let paths = paths.each_ref().map(|p| p.to_str().unwrap());
    client_cert_clones(&t, "test-cloneClientCert", paths);
}

#[test]
fn clone_client_cert_with_homedir_certs() {
    let t = Test::new();
    client_cert_clones(
        &t,
        "test-cloneClientCert-homedir",
        [
            "~/lfs-client-cert-file",
            "~/lfs-client-key-file",
            "~/lfs-client-key-file-encrypted",
        ],
    );
}

#[test]
fn clone_with_flags() {
    let t = Test::new();
    let name = "t-clone-flags";
    t.remote(name);
    let repo = t.clone(name, name);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    repo.commit_all("Track *.dat");
    repo.add_commits(&[
        Commit::new()
            .days_ago(10)
            .file("file1.dat", sized("file1", 100))
            .file("file2.dat", sized("file2", 75)),
        Commit::new()
            .days_ago(7)
            .new_branch("branch2")
            .file("fileonbranch2.dat", sized("fileonbranch2", 66)),
        Commit::new()
            .days_ago(3)
            .on("main")
            .file("file3.dat", sized("file3", 120))
            .file("file4.dat", sized("file4", 30)),
    ]);
    repo.git(["push", "origin", "main", "branch2"]).ok();

    let url = t.remote_url(name);
    let dir = "testflagsclone1";
    let template = t.dir().join("templatedir");
    std::fs::create_dir(&template).unwrap();

    // Many of these do nothing here, but they aren't rejected.
    lfs_clone(
        &t,
        &[
            "--template",
            template.to_str().unwrap(),
            "--local",
            "--no-hardlinks",
            "--shared",
            "--verbose",
            "--progress",
            "--recursive",
            &url,
            dir,
        ],
    )
    .ok();
    remove_dir(&t, dir);

    lfs_clone(&t, &["--quiet", "--no-checkout", &url, dir]).ok();
    assert!(!t.dir().join(dir).join("file1.dat").exists());
    remove_dir(&t, dir);

    lfs_clone(
        &t,
        &[
            "--branch",
            "branch2",
            "--recurse-submodules",
            "--origin",
            "differentorigin",
            &url,
            dir,
        ],
    )
    .ok();
    let clone = t.repo(dir);
    // Only on branch2.
    assert!(clone.path().join("fileonbranch2.dat").exists());
    clone.git(["remote", "get-url", "differentorigin"]).ok();
    assert_cloned(&clone);
    remove_dir(&t, dir);

    let git_dir = t.dir().join("separategitdir");
    lfs_clone(
        &t,
        &["--separate-git-dir", git_dir.to_str().unwrap(), &url, dir],
    )
    .ok();
    let dot_git = t.dir().join(dir).join(".git");
    assert!(dot_git.is_file(), ".git isn't a file");
    assert!(git_dir.join("objects").is_dir());
    assert_hooks(&git_dir);
    let clone = t.repo(dir);
    assert!(!clone.path().join("lfs").exists(), "lfs directory");
    clone.assert_clean_status();
    remove_dir(&t, dir);

    lfs_clone(&t, &["--bare", &url, dir]).ok();
    assert!(t.dir().join(dir).join("objects").is_dir());
    remove_dir(&t, dir);

    lfs_clone(&t, &["-l", "-v", "-n", "-s", "-b", "branch2", &url, dir]).ok();
}

/// Commit `a.dat` and `b.dat`, with "a" and "b", to the new remote
/// `name`.
fn ab_setup<'a>(t: &'a Test, name: &str, extra: &[&str]) -> Repo<'a> {
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    repo.write("a.dat", "a");
    for file in extra {
        repo.write(file, "a");
    }
    repo.write("b.dat", "b");
    repo.git(["add", "."]).ok();
    let changed = format!("{} files changed", 3 + extra.len());
    repo.git(["commit", "-m", "add a.dat, b.dat"])
        .ok()
        .has("main (root-commit)")
        .has(&changed)
        .has("create mode 100644 a.dat")
        .has("create mode 100644 b.dat")
        .has("create mode 100644 .gitattributes");
    repo
}

#[test]
fn clone_with_include_exclude_args() {
    let t = Test::new();
    let name = "clone_include_exclude";
    let repo = ab_setup(&t, name, &["a-dupe.dat", "dupe-a.dat"]);
    repo.git(["push", "origin", "main"])
        .ok()
        .has("main -> main")
        .has("Uploading LFS objects: 100% (2/2), 2 B");
    let url = t.remote_url(name);
    let (a, b) = (oid("a"), oid("b"));

    lfs_clone(&t, &[&url, "clone_with_includes", "-I", "a*.dat"]).ok();
    let clone = t.repo("clone_with_includes");
    clone.assert_local_object(&a, 1);
    clone.refute_local_object(&b);
    assert_eq!(clone.read("a.dat"), "a");
    assert_eq!(clone.read("a-dupe.dat"), "a");
    assert_eq!(clone.read("dupe-a.dat"), pointer(&a, 1));
    assert_eq!(clone.read("b.dat"), pointer(&b, 1));
    assert_cloned(&clone);

    lfs_clone(
        &t,
        &[&url, "clone_with_excludes", "-I", "b.dat", "-X", "a.dat"],
    )
    .ok();
    let clone = t.repo("clone_with_excludes");
    clone.assert_local_object(&b, 1);
    clone.refute_local_object(&a);
    assert_eq!(clone.read("a.dat"), pointer(&a, 1));
    assert_eq!(clone.read("b.dat"), "b");
    assert_cloned(&clone);
}

#[test]
fn clone_with_lfsconfig() {
    let t = Test::new();
    let name = "clone_with_lfsconfig";
    let repo = ab_setup(&t, name, &[]);
    let url = t.remote_url(name);
    let (a, b) = (oid("a"), oid("b"));

    let set_lfsconfig = |settings: &[(&str, &str)], message: &str| {
        for (key, value) in settings {
            repo.git(["config", "-f", ".lfsconfig", *key, *value]).ok();
        }
        repo.git(["add", ".lfsconfig"]).ok();
        repo.git(["commit", "-m", message])
            .ok()
            .has("main")
            .has("1 file changed");
    };
    // Clone into `dir` with `args`, and check which object it fetched.
    let check = |dir: &str, args: &[&str], fetched: &str, skipped: &str| {
        let mut all = vec![url.as_str(), dir];
        all.extend(args);
        lfs_clone(&t, &all).ok();
        let clone = t.repo(dir);
        clone.assert_local_object(fetched, 1);
        clone.refute_local_object(skipped);
        assert_cloned(&clone);
    };

    set_lfsconfig(&[("lfs.fetchinclude", "a*")], "config lfs.fetchinclude a*");
    repo.git(["push", "origin", "main"])
        .ok()
        .has("main -> main")
        .has("Uploading LFS objects: 100% (2/2), 2 B");
    check("clone_with_config_include", &[], &a, &b);
    check(
        "clone_with_config_include_and_args",
        &["-I", "b.dat"],
        &b,
        &a,
    );

    set_lfsconfig(
        &[("lfs.fetchinclude", "b*"), ("lfs.fetchexclude", "a*")],
        "config lfs.fetchinclude b*",
    );
    repo.git(["push", "origin", "main"])
        .ok()
        .has("main -> main");
    check("clone_with_config_exclude", &[], &b, &a);
    check(
        "clone_with_config_exclude_and_args",
        &["-I", "a.dat", "-X", "b.dat"],
        &a,
        &b,
    );
}

#[test]
fn clone_without_clean_filter() {
    let t = Test::new();
    let name = "clone_with_clean";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    repo.write("a.dat", "a");
    repo.git(["add", "a.dat", ".gitattributes"]).ok();
    repo.git(["commit", "-m", "add a.dat"])
        .ok()
        .has("main (root-commit)");
    repo.git(["push", "origin", "main"])
        .ok()
        .has("main -> main")
        .has("Uploading LFS objects: 100% (1/1), 1 B");

    t.command(t.dir(), git_lfs()).args(["uninstall"]).run().ok();
    t.command(t.dir(), "git")
        .args(["config", "--list"])
        .run()
        .ok()
        .lacks("filter.lfs.clean");

    let dir = "clone_without_clean";
    let run = lfs_clone(&t, &[&t.remote_url(name), dir, "-I", "a*.dat"]).ok();
    assert!(run.stdout().contains("Git LFS is not installed"), "{run}");
    let clone = t.repo(dir);
    let a = oid("a");
    clone.assert_local_object(&a, 1);
    assert_eq!(clone.read("a.dat"), pointer(&a, 1));
}

/// Track `*.dat`, write `contents` to `file`, and add the submodules in
/// `submodules`, as (path, remote) pairs.
fn commit_with_submodules(
    t: &Test,
    repo: &Repo,
    file: &str,
    contents: &str,
    submodules: &[(&str, &str)],
    message: &str,
) {
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    repo.write(file, contents);
    for (path, remote) in submodules {
        repo.git(["submodule", "add", &t.remote_url(remote), path])
            .ok();
        repo.git(["submodule", "update"]).ok();
    }
    repo.git(["add", "."]).ok();
    repo.git(["commit", "-m", message]).ok();
    repo.git(["push", "origin", "main"]).ok();
}

#[test]
fn clone_with_submodules() {
    let t = Test::new();
    let (root, sub1, sub2) = ("submod-root", "submod-level1", "submod-level2");
    for name in [root, sub1, sub2] {
        t.remote(name);
    }
    let contents_sub2 = "Inception. Now, before you bother telling me it's impossible...";
    let contents_sub1 = "We're dreaming?";
    let contents_root = "Downwards is the only way forwards.";

    let repo = t.clone(sub2, "submod2");
    commit_with_submodules(
        &t,
        &repo,
        "sub2.dat",
        contents_sub2,
        &[],
        "Nested submodule level 2",
    );
    let repo = t.clone(sub1, "submod1");
    commit_with_submodules(
        &t,
        &repo,
        "sub1.dat",
        contents_sub1,
        &[("sub2", sub2)],
        "Nested submodule level 1",
    );
    let repo = t.clone(root, "rootrepo");
    commit_with_submodules(
        &t,
        &repo,
        "root.dat",
        contents_root,
        &[("sub1", sub1)],
        "Root repo",
    );

    lfs_clone(&t, &["--recursive", &t.remote_url(root), "submod-clone"]).ok();
    let clone = t.repo("submod-clone");
    assert_cloned(&clone);
    for (dir, file, contents) in [
        ("", "root.dat", contents_root),
        ("sub1", "sub1.dat", contents_sub1),
        ("sub1/sub2", "sub2.dat", contents_sub2),
    ] {
        let repo = clone.at(dir);
        repo.assert_local_object(&oid(contents), contents.len());
        assert_eq!(repo.read(file).len(), contents.len(), "{dir}/{file}");
    }
}

#[test]
fn clone_in_current_directory() {
    let t = Test::new();
    let name = "clone_in_current_dir";
    let repo = t.remote_and_clone(name);
    repo.lfs(["track", "*.dat"]).ok().has(r#"Tracking "*.dat""#);
    let contents = "contents";
    repo.write("a.dat", contents);
    repo.git(["add", ".gitattributes", "a.dat"]).ok();
    repo.git(["commit", "-m", "initial commit"])
        .ok()
        .has("main (root-commit)")
        .has("2 files changed")
        .has("create mode 100644 a.dat")
        .has("create mode 100644 .gitattributes");
    repo.git(["push", "origin", "main"]).ok();

    let dir = format!("{name}-clone");
    std::fs::create_dir(t.dir().join(&dir)).unwrap();
    let clone = t.repo(&dir);
    clone
        .lfs_command()
        .args(["clone", &t.remote_url(name), "."])
        .run()
        .ok();
    clone.lfs(["fsck"]).ok();
    clone.assert_local_object(&oid(contents), 8);
    assert_cloned(&clone);
}

#[test]
fn clone_empty_repository() {
    let t = Test::new();
    let name = "clone_empty";
    t.remote(name);
    lfs_clone(&t, &[&t.remote_url(name), name]).ok();
}

#[test]
fn clone_bare_empty_repository() {
    let t = Test::new();
    let name = "clone_bare_empty";
    t.remote(name);
    lfs_clone(&t, &[&t.remote_url(name), name, "--bare"]).ok();
}

/// The server wants a cookie for repositories named `require-cookie-*`.
/// Go's cookie jar ignores cookies for IP addresses, so the clone uses
/// localhost.
#[test]
fn clone_http_server_proxy_require_cookies() {
    let t = Test::new();
    let cookies = t.dir().join("cookies.txt");
    std::fs::write(
        &cookies,
        "localhost\tTRUE\t/\tFALSE\t2145916800\tCOOKIE_GITLFS\tsecret\n",
    )
    .unwrap();
    t.git_config_global("http.cookieFile", cookies.to_str().unwrap());
    clone_both_ways(&t, &t.localhost_url(), "require-cookie-test");
}
