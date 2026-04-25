//! `git lfs pointer` — debug helper that builds and inspects pointer files.
//!
//! Three rough modes:
//! - `--check` (with `--file` or `--stdin`): pure validity check. Exit 0
//!   if input is a parseable pointer, 1 if not, 2 if `--strict` and the
//!   pointer wasn't byte-canonical.
//! - `--file <path>`: hash a working-tree file, build the canonical
//!   pointer, print to stdout. With `--pointer` / `--stdin` *also* given,
//!   compare against an existing pointer and report match/mismatch.
//! - `--pointer <path>` or `--stdin`: parse and display an existing
//!   pointer (echo to stderr).
//!
//! Note: upstream's compare mode uses `git hash-object` on the encoded
//! bytes to compute git blob OIDs and compares those. We compare the
//! raw byte equality of our canonical encoding vs. the supplied pointer
//! bytes — equivalent semantics for any real input (a byte-identical
//! pointer hashes the same), without dragging git into the hot path.

use std::io::Read;
use std::path::PathBuf;

use git_lfs_pointer::{Oid, Pointer};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum PointerError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
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
}

/// Run the command. Returns the intended process exit code (0 = success,
/// 1 = mismatch / parse error / "nothing to do", 2 = `--strict` failed).
pub fn run(opts: &Options) -> Result<i32, PointerError> {
    if opts.check {
        return run_check(opts);
    }

    let mut something = false;
    let mut built: Option<Vec<u8>> = None;

    if let Some(path) = &opts.file {
        something = true;
        let bytes = std::fs::read(path)?;
        let oid_bytes: [u8; 32] = Sha256::digest(&bytes).into();
        let oid = Oid::from_bytes(oid_bytes);
        let pointer = Pointer::new(oid, bytes.len() as u64);
        let encoded = pointer.encode();

        eprintln!("Git LFS pointer for {}", path.display());
        eprintln!();
        // Pointer text goes to stdout (matches upstream).
        print!("{encoded}");
        built = Some(encoded.into_bytes());
    }

    let mut compared: Option<Vec<u8>> = None;
    if let Some(path) = &opts.pointer {
        if opts.stdin {
            return Err(PointerError::Usage(
                "cannot read from STDIN and --pointer".into(),
            ));
        }
        something = true;
        let bytes = std::fs::read(path)?;
        eprintln!();
        eprintln!("Pointer from {}", path.display());
        eprintln!();
        // Echo the parsed-from input to stderr — matches upstream so a
        // user diffing two `git lfs pointer` invocations sees both
        // texts side-by-side.
        eprint!("{}", String::from_utf8_lossy(&bytes));
        if Pointer::parse(&bytes).is_err() {
            eprintln!();
            eprintln!("warning: input does not parse as a pointer");
            return Ok(1);
        }
        compared = Some(bytes);
    } else if opts.stdin {
        something = true;
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        eprintln!();
        eprintln!("Pointer from STDIN");
        eprintln!();
        eprint!("{}", String::from_utf8_lossy(&bytes));
        if Pointer::parse(&bytes).is_err() {
            eprintln!();
            eprintln!("warning: input does not parse as a pointer");
            return Ok(1);
        }
        compared = Some(bytes);
    }

    if let (Some(a), Some(b)) = (built, compared)
        && a != b
    {
        eprintln!();
        eprintln!("Pointers do not match");
        return Ok(1);
    }

    if !something {
        eprintln!("Nothing to do!");
        return Ok(1);
    }
    Ok(0)
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
        // Missing trailing newline parses but isn't byte-canonical
        // (see pointer/src/lib.rs `non_canonical_examples`).
        let oid = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";
        let noncanon = format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 12345"
        );

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
        assert!(run_check(&opts).is_err());
    }

    #[test]
    fn check_neither_file_nor_stdin_errors() {
        let opts = Options {
            check: true,
            ..Default::default()
        };
        assert!(run_check(&opts).is_err());
    }
}
