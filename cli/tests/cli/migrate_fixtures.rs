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
    /// A symlink to `gitattrs` (`link`).
    Link,
    /// With an unrelated macro definition (`macro`).
    Macro,
    /// With a macro that sets the LFS attributes, used for `*.txt`
    /// (`lfsmacro`).
    LfsMacro,
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
