//! List the objects reachable from some refs but not others.
//!
//! [`rev_list`] runs `git rev-list --objects` and returns every commit,
//! tree, and blob it prints. [`scanner`](crate::scanner) uses it to find
//! the blobs that might be LFS pointers.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::Error;

/// One object printed by `git rev-list --objects`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevListEntry {
    /// Object ID in hex.
    pub oid: String,
    /// Path of a tree or blob, `None` for a commit.
    ///
    /// A root tree has an empty name, `Some("")`.
    pub name: Option<String>,
}

/// List the objects reachable from `include` but not from `exclude`.
///
/// Runs `git rev-list --objects --do-walk --stdin`, writing the refs to
/// stdin so a long list can't overflow the command line. Entries come in
/// the order git prints them. Git's stderr goes to ours, so on failure the
/// returned error only has the exit status.
pub fn rev_list(
    cwd: &Path,
    include: &[&str],
    exclude: &[&str],
) -> Result<Vec<RevListEntry>, Error> {
    rev_list_with_args(cwd, include, exclude, &[])
}

/// [`rev_list`] with extra arguments placed before `--stdin`.
///
/// Arguments passed here, unlike refs, show up in the `GIT_TRACE` output.
/// `t-pre-push.sh` checks the trace for `--not --remotes=origin`.
pub fn rev_list_with_args(
    cwd: &Path,
    include: &[&str],
    exclude: &[&str],
    extra_cmdline_args: &[&str],
) -> Result<Vec<RevListEntry>, Error> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(cwd);
    cmd.args(["rev-list", "--objects", "--do-walk"]);
    cmd.args(extra_cmdline_args);
    cmd.args(["--stdin", "--"]);
    // Inherit stderr so `GIT_TRACE` output reaches the user, which
    // t-pre-push.sh relies on. The cost is that git's error message
    // doesn't make it into our error.
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    {
        let mut stdin = child.stdin.take().expect("piped");
        for r in include {
            writeln!(stdin, "{r}")?;
        }
        for r in exclude {
            writeln!(stdin, "^{r}")?;
        }
        // Close stdin so rev-list starts walking.
    }

    let stdout = child.stdout.take().expect("piped");
    let mut entries = Vec::new();
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        entries.push(parse_line(&line));
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(Error::Failed(format!("git rev-list failed: {status}")));
    }
    Ok(entries)
}

fn parse_line(line: &str) -> RevListEntry {
    match line.split_once(' ') {
        Some((oid, name)) => RevListEntry {
            oid: oid.to_owned(),
            name: Some(name.to_owned()),
        },
        None => RevListEntry {
            oid: line.to_owned(),
            name: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::commit_helper::*;

    #[test]
    fn parse_line_commit_only() {
        let e = parse_line("1234567");
        assert_eq!(e.oid, "1234567");
        assert!(e.name.is_none());
    }

    #[test]
    fn parse_line_blob_with_path() {
        let e = parse_line("1234567 path/to/file.bin");
        assert_eq!(e.oid, "1234567");
        assert_eq!(e.name.as_deref(), Some("path/to/file.bin"));
    }

    #[test]
    fn rev_list_empty_include_returns_nothing() {
        let repo = init_repo();
        commit_file(&repo, "a.txt", b"hello");
        let entries = rev_list(repo.path(), &[], &[]).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn rev_list_one_commit_yields_commit_tree_and_blob() {
        let repo = init_repo();
        commit_file(&repo, "a.txt", b"hello");
        let entries = rev_list(repo.path(), &["HEAD"], &[]).unwrap();

        // We expect one commit, one root tree, one blob. Order is
        // git-defined but: commit comes first, then the tree, then blobs.
        assert_eq!(entries.len(), 3, "{entries:?}");
        assert!(entries[0].name.is_none(), "commit has no name");
        let blob = entries.iter().find(|e| e.name.as_deref() == Some("a.txt"));
        assert!(blob.is_some(), "no blob with path 'a.txt' in {entries:?}");
    }

    #[test]
    fn rev_list_excludes_filter_ancestors() {
        let repo = init_repo();
        commit_file(&repo, "a.txt", b"first");
        let first = head_oid(&repo);
        commit_file(&repo, "b.txt", b"second");

        // include=HEAD, exclude=first → only the second commit, its tree,
        // and the new blob (a.txt is unchanged so not re-emitted).
        let entries = rev_list(repo.path(), &["HEAD"], &[&first]).unwrap();
        let blobs: Vec<_> = entries.iter().filter_map(|e| e.name.as_deref()).collect();
        assert!(blobs.contains(&"b.txt"), "{entries:?}");
        assert!(!blobs.contains(&"a.txt"), "{entries:?}");
    }

    #[test]
    fn rev_list_unknown_ref_errors() {
        let repo = init_repo();
        commit_file(&repo, "a.txt", b"x");
        // We only inspect that it failed. Stderr goes to the parent (so
        // `GIT_TRACE=1` users see git's error directly), which means our
        // wrapped message doesn't carry git's text.
        let err = rev_list(repo.path(), &["does-not-exist"], &[]).unwrap_err();
        assert!(matches!(err, Error::Failed(_)), "got {err:?}");
    }
}
