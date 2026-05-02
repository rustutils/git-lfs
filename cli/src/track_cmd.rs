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
    /// `--no-modify-attrs`: skip the `.gitattributes` write entirely
    /// (the user has hand-edited the file). Still walks the index and
    /// bumps mtimes on matching files so git's stat-cache invalidates.
    pub no_modify_attrs: bool,
}

pub fn run(args: Args<'_>) -> Result<u8, Box<dyn std::error::Error>> {
    if args.lockable && args.not_lockable {
        return Err("--lockable and --not-lockable are mutually exclusive".into());
    }

    // Both write and listing modes require a real working tree. Match
    // git's exit code (128) for "not a repo" and "must be in a work
    // tree" failures. `work_tree` is whatever git would treat as the
    // top of the work tree from `cwd` — including a `GIT_WORK_TREE`
    // override that points outside `cwd`. The `attrs_dir` is where we
    // actually read/write `.gitattributes`: cwd when we're inside the
    // work tree (so `cd a; git lfs track foo` writes to
    // `a/.gitattributes`), the work-tree root otherwise (so a
    // `GIT_WORK_TREE`-from-outside invocation still lands in the
    // tracked tree).
    let work_tree = match check_repo_context(args.cwd) {
        Ok(p) => p,
        Err(code) => return Ok(code),
    };
    let attrs_dir = if git_bool(args.cwd, "--is-inside-work-tree") == Some(true) {
        args.cwd.to_path_buf()
    } else {
        work_tree.clone()
    };

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

    // Cross-file "already tracked" detection. Upstream treats a
    // pattern as already-tracked if any `.gitattributes` in the work
    // tree carries the equivalent entry (after joining with the
    // cwd-relative-to-root prefix). Without this, `cd a; git lfs
    // track "test.file"` wouldn't notice that the parent's
    // `.gitattributes` already has `a/test.file`.
    //
    // Lockable mode handling matches upstream: when `--lockable` /
    // `--not-lockable` is set, the existing entry has to also match
    // that flag for us to skip writing — otherwise we want
    // `track::track` to update the line.
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
        // User asserts `.gitattributes` is already correct — skip the
        // already-supported partitioning and treat every input as a
        // fresh track so the index walk + mtime bump fires for matching
        // files.
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
        // Skip the write — the user has hand-edited `.gitattributes`.
        // Still synthesize an outcome so the mtime-bump loop below
        // sees every input pattern as freshly tracked.
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

    // Re-emit messages in the user's input order so ordered scripts
    // still see the right pattern echoed first. We track which input
    // patterns went which way; for "already supported" the message
    // uses the user's input verbatim, for tracked ones it uses the
    // (potentially escaped) output from `track::track`.
    let mut tracked_iter = outcome.patterns.iter();
    for pat in args.patterns {
        if already_supported.iter().any(|p| p == pat) {
            // Cross-file match — the pattern lives in another
            // `.gitattributes`; print the user's input as-is.
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
                    matches,
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

/// Resolve the working-tree root for `cwd`, mirroring git's exit code
/// (128) for both "not a git repository" and "must be in a work tree"
/// failures. Returns the work-tree path on success — this honors
/// `GIT_WORK_TREE`, so it's correct when `cwd` is *outside* the work
/// tree (e.g. a parent dir with `GIT_WORK_TREE`/`GIT_DIR` set as
/// relative paths).
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

/// Compute a `PatternEntry`'s full repo-relative path: the directory
/// of its source `.gitattributes` (e.g. `"a/.gitattributes"` →
/// `"a"`), joined with the literal pattern. Used to compare against
/// `<cwd_prefix>/<user_pattern>` for the "already tracked anywhere"
/// check.
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

/// Join the cwd's repo-root-relative prefix (from `git rev-parse
/// --show-prefix`) with a user-supplied pattern, normalizing the
/// `./` prefix git tooling sometimes inserts.
fn join_repo_relative(cwd_prefix: &str, pattern: &str) -> String {
    let trimmed = pattern.strip_prefix("./").unwrap_or(pattern);
    let prefix = cwd_prefix.trim_end_matches('/');
    if prefix.is_empty() {
        trimmed.to_owned()
    } else {
        format!("{prefix}/{trimmed}")
    }
}

/// `git rev-parse --show-prefix` — the cwd's path relative to the
/// repo root, with a trailing `/`. Empty string at the repo root,
/// `None` outside any repo.
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
