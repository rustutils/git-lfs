//! `git lfs track`: add LFS patterns to `.gitattributes`, or list them.
//!
//! The file itself is edited by [`crate::track`]. This module checks the
//! repository, skips patterns that are already tracked, touches the files
//! a new pattern matches, and sets lockable files read-only or writable.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

use git_lfs_git::config::{self, ConfigScope};

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
    /// `--filename`: each pattern is a file name, not a glob.
    pub filename: bool,
    /// `--no-modify-attrs`: don't write `.gitattributes`, but still touch
    /// the matching files.
    pub no_modify_attrs: bool,
}

pub fn run(args: Args<'_>) -> Result<u8, Box<dyn std::error::Error>> {
    if args.lockable && args.not_lockable {
        return Err("--lockable and --not-lockable are mutually exclusive".into());
    }

    // `.gitattributes` in the current directory is the one edited, or the
    // one at the root when run from outside the work tree with
    // `GIT_WORK_TREE` set.
    let work_tree = match check_repo_context(args.cwd) {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };
    if let Err(code) = verify_repository_version(args.cwd) {
        return Ok(code);
    }
    let attrs_dir = if git_bool(args.cwd, "--is-inside-work-tree") == Some(true) {
        args.cwd.to_path_buf()
    } else {
        work_tree.clone()
    };

    // Best effort, like upstream. Any value of
    // `GIT_LFS_TRACK_NO_INSTALL_HOOKS` turns it off, where upstream reads
    // it as a boolean.
    if std::env::var_os("GIT_LFS_TRACK_NO_INSTALL_HOOKS").is_none() {
        let _ = install::try_install_hooks(args.cwd);
    }

    if args.patterns.is_empty() {
        return list(args.cwd, args.json, args.no_excluded);
    }

    // t-track.sh greps stdout for this message. Unlike upstream, one
    // forbidden pattern stops the others from being tracked too.
    for pat in args.patterns {
        if let Some(forbidden) = track::forbidden_match(args.cwd, pat) {
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

    // Like upstream, a pattern is already tracked if any `.gitattributes`
    // has it for the same path, so `git lfs track x` run in `a` finds
    // `a/x` in the root file. With `--lockable` or `--not-lockable`, the
    // entry must also have the requested state.
    let cwd_prefix = git_rev_parse_show_prefix(args.cwd).unwrap_or_default();
    let known_full: Vec<(String, bool)> = match git_lfs_git::path::git_dir(args.cwd)
        .ok()
        .and_then(|gd| gd.parent().map(Path::to_path_buf))
        .and_then(|root| git_lfs_git::attr::list_lfs_patterns(&root).ok())
    {
        Some(listing) => listing
            .tracked()
            .map(|entry| (full_pattern_path(entry), entry.lockable))
            .collect(),
        None => Vec::new(),
    };
    let lockable_unchanged = matches!(lockable, LockableMode::Default);
    let mut already_supported: Vec<String> = Vec::new();
    let mut to_track: Vec<String> = Vec::new();
    if args.no_modify_attrs {
        to_track.extend(args.patterns.iter().cloned());
    } else {
        for pat in args.patterns {
            let full = join_repo_relative(&cwd_prefix, pat);
            let already = known_full.iter().any(|(p, lockable_flag)| {
                *p == full
                    && (lockable_unchanged
                        || (lockable == LockableMode::Yes && *lockable_flag)
                        || (lockable == LockableMode::No && !*lockable_flag))
            });
            if already {
                already_supported.push(pat.clone());
            } else {
                to_track.push(pat.clone());
            }
        }
    }

    let opts = TrackOptions {
        lockable,
        dry_run: args.dry_run,
        literal_filename: args.filename,
    };
    let outcome = if args.no_modify_attrs {
        // Treat every pattern as added, so its files are touched below.
        track::TrackOutcome {
            patterns: to_track
                .iter()
                .map(|pat| track::TrackedPattern {
                    pattern: pat.clone(),
                    result: track::TrackResult::Added,
                })
                .collect(),
        }
    } else {
        track::track(&attrs_dir, &to_track, opts)?
    };

    // Report in argument order. `outcome.patterns` holds the arguments
    // that weren't already supported, in order.
    let mut tracked_iter = outcome.patterns.iter();
    for pat in args.patterns {
        if already_supported.iter().any(|p| p == pat) {
            println!("\"{pat}\" already supported");
            continue;
        }
        let Some(p) = tracked_iter.next() else {
            continue;
        };
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

    // Files in the index that an added or changed pattern matches get a
    // new mtime, so Git sees them as modified. Like upstream, they aren't
    // staged again. `Touching` is printed with `--verbose` or
    // `--dry-run`. With `--lockable` or `--not-lockable`, permissions are
    // set for the files of every pattern, including ones already tracked.
    // `--dry-run` changes no files.
    //
    // `attrs` and `held` are built on first use, so no lock query is sent
    // when no file matches.
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
                if args.verbose || args.dry_run {
                    println!("Touching \"{path}\"");
                }
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
                    held = Some(HeldLocks::load(args.cwd));
                }
                lockable::apply_modes(
                    args.cwd,
                    matches,
                    attrs.as_ref().unwrap(),
                    held.as_ref().unwrap(),
                )?;
            }
            LockableMode::No => {
                for path in &matches {
                    lockable::force_writable(args.cwd, path)?;
                }
            }
            LockableMode::Default => {}
        }
    }

    Ok(0)
}

/// List the tracked and excluded patterns, for `track` with no patterns.
fn list(cwd: &Path, json: bool, no_excluded: bool) -> Result<u8, Box<dyn std::error::Error>> {
    let listing = git_lfs_git::attr::list_lfs_patterns(cwd)?;
    if json {
        // t-track.sh compares this with upstream's output, so the key
        // order and single-space indent matter.
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

/// The root of the work tree, which honors `GIT_WORK_TREE`.
///
/// Outside a repository, or inside the Git directory, prints an error and
/// returns exit code 128.
fn check_repo_context(cwd: &Path) -> Result<std::path::PathBuf, u8> {
    let work_tree = match git_lfs_git::work_tree_root(cwd) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("fatal: not in a git repository");
            return Err(128);
        }
    };
    if git_bool(cwd, "--is-inside-git-dir") == Some(true) {
        eprintln!("fatal: this operation must be run in a work tree");
        return Err(128);
    }
    Ok(work_tree)
}

/// Check `lfs.repositoryformatversion` in the local config, like
/// upstream's `verifyRepositoryVersion`.
///
/// An unset value is set to 0. Any value but 0 prints an error and returns
/// exit code 128.
fn verify_repository_version(cwd: &Path) -> Result<(), u8> {
    const KEY: &str = "lfs.repositoryformatversion";
    match config::get(cwd, ConfigScope::Local, KEY) {
        Ok(None) => {
            let _ = config::set(cwd, ConfigScope::Local, KEY, "0");
            Ok(())
        }
        Ok(Some(val)) if val == "0" => Ok(()),
        Ok(Some(val)) => {
            println!("Unknown repository format version: {val}");
            Err(128)
        }
        Err(_) => Ok(()),
    }
}

/// The `true` or `false` that `git rev-parse <flag>` prints.
///
/// `None` if Git fails or prints something else.
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

/// A pattern joined to the directory of the `.gitattributes` it came
/// from, so `x` in `a/.gitattributes` gives `a/x`.
fn full_pattern_path(entry: &git_lfs_git::attr::PatternEntry) -> String {
    let source_dir = std::path::Path::new(&entry.source)
        .parent()
        .map(|d| d.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let pat = entry.pattern.trim_start_matches('/');
    if source_dir.is_empty() {
        pat.to_owned()
    } else {
        format!("{source_dir}/{pat}")
    }
}

/// Join a pattern argument, minus any leading `./`, to the current
/// directory's prefix from [`git_rev_parse_show_prefix`].
fn join_repo_relative(cwd_prefix: &str, pattern: &str) -> String {
    let trimmed = pattern.strip_prefix("./").unwrap_or(pattern);
    let prefix = cwd_prefix.trim_end_matches('/');
    if prefix.is_empty() {
        trimmed.to_owned()
    } else {
        format!("{prefix}/{trimmed}")
    }
}

/// The current directory relative to the root, with a trailing `/`.
///
/// Empty at the root, and `None` if `git rev-parse --show-prefix` fails.
fn git_rev_parse_show_prefix(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-prefix"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Set the mtime of `cwd/path` to now.
///
/// Opens the file for writing, so a read-only file fails. The caller
/// ignores errors, where upstream reports them and exits 2.
fn touch_mtime(cwd: &Path, path: &str) -> std::io::Result<()> {
    let full = cwd.join(path);
    let now = std::time::SystemTime::now();
    let f = std::fs::OpenOptions::new().write(true).open(&full)?;
    f.set_modified(now)
}
