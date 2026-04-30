//! `git rev-list --objects --do-walk --stdin` wrapper.
//!
//! Walks history reachable from `include` refs but not from `exclude`
//! refs, emitting every commit + tree + blob OID along the way (with the
//! blob's path appended for blobs and trees that have a name in the
//! parent tree). This is the entry point upstream uses to find every
//! object that *could* be an LFS pointer; we then narrow with
//! `cat-file --batch-check` and read the survivors with `cat-file --batch`.
//!
//! Output format from `git rev-list --objects` is one object per line,
//! either `<oid>` (commit) or `<oid> <name>` (tree/blob with a path).

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::Error;

/// One entry yielded by [`rev_list`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevListEntry {
    pub oid: String,
    /// `Some` for trees and blobs that have a path in their parent tree;
    /// `None` for commits and root trees.
    pub name: Option<String>,
}

/// Run `git rev-list --objects --do-walk --stdin -- ` against `cwd` with
/// the given include/exclude refs and collect every emitted object.
///
/// Refs are passed via stdin (one per line) so we don't blow the
/// command-line length limit on big refspecs. Excludes are prefixed with
/// `^` per `git-rev-list(1)`.
///
/// Returns OIDs in the order git emitted them. Callers that need
/// deduplication should layer it on top.
pub fn rev_list(
    cwd: &Path,
    include: &[&str],
    exclude: &[&str],
) -> Result<Vec<RevListEntry>, Error> {
    rev_list_with_args(cwd, include, exclude, &[])
}

/// [`rev_list`] with extra command-line args spliced before `--stdin`.
///
/// Used for the upstream `--not --remotes=<name>` optimization: pre-push
/// invokes rev-list with that pair on the command line so the trace
/// (`GIT_TRACE=1`) shows it verbatim — `t-pre-push.sh` greps for
/// `rev-list.*--not --remotes=origin` to confirm the optimization
/// kicked in for a `git push <url>` whose URL matches a configured
/// remote.
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
    // Inherit stderr so `GIT_TRACE=1` users see the rev-list
    // invocation. t-pre-push 37 greps the trace for a literal
    // `rev-list.*--not --remotes=origin` to confirm the upstream
    // optimization fired. The cost is failure messages no longer
    // appear in our wrapped Error — exit status still tells us
    // *that* it failed, just not why.
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
        // Dropping `stdin` closes the pipe so rev-list can finish reading.
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

        // include=HEAD, exclude=first → only the second commit + its tree
        // + the new blob (a.txt is unchanged so not re-emitted).
        let entries = rev_list(repo.path(), &["HEAD"], &[&first]).unwrap();
        let blobs: Vec<_> = entries.iter().filter_map(|e| e.name.as_deref()).collect();
        assert!(blobs.contains(&"b.txt"), "{entries:?}");
        assert!(!blobs.contains(&"a.txt"), "{entries:?}");
    }

    #[test]
    fn rev_list_unknown_ref_errors() {
        let repo = init_repo();
        commit_file(&repo, "a.txt", b"x");
        // We only inspect that it failed — stderr inherits to the
        // parent (so `GIT_TRACE=1` users see git's error directly),
        // which means our wrapped message no longer carries git's
        // text.
        let err = rev_list(repo.path(), &["does-not-exist"], &[]).unwrap_err();
        assert!(matches!(err, Error::Failed(_)), "got {err:?}");
    }
}
