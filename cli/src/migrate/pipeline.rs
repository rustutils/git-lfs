//! Subprocess plumbing shared by `migrate import` and `migrate export`.
//!
//! Spawns `git fast-export --full-tree` and `git fast-import --force`,
//! drains their stderr concurrently (so neither blocks on a full pipe
//! buffer), runs the [`Transform`] on this thread, and reports
//! per-child errors on failure.

use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;

use git_lfs_store::Store;

use super::MigrateError;
use super::transform::{Mode, Options as TransformOptions, Stats, Transform};

pub fn run_pipeline(
    cwd: &Path,
    include_refs: &[String],
    exclude_refs: &[String],
    transform_opts: TransformOptions,
    mode: Mode,
    store: &Store,
) -> Result<Stats, MigrateError> {
    let mut export = spawn_fast_export(cwd, include_refs, exclude_refs)?;
    let mut import = spawn_fast_import(cwd)?;

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
    // --full-tree: every commit re-states its full tree, so we don't
    // need to chase inheritance.
    // --reference-excluded-parents: keeps from/merge references valid
    // when the user excluded a parent.
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(cwd).args([
        "fast-export",
        "--full-tree",
        "--reencode=yes",
        "--reference-excluded-parents",
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

fn spawn_fast_import(cwd: &Path) -> Result<Child, MigrateError> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        .args(["fast-import", "--force", "--quiet"]);
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

pub fn refresh_working_tree(cwd: &Path) -> Result<(), MigrateError> {
    if !super::head_exists(cwd) {
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

pub fn working_tree_dirty(cwd: &Path) -> Result<bool, MigrateError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain"])
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
