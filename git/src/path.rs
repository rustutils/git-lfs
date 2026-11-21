//! Find the directories of a repository.

use std::path::{Path, PathBuf};

use crate::{Error, run_git};

/// The absolute path of the Git directory of the worktree containing `cwd`.
///
/// In a linked worktree this is `.git/worktrees/<name>`, not the main
/// `.git`. Use it for per-worktree state such as `HEAD` and the index. Use
/// [`git_common_dir`] for state shared by all worktrees. Errors if `cwd`
/// isn't in a repository.
pub fn git_dir(cwd: &Path) -> Result<PathBuf, Error> {
    run_git(cwd, &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
}

/// The Git directory shared by all worktrees of the repository containing
/// `cwd`.
///
/// This is the main `.git`, even from a linked worktree. Without linked
/// worktrees it's the same as [`git_dir`]. Use it for state stored once per
/// repository, such as objects and the LFS directory.
///
/// The path is absolute if `cwd` is.
pub fn git_common_dir(cwd: &Path) -> Result<PathBuf, Error> {
    let raw = run_git(cwd, &["rev-parse", "--git-common-dir"])?;
    let p = PathBuf::from(&raw);
    // `--git-common-dir` can print a path relative to `cwd`, such as
    // `.git`, `.`, or `.git/.`.
    let absolute = if p.is_absolute() { p } else { cwd.join(p) };
    Ok(clean_curdir(&absolute))
}

/// Remove `.` components from `p`, and `..` components with the component
/// before them.
///
/// This only looks at the path, not the file system, like Go's
/// `filepath.Clean`.
fn clean_curdir(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => continue,
            Component::ParentDir => {
                // Keep `..` after a root, a prefix, or another `..`.
                let pop_ok = matches!(out.last(), Some(Component::Normal(_)));
                if pop_ok {
                    out.pop();
                } else {
                    out.push(c);
                }
            }
            other => out.push(other),
        }
    }
    let mut buf = PathBuf::new();
    for c in &out {
        buf.push(c.as_os_str());
    }
    buf
}

/// The LFS directory, `lfs` in the [`git_common_dir`].
///
/// All worktrees share it. The directory is not created.
pub fn lfs_dir(cwd: &Path) -> Result<PathBuf, Error> {
    Ok(git_common_dir(cwd)?.join("lfs"))
}

/// The root of the working tree containing `cwd`.
///
/// Honors `GIT_WORK_TREE`, so `cwd` may be outside the working tree.
/// Errors in a bare repository and outside any repository.
pub fn work_tree_root(cwd: &Path) -> Result<PathBuf, Error> {
    run_git(cwd, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// The LFS object directories of the repository's alternate object stores.
///
/// A repository made with `git clone --shared` or `--reference` can read
/// LFS objects from its source this way instead of downloading them.
///
/// Alternates come from `GIT_ALTERNATE_OBJECT_DIRECTORIES`, then from
/// `objects/info/alternates` in the [`git_common_dir`]. Each names a Git
/// objects directory, such as `/src/.git/objects`, and its LFS objects are
/// in `lfs/objects` next to it. Only directories that exist are returned,
/// without duplicates.
///
/// Like upstream, relative paths in the alternates file are not resolved
/// against the objects directory.
pub fn lfs_alternate_dirs(cwd: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |objs_dir: &Path| {
        if let Some(parent) = objs_dir.parent() {
            let candidate = parent.join("lfs").join("objects");
            if candidate.is_dir() && !dirs.iter().any(|d| d == &candidate) {
                dirs.push(candidate);
            }
        }
    };

    if let Some(env) = std::env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES") {
        for raw in std::env::split_paths(&env) {
            if !raw.as_os_str().is_empty() {
                push(&raw);
            }
        }
    }

    let alternates_file = git_common_dir(cwd)?
        .join("objects")
        .join("info")
        .join("alternates");
    if let Ok(contents) = std::fs::read_to_string(&alternates_file) {
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let raw = unquote_alternate(trimmed);
            push(Path::new(raw.as_ref()));
        }
    }

    Ok(dirs)
}

/// Unquote a C-style quoted line of `objects/info/alternates`.
///
/// Expands the escapes `\\`, `\"`, `\n`, `\t`, and `\r`, and keeps any
/// other escape as is. Returns a line without a leading quote unchanged.
fn unquote_alternate(line: &str) -> std::borrow::Cow<'_, str> {
    if !line.starts_with('"') {
        return std::borrow::Cow::Borrowed(line);
    }
    let Some(end) = line.rfind('"') else {
        return std::borrow::Cow::Borrowed(line);
    };
    if end == 0 {
        return std::borrow::Cow::Borrowed(line);
    }
    let inner = &line[1..end];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            // Git also supports octal escapes, which alternate paths
            // rarely need.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        // Fail loudly if the test process inherits GIT_DIR or
        // GIT_WORK_TREE. With those set, `git init <tempdir>` ignores the
        // path arg and operates on the inherited git-dir instead, so the
        // tempdir never becomes a real repo and the test silently
        // exercises the wrong state. The `Justfile` pre-commit recipe
        // strips these. This assertion is the canary if anything slips.
        for var in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
            assert!(
                std::env::var_os(var).is_none(),
                "{var} is set in the test process — git subprocesses will \
                 ignore the per-test tempdir. Run via `just pre-commit` (which \
                 strips it) or `env -u {var} cargo test`."
            );
        }
        let tmp = TempDir::new().unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet"])
            .arg(tmp.path())
            .status()
            .unwrap();
        assert!(status.success(), "git init failed");
        tmp
    }

    #[test]
    fn git_dir_is_absolute() {
        let tmp = init_repo();
        let dir = git_dir(tmp.path()).unwrap();
        assert!(dir.is_absolute(), "{dir:?}");
        assert_eq!(dir.file_name().unwrap(), ".git");
    }

    #[test]
    fn lfs_dir_under_git_dir() {
        let tmp = init_repo();
        let dir = lfs_dir(tmp.path()).unwrap();
        assert!(dir.ends_with(".git/lfs"));
    }

    #[test]
    fn git_common_dir_matches_git_dir_for_main_worktree() {
        let tmp = init_repo();
        // Outside any linked-worktree setup, the two are identical.
        assert_eq!(
            git_dir(tmp.path()).unwrap(),
            git_common_dir(tmp.path()).unwrap()
        );
    }

    #[test]
    fn outside_repo_errors() {
        let tmp = TempDir::new().unwrap();
        let err = git_dir(tmp.path()).unwrap_err();
        assert!(matches!(err, Error::Failed(_)), "got {err:?}");
    }

    #[test]
    fn lfs_alternate_dirs_empty_without_alternates_file() {
        let tmp = init_repo();
        let dirs = lfs_alternate_dirs(tmp.path()).unwrap();
        assert!(dirs.is_empty());
    }

    #[test]
    fn lfs_alternate_dirs_resolves_via_alternates_file() {
        let source = init_repo();
        let lfs_objs = source.path().join(".git/lfs/objects");
        std::fs::create_dir_all(&lfs_objs).unwrap();

        let target = init_repo();
        let alt_path = target.path().join(".git/objects/info/alternates");
        std::fs::create_dir_all(alt_path.parent().unwrap()).unwrap();
        std::fs::write(
            &alt_path,
            format!("{}\n", source.path().join(".git/objects").display()),
        )
        .unwrap();

        let dirs = lfs_alternate_dirs(target.path()).unwrap();
        assert_eq!(dirs, vec![lfs_objs]);
    }

    #[test]
    fn lfs_alternate_dirs_skips_blank_and_comment_lines() {
        let source = init_repo();
        std::fs::create_dir_all(source.path().join(".git/lfs/objects")).unwrap();

        let target = init_repo();
        let alt_path = target.path().join(".git/objects/info/alternates");
        std::fs::create_dir_all(alt_path.parent().unwrap()).unwrap();
        std::fs::write(
            &alt_path,
            format!(
                "# preamble comment\n\n{}\n",
                source.path().join(".git/objects").display()
            ),
        )
        .unwrap();

        let dirs = lfs_alternate_dirs(target.path()).unwrap();
        assert_eq!(dirs.len(), 1);
    }

    #[test]
    fn lfs_alternate_dirs_handles_quoted_path() {
        let source = init_repo();
        let lfs_objs = source.path().join(".git/lfs/objects");
        std::fs::create_dir_all(&lfs_objs).unwrap();

        let target = init_repo();
        let alt_path = target.path().join(".git/objects/info/alternates");
        std::fs::create_dir_all(alt_path.parent().unwrap()).unwrap();
        std::fs::write(
            &alt_path,
            format!("\"{}\"\n", source.path().join(".git/objects").display()),
        )
        .unwrap();

        let dirs = lfs_alternate_dirs(target.path()).unwrap();
        assert_eq!(dirs, vec![lfs_objs]);
    }

    #[test]
    fn unquote_alternate_handles_escapes() {
        assert_eq!(unquote_alternate("/plain/path"), "/plain/path");
        assert_eq!(unquote_alternate(r#""/quoted/path""#), "/quoted/path");
        assert_eq!(unquote_alternate(r#""a\\b""#), "a\\b");
        assert_eq!(unquote_alternate(r#""a\"b""#), "a\"b");
        assert_eq!(unquote_alternate(r#""line1\nline2""#), "line1\nline2");
    }

    #[test]
    fn lfs_alternate_dirs_skips_alternates_without_lfs_storage() {
        // A .git that has objects/ but no lfs/objects/, as in repos that
        // don't use LFS, should be silently skipped.
        let source = init_repo();
        // Note: deliberately *not* creating .git/lfs/objects.
        let target = init_repo();
        let alt_path = target.path().join(".git/objects/info/alternates");
        std::fs::create_dir_all(alt_path.parent().unwrap()).unwrap();
        std::fs::write(
            &alt_path,
            format!("{}\n", source.path().join(".git/objects").display()),
        )
        .unwrap();

        let dirs = lfs_alternate_dirs(target.path()).unwrap();
        assert!(dirs.is_empty());
    }

    #[test]
    fn lfs_dir_resolves_to_main_repo_from_linked_worktree() {
        let main = init_repo();
        let run = |args: &[&str]| {
            let st = Command::new("git")
                .arg("-C")
                .arg(main.path())
                .args(args)
                .status()
                .unwrap();
            assert!(st.success(), "git {args:?} failed");
        };
        run(&["config", "user.email", "t@e"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "commit.gpgsign", "false"]);
        run(&["commit", "--allow-empty", "-q", "-m", "init"]);
        run(&["branch", "feature"]);

        let wt_holder = TempDir::new().unwrap();
        let wt_dir = wt_holder.path().join("wt");
        let status = Command::new("git")
            .arg("-C")
            .arg(main.path())
            .args(["worktree", "add"])
            .arg(&wt_dir)
            .arg("feature")
            .status()
            .unwrap();
        assert!(status.success(), "git worktree add failed");

        let main_lfs = lfs_dir(main.path()).unwrap();
        let wt_lfs = lfs_dir(&wt_dir).unwrap();
        std::fs::create_dir_all(&main_lfs).unwrap();
        let canon = |p: PathBuf| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::new());
        assert_eq!(canon(main_lfs), canon(wt_lfs));
    }
}
