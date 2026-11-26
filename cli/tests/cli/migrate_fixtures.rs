//! The repositories of `tests/fixtures/migrate.sh`, which the migrate
//! suites share. Each function keeps the name of its shell function,
//! without the `setup_` prefix, and returns the repository at the same
//! point the shell leaves its working directory.
//!
//! The files hold [`sized`] text where the shell writes random data of the
//! same size.

use crate::harness::{Repo, Test, sized};

/// How a fixture changes its `.gitattributes`, like the argument some of
/// the shell functions take.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Attrs {
    Plain,
    /// Executable (`0755`).
    Executable,
    /// A symlink to `gitattrs` (`link`).
    Link,
    /// With an unrelated macro definition (`macro`).
    Macro,
    /// With a macro that sets the LFS attributes, used for `*.txt`
    /// (`lfsmacro`).
    LfsMacro,
    /// With `a.dat`, `a.bat`, `a.out`, and `a.bin`, tracked by LFS through
    /// `.gitattributes`, `info/attributes`, and the global attributes
    /// file, each overriding another (`special`).
    Special,
}

/// Write `contents` of `size` to each of `files`.
fn write_sized(repo: &Repo, files: &[(&str, usize)]) {
    for (file, size) in files {
        repo.write(file, sized(file, *size));
    }
}

fn commit(repo: &Repo, files: &[&str], message: &str) {
    repo.git(["add"].iter().chain(files)).ok();
    repo.git(["commit", "-m", message]).ok();
}

/// Make `link` a symlink to `target`, like the shell suites' `add_symlink`
/// does through the index.
fn symlink(repo: &Repo, target: &str, link: &str) {
    std::os::unix::fs::symlink(target, repo.path().join(link)).unwrap();
}

/// `a.txt` committed outside LFS, then `.gitattributes` tracking `*.txt`
/// and `*.other`, changed as `attrs` says.
pub fn local_branch_with_gitattrs<'a>(t: &'a Test, attrs: Attrs) -> Repo<'a> {
    let repo = t.init("migrate-single-local-branch-with-attrs");
    write_sized(&repo, &[("a.txt", 120)]);
    commit(&repo, &["a.txt"], "initial commit");
    repo.lfs(["track", "*.txt"]).ok();
    repo.lfs(["track", "*.other"]).ok();
    if attrs == Attrs::Executable {
        use std::os::unix::fs::PermissionsExt;
        let path = repo.path().join(".gitattributes");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    if attrs == Attrs::Link {
        std::fs::rename(
            repo.path().join(".gitattributes"),
            repo.path().join("gitattrs"),
        )
        .unwrap();
        symlink(&repo, "gitattrs", ".gitattributes");
        repo.git(["add", "gitattrs"]).ok();
    }
    commit(&repo, &[".gitattributes"], "add .gitattributes");
    repo
}

/// `a.txt` and `a.md` committed outside LFS, then `.gitattributes`
/// tracking `*.txt`, and `b/a.md` with `b/.gitattributes` tracking `*.md`.
pub fn local_branch_with_nested_gitattrs(t: &Test) -> Repo<'_> {
    let repo = t.init("migrate-single-local-branch-nested-attrs");
    write_sized(&repo, &[("a.txt", 120), ("a.md", 140), ("b/a.md", 140)]);
    commit(&repo, &["a.txt", "a.md", "b/a.md"], "initial commit");
    repo.lfs(["track", "*.txt"]).ok();
    commit(&repo, &[".gitattributes"], "add .gitattributes");
    repo.at("b").lfs(["track", "*.md"]).ok();
    commit(&repo, &["b/.gitattributes"], "add nested .gitattributes");
    repo
}

/// An empty commit, then `a.txt` and `name` committed outside LFS.
pub fn single_local_branch_untracked<'a>(t: &'a Test, name: &str) -> Repo<'a> {
    let repo = t.init("migrate-single-local-branch-untracked");
    repo.git(["commit", "--allow-empty", "-m", "initial commit"])
        .ok();
    write_sized(&repo, &[("a.txt", 120), (name, 140)]);
    commit(&repo, &["a.txt", name], &format!("add a.txt and {name}"));
    repo
}

/// `*.txt` and `*.md` tracked, then `a.txt` and `a.md` committed as
/// pointers.
pub fn single_local_branch_tracked<'a>(t: &'a Test, attrs: Attrs) -> Repo<'a> {
    let repo = t.init("migrate-single-local-branch-tracked");
    repo.write(
        ".gitattributes",
        "*.txt filter=lfs diff=lfs merge=lfs -text\n*.md filter=lfs diff=lfs merge=lfs -text\n",
    );
    commit(&repo, &[".gitattributes"], "initial commit");
    write_sized(&repo, &[("a.txt", 120), ("a.md", 140)]);
    commit(&repo, &["a.txt", "a.md"], "add a.{txt,md}");
    if attrs == Attrs::Link {
        repo.git(["mv", ".gitattributes", "gitattrs"]).ok();
        symlink(&repo, "gitattrs", ".gitattributes");
        commit(&repo, &[".gitattributes"], "link .gitattributes");
    }
    repo
}

/// `a.txt` and `dir/b.txt` committed without the LFS filters, where the
/// attributes track the first but not the second.
pub fn single_local_branch_complex_tracked(t: &Test) -> Repo<'_> {
    let repo = t.init("migrate-single-local-branch-complex-tracked");
    repo.write(
        ".gitattributes",
        "*.txt filter=lfs diff=lfs merge=lfs -text\n",
    );
    repo.write("dir/.gitattributes", "*.txt !filter !diff !merge\n");
    repo.write("a.txt", "a");
    repo.write("dir/b.txt", "b");
    repo.lfs(["uninstall"]).ok();
    commit(
        &repo,
        &[".gitattributes", "dir/.gitattributes", "a.txt", "dir/b.txt"],
        "initial commit",
    );
    repo.lfs(["install"]).ok();
    repo
}

/// `a.txt` committed without the LFS filters, though the attributes track
/// `*.txt`, changed as `attrs` says.
pub fn single_local_branch_tracked_corrupt<'a>(t: &'a Test, attrs: Attrs) -> Repo<'a> {
    let repo = t.init("migrate-single-local-branch-with-attrs-corrupt");
    repo.lfs(["uninstall"]).ok();
    write_sized(&repo, &[("a.txt", 120)]);
    let mut attributes = String::from("*.txt filter=lfs diff=lfs merge=lfs -text\n");
    match attrs {
        Attrs::LfsMacro => {
            attributes = "[attr]lfs filter=lfs diff=lfs merge=lfs -text\n*.txt lfs\n".into();
        }
        Attrs::Macro => attributes += "[attr]foo foo\n",
        Attrs::Special => {
            repo.write(
                ".git/info/attributes",
                "*.dat filter=lfs\n*.bin filter=lfs diff=lfs merge=lfs -text\n",
            );
            attributes += "*.dat filter=overridden diff=lfs merge=lfs -text\n";
            let global = t.home().join(".config/git");
            std::fs::create_dir_all(&global).unwrap();
            std::fs::write(
                global.join("attributes"),
                "*.bat filter=overridden diff=lfs merge=lfs -text\n*.out filter=lfs\n",
            )
            .unwrap();
            attributes += "*.bat filter=lfs\n*.out diff=lfs merge=lfs -text\n";
            let files = [
                ("a.dat", 120),
                ("a.bat", 120),
                ("a.out", 120),
                ("a.bin", 120),
            ];
            write_sized(&repo, &files);
            repo.git(["add", "a.dat", "a.bat", "a.out", "a.bin"]).ok();
        }
        _ => {}
    }
    if attrs == Attrs::Link {
        repo.write("gitattrs", attributes);
        symlink(&repo, "gitattrs", ".gitattributes");
    } else {
        repo.write(".gitattributes", attributes);
    }
    commit(&repo, &[".gitattributes", "a.txt"], "initial commit");
    repo.lfs(["install"]).ok();
    repo
}

/// `a.txt` and `a.md` on `main`, and `a.md` changed on `my-feature`, with
/// `main` checked out.
pub fn multiple_local_branches(t: &Test) -> Repo<'_> {
    let repo = t.init("migrate-info-multiple-local-branches");
    write_sized(&repo, &[("a.txt", 120), ("a.md", 140)]);
    commit(&repo, &["a.txt", "a.md"], "initial commit");
    repo.git(["checkout", "-b", "my-feature"]).ok();
    write_sized(&repo, &[("a.md", 30)]);
    commit(&repo, &["a.md"], "add an additional 30 bytes to a.md");
    repo.git(["checkout", "main"]).ok();
    repo
}

/// [`multiple_local_branches`], then `.gitattributes` tracking `a.txt` and
/// `a.md` committed on `main`. The shell tracks `*.txt` and `*.md`
/// unquoted, which expand to those names.
pub fn multiple_local_branches_with_gitattrs(t: &Test) -> Repo<'_> {
    let repo = multiple_local_branches(t);
    track_and_commit(&repo, &["a.txt", "a.md"]);
    repo
}

/// Track each of `patterns`, then commit `.gitattributes`.
fn track_and_commit(repo: &Repo, patterns: &[&str]) {
    for pattern in patterns {
        repo.lfs(["track", pattern]).ok();
    }
    commit(repo, &[".gitattributes"], "add .gitattributes");
}

/// [`multiple_local_branches`], with `refs/pull/1/head` at `my-feature`
/// and `refs/pull/1/base` at `main`.
pub fn multiple_local_branches_non_standard(t: &Test) -> Repo<'_> {
    let repo = multiple_local_branches(t);
    for (name, target) in [("head", "my-feature"), ("base", "main")] {
        let sha = repo.git(["rev-parse", target]).ok().stdout();
        repo.git(["update-ref", &format!("refs/pull/1/{name}"), sha.trim()])
            .ok();
    }
    repo
}

/// [`multiple_local_branches`], with a file without an extension.
pub fn multiple_local_branches_with_alternate_names(t: &Test) -> Repo<'_> {
    let repo = t.init("migrate-info-multiple-local-branches");
    write_sized(&repo, &[("no_extension", 120), ("a.txt", 140)]);
    commit(&repo, &["no_extension", "a.txt"], "initial commit");
    repo.git(["checkout", "-b", "my-feature"]).ok();
    write_sized(&repo, &[("a.txt", 30), ("no_extension", 100)]);
    commit(
        &repo,
        &["no_extension", "a.txt"],
        "add an additional 30 bytes to a.txt",
    );
    repo.git(["checkout", "main"]).ok();
    repo
}

/// [`multiple_local_branches`], with every file tracked.
pub fn multiple_local_branches_tracked(t: &Test) -> Repo<'_> {
    let repo = t.init("migrate-info-multiple-local-branches");
    repo.write(
        ".gitattributes",
        "*.txt filter=lfs diff=lfs merge=lfs -text\n*.md filter=lfs diff=lfs merge=lfs -text\n",
    );
    commit(&repo, &[".gitattributes"], "initial commit");
    write_sized(&repo, &[("a.txt", 120), ("a.md", 140)]);
    commit(&repo, &["a.txt", "a.md"], "add a.{txt,md}");
    repo.git(["checkout", "-b", "my-feature"]).ok();
    write_sized(&repo, &[("a.md", 30)]);
    commit(&repo, &["a.md"], "add an additional 30 bytes to a.md");
    repo.git(["checkout", "main"]).ok();
    repo
}

/// A clone with `a.txt` and `a.md` pushed to `origin/main`, then changed in
/// an unpushed commit on `main`.
pub fn single_remote_branch(t: &Test) -> Repo<'_> {
    let repo = t.remote_and_clone("migrate-info-single-remote-branch");
    write_sized(&repo, &[("a.txt", 120), ("a.md", 140)]);
    commit(&repo, &["a.txt", "a.md"], "initial commit");
    repo.git(["push", "origin", "main"]).ok();
    write_sized(&repo, &[("a.txt", 30), ("a.md", 50)]);
    commit(
        &repo,
        &["a.md", "a.txt"],
        "add an additional 30, 50 bytes to a.{txt,md}",
    );
    repo
}

/// [`single_remote_branch`], then `.gitattributes` tracking `a.txt` and
/// `a.md` committed, like [`multiple_local_branches_with_gitattrs`].
pub fn single_remote_branch_with_gitattrs(t: &Test) -> Repo<'_> {
    let repo = single_remote_branch(t);
    track_and_commit(&repo, &["a.txt", "a.md"]);
    repo
}

/// A clone with one commit pushed, one more on `main`, and a third on
/// `my-feature`, each changing `a.txt` and `a.md`, with `main` checked
/// out.
pub fn multiple_remote_branches(t: &Test) -> Repo<'_> {
    let repo = t.remote_and_clone("migrate-info-exclude-remote-refs-given-branch");
    write_sized(&repo, &[("a.txt", 10), ("a.md", 11)]);
    commit(&repo, &["a.txt", "a.md"], "add 10, 11 bytes, a.{txt,md}");
    repo.git(["push", "origin", "main"]).ok();
    write_sized(&repo, &[("a.txt", 20), ("a.md", 21)]);
    commit(&repo, &["a.txt", "a.md"], "add 20, 21 bytes, a.{txt,md}");
    repo.git(["checkout", "-b", "my-feature"]).ok();
    write_sized(&repo, &[("a.txt", 30), ("a.md", 31)]);
    commit(&repo, &["a.txt", "a.md"], "add 30, 31 bytes, a.{txt,md}");
    repo.git(["checkout", "main"]).ok();
    repo
}

/// `foo/bar/baz/a.txt` committed.
pub fn single_local_branch_deep_trees(t: &Test) -> Repo<'_> {
    let repo = t.init("migrate-single-local-branch-with-deep-trees");
    write_sized(&repo, &[("foo/bar/baz/a.txt", 120)]);
    commit(&repo, &["foo/bar/baz/a.txt"], "initial commit");
    repo
}

/// `a file.txt` committed.
pub fn local_branch_with_space(t: &Test) -> Repo<'_> {
    let repo = t.init("migrate-local-branch-with-space");
    write_sized(&repo, &[("a file.txt", 50)]);
    commit(&repo, &["a file.txt"], "initial commit");
    repo
}

/// `a.txt` committed with 1 byte, then with 2 bytes and tagged `v1.0.0`,
/// with an annotated tag if `annotated`.
pub fn single_local_branch_with_tags(t: &Test, annotated: bool) -> Repo<'_> {
    let repo = t.init("migrate-single-local-branch-tags");
    write_sized(&repo, &[("a.txt", 1)]);
    commit(&repo, &["a.txt"], "initial commit");
    write_sized(&repo, &[("a.txt", 2)]);
    commit(&repo, &["a.txt"], "secondary commit");
    if annotated {
        repo.git(["tag", "v1.0.0", "-m", "v1.0.0"]).ok();
    } else {
        repo.git(["tag", "v1.0.0"]).ok();
    }
    repo
}

/// A clone with a second remote, `fork`. `a.txt` is pushed to `origin`,
/// then changed and pushed to `fork`.
pub fn multiple_remotes(t: &Test) -> Repo<'_> {
    let repo = t.remote_and_clone("migrate-multiple-remotes");
    t.remote("migrate-multiple-remotes-fork");
    repo.git([
        "remote",
        "add",
        "fork",
        &t.remote_url("migrate-multiple-remotes-fork"),
    ])
    .ok();
    repo.write("a.txt", sized("a.txt", 16));
    commit(&repo, &["a.txt"], "initial commit");
    repo.git(["push", "origin", "main"]).ok();
    repo.write("a.txt", sized("a.txt again", 16));
    commit(&repo, &["a.txt"], "another commit");
    repo.git(["push", "fork", "main"]).ok();
    repo
}

/// `a.txt`, `foo/a.txt`, `bar.txt/b.md`, and `bar.txt/b.txt` committed.
pub fn single_local_branch_same_file_tree_ext(t: &Test) -> Repo<'_> {
    let repo = t.init("migrate-single-local-branch-with-same-file-tree-ext");
    let files = ["a.txt", "foo/a.txt", "bar.txt/b.md", "bar.txt/b.txt"];
    for file in files {
        repo.write(file, sized(file, 120));
    }
    commit(&repo, &files, "initial commit");
    repo
}

/// `a.txt` committed, then `link.txt`, a symlink to it.
pub fn local_branch_with_symlink(t: &Test) -> Repo<'_> {
    let repo = t.init("migrate-single-local-branch-with-symlink");
    write_sized(&repo, &[("a.txt", 120)]);
    commit(&repo, &["a.txt"], "initial commit");
    symlink(&repo, "a.txt", "link.txt");
    commit(&repo, &["link.txt"], "add symlink");
    repo
}

/// `a.txt` committed with the text `a.txt`, then changed in the working
/// tree.
pub fn local_branch_with_dirty_copy(t: &Test) -> Repo<'_> {
    let repo = t.init("migrate-single-local-branch-with-dirty-copy");
    repo.write("a.txt", "a.txt");
    commit(&repo, &["a.txt"], "initial commit");
    repo.append("a.txt", "2");
    repo
}

/// `a.txt` and `dir/a.txt` committed, both with the text `a.txt`.
pub fn local_branch_with_copied_file(t: &Test) -> Repo<'_> {
    let repo = t.init("migrate-single-local-branch-with-copied-file");
    repo.write("a.txt", "a.txt");
    repo.write("dir/a.txt", "a.txt");
    commit(&repo, &["a.txt", "dir/a.txt"], "initial commit");
    repo
}

/// Three `.bin` files with special characters in their names committed.
pub fn local_branch_with_special_character_files(t: &Test) -> Repo<'_> {
    let repo = t.init("migrate-single-local-branch-with-special-filenames");
    write_sized(
        &repo,
        &[
            ("test - special.bin", 80),
            ("test (test2) special.bin", 100),
            ("test * ** special.bin", 120),
        ],
    );
    repo.git(["add", "--", "*.bin"]).ok();
    repo.git(["commit", "-m", "initial commit"]).ok();
    repo
}

/// Turn `repo` into a bare repository, like the shell suites' `make_bare`,
/// and return it.
pub fn make_bare<'a>(t: &'a Test, repo: Repo<'a>) -> Repo<'a> {
    let dir = repo.path().to_path_buf();
    let name = dir.file_name().unwrap().to_str().unwrap().to_owned();
    let bare = format!("{name}.git");
    std::fs::rename(dir.join(".git"), dir.with_file_name(&bare)).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let repo = t.repo(&bare);
    repo.git(["config", "--bool", "core.bare", "true"]).ok();
    repo
}
