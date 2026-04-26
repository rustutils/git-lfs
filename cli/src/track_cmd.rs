//! `git lfs track` CLI handler — flag parsing lives in `main.rs`; this
//! module owns the rendering, blocklist diagnostics, ls-files scan, and
//! JSON output.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::install;
use crate::lockable::{self, HeldLocks};
use crate::track::{self, LockableMode, TrackOptions, TrackResult};

pub struct Args<'a> {
    pub cwd: &'a Path,
    pub patterns: &'a [String],
    pub lockable: bool,
    pub not_lockable: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub json: bool,
    pub no_excluded: bool,
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
    };
    let outcome = track::track(args.cwd, args.patterns, opts)?;

    for p in &outcome.patterns {
        match p.result {
            TrackResult::Added | TrackResult::Replaced => {
                println!("Tracking \"{}\"", p.pattern);
            }
            TrackResult::AlreadyTracked => {
                println!("\"{}\" already supported", p.pattern);
            }
        }
    }

    // Re-stage files already in the index that match the new pattern
    // (so they go through the LFS clean filter on the next commit) and
    // apply the lockable invariant. The chmod side-effects happen even
    // for `AlreadyTracked` patterns so a re-issued `--lockable` /
    // `--not-lockable` against a previously-tracked pattern still
    // converges the working tree. With `--dry-run`, neither side
    // happens.
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
            }
            if !args.dry_run && !matches.is_empty() {
                git_add(args.cwd, &matches)?;
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
fn list(
    cwd: &Path,
    json: bool,
    no_excluded: bool,
) -> Result<u8, Box<dyn std::error::Error>> {
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

fn git_add(cwd: &Path, paths: &[String]) -> std::io::Result<()> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(cwd).args(["add", "--"]);
    for p in paths {
        cmd.arg(p);
    }
    let status = cmd.status()?;
    if !status.success() {
        return Err(std::io::Error::other("git add failed during track"));
    }
    Ok(())
}
