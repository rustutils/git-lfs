//! `git lfs logs`: list, show, and clear the crash logs in `.git/lfs/logs`.
//!
//! No command writes a crash log on failure. Only `git lfs logs boomtown`
//! writes one, a sample in the shape of upstream's `logPanicToWriter`:
//!
//! ```text
//! git-lfs <version>
//! <git --version>
//!
//! $ git-lfs logs boomtown
//! Sample panic message: Sample error message: Sample wrapped error message
//! ```
//!
//! Log names are UTC timestamps with nanoseconds,
//! `YYYYMMDDTHHMMSS.NNNNNNNNN.log`, so sorting them by name sorts them by
//! time.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum LogsError {
    #[error(transparent)]
    Git(#[from] git_lfs_git::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("Error reading log: {0}")]
    Read(String),
}

/// List the log names, oldest first.
pub fn list(cwd: &Path) -> Result<u8, LogsError> {
    let dir = log_dir(cwd)?;
    for name in sorted_logs(&dir) {
        println!("{name}");
    }
    Ok(0)
}

/// Print the newest log.
pub fn last(cwd: &Path) -> Result<u8, LogsError> {
    let dir = log_dir(cwd)?;
    let logs = sorted_logs(&dir);
    let Some(name) = logs.last() else {
        println!("No logs to show");
        return Ok(0);
    };
    show(cwd, name)
}

/// Print the log called `name`.
///
/// A missing or unreadable log is a [`LogsError::Read`], which exits 1.
/// Upstream exits 2.
pub fn show(cwd: &Path, name: &str) -> Result<u8, LogsError> {
    let dir = log_dir(cwd)?;
    let bytes = fs::read(dir.join(name)).map_err(|_| LogsError::Read(name.to_owned()))?;
    use std::io::Write;
    std::io::stdout().write_all(&bytes)?;
    Ok(0)
}

/// Delete the log directory.
pub fn clear(cwd: &Path) -> Result<u8, LogsError> {
    let dir = log_dir(cwd)?;
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    println!("Cleared {}", dir.display());
    Ok(0)
}

/// Write a sample crash log and return exit code 2.
///
/// `argv` is the command line without the program name.
pub fn boomtown(cwd: &Path, argv: &[String]) -> Result<u8, LogsError> {
    let dir = log_dir(cwd)?;
    fs::create_dir_all(&dir)?;
    let filename = format!("{}.log", panic_timestamp());
    let path = dir.join(&filename);

    let git_version = git_version().unwrap_or_else(|| "git: <unknown>".to_owned());
    // Upstream logs the program's base name, and tests grep for
    // `$ git-lfs ...`.
    let prog = std::env::args()
        .next()
        .and_then(|arg0| {
            Path::new(&arg0)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "git-lfs".to_owned());
    let cmd_line = if argv.is_empty() {
        prog
    } else {
        format!("{prog} {}", argv.join(" "))
    };
    let body = format!(
        "{} {}\n{}\n\n$ {}\nSample panic message: Sample error message: Sample wrapped error message\n",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        git_version,
        cmd_line,
    );
    fs::write(&path, &body)?;

    eprintln!("Sample panic message: Sample error message: Sample wrapped error message");
    eprintln!();
    eprintln!("Errors logged to '{}'.", path.display());
    eprintln!("Use `git lfs logs last` to view the log.");
    Ok(2)
}

/// The log directory, which may not exist.
fn log_dir(cwd: &Path) -> Result<PathBuf, LogsError> {
    Ok(git_lfs_git::lfs_dir(cwd)?.join("logs"))
}

/// The names of the files in `dir`, sorted, or none if it can't be read.
fn sorted_logs(dir: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .collect();
    names.sort();
    names
}

/// The current UTC time as `YYYYMMDDTHHMMSS.NNNNNNNNN`.
///
/// Like upstream's `Format("20060102T150405.999999999")`, except that
/// upstream drops trailing zeros from the nanoseconds.
fn panic_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let nanos = now.subsec_nanos();
    let secs = now.as_secs() as i64;
    let (y, mo, d, hr, mn, se) = epoch_to_ymdhms(secs);
    format!("{y:04}{mo:02}{d:02}T{hr:02}{mn:02}{se:02}.{nanos:09}")
}

/// Split Unix time into UTC year, month, day, hour, minute, and second.
fn epoch_to_ymdhms(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    const SECS_PER_DAY: i64 = 86_400;
    let days = secs.div_euclid(SECS_PER_DAY);
    let tod = secs.rem_euclid(SECS_PER_DAY);
    let hr = (tod / 3600) as u32;
    let mn = ((tod % 3600) / 60) as u32;
    let se = (tod % 60) as u32;

    // Days since 1970-01-01.
    let mut year = 1970;
    let mut remaining = days;
    loop {
        let dy = if is_leap(year) { 366 } else { 365 };
        if remaining < dy {
            break;
        }
        remaining -= dy;
        year += 1;
    }
    let months = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1u32;
    for (i, &dm) in months.iter().enumerate() {
        let days_in = if i == 1 && is_leap(year) { 29 } else { dm };
        if remaining < days_in {
            break;
        }
        remaining -= days_in;
        month += 1;
    }
    (year, month, remaining as u32 + 1, hr, mn, se)
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The output of `git --version`, or `None` if it fails.
fn git_version() -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_to_ymdhms_basics() {
        // 1970-01-01T00:00:00
        assert_eq!(epoch_to_ymdhms(0), (1970, 1, 1, 0, 0, 0));
        // 2000-01-01T00:00:00 = 946684800
        assert_eq!(epoch_to_ymdhms(946_684_800), (2000, 1, 1, 0, 0, 0));
        // 2024-02-29T12:00:00 (leap day)
        assert_eq!(epoch_to_ymdhms(1_709_208_000), (2024, 2, 29, 12, 0, 0));
    }
}
