//! `t-umask.sh`.
//!
//! The shell sets a umask for everything after it. Here each command that
//! creates files runs under one with `sh -c 'umask …; exec …'`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::harness::{Repo, Test, git_lfs};

/// Run `program` with `args` in `dir` under `mask`, with `stdin`.
fn under_umask(dir: &Repo, mask: &str, program: &str, args: &[&str], stdin: &str) {
    dir.command("sh")
        .args([
            "-c",
            &format!("umask {mask} && exec \"$0\" \"$@\""),
            program,
        ])
        .args(args)
        .stdin(stdin)
        .run()
        .ok();
}

/// Run `git lfs clean` on `contents` in `repo` under `mask`.
fn clean(repo: &Repo, mask: &str, contents: &str) {
    under_umask(
        repo,
        mask,
        &git_lfs().to_string_lossy(),
        &["clean"],
        contents,
    );
}

/// The permission bits of the object `oid` in `repo`.
fn object_mode(repo: &Repo, oid: &str) -> u32 {
    let path = repo.local_object_path(oid);
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

/// Check that every directory under `repo`'s `.git/lfs` has `mode`, apart
/// from a setgid bit.
#[track_caller]
fn assert_dir_modes(repo: &Repo, mode: u32) {
    fn walk(dir: &Path, mode: u32) {
        let actual = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(actual, mode, "{} is {actual:o}", dir.display());
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, mode);
            }
        }
    }
    walk(&repo.path().join(".git/lfs"), mode);
}

const WHATEVER: &str = "cd293be6cea034bd45a0352775a219ef5dc7825ce55d1f7dae9762d80ce64411";
const RANDOM: &str = "87c1b129fbadd7b6e9abc0a9ef7695436d767aece042bec198a97e949fcbe14c";
const SOMETHING_ELSE: &str = "a1621be95040239ee14362c16e20510ddc20f527d772d823b2a1679b33f5cd74";
const WHO_CARES: &str = "261ded5f01a8ca18d9fb1958e8f58c53fa77648cc88a6d67c93d241a91133f3e";

#[test]
fn honors_umask() {
    let t = Test::new();
    let repo = t.init("simple");

    clean(&repo, "027", "whatever\n");
    assert_eq!(object_mode(&repo, WHATEVER), 0o640);

    clean(&repo, "007", "random\n");
    assert_eq!(object_mode(&repo, RANDOM), 0o660);
}

/// Clone the remote repository `name` into `dir` under `mask`, and clean
/// a file in it.
fn clone_and_clean<'a>(t: &'a Test, name: &str, dir: &str, mask: &str) -> Repo<'a> {
    let url = t.remote_url(name);
    under_umask(&t.repo(""), mask, "git", &["clone", &url, dir], "");
    let repo = t.repo(dir);
    clean(&repo, mask, "whatever\n");
    repo
}

#[test]
fn honors_umask_for_directories() {
    let t = Test::new();
    let name = "simple-directories";
    t.remote_with_file(name, "a.dat");

    let repo = clone_and_clean(&t, name, &format!("{name}-a"), "027");
    assert_dir_modes(&repo, 0o750);

    let repo = clone_and_clean(&t, name, &format!("{name}-b"), "007");
    assert_dir_modes(&repo, 0o770);
}

#[test]
fn honors_core_sharedrepository() {
    let t = Test::new();
    let repo = t.init("shared-repo");

    repo.git(["config", "core.sharedRepository", "0660"]).ok();
    clean(&repo, "027", "whatever\n");
    assert_eq!(object_mode(&repo, WHATEVER), 0o660);

    repo.git(["config", "core.sharedRepository", "everybody"])
        .ok();
    clean(&repo, "027", "random\n");
    assert_eq!(object_mode(&repo, RANDOM), 0o664);

    repo.git(["config", "core.sharedRepository", "false"]).ok();
    clean(&repo, "027", "something else\n");
    assert_eq!(object_mode(&repo, SOMETHING_ELSE), 0o640);

    clean(&repo, "007", "who cares\n");
    assert_eq!(object_mode(&repo, WHO_CARES), 0o660);
}

#[test]
fn honors_core_sharedrepository_for_directories() {
    let t = Test::new();
    let name = "shared-repo-directories";
    t.remote_with_file(name, "a.dat");

    for (suffix, shared, mask, mode) in [
        ("a", "0660", "027", 0o770),
        ("b", "everybody", "027", 0o775),
        ("c", "false", "027", 0o750),
        ("d", "false", "007", 0o770),
    ] {
        t.git_config_global("core.sharedRepository", shared);
        let repo = clone_and_clean(&t, name, &format!("{name}-{suffix}"), mask);
        assert_dir_modes(&repo, mode);
    }
}
