//! Repository path discovery.

use std::path::{Path, PathBuf};

use crate::{Error, run_git};

/// Path to the **per-worktree** `.git` directory of the repository
/// containing `cwd`. Always returns an absolute path. Errors if `cwd`
/// isn't inside a git repository.
///
/// In a linked worktree this is `.git/worktrees/<name>/`, *not* the
/// shared main `.git/`. Use this when you want per-worktree state
/// (HEAD, index, info/, hooks-when-`--worktree`-scoped). For shared
/// storage (objects, packs, LFS cache, alternates), use
/// [`git_common_dir`].
pub fn git_dir(cwd: &Path) -> Result<PathBuf, Error> {
    run_git(cwd, &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
}

/// Path to the **shared** `.git` directory of the repository containing
/// `cwd`. Equivalent to [`git_dir`] in repos without linked worktrees;
/// in worktree-having repos it always returns the main `.git/` rather
/// than the per-worktree subtree.
///
/// Use this for anything stored once per repo: object database,
/// `objects/info/alternates`, default hooks, and the LFS object cache
/// at `.git/lfs/`. Mirrors upstream's `git.GitCommonDir()` /
/// `Configuration.LocalGitStorageDir()`.
pub fn git_common_dir(cwd: &Path) -> Result<PathBuf, Error> {
    let raw = run_git(cwd, &["rev-parse", "--git-common-dir"])?;
    let p = PathBuf::from(&raw);
    // `--git-common-dir` can return a relative path (`.git` from the
    // worktree root, `.` from inside the .git dir, sometimes `.git/.`).
    // Anchor against `cwd` so the result is absolute (matching
    // `git_dir`'s behavior), and lexically clean any leftover
    // `CurDir` components so `LocalGitStorageDir` doesn't end up with
    // a stray `/.`.
    let absolute = if p.is_absolute() { p } else { cwd.join(p) };
    Ok(clean_curdir(&absolute))
}

/// Lexically clean `p` — drop `CurDir` (`.`) components, collapse
/// `ParentDir` (`..`) by popping the previous component when one
/// exists. Pure path-string normalization, no I/O. Mirrors Go's
/// `path/filepath.Clean` for the cases produced by
/// `git rev-parse --git-common-dir`: `.git`, `./.git`, `.git/.`,
/// `a/../.git`, etc.
fn clean_curdir(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out: Vec<Component> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => continue,
            Component::ParentDir => {
                // Only collapse when the previous component is a
                // poppable normal segment. Don't pop a root or
                // prefix; don't pop another `..` (would change the
                // meaning when the path *starts* with `..`).
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

/// Path to the LFS storage directory (`<common-git-dir>/lfs`). The
/// directory is not created. Routed through [`git_common_dir`] so a
/// linked worktree shares the same on-disk LFS object cache as its
/// main repo — `git lfs prune` from one worktree sees the same 100%
/// of objects as `git lfs fetch` from another.
pub fn lfs_dir(cwd: &Path) -> Result<PathBuf, Error> {
    Ok(git_common_dir(cwd)?.join("lfs"))
}

/// Path to the working-tree root of the repository containing `cwd`.
/// Honors `GIT_WORK_TREE`, so this returns the right thing even when
/// `cwd` is *outside* the work tree (e.g. tests that set both
/// `GIT_DIR` and `GIT_WORK_TREE` as relative paths from a parent dir).
/// Errors for bare repos (no work tree) and outside-any-repo callers.
pub fn work_tree_root(cwd: &Path) -> Result<PathBuf, Error> {
    run_git(cwd, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// LFS-objects directories belonging to alternate object stores
/// referenced by this repository. Used to satisfy a `git lfs smudge`
/// or `git lfs fetch` from a `git clone --shared <source>` checkout
/// without re-downloading bytes the source already has.
///
/// Sources, in order:
/// 1. `GIT_ALTERNATE_OBJECT_DIRECTORIES` env var (path-list separated).
/// 2. `<git-dir>/objects/info/alternates` — one object directory per
///    line; blank lines and `#`-comments skipped.
///
/// Each entry names a git *objects* directory (e.g.
/// `/path/to/source/.git/objects`); the matching LFS-objects
/// directory lives next to it at `<entry>/../lfs/objects`. Only
/// directories that actually exist are returned.
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

    // `objects/info/alternates` is shared across linked worktrees —
    // it lives in the common git-dir, not the per-worktree one.
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

/// Strip C-style quotes from one `objects/info/alternates` line and
/// expand the common escapes (`\\`, `\"`, `\n`, `\t`, `\r`). Git emits
/// these when an alternate path contains characters that would
/// otherwise be ambiguous on the line. Returns the input unchanged
/// when there's no leading quote, so plain paths are still handled.
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
            // Anything else: emit literally — git supports more
            // (octal, \xNN), but the alternate-paths use case
            // basically never needs them.
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

    // Note: the multi-worktree case (verifying that `lfs_dir` from a
    // linked worktree resolves to the *main* repo's `.git/lfs/`) is
    // covered end-to-end by the vendored `t-worktree.sh` and
    // `t-prune-worktree.sh` shell tests. A unit-test version was tried
    // but flaked under parallel `cargo test` execution because
    // `git worktree add` touches HOME / global config in ways that
    // racing threads can perturb. The shell suite runs serially per
    // file under prove and is the authoritative coverage.

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
        // A .git that has /objects/ but no /lfs/objects/ — common for
        // repos that don't use LFS — should be silently skipped.
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
}
