//! `git lfs merge-driver` — the LFS-aware Git merge driver.
//!
//! Git wires this in through `[merge "lfs"] driver = git lfs
//! merge-driver --ancestor %O --current %A --other %B --marker-size %L
//! --output %A`. For each of `%O` / `%A` / `%B`, Git supplies a path to
//! a temp file containing that side of the merge. If the file is a
//! pointer we smudge it (fetching on demand); otherwise we treat the
//! bytes as already-merged content. The three resolved files plus a
//! fresh tempfile for `%D` are substituted into the merge program
//! (default `git merge-file --stdout --marker-size=%L %A %O %B >%D`)
//! and run via `sh -c`. The merged content is cleaned back into a
//! pointer and written to `--output` (which is the same path Git will
//! pick up). A non-zero exit from the merge program means conflicts —
//! that exit code is propagated.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::Path;
use std::process::Command;

use git_lfs_filter::{FetchError, SmudgeError, SmudgeExtension, clean, smudge_with_fetch};
use git_lfs_pointer::Pointer;
use git_lfs_store::Store;
use tempfile::NamedTempFile;

use crate::fetcher::LfsFetcher;

const DEFAULT_PROGRAM: &str = "git merge-file --stdout --marker-size=%L %A %O %B >%D";

#[derive(Debug, thiserror::Error)]
pub enum MergeDriverError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Smudge(#[from] SmudgeError),
    #[error(transparent)]
    Clean(#[from] git_lfs_filter::CleanError),
    #[error("fetch failed: {0}")]
    Fetch(FetchError),
    #[error("the --ancestor, --current, --other, and --output options are mandatory")]
    MissingOptions,
}

pub struct MergeDriverOpts<'a> {
    pub ancestor: Option<&'a str>,
    pub current: Option<&'a str>,
    pub other: Option<&'a str>,
    pub output: Option<&'a str>,
    pub program: Option<&'a str>,
    pub marker_size: u32,
}

pub fn run(cwd: &Path, opts: &MergeDriverOpts<'_>) -> Result<u8, MergeDriverError> {
    let (ancestor, current, other, output) =
        match (opts.ancestor, opts.current, opts.other, opts.output) {
            (Some(o), Some(a), Some(b), Some(out)) => (o, a, b, out),
            _ => return Err(MergeDriverError::MissingOptions),
        };

    let store = Store::new(git_lfs_git::lfs_dir(cwd)?)
        .with_references(git_lfs_git::lfs_alternate_dirs(cwd).unwrap_or_default());
    let smudge_extensions = crate::collect_smudge_extensions(cwd);
    let clean_extensions = crate::collect_clean_extensions(cwd);
    let fetcher = LfsFetcher::from_repo(cwd, &store)?;

    let tmp_dir = store.tmp_dir();
    fs::create_dir_all(&tmp_dir)?;

    // Each NamedTempFile guard outlives the `sh -c` invocation below;
    // the path is what we substitute into the program template.
    let a_tmp = resolve_input(&tmp_dir, "A", current, &store, &fetcher, &smudge_extensions)?;
    let o_tmp = resolve_input(
        &tmp_dir,
        "O",
        ancestor,
        &store,
        &fetcher,
        &smudge_extensions,
    )?;
    let b_tmp = resolve_input(&tmp_dir, "B", other, &store, &fetcher, &smudge_extensions)?;
    let d_tmp = NamedTempFile::new_in(&tmp_dir)?;

    let mut specifiers: HashMap<char, String> = HashMap::new();
    specifiers.insert('A', a_tmp.path().display().to_string());
    specifiers.insert('O', o_tmp.path().display().to_string());
    specifiers.insert('B', b_tmp.path().display().to_string());
    specifiers.insert('D', d_tmp.path().display().to_string());
    specifiers.insert('L', opts.marker_size.to_string());

    let program = opts.program.unwrap_or(DEFAULT_PROGRAM);
    let formatted = format_percent_sequences(program, &specifiers);

    let status = Command::new("sh").args(["-c", &formatted]).status()?;
    // Non-zero from the merge program signals conflicts; we still
    // process the (partial) output file and propagate the exit code.
    let exit_status = status.code().unwrap_or(1) as u8;

    let mut input = File::open(d_tmp.path())?;
    let out_file = File::create(output)?;
    let mut output_writer = BufWriter::new(out_file);
    // %A's path stands in for the original file path in extension
    // %f substitutions — that's what the file is going to be saved
    // back to from Git's perspective.
    clean(
        &store,
        &mut input,
        &mut output_writer,
        current,
        &clean_extensions,
    )?;
    use std::io::Write;
    output_writer.flush()?;

    Ok(exit_status)
}

/// Read `path` (one of the three Git-supplied temp files), decide
/// whether it's a pointer or already-merged plain content, and stage
/// the bytes in a fresh tempfile under `<lfs>/tmp/` whose path is
/// returned. The returned [`NamedTempFile`] owns the lifetime — drop
/// it after `sh -c` completes.
fn resolve_input(
    tmp_dir: &Path,
    tag: &str,
    path: &str,
    store: &Store,
    fetcher: &LfsFetcher,
    smudge_extensions: &[SmudgeExtension],
) -> Result<NamedTempFile, MergeDriverError> {
    let tmp = tempfile::Builder::new()
        .prefix(&format!("merge-driver-{tag}-"))
        .tempfile_in(tmp_dir)?;

    // Smudge handles both branches: if the input parses as a pointer,
    // it fetches and writes content; otherwise it pass-throughs the
    // bytes. Either way the tempfile ends up holding the
    // working-tree-shaped content.
    let mut src = File::open(path)?;
    // `tempfile_in` already opened the file for writing, but
    // `NamedTempFile::as_file_mut` borrows immutably from the path
    // call site; reopening for write avoids the lifetime tangle.
    let mut dst = File::create(tmp.path())?;
    let res = smudge_with_fetch(
        store,
        &mut src,
        &mut dst,
        path,
        smudge_extensions,
        |p: &Pointer| fetcher.fetch(p),
    );
    match res {
        Ok(_) => {}
        Err(SmudgeError::FetchFailed(e)) => return Err(MergeDriverError::Fetch(e)),
        Err(e) => return Err(e.into()),
    }
    use std::io::Write;
    dst.flush()?;
    Ok(tmp)
}

/// Walk `pattern` substituting `%X` from `replacements` (shell-quoted),
/// `%%` for a literal `%`. Mirrors upstream's
/// `subprocess.FormatPercentSequences`. Unrecognized `%X` sequences
/// are silently dropped.
fn format_percent_sequences(pattern: &str, replacements: &HashMap<char, String>) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut state = 0;
    for c in pattern.chars() {
        if state == 0 && c == '%' {
            state = 1;
            continue;
        }
        if state == 1 {
            state = 0;
            if c == '%' {
                out.push('%');
            } else if let Some(val) = replacements.get(&c) {
                out.push_str(&shell_quote_single(val));
            }
            // unrecognized %X: drop the X, matching upstream.
            continue;
        }
        out.push(c);
    }
    out
}

/// Wrap `s` in single-quotes for sh, escaping embedded `'` as `'\''`.
/// Skips quoting for strings consisting only of `[A-Za-z0-9_@/.-]`,
/// matching upstream's `shellWordRe`.
fn shell_quote_single(s: &str) -> String {
    let is_simple = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '/' | '.' | '-'));
    if is_simple {
        s.to_owned()
    } else {
        let escaped = s.replace('\'', "'\\''");
        format!("'{escaped}'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_substitution_simple() {
        let mut r = HashMap::new();
        r.insert('A', "alpha".to_owned());
        r.insert('B', "beta".to_owned());
        assert_eq!(
            format_percent_sequences("merge %A and %B", &r),
            "merge alpha and beta"
        );
    }

    #[test]
    fn percent_substitution_quotes_unsafe() {
        let mut r = HashMap::new();
        r.insert('A', "/tmp/foo bar".to_owned());
        assert_eq!(format_percent_sequences("cat %A", &r), "cat '/tmp/foo bar'");
    }

    #[test]
    fn percent_substitution_escapes_single_quote() {
        let mut r = HashMap::new();
        r.insert('A', "it's".to_owned());
        assert_eq!(format_percent_sequences("%A", &r), "'it'\\''s'");
    }

    #[test]
    fn double_percent_is_literal() {
        let mut r = HashMap::new();
        r.insert('A', "x".to_owned());
        assert_eq!(format_percent_sequences("100%% done %A", &r), "100% done x");
    }

    #[test]
    fn unknown_specifier_dropped() {
        let r = HashMap::new();
        assert_eq!(format_percent_sequences("foo %Z bar", &r), "foo  bar");
    }

    #[test]
    fn shell_quote_simple_passes_through() {
        assert_eq!(shell_quote_single("simple_word.txt"), "simple_word.txt");
        assert_eq!(shell_quote_single("/tmp/x"), "/tmp/x");
    }

    #[test]
    fn shell_quote_complex_is_quoted() {
        assert_eq!(shell_quote_single("a b"), "'a b'");
        assert_eq!(shell_quote_single(""), "''");
    }
}
