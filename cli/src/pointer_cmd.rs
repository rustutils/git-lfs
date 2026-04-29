//! `git lfs pointer` — debug helper that builds and inspects pointer files.
//!
//! Three modes:
//!
//! - `--check` (with `--file` or `--stdin`): pure validity check. Exit
//!   0 if the input is a parseable pointer, 1 if not, 2 if `--strict`
//!   and the pointer wasn't byte-canonical.
//! - `--file <path>`: hash a working-tree file, build the canonical
//!   pointer, print to stdout. With `--pointer` / `--stdin` *also*
//!   given, compare against an existing pointer and print whether
//!   they match. Comparison uses git's blob-OID semantics — same as
//!   upstream.
//! - `--pointer <path>` or `--stdin`: parse and display an existing
//!   pointer (echo to stderr).
//!
//! Output ordering and stream destinations match upstream exactly so
//! the upstream `t-pointer.sh` shell tests pass against this binary.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;

use git_lfs_pointer::{Oid, Pointer};
use sha1::{Digest, Sha1};
use sha2::Sha256;

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

    // upstream: comparing := pointerCompare != "" || pointerStdin
    // then if --file is set, the value is preserved; else reset to false.
    // Net effect: we print Git-blob-OID lines iff we have BOTH a built
    // and a parsed pointer to compare.
    let comparing = opts.file.is_some() && (opts.pointer.is_some() || opts.stdin);

    let mut something = false;
    let mut built_oid: Option<String> = None;
    let mut built_text: Option<Vec<u8>> = None;

    let stdout = std::io::stdout();
    let stderr = std::io::stderr();

    if let Some(path) = &opts.file {
        something = true;
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                // Match upstream's `os.Open(...)` error format:
                // `open <path>: <reason>` (Go's stdlib formats it
                // that way). Print verbatim — no `git-lfs:` prefix.
                eprintln!("open {}: {}", path.display(), e);
                return Ok(1);
            }
        };
        let oid_bytes: [u8; 32] = Sha256::digest(&bytes).into();
        let oid = Oid::from_bytes(oid_bytes);
        let pointer = Pointer::new(oid, bytes.len() as u64);
        let encoded = pointer.encode();

        // stderr: header + blank line, then stdout: pointer text.
        // Flush each before crossing streams so a `2>&1`-merged
        // capture sees the lines in the order we wrote them.
        let mut e = stderr.lock();
        let mut o = stdout.lock();
        writeln!(e, "Git LFS pointer for {}", path.display())?;
        writeln!(e)?;
        e.flush()?;
        write!(o, "{encoded}")?;
        o.flush()?;
        if comparing {
            // `\nGit blob OID: <hex>\n\n` to stderr. Two trailing
            // newlines because the next section's "Pointer from X"
            // header carries only one.
            let blob_oid = git_blob_oid(encoded.as_bytes());
            writeln!(e)?;
            writeln!(e, "Git blob OID: {blob_oid}")?;
            writeln!(e)?;
            e.flush()?;
        }
        built_oid = Some(git_blob_oid(encoded.as_bytes()));
        built_text = Some(encoded.into_bytes());
    }

    let mut compared_oid: Option<String> = None;
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
        )?;
        if Pointer::parse(&bytes).is_err() {
            return Ok(1);
        }
    } else if opts.stdin {
        if std::io::stdin().is_terminal() {
            // No piped input — emit the friendly error upstream's
            // requireStdin uses. No `git-lfs:` prefix.
            eprintln!(
                "Cannot read from STDIN. The --stdin flag expects a pointer file from STDIN."
            );
            return Ok(1);
        }
        something = true;
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        emit_compared_section(&stderr, "STDIN", &bytes, comparing, &mut compared_oid)?;
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
        e.flush()?;
        return Ok(1);
    }
    let _ = built_text; // built_text retained for symmetry; dropped here.

    if !something {
        let mut e = stderr.lock();
        writeln!(e, "Nothing to do!")?;
        e.flush()?;
        return Ok(1);
    }
    Ok(0)
}

/// Emit the `Pointer from <name>\n\n[…]` block to stderr. On a parse
/// failure we print the error *without* echoing the input — matching
/// upstream's order (parse first, then echo iff successful).
fn emit_compared_section(
    stderr: &std::io::Stderr,
    name: &str,
    bytes: &[u8],
    comparing: bool,
    compared_oid: &mut Option<String>,
) -> std::io::Result<()> {
    let mut e = stderr.lock();
    writeln!(e, "Pointer from {name}")?;
    writeln!(e)?;
    if Pointer::parse(bytes).is_err() {
        // No `git-lfs:` prefix and no trailing newline — tests
        // compare with `printf %s` which doesn't add one.
        write!(e, "Pointer file error: invalid header")?;
        e.flush()?;
        return Ok(());
    }
    // Successful parse: echo the input verbatim so a user diffing
    // two `git lfs pointer` invocations sees both texts.
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

/// Compute git's blob OID over `content` — `SHA-1("blob <len>\0<content>")`.
/// Matches what `git hash-object --stdin` produces and what upstream
/// `pointer` calls into via the git binary.
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

    #[test]
    fn git_blob_oid_matches_known_value() {
        // SHA-1 of "blob 5\0hello" = b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0
        // (this is what `printf 'hello' | git hash-object --stdin` returns)
        let oid = git_blob_oid(b"hello");
        assert_eq!(oid, "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0");
    }

    #[test]
    fn git_blob_oid_for_canonical_pointer() {
        // The OID upstream's t-pointer.sh expects for the standard
        // "simple\n" 7-byte pointer.
        let pointer = format!(
            "version https://git-lfs.github.com/spec/v1\n\
             oid sha256:6c17f2007cbe934aee6e309b28b2dba3c119c5dff2ef813ed124699efe319868\n\
             size 7\n",
        );
        assert_eq!(
            git_blob_oid(pointer.as_bytes()),
            "e18acd45d7e3ce0451d1d637f9697aa508e07dee",
        );
    }
}
