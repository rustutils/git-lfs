//! The progress log named by `GIT_LFS_PROGRESS`.
//!
//! Each line is `<event> <n>/<count> <done>/<size> <name>`, as in upstream,
//! where `<n>/<count>` numbers the file among those in the operation. A
//! `<size>` that isn't known yet is `?`s, as wide as `<done>`. Lines are
//! appended, and the file and its directory are created as needed.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

/// An open progress log.
#[derive(Debug)]
pub struct ProgressLog {
    file: Mutex<File>,
}

impl ProgressLog {
    /// Open the log `GIT_LFS_PROGRESS` names, if it's set.
    ///
    /// A relative path, or one that can't be opened, prints upstream's
    /// `Error creating progress logger` message and gives `None`.
    pub fn from_env() -> Option<Self> {
        let path = std::env::var_os("GIT_LFS_PROGRESS").filter(|p| !p.is_empty())?;
        let path = Path::new(&path);
        let opened = if path.is_absolute() {
            Self::open(path).map_err(|e| e.to_string())
        } else {
            Err("GIT_LFS_PROGRESS must be an absolute path".to_owned())
        };
        match opened {
            Ok(log) => Some(log),
            Err(e) => {
                eprintln!("Error creating progress logger: {e}");
                None
            }
        }
    }

    fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    /// Append one line. A failed write is ignored.
    pub fn line(
        &self,
        event: &str,
        n: usize,
        count: usize,
        done: u64,
        size: Option<u64>,
        name: &str,
    ) {
        let line = format!(
            "{event} {n}/{count} {} {name}\n",
            format_fraction(done, size)
        );
        let mut file = self.file.lock().unwrap();
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

/// `done/size`, with `?`s for an unknown size, like upstream's
/// `FormatFraction`.
fn format_fraction(done: u64, size: Option<u64>) -> String {
    let done = done.to_string();
    match size {
        Some(size) => format!("{done}/{size}"),
        None => format!("{done}/{}", "?".repeat(done.len())),
    }
}

/// Log that one file is done, as `<event> 1/1 <size>/<size> <name>`.
///
/// For the filters, which handle one file at a time. Does nothing without
/// `GIT_LFS_PROGRESS` or a name.
pub(crate) fn log_file(event: &str, name: &str, size: u64) {
    if name.is_empty() {
        return;
    }
    if let Some(log) = ProgressLog::from_env() {
        log.line(event, 1, 1, size, Some(size), name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fraction_with_unknown_size_uses_question_marks() {
        assert_eq!(format_fraction(12, Some(30)), "12/30");
        assert_eq!(format_fraction(123, None), "123/???");
    }

    #[test]
    fn lines_are_appended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs/progress.log");
        let log = ProgressLog::open(&path).unwrap();
        log.line("clean", 1, 1, 5, Some(5), "a.dat");
        let log = ProgressLog::open(&path).unwrap();
        log.line("download", 2, 3, 1, None, "b.dat");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "clean 1/1 5/5 a.dat\ndownload 2/3 1/? b.dat\n"
        );
    }
}
