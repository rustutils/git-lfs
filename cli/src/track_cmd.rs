//! `git lfs track` CLI handler — flag parsing lives in `main.rs`; this
//! module owns the rendering, blocklist diagnostics, ls-files scan, and
//! JSON output.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::install;
use crate::lockable::{self, HeldLocks};
use crate::track::{self, LockableMode, TrackOptions, TrackResult, unescape_attr_pattern};

pub struct Args<'a> {
    pub cwd: &'a Path,
    pub patterns: &'a [String],
    pub lockable: bool,
    pub not_lockable: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub json: bool,
    pub no_excluded: bool,
    /// `--filename`: each pattern is a literal name; escape glob
    /// metacharacters before writing to `.gitattributes`.
    pub filename: bool,
}

pub fn run(args: Args<'_>) -> Result<u8, Box<dyn std::error::Error>> {
    if args.lockable && args.not_lockable {
        return Err("--lockable and --not-lockable are mutually exclusive".into());
    }

    // Both write and listing modes require a real working tree. Match
    // git's exit code (128) for "not a repo" and "must be in a work
    // tree" failures.
    if let Some(code) = check_repo_context(args.cwd) {
        return Ok(code);
    }

    // Auto-install hooks (mirroring upstream's `installHooks(false)`
    // call from `git lfs track`). Honors `GIT_LFS_TRACK_NO_INSTALL_HOOKS`
    // so users who deliberately skip hook installation aren't bothered.
    // Best-effort — we never fail the track on hook trouble.
    if std::env::var_os("GIT_LFS_TRACK_NO_INSTALL_HOOKS").is_none() {
        let _ = install::try_install_hooks(args.cwd);
    }

    if args.patterns.is_empty() {
        return list(args.cwd, args.json, args.no_excluded);
    }

    // Blocklist check: print the diagnostic to stdout (per upstream's
    // contract — `t-track.sh` redirects stdout to a log and greps it),
    // and exit non-zero without touching `.gitattributes`.
    for pat in args.patterns {
        if let Some(forbidden) = track::forbidden_match(pat) {
            println!("Pattern '{pat}' matches forbidden file '{forbidden}'");
            return Ok(1);
        }
    }

    let lockable = if args.lockable {
        LockableMode::Yes
    } else if args.not_lockable {
        LockableMode::No
    } else {
        LockableMode::Default
    };

    let opts = TrackOptions {
        lockable,
        dry_run: args.dry_run,
        literal_filename: args.filename,
    };
    let outcome = track::track(args.cwd, args.patterns, opts)?;

    for p in &outcome.patterns {
        let display = unescape_attr_pattern(&p.pattern);
        match p.result {
            TrackResult::Added | TrackResult::Replaced => {
                println!("Tracking \"{display}\"");
            }
            TrackResult::AlreadyTracked => {
                println!("\"{display}\" already supported");
            }
        }
    }

    // For each newly-added pattern, walk the index for files that
    // match. We print a `Touching "<path>"` line and bump the file's
    // mtime — that's it. We do NOT run `git add` on them, even though
    // the freshly-tracked attribute means a future `git add` would
    // route them through the LFS clean filter.
    //
    // Why mtime-only: if the user has already committed `existing.dat`
    // as a raw blob, the blunt `git add` approach silently re-stages
    // it as a pointer the next time they commit something else.
    // Upstream (`commands/command_track.go`) only touches the mtime so
    // git's stat cache invalidates and the user sees the file as
    // "modified" on the next status — explicit `git add` is left to
    // them.
    //
    // The chmod side-effects happen even for `AlreadyTracked` patterns
    // so a re-issued `--lockable` / `--not-lockable` against a
    // previously-tracked pattern still converges the working tree.
    // With `--dry-run`, neither chmod nor the mtime touch fires.
    //
    // Both `attrs` and `held` are lazy: built only when the first
    // pattern with matching files needs them. This avoids the
    // credential-helper churn of an unnecessary `verify_locks` call
    // when there are no .dat-or-whatever files in the index yet.
    let mut attrs: Option<git_lfs_git::AttrSet> = None;
    let mut held: Option<HeldLocks> = None;

    for p in &outcome.patterns {
        let restage = !matches!(p.result, TrackResult::AlreadyTracked);
        let matches = lockable::ls_files_matching(args.cwd, &p.pattern)?;
        if restage {
            if args.verbose {
                println!(
                    "Found {} files previously added to Git matching pattern: {}",
                    matches.len(),
                    p.pattern
                );
            }
            for path in &matches {
                println!("Touching \"{path}\"");
                if !args.dry_run {
                    let _ = touch_mtime(args.cwd, path);
                }
            }
        }

        if args.dry_run || matches.is_empty() {
            continue;
        }
        match lockable {
            LockableMode::Yes => {
                if attrs.is_none() {
                    attrs = Some(git_lfs_git::AttrSet::from_workdir(args.cwd)?);
                }
                if held.is_none() {
                    held = Some(HeldLocks::from_server(args.cwd));
                }
                lockable::apply_modes(
                    args.cwd,
                    matches.into_iter(),
                    attrs.as_ref().unwrap(),
                    held.as_ref().unwrap(),
                )?;
            }
            LockableMode::No => {
                // Pattern just lost its `lockable` attribute; undo any
                // earlier read-only state on matching files.
                for path in &matches {
                    lockable::force_writable(args.cwd, path)?;
                }
            }
            LockableMode::Default => {}
        }
    }

    Ok(0)
}

/// Render the listing mode (no patterns).
fn list(cwd: &Path, json: bool, no_excluded: bool) -> Result<u8, Box<dyn std::error::Error>> {
    let listing = git_lfs_git::attr::list_lfs_patterns(cwd)?;
    if json {
        // Upstream uses single-space indentation, and the test diffs
        // against a literal — both key order and indent matter.
        #[derive(Serialize)]
        struct Entry<'a> {
            pattern: &'a str,
            source: &'a str,
            lockable: bool,
            tracked: bool,
        }
        #[derive(Serialize)]
        struct Doc<'a> {
            patterns: Vec<Entry<'a>>,
        }
        let doc = Doc {
            patterns: listing
                .patterns
                .iter()
                .map(|p| Entry {
                    pattern: &p.pattern,
                    source: &p.source,
                    lockable: p.lockable,
                    tracked: p.tracked,
                })
                .collect(),
        };
        let mut buf = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
        doc.serialize(&mut ser)?;
        println!("{}", String::from_utf8(buf)?);
        return Ok(0);
    }

    println!("Listing tracked patterns");
    for p in listing.tracked() {
        let lock = if p.lockable { " [lockable]" } else { "" };
        println!("    {}{} ({})", p.pattern, lock, p.source);
    }
    if !no_excluded {
        let excluded: Vec<_> = listing.excluded().collect();
        if !excluded.is_empty() {
            println!("Listing excluded patterns");
            for p in excluded {
                println!("    {} ({})", p.pattern, p.source);
            }
        }
    }
    Ok(0)
}

/// Detect whether `cwd` is in a working tree but not inside `.git/`.
/// Returns `Some(128)` (mirroring git's exit code) for both "not a git
/// repository" and "must be in a work tree" failures.
fn check_repo_context(cwd: &Path) -> Option<u8> {
    let inside_work_tree = git_bool(cwd, "--is-inside-work-tree");
    if inside_work_tree != Some(true) {
        eprintln!("fatal: not in a git repository");
        return Some(128);
    }
    if git_bool(cwd, "--is-inside-git-dir") == Some(true) {
        eprintln!("fatal: this operation must be run in a work tree");
        return Some(128);
    }
    None
}

/// `git rev-parse <flag>` parsed as `true` / `false`. Returns `None` if
/// git itself errored (e.g. not in a repo at all).
fn git_bool(cwd: &Path, flag: &str) -> Option<bool> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", flag])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&out.stdout).trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Bump `<cwd>/<path>`'s mtime to now (matches upstream's
/// `os.Chtimes`). Errors are swallowed by the caller — a file that
/// vanished between `git ls-files` and now isn't a hard failure for
/// `track`, so we don't want to abort the whole command for it.
fn touch_mtime(cwd: &Path, path: &str) -> std::io::Result<()> {
    let full = cwd.join(path);
    let now = std::time::SystemTime::now();
    let f = std::fs::OpenOptions::new().write(true).open(&full)?;
    f.set_modified(now)
}
