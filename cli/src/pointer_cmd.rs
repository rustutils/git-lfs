//! `git lfs pointer`: build a pointer for a file, or check one.
//!
//! - `--check` with `--file` or `--stdin` exits 0 for a pointer and 1 for
//!   anything else. With `--strict`, a pointer that isn't canonical exits 2.
//! - `--file` prints the pointer for a file, through the configured
//!   extensions unless `--no-extensions` is given.
//! - `--pointer` or `--stdin` parses a pointer and echoes it to stderr.
//!   Given with `--file`, the two pointers are compared by Git blob ID.
//!
//! t-pointer.sh checks which stream each line goes to and in what order.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use git_lfs_filter::{CleanError, CleanExtension, build_pointer_with_extensions};
use git_lfs_pointer::{Oid, Pointer};
use sha1::{Digest, Sha1};
use sha2::Sha256;

#[derive(Debug, thiserror::Error)]
pub enum PointerError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Clean(#[from] CleanError),
    #[error("{0}")]
    Usage(String),
}

#[derive(Debug, Default)]
pub struct Options {
    pub file: Option<PathBuf>,
    pub pointer: Option<PathBuf>,
    pub stdin: bool,
    pub check: bool,
    pub strict: bool,
    pub no_strict: bool,
    /// `--no-extensions`: build the `--file` pointer without extensions.
    pub no_extensions: bool,
    /// The configured clean extensions. Empty outside a repository.
    pub extensions: Vec<CleanExtension>,
}

/// Run the command and return the exit code.
///
/// The code is 1 if the pointers don't match, a pointer can't be parsed, or
/// there's nothing to do, and 2 if `--check --strict` finds a pointer that
/// isn't canonical.
pub fn run(opts: &Options) -> Result<i32, PointerError> {
    if opts.check {
        return run_check(opts);
    }

    // Blob IDs are printed only when there are two pointers to compare.
    let comparing = opts.file.is_some() && (opts.pointer.is_some() || opts.stdin);

    let mut something = false;
    let mut built_oid: Option<String> = None;

    let stdout = std::io::stdout();
    let stderr = std::io::stderr();

    let use_extensions = !opts.no_extensions && !opts.extensions.is_empty();

    if let Some(path) = &opts.file {
        something = true;
        let pointer = if use_extensions {
            let mut file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("open {}: {}", path.display(), e);
                    return Ok(1);
                }
            };
            let leaf = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            build_pointer_with_extensions(
                &mut file,
                &leaf,
                &opts.extensions,
                &std::env::temp_dir(),
            )?
        } else {
            let bytes = match std::fs::read(path) {
                Ok(b) => b,
                Err(e) => {
                    // The format of Go's `os.Open` errors.
                    eprintln!("open {}: {}", path.display(), e);
                    return Ok(1);
                }
            };
            let oid_bytes: [u8; 32] = Sha256::digest(&bytes).into();
            Pointer::new(Oid::from_bytes(oid_bytes), bytes.len() as u64)
        };
        let encoded = pointer.encode();

        // Flush before switching streams, so output merged with `2>&1`
        // keeps its order.
        let mut e = stderr.lock();
        let mut o = stdout.lock();
        writeln!(e, "Git LFS pointer for {}", path.display())?;
        if use_extensions {
            writeln!(
                e,
                "warning: Using LFS extensions, use --no-extensions for a plain pointer."
            )?;
        }
        writeln!(e)?;
        e.flush()?;
        write!(o, "{encoded}")?;
        o.flush()?;
        if comparing {
            // The extra blank line separates this from the next section.
            let blob_oid = git_blob_oid(encoded.as_bytes());
            writeln!(e)?;
            writeln!(e, "Git blob OID: {blob_oid}")?;
            writeln!(e)?;
            e.flush()?;
        }
        built_oid = Some(git_blob_oid(encoded.as_bytes()));
    }

    let mut compared_oid: Option<String> = None;
    let mut compared_has_extensions = false;
    if let Some(path) = &opts.pointer {
        if opts.stdin {
            return Err(PointerError::Usage(
                "cannot read from STDIN and --pointer".into(),
            ));
        }
        something = true;
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("open {}: {}", path.display(), e);
                return Ok(1);
            }
        };
        emit_compared_section(
            &stderr,
            &path.display().to_string(),
            &bytes,
            comparing,
            &mut compared_oid,
            &mut compared_has_extensions,
        )?;
        if Pointer::parse(&bytes).is_err() {
            return Ok(1);
        }
    } else if opts.stdin {
        if std::io::stdin().is_terminal() {
            // Upstream's `requireStdin` message.
            eprintln!(
                "Cannot read from STDIN. The --stdin flag expects a pointer file from STDIN."
            );
            return Ok(1);
        }
        something = true;
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        emit_compared_section(
            &stderr,
            "STDIN",
            &bytes,
            comparing,
            &mut compared_oid,
            &mut compared_has_extensions,
        )?;
        if Pointer::parse(&bytes).is_err() {
            return Ok(1);
        }
    }

    if comparing
        && let (Some(a), Some(b)) = (&built_oid, &compared_oid)
        && a != b
    {
        let mut e = stderr.lock();
        writeln!(e)?;
        writeln!(e, "Pointers do not match")?;
        if use_extensions || compared_has_extensions {
            writeln!(e, "note: Mismatch may be due to differing LFS extensions.")?;
        }
        e.flush()?;
        return Ok(1);
    }

    if !something {
        let mut e = stderr.lock();
        writeln!(e, "Nothing to do!")?;
        e.flush()?;
        return Ok(1);
    }
    Ok(0)
}

/// Print the `Pointer from <name>` section to stderr.
///
/// The input is echoed only if it parses. Otherwise an error is printed in
/// its place. Sets `has_extensions` if the pointer has extensions, and
/// `compared_oid` to its blob ID when `comparing`.
fn emit_compared_section(
    stderr: &std::io::Stderr,
    name: &str,
    bytes: &[u8],
    comparing: bool,
    compared_oid: &mut Option<String>,
    has_extensions: &mut bool,
) -> std::io::Result<()> {
    let mut e = stderr.lock();
    writeln!(e, "Pointer from {name}")?;
    writeln!(e)?;
    let parsed = match Pointer::parse(bytes) {
        Ok(p) => p,
        Err(_) => {
            // No trailing newline: t-pointer.sh compares with `printf %s`.
            write!(e, "Pointer file error: invalid header")?;
            e.flush()?;
            return Ok(());
        }
    };
    *has_extensions = !parsed.extensions.is_empty();
    e.write_all(bytes)?;
    if comparing {
        let oid = git_blob_oid(bytes);
        writeln!(e)?;
        writeln!(e, "Git blob OID: {oid}")?;
        *compared_oid = Some(oid);
    }
    e.flush()?;
    Ok(())
}

/// The SHA-1 Git blob ID of `content`.
///
/// Upstream runs `git hash-object`, which gives a SHA-256 ID in a SHA-256
/// repository. This is always SHA-1.
fn git_blob_oid(content: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", content.len()).as_bytes());
    h.update(content);
    let bytes: [u8; 20] = h.finalize().into();
    bytes.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn run_check(opts: &Options) -> Result<i32, PointerError> {
    if opts.strict && opts.no_strict {
        return Err(PointerError::Usage(
            "cannot combine --strict with --no-strict".into(),
        ));
    }
    if opts.pointer.is_some() {
        return Err(PointerError::Usage(
            "cannot combine --check with --pointer".into(),
        ));
    }
    let bytes = match (&opts.file, opts.stdin) {
        (Some(_), true) => {
            return Err(PointerError::Usage(
                "with --check, --file cannot be combined with --stdin".into(),
            ));
        }
        (Some(path), false) => std::fs::read(path)?,
        (None, true) => {
            let mut buf = Vec::new();
            std::io::stdin().read_to_end(&mut buf)?;
            buf
        }
        (None, false) => {
            return Err(PointerError::Usage(
                "must specify either --file or --stdin with --check".into(),
            ));
        }
    };
    match Pointer::parse(&bytes) {
        Ok(p) => {
            if opts.strict && !p.canonical {
                Ok(2)
            } else {
                Ok(0)
            }
        }
        Err(_) => Ok(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn check_valid_pointer_returns_zero() {
        let p = Pointer::new(Oid::EMPTY, 0).encode();
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("p.txt");
        std::fs::write(&path, &p).unwrap();
        let opts = Options {
            check: true,
            file: Some(path),
            ..Default::default()
        };
        assert_eq!(run_check(&opts).unwrap(), 0);
    }

    #[test]
    fn check_invalid_returns_one() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("p.txt");
        std::fs::write(&path, b"not a pointer").unwrap();
        let opts = Options {
            check: true,
            file: Some(path),
            ..Default::default()
        };
        assert_eq!(run_check(&opts).unwrap(), 1);
    }

    #[test]
    fn check_strict_returns_two_for_noncanonical() {
        let oid = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";
        let noncanon =
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 12345");

        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("p.txt");
        std::fs::write(&path, noncanon.as_bytes()).unwrap();
        let opts = Options {
            check: true,
            strict: true,
            file: Some(path),
            ..Default::default()
        };
        assert_eq!(run_check(&opts).unwrap(), 2);
    }

    #[test]
    fn check_strict_and_no_strict_combined_errors() {
        let opts = Options {
            check: true,
            strict: true,
            no_strict: true,
            file: Some(PathBuf::from("/dev/null")),
            ..Default::default()
        };
        let err = run_check(&opts).unwrap_err();
        assert!(matches!(err, PointerError::Usage(_)));
    }

    #[test]
    fn check_with_pointer_flag_errors() {
        let opts = Options {
            check: true,
            pointer: Some(PathBuf::from("/dev/null")),
            file: Some(PathBuf::from("/dev/null")),
            ..Default::default()
        };
        let err = run_check(&opts).unwrap_err();
        assert!(matches!(err, PointerError::Usage(_)), "got {err:?}");
    }

    #[test]
    fn check_neither_file_nor_stdin_errors() {
        let opts = Options {
            check: true,
            ..Default::default()
        };
        let err = run_check(&opts).unwrap_err();
        assert!(matches!(err, PointerError::Usage(_)), "got {err:?}");
    }

    #[test]
    fn git_blob_oid_matches_known_value() {
        // `printf hello | git hash-object --stdin`
        let oid = git_blob_oid(b"hello");
        assert_eq!(oid, "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0");
    }

    #[test]
    fn git_blob_oid_for_canonical_pointer() {
        // The blob ID t-pointer.sh expects.
        let pointer = "version https://git-lfs.github.com/spec/v1\n\
             oid sha256:6c17f2007cbe934aee6e309b28b2dba3c119c5dff2ef813ed124699efe319868\n\
             size 7\n"
            .to_string();
        assert_eq!(
            git_blob_oid(pointer.as_bytes()),
            "e18acd45d7e3ce0451d1d637f9697aa508e07dee",
        );
    }
}
