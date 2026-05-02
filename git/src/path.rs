//! Repository path discovery.

use std::path::{Path, PathBuf};

use crate::{Error, run_git};

/// Path to the `.git` directory of the repository containing `cwd`. Always
/// returns an absolute path. Errors if `cwd` isn't inside a git repository.
pub fn git_dir(cwd: &Path) -> Result<PathBuf, Error> {
    run_git(cwd, &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
}

/// Path to the LFS storage directory for the repository (`<git-dir>/lfs`).
/// The directory is not created.
pub fn lfs_dir(cwd: &Path) -> Result<PathBuf, Error> {
    Ok(git_dir(cwd)?.join("lfs"))
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

    let alternates_file = git_dir(cwd)?
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
