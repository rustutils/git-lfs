//! The `git fast-export` to `git fast-import` pipeline behind
//! `migrate import` and `migrate export`, plus the working tree checks
//! around it.
//!
//! The [`Transform`] runs on the calling thread between the two
//! processes. Their stderr is drained on separate threads so neither
//! blocks on a full pipe, and echoed with a `[fast-export]` or
//! `[fast-import]` prefix.

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;

use git_lfs_store::Store;

use super::MigrateError;
use super::transform::{Mode, Options as TransformOptions, Stats, Transform};

/// Rewrite the history between `include_refs` and `exclude_refs`.
///
/// fast-import updates each ref fast-export names, with `--force`.
pub fn run_pipeline(
    cwd: &Path,
    include_refs: &[String],
    exclude_refs: &[String],
    transform_opts: TransformOptions,
    mode: Mode,
    store: &Store,
) -> Result<Stats, MigrateError> {
    run_pipeline_with_export_marks(
        cwd,
        include_refs,
        exclude_refs,
        transform_opts,
        mode,
        store,
        None,
    )
}

/// Like [`run_pipeline`], and also has fast-import write its marks to
/// `marks_path`.
///
/// With [`Stats::commit_marks`], the marks map each old commit to its
/// rewritten one.
pub fn run_pipeline_with_export_marks(
    cwd: &Path,
    include_refs: &[String],
    exclude_refs: &[String],
    mut transform_opts: TransformOptions,
    mode: Mode,
    store: &Store,
    marks_path: Option<&Path>,
) -> Result<Stats, MigrateError> {
    // Fixup also applies the attribute files that aren't in commits.
    if mode == Mode::Fixup {
        transform_opts.info_attrs = read_info_attrs(cwd).unwrap_or_default();
        transform_opts.global_attrs = read_global_attrs(cwd).unwrap_or_default();
    }
    transform_opts.cwd = cwd.to_path_buf();
    let mut export = spawn_fast_export(cwd, include_refs, exclude_refs)?;
    let mut import = spawn_fast_import(cwd, marks_path.map(PathBuf::from).as_deref())?;

    let export_stdout = export.stdout.take().expect("piped");
    let import_stdin = import.stdin.take().expect("piped");
    let export_stderr = export.stderr.take().expect("piped");
    let import_stderr = import.stderr.take().expect("piped");

    let export_err_thread = drain_stderr("fast-export", export_stderr);
    let import_err_thread = drain_stderr("fast-import", import_stderr);

    let stats = Transform::new(store, transform_opts, mode)
        .run(export_stdout, import_stdin)
        .map_err(MigrateError::Io)?;

    let export_status = export.wait().map_err(MigrateError::Io)?;
    let import_status = import.wait().map_err(MigrateError::Io)?;

    let export_err = export_err_thread.join().expect("stderr thread");
    let import_err = import_err_thread.join().expect("stderr thread");

    if !export_status.success() {
        return Err(MigrateError::Other(format!(
            "git fast-export failed: {}",
            export_err.trim()
        )));
    }
    if !import_status.success() {
        return Err(MigrateError::Other(format!(
            "git fast-import failed: {}",
            import_err.trim()
        )));
    }
    Ok(stats)
}

fn spawn_fast_export(
    cwd: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<Child, MigrateError> {
    // `--full-tree` lists every file in every commit, so the transform
    // never has to look at a parent. `--reference-excluded-parents`
    // refers to excluded parents by SHA so their commits aren't
    // rewritten.
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(cwd).args([
        "fast-export",
        "--full-tree",
        "--reencode=yes",
        "--reference-excluded-parents",
        // The transform pairs these with fast-import's marks to map
        // old commits to new ones.
        "--show-original-ids",
    ]);
    for r in include {
        cmd.arg(r);
    }
    for r in exclude {
        cmd.arg(format!("^{r}"));
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn().map_err(MigrateError::Io)
}

fn spawn_fast_import(cwd: &Path, marks_path: Option<&Path>) -> Result<Child, MigrateError> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        .args(["fast-import", "--force", "--quiet"]);
    if let Some(p) = marks_path {
        cmd.arg(format!("--export-marks={}", p.display()));
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn().map_err(MigrateError::Io)
}

fn drain_stderr<R: Read + Send + 'static>(label: &'static str, r: R) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = BufReader::new(r).read_to_end(&mut buf);
        let s = String::from_utf8_lossy(&buf).into_owned();
        if !s.is_empty() {
            eprint!("[{label}] {s}");
        }
        s
    })
}

/// Print each ref's current value to stderr, so the user can undo.
pub fn print_pre_migrate_refs(cwd: &Path, refs: &[String]) {
    eprintln!("Pre-migrate ref values (record these for manual rollback):");
    for r in refs {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["rev-parse", r])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let sha = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                eprintln!("  {r} = {sha}");
            }
            _ => eprintln!("  {r} = <unresolved>"),
        }
    }
}

/// Force-checkout HEAD so the working tree matches the rewritten history.
///
/// This overwrites uncommitted changes to tracked files. Does nothing in
/// a bare repository or with no HEAD.
pub fn refresh_working_tree(cwd: &Path) -> Result<(), MigrateError> {
    if !super::head_exists(cwd) {
        return Ok(());
    }
    let bare = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--is-bare-repository"])
        .output()
        .map_err(MigrateError::Io)?;
    if bare.status.success() && String::from_utf8_lossy(&bare.stdout).trim() == "true" {
        return Ok(());
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["checkout", "-f", "HEAD", "--"])
        .output()
        .map_err(MigrateError::Io)?;
    if !out.status.success() {
        return Err(MigrateError::Other(format!(
            "git checkout -f failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Ask before rewriting history over a dirty working tree.
///
/// Rewriting ends with `git checkout -f`, which discards uncommitted
/// changes. Like upstream, this asks on stderr and reads the answer from
/// stdin, and anything but yes refuses. `yes` (`--yes`) skips the
/// question.
pub fn ensure_working_copy_clean(cwd: &Path, yes: bool) -> Result<(), MigrateError> {
    if !working_tree_dirty(cwd)? {
        return Ok(());
    }
    let proceed = yes || confirm_override(std::io::stdin().lock(), &mut std::io::stderr())?;
    if !proceed {
        return Err(MigrateError::Usage("working copy must not be dirty".into()));
    }
    eprintln!("changes in your working copy will be overridden ...");
    Ok(())
}

/// Ask until the answer is yes or no, and return whether it was yes.
///
/// An empty answer, or the end of input, is no. Other answers ask again.
fn confirm_override(
    mut input: impl std::io::BufRead,
    out: &mut impl std::io::Write,
) -> Result<bool, MigrateError> {
    loop {
        write!(
            out,
            "override changes in your working copy?  \
             All uncommitted changes will be lost! [y/N] "
        )?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(false);
        }
        match line.trim() {
            "n" | "N" | "" => return Ok(false),
            "y" | "Y" => return Ok(true),
            _ => {}
        }
        if !line.ends_with('\n') {
            writeln!(out)?;
        }
    }
}

/// Whether tracked files have uncommitted changes.
///
/// Always false in a bare repository.
pub fn working_tree_dirty(cwd: &Path) -> Result<bool, MigrateError> {
    let bare_out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--is-bare-repository"])
        .output()
        .map_err(MigrateError::Io)?;
    if bare_out.status.success() && String::from_utf8_lossy(&bare_out.stdout).trim() == "true" {
        return Ok(false);
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        // Untracked files don't count, as upstream. The shell tests
        // often write a log file into the working tree first.
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .map_err(MigrateError::Io)?;
    if !out.status.success() {
        return Err(MigrateError::Other(format!(
            "git status failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(!out.stdout.trim_ascii().is_empty())
}

/// Read the repository's `info/attributes`, or `None` if it's missing
/// or unreadable.
fn read_info_attrs(cwd: &Path) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--git-path", "info/attributes"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if raw.is_empty() {
        return None;
    }
    let path = if Path::new(&raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        cwd.join(raw)
    };
    std::fs::read(path).ok()
}

/// Read the global attributes file, or `None` if it's missing or
/// unreadable.
///
/// That's `core.attributesFile`, or as in Git
/// `$XDG_CONFIG_HOME/git/attributes` or `$HOME/.config/git/attributes`
/// when it's unset.
fn read_global_attrs(cwd: &Path) -> Option<Vec<u8>> {
    let configured = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["config", "--get", "core.attributesFile"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                if s.is_empty() { None } else { Some(s) }
            } else {
                None
            }
        });

    let path = if let Some(p) = configured {
        git_lfs_git::expand_tilde(&p)
    } else {
        let xdg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        xdg.join("git").join("attributes")
    };
    std::fs::read(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answer `confirm_override` gives for `input`, and how many times
    /// it asked.
    fn ask(input: &str) -> (bool, usize) {
        let mut out = Vec::new();
        let yes = confirm_override(input.as_bytes(), &mut out).unwrap();
        let out = String::from_utf8(out).unwrap();
        (yes, out.matches("override changes").count())
    }

    #[test]
    fn yes_proceeds() {
        assert_eq!(ask("y\n"), (true, 1));
        assert_eq!(ask("Y\n"), (true, 1));
    }

    #[test]
    fn no_empty_and_end_of_input_refuse() {
        assert_eq!(ask("n\n"), (false, 1));
        assert_eq!(ask("\n"), (false, 1));
        assert_eq!(ask(""), (false, 1));
    }

    #[test]
    fn another_answer_asks_again() {
        assert_eq!(ask("x\ny\n"), (true, 2));
        assert_eq!(ask("x\n"), (false, 2));
    }
}
