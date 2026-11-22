//! `git lfs merge-driver`: a Git merge driver for LFS files.
//!
//! It's configured as `merge.lfs.driver`, with Git's `%O`, `%A`, and `%B`
//! files passed as `--ancestor`, `--current`, and `--other`. Each input
//! that is a pointer is smudged into a temporary file, downloading the
//! object if needed. Other inputs are copied as they are. The merge
//! program runs under `sh -c` on those files and writes to a fourth one,
//! `%D`, which is then cleaned into a pointer at `--output`.
//!
//! The merge program's exit code is returned, since a nonzero code means
//! there were conflicts.

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

    // The temporary files are deleted when these guards drop.
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
    // A nonzero exit means conflicts. The output, conflict markers and
    // all, is still cleaned and written.
    let exit_status = status.code().unwrap_or(1) as u8;

    let mut input = File::open(d_tmp.path())?;
    let out_file = File::create(output)?;
    let mut output_writer = BufWriter::new(out_file);
    // Extensions get the `--current` path as `%f`, since that's the file
    // Git writes the result to.
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

/// Copy one of Git's merge inputs into a temporary file in `tmp_dir`,
/// smudging it first if it's a pointer.
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

    // Smudge copies content that isn't a pointer as it is.
    let mut src = File::open(path)?;
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
        Err(SmudgeError::FetchFailed { source, .. }) => {
            return Err(MergeDriverError::Fetch(source));
        }
        Err(e) => return Err(e.into()),
    }
    use std::io::Write;
    dst.flush()?;
    Ok(tmp)
}

/// Replace each `%X` in `pattern` with the shell-quoted `replacements[X]`.
///
/// `%%` becomes `%`, and an unknown `%X` is dropped, like upstream's
/// `subprocess.FormatPercentSequences`.
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
            continue;
        }
        out.push(c);
    }
    out
}

/// Quote `s` for sh, unless it only has characters in `[A-Za-z0-9_@/.-]`.
///
/// Like upstream's `ShellQuoteSingle`.
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
