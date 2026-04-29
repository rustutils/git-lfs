use std::io::{self, BufRead, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use git_lfs_filter::{clean, filter_process, smudge_with_fetch};
use git_lfs_git::ConfigScope;
use git_lfs_store::Store;

mod checkout;
mod clone;
mod env;
mod fetch;
mod fetcher;
mod fsck;
mod hooks;
mod install;
mod lock;
mod lockable;
mod locks_verify;
mod ls_files;
mod migrate;
mod pointer_cmd;
mod pre_push;
mod prune;
mod pull;
mod push;
mod status;
mod track;
mod track_cmd;

use fetcher::LfsFetcher;

#[derive(Parser)]
#[command(
    name = "git-lfs",
    about = "Git LFS — large file storage for git",
    // We want `git lfs --version` to print the same banner as
    // `git lfs version`. clap's auto-derived `--version` would
    // emit `git-lfs <version>` (one token, no `/` separator),
    // which doesn't match the user-agent style upstream uses.
    // Suppress clap's flag and handle --version ourselves.
    disable_version_flag = true,
    max_term_width = 100,
)]
struct Cli {
    /// Print the version banner and exit.
    #[arg(long, short = 'V', global = true)]
    version: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum MigrateCmd {
    /// Rewrite history so files matching the include filter become LFS
    /// pointers. With `--no-rewrite`, history is preserved and one
    /// new commit is appended on top of HEAD with the named paths
    /// converted in place.
    Import {
        /// Without `--no-rewrite`: branches/refs to rewrite (empty =
        /// current branch). With `--no-rewrite`: working-tree paths
        /// to convert.
        args: Vec<String>,
        /// Walk every local branch and tag.
        #[arg(long)]
        everything: bool,
        /// Convert paths matching this glob (repeatable). Required
        /// unless `--above` is set or `--no-rewrite` is given.
        #[arg(short = 'I', long = "include")]
        include: Vec<String>,
        /// Exclude paths matching this glob (repeatable).
        #[arg(short = 'X', long = "exclude")]
        exclude: Vec<String>,
        /// Only convert files at least this large (e.g. `1mb`,
        /// `500k`).
        #[arg(long, default_value = "")]
        above: String,
        /// Don't rewrite history. Read named paths from the working
        /// tree, convert in place, append one new commit on top of
        /// HEAD.
        #[arg(long)]
        no_rewrite: bool,
        /// Commit message for the `--no-rewrite` commit.
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Inverse of import: rewrite history so LFS pointers become the
    /// raw bytes they reference. Requires the LFS objects to already
    /// be in the local store — `git lfs fetch` first if not. Pointers
    /// whose objects are missing are left as-is.
    Export {
        /// Branches / refs to rewrite. Empty = current branch.
        branches: Vec<String>,
        /// Walk every local branch and tag.
        #[arg(long)]
        everything: bool,
        /// Convert pointers at paths matching this glob (repeatable).
        /// Required.
        #[arg(short = 'I', long = "include")]
        include: Vec<String>,
        /// Don't convert pointers at paths matching this glob.
        #[arg(short = 'X', long = "exclude")]
        exclude: Vec<String>,
    },
    /// Walk history and report file extensions by total size.
    /// Read-only — no objects or history change.
    Info {
        /// Branches / refs to scan. Empty = current branch.
        branches: Vec<String>,
        /// Walk every local branch and tag.
        #[arg(long)]
        everything: bool,
        /// Only include paths matching this glob (repeatable).
        #[arg(short = 'I', long = "include")]
        include: Vec<String>,
        /// Exclude paths matching this glob (repeatable).
        #[arg(short = 'X', long = "exclude")]
        exclude: Vec<String>,
        /// Only count files at least this large (e.g. `1mb`, `500k`).
        #[arg(long, default_value = "")]
        above: String,
        /// Maximum extension rows to show.
        #[arg(long, default_value_t = 5)]
        top: usize,
        /// How to handle existing LFS pointer blobs:
        /// `follow` (default), `ignore`, or `no-follow`.
        #[arg(long, default_value = "follow")]
        pointers: String,
    },
}

#[derive(Subcommand)]
enum Command {
    /// Run the clean filter: read content on stdin, write a pointer on stdout.
    Clean {
        /// Working-tree path of the file being cleaned (currently unused).
        path: Option<PathBuf>,
    },
    /// Run the smudge filter: read a pointer on stdin, write content on stdout.
    Smudge {
        /// Working-tree path of the file being smudged (currently unused).
        path: Option<PathBuf>,
    },
    /// Configure git to invoke git-lfs as the clean/smudge/process filter,
    /// and install the LFS git hooks.
    Install {
        /// Set config in the local repo only (default: --global).
        #[arg(short, long)]
        local: bool,
        /// Overwrite existing config and hooks.
        #[arg(short, long)]
        force: bool,
        /// Only set the filter config; don't install hooks.
        #[arg(long)]
        skip_repo: bool,
    },
    /// Reverse of `install`: clear the `filter.lfs.*` config and remove
    /// the LFS git hooks. Hooks that don't match what we'd write are left
    /// untouched.
    Uninstall {
        /// Operate on the local repo only (default: --global).
        #[arg(short, long)]
        local: bool,
        /// Only unset config; don't touch hooks.
        #[arg(long)]
        skip_repo: bool,
    },
    /// Track a file pattern with git-lfs by adding it to .gitattributes.
    /// With no patterns, lists currently-tracked patterns.
    Track {
        /// File patterns to track (e.g. "*.jpg", "data/*.bin").
        patterns: Vec<String>,
        /// Mark the tracked pattern as `lockable` (`*.psd lockable`).
        #[arg(short = 'l', long)]
        lockable: bool,
        /// Re-track an existing pattern, removing its `lockable` flag.
        #[arg(long)]
        not_lockable: bool,
        /// Print what would happen without modifying `.gitattributes` or
        /// re-staging files.
        #[arg(long)]
        dry_run: bool,
        /// Extra logging: print "Found N files previously added to Git
        /// matching pattern" lines.
        #[arg(short, long)]
        verbose: bool,
        /// Listing mode only: emit JSON instead of the human-readable
        /// listing.
        #[arg(long)]
        json: bool,
        /// Listing mode only: suppress the "Listing excluded patterns"
        /// section.
        #[arg(long)]
        no_excluded: bool,
    },
    /// Stop tracking a file pattern with git-lfs by removing it from
    /// .gitattributes. The matching pointer files in history (and the
    /// objects in the local store) are left in place.
    Untrack {
        /// File patterns to untrack.
        patterns: Vec<String>,
    },
    /// Run the long-running filter-process protocol with git over stdin/stdout.
    /// This is what git invokes via filter.lfs.process and is the batched
    /// alternative to per-invocation `clean`/`smudge`.
    FilterProcess,
    /// Download every LFS object reachable from the given refs (default: HEAD)
    /// that isn't already in the local store. Walks history, dedupes by OID.
    Fetch {
        /// First positional arg is treated as a remote name (if it
        /// resolves); subsequent args are refs.
        args: Vec<String>,
        /// List the objects that would be fetched without downloading
        /// them (one `fetch <oid> => <path>` line per object).
        #[arg(long)]
        dry_run: bool,
        /// JSON output. With `--dry-run`, queries the server's batch
        /// endpoint to populate `actions` URLs.
        #[arg(long)]
        json: bool,
        /// Walk every local ref under `refs/heads/*` + `refs/tags/*`.
        #[arg(long)]
        all: bool,
        /// Re-download objects we already have (e.g. recovery from a
        /// corrupt local store).
        #[arg(long)]
        refetch: bool,
        /// Read refs from stdin, one per line. Blank lines dropped.
        #[arg(long)]
        stdin: bool,
        /// Run `prune` after the fetch completes.
        #[arg(long)]
        prune: bool,
        /// Comma-separated globs; only matching paths are fetched.
        /// Falls back to `lfs.fetchinclude` when omitted.
        #[arg(short = 'I', long)]
        include: Vec<String>,
        /// Comma-separated globs; matching paths are skipped. Falls
        /// back to `lfs.fetchexclude` when omitted.
        #[arg(short = 'X', long)]
        exclude: Vec<String>,
    },
    /// `fetch` then re-run the smudge filter so the working tree contains
    /// real LFS file contents instead of pointer text. Requires
    /// `git lfs install` to have wired up the smudge filter.
    Pull {
        /// Refs to scan for LFS pointers. Defaults to `HEAD`.
        refs: Vec<String>,
        /// Comma-separated globs; only matching paths are pulled.
        /// Falls back to `lfs.fetchinclude` when omitted.
        #[arg(short = 'I', long)]
        include: Vec<String>,
        /// Comma-separated globs; matching paths are skipped. Falls
        /// back to `lfs.fetchexclude` when omitted.
        #[arg(short = 'X', long)]
        exclude: Vec<String>,
    },
    /// Upload every LFS object reachable from the given refs that the
    /// remote doesn't already have. The "doesn't have" set is approximated
    /// by `refs/remotes/<remote>/*`; the LFS server's batch API also
    /// dedupes server-side so missing exclusions don't waste bandwidth.
    Push {
        /// Name of the remote (e.g. "origin") whose tracking refs are
        /// excluded from the upload set.
        remote: String,
        /// Refs (or, with `--object-id`, raw OIDs) to push. With
        /// `--all`, restricts the all-refs walk to these; with
        /// `--stdin`, ignored (a warning is emitted).
        args: Vec<String>,
        /// List the objects that would be pushed without actually
        /// uploading them (one `push <oid> => <path>` line per object).
        #[arg(long)]
        dry_run: bool,
        /// Push every local ref under `refs/heads/*` and `refs/tags/*`
        /// (intersected with `args` if any are given).
        #[arg(long)]
        all: bool,
        /// Read refs (or OIDs, with `--object-id`) from stdin, one per
        /// line. Blank lines are skipped.
        #[arg(long)]
        stdin: bool,
        /// Treat positional args / stdin entries as raw LFS OIDs
        /// rather than git refs, and upload those objects directly
        /// from the local store.
        #[arg(long)]
        object_id: bool,
    },
    /// Deprecated. Wraps `git clone` so the working tree is populated
    /// with pointer text first, then runs `git lfs pull` to download
    /// LFS content in batch. Modern `git clone` parallelizes the
    /// smudge filter and is no slower; prefer it.
    Clone {
        /// `git clone` and LFS pass-through args. The repository URL
        /// is required; an optional target directory follows.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Git post-checkout hook entry point. Receives `<prev-sha>
    /// <post-sha> <flag>` (flag is "1" if HEAD moved). Currently a
    /// no-op stub — exists so installed hook scripts don't fail. Real
    /// behavior arrives with `track --lockable`.
    PostCheckout { args: Vec<String> },
    /// Git post-commit hook entry point. No arguments. Currently a
    /// no-op stub.
    PostCommit { args: Vec<String> },
    /// Git post-merge hook entry point. Receives `<squash-flag>`.
    /// Currently a no-op stub.
    PostMerge { args: Vec<String> },
    /// Git pre-push hook entry point — not typically invoked by hand.
    /// Reads `<local-ref> <local-sha> <remote-ref> <remote-sha>` lines
    /// from stdin and uploads the LFS objects newly reachable from each
    /// `<local-sha>`.
    PrePush {
        /// Name of the remote being pushed to.
        remote: String,
        /// URL of the remote (informational; we use `lfs.url` config).
        url: Option<String>,
        /// List the objects that would be pushed without actually
        /// uploading them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the git-lfs version and exit.
    Version,
    /// Debug helper: build a pointer from a file, parse one from disk
    /// or stdin, or just check whether some bytes are a valid pointer.
    Pointer {
        /// Build a pointer from this file (read content, hash, encode).
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Parse and display this existing pointer file.
        #[arg(short, long)]
        pointer: Option<PathBuf>,
        /// Read a pointer from stdin (mutually exclusive with --pointer).
        #[arg(long)]
        stdin: bool,
        /// Validity check mode: exit 0 if input parses, 1 if not, 2 if
        /// `--strict` and not byte-canonical.
        #[arg(long)]
        check: bool,
        /// In `--check`, also reject non-canonical pointers.
        #[arg(long)]
        strict: bool,
        /// Explicitly disable strict mode (paired with `--strict`).
        #[arg(long)]
        no_strict: bool,
    },
    /// Show the LFS environment: version, endpoints, on-disk paths, and
    /// the three `filter.lfs.*` config values.
    Env,
    /// Analyze or rewrite history for LFS conversion. Phase 1 ships
    /// `info` only; `import` and `export` will land in subsequent phases.
    Migrate {
        #[command(subcommand)]
        cmd: MigrateCmd,
    },
    /// Replace pointer text in the working tree with actual LFS object
    /// content. With no args, materializes every LFS pointer in HEAD's
    /// tree. With paths (literal file names or trailing-slash directory
    /// prefixes), restricts to matching pointers.
    Checkout {
        /// Paths to check out. Empty = everything in HEAD's tree.
        paths: Vec<String>,
    },
    /// Delete local LFS objects that aren't reachable from HEAD or any
    /// unpushed commit. Reclaims disk for repos whose history has moved
    /// past their objects.
    Prune {
        /// Don't delete anything; just report what would go.
        #[arg(short, long)]
        dry_run: bool,
        /// Print each prunable object's OID and size.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Check the integrity of LFS objects and pointers reachable from
    /// `<refspec>` (default: HEAD). Exit 1 if anything is corrupt.
    Fsck {
        /// Ref to scan. Defaults to HEAD.
        refspec: Option<String>,
        /// Only check objects (verify store contents match pointer OIDs).
        #[arg(long)]
        objects: bool,
        /// Only check pointers (flag non-canonical pointer encodings).
        #[arg(long)]
        pointers: bool,
        /// Report problems but don't move corrupt objects to `<lfs>/bad/`.
        #[arg(short, long)]
        dry_run: bool,
    },
    /// Show staged + unstaged changes, classifying each blob as LFS,
    /// Git, or working-tree File.
    Status {
        /// Stable one-line-per-change format for scripts.
        #[arg(short, long)]
        porcelain: bool,
        /// Stable JSON output for scripts; only LFS entries are reported.
        #[arg(short, long)]
        json: bool,
    },
    /// Acquire an exclusive server-side lock on one or more files.
    /// Other users will be unable to push changes to a locked file.
    Lock {
        /// Paths to lock (repo-relative or absolute, must resolve inside
        /// the working tree).
        paths: Vec<String>,
        /// Specify which remote to use when interacting with locks.
        #[arg(short, long)]
        remote: Option<String>,
        /// Refspec to associate the lock with. Defaults to the current
        /// branch's tracked upstream (`branch.<current>.merge`) or the
        /// current branch's full ref (`refs/heads/<branch>`).
        #[arg(long = "ref")]
        refspec: Option<String>,
        /// Stable JSON output for scripts.
        #[arg(short, long)]
        json: bool,
    },
    /// List file locks held on the server.
    Locks {
        /// Specify which remote to use when interacting with locks.
        #[arg(short, long)]
        remote: Option<String>,
        /// Filter results to a particular path.
        #[arg(short, long)]
        path: Option<String>,
        /// Filter results to a particular lock id.
        #[arg(short, long)]
        id: Option<String>,
        /// Maximum number of results to return.
        #[arg(short, long)]
        limit: Option<u32>,
        /// Refspec to filter locks by (defaults to current branch /
        /// tracked upstream — same auto-resolution as `git lfs lock`).
        #[arg(long = "ref")]
        refspec: Option<String>,
        /// Verify ownership: prefix locks owned by the authenticated user
        /// with `O ` (others get `  `).
        #[arg(long)]
        verify: bool,
        /// Stable JSON output for scripts.
        #[arg(short, long)]
        json: bool,
    },
    /// Release a file lock previously acquired with `git lfs lock`.
    /// Either provide one or more paths, or `--id <id>` (mutually
    /// exclusive).
    Unlock {
        /// Paths to unlock; mutually exclusive with `--id`.
        paths: Vec<String>,
        /// Lock id to release; mutually exclusive with paths.
        #[arg(short, long)]
        id: Option<String>,
        /// Forcibly break another user's lock(s).
        #[arg(short, long)]
        force: bool,
        /// Specify which remote to use when interacting with locks.
        #[arg(short, long)]
        remote: Option<String>,
        /// Refspec to send with the unlock request (defaults to current
        /// branch / tracked upstream).
        #[arg(long = "ref")]
        refspec: Option<String>,
        /// Stable JSON output for scripts.
        #[arg(short, long)]
        json: bool,
    },
    /// List LFS-tracked files visible at a ref (default: HEAD), or across
    /// all reachable history with `--all`.
    LsFiles {
        /// Ref to list. Defaults to HEAD.
        refspec: Option<String>,
        /// Show full 64-char OID instead of the 10-char prefix.
        #[arg(short, long)]
        long: bool,
        /// Append humanized size in parens.
        #[arg(short, long)]
        size: bool,
        /// Print only the path.
        #[arg(short, long)]
        name_only: bool,
        /// Walk every reachable ref's full history.
        #[arg(short, long)]
        all: bool,
        /// Multi-line per-file block (size, checkout, download, oid, version).
        #[arg(short, long)]
        debug: bool,
        /// Stable JSON output for scripts.
        #[arg(short, long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.version {
        println!("git-lfs/{} (rust)", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let Some(command) = cli.command else {
        // Mimic clap's default error path when no subcommand is given.
        Cli::command().print_help().ok();
        return ExitCode::FAILURE;
    };
    match dispatch(command) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("git-lfs: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `GIT_LFS_SKIP_SMUDGE=1` (any value other than empty/0/false) tells
/// the smudge filter to leave pointer text in place rather than fetch.
/// Used by clones that intentionally don't materialize content (e.g.
/// CI partial clones, t-pull's "skip" tests).
fn skip_smudge_env() -> bool {
    match std::env::var_os("GIT_LFS_SKIP_SMUDGE") {
        None => false,
        Some(v) => {
            let s = v.to_string_lossy();
            !matches!(s.as_ref(), "" | "0" | "false" | "False" | "FALSE")
        }
    }
}

fn dispatch(cmd: Command) -> Result<u8, Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;

    match cmd {
        Command::Clean { path: _ } => {
            let _ = install::try_install_hooks(&cwd);
            let store = Store::new(git_lfs_git::lfs_dir(&cwd)?);
            let stdin = io::stdin().lock();
            let mut input: Box<dyn Read> = Box::new(stdin);
            let mut output: Box<dyn Write> = Box::new(BufWriter::new(io::stdout().lock()));
            clean(&store, &mut input, &mut output)?;
            output.flush()?;
        }
        Command::Smudge { path: _ } => {
            let _ = install::try_install_hooks(&cwd);
            let store = Store::new(git_lfs_git::lfs_dir(&cwd)?);
            let stdin = io::stdin().lock();
            let mut input: Box<dyn Read> = Box::new(stdin);
            let mut output: Box<dyn Write> = Box::new(BufWriter::new(io::stdout().lock()));
            if skip_smudge_env() {
                io::copy(&mut input, &mut output)?;
            } else {
                let fetcher = LfsFetcher::from_repo(&cwd, &store)?;
                smudge_with_fetch(&store, &mut input, &mut output, |p| fetcher.fetch(p))?;
            }
            output.flush()?;
        }
        Command::Install {
            local,
            force,
            skip_repo,
        } => {
            let opts = install::InstallOptions {
                scope: if local {
                    ConfigScope::Local
                } else {
                    ConfigScope::Global
                },
                force,
                skip_repo,
            };
            install::install(&cwd, &opts)?;
            println!("Git LFS initialized.");
        }
        Command::Uninstall { local, skip_repo } => {
            let opts = install::UninstallOptions {
                scope: if local {
                    ConfigScope::Local
                } else {
                    ConfigScope::Global
                },
                skip_repo,
            };
            install::uninstall(&cwd, &opts)?;
            if local {
                println!("Local Git LFS configuration has been removed.");
            } else {
                println!("Global Git LFS configuration has been removed.");
            }
        }
        Command::Clone { args } => {
            clone::run(&cwd, &args)?;
        }
        Command::FilterProcess => {
            let _ = install::try_install_hooks(&cwd);
            let store = Store::new(git_lfs_git::lfs_dir(&cwd)?);
            let fetcher = LfsFetcher::from_repo(&cwd, &store)?;
            let stdin = io::stdin().lock();
            let stdout = io::stdout().lock();
            filter_process(&store, stdin, stdout, |p| fetcher.fetch(p), skip_smudge_env())?;
        }
        Command::Fetch {
            args,
            dry_run,
            json,
            all,
            refetch,
            stdin,
            prune,
            include,
            exclude,
        } => {
            let stdin_lines: Vec<String> = if stdin {
                io::stdin()
                    .lock()
                    .lines()
                    .filter_map(|l| l.ok())
                    .map(|l| l.trim().to_owned())
                    .filter(|l| !l.is_empty())
                    .collect()
            } else {
                Vec::new()
            };
            let opts = fetch::FetchOptions {
                args: &args,
                stdin_lines: &stdin_lines,
                dry_run,
                json,
                all,
                refetch,
                stdin,
                prune,
                include: &include,
                exclude: &exclude,
            };
            match fetch::fetch(&cwd, &opts) {
                Ok(outcome) => {
                    if !outcome.report.failed.is_empty() {
                        return Err("one or more objects failed to download".into());
                    }
                }
                Err(fetch::FetchCommandError::Usage(msg)) if msg == "Not in a Git repository." => {
                    // Test `t-fetch.sh::fetch: outside git repository`
                    // greps for this on stdout (`2>&1 > fetch.log`
                    // captures stdout only). Match upstream and emit
                    // here, then exit 128.
                    println!("{msg}");
                    return Ok(128);
                }
                Err(e) => return Err(e.into()),
            }
        }
        Command::Pull { refs, include, exclude } => {
            pull::pull_with_filter(&cwd, &refs, &include, &exclude)?;
        }
        Command::Push {
            remote,
            args,
            dry_run,
            all,
            stdin,
            object_id,
        } => {
            let stdin_lines: Vec<String> = if stdin {
                io::stdin()
                    .lock()
                    .lines()
                    .filter_map(|l| l.ok())
                    .map(|l| l.trim().to_owned())
                    .filter(|l| !l.is_empty())
                    .collect()
            } else {
                Vec::new()
            };
            let opts = push::PushOptions {
                args: &args,
                stdin_lines: &stdin_lines,
                dry_run,
                all,
                stdin,
                object_id,
            };
            let outcome = push::push(&cwd, &remote, &opts)?;
            if outcome.aborted {
                return Ok(2);
            }
            if !outcome.report.failed.is_empty() {
                return Err("one or more objects failed to upload".into());
            }
        }
        Command::PostCheckout { args } => {
            hooks::post_checkout(&cwd, &args)?;
        }
        Command::PostCommit { args } => {
            hooks::post_commit(&cwd, &args)?;
        }
        Command::PostMerge { args } => {
            hooks::post_merge(&cwd, &args)?;
        }
        Command::PrePush {
            remote,
            url: _,
            dry_run,
        } => {
            let stdin = io::stdin().lock();
            let outcome = pre_push::pre_push(&cwd, &remote, stdin, dry_run)?;
            if outcome.aborted {
                return Ok(2);
            }
            if !outcome.report.failed.is_empty() {
                return Err("pre-push: one or more objects failed to upload".into());
            }
        }
        Command::Track {
            patterns,
            lockable,
            not_lockable,
            dry_run,
            verbose,
            json,
            no_excluded,
        } => {
            return track_cmd::run(track_cmd::Args {
                cwd: &cwd,
                patterns: &patterns,
                lockable,
                not_lockable,
                dry_run,
                verbose,
                json,
                no_excluded,
            });
        }
        Command::Version => {
            println!("git-lfs/{} (rust)", env!("CARGO_PKG_VERSION"));
        }
        Command::Pointer {
            file,
            pointer,
            stdin,
            check,
            strict,
            no_strict,
        } => {
            let opts = pointer_cmd::Options {
                file,
                pointer,
                stdin,
                check,
                strict,
                no_strict,
            };
            // Pointer's exit codes are semantic: 1 = mismatch / parse
            // failure, 2 = `--strict` non-canonical. Propagate verbatim.
            let code = pointer_cmd::run(&opts)?;
            return Ok(code as u8);
        }
        Command::Env => {
            env::run(&cwd)?;
        }
        Command::Migrate { cmd } => match cmd {
            MigrateCmd::Export {
                branches,
                everything,
                include,
                exclude,
            } => {
                let opts = migrate::ExportOptions {
                    branches,
                    everything,
                    include,
                    exclude,
                };
                migrate::export(&cwd, &opts)?;
            }
            MigrateCmd::Import {
                args,
                everything,
                include,
                exclude,
                above,
                no_rewrite,
                message,
            } => {
                let above_bytes = migrate::parse_size(&above)?;
                // Split: in --no-rewrite mode the positional args are
                // working-tree paths; otherwise they're branches.
                let (branches, paths) = if no_rewrite {
                    (Vec::new(), args)
                } else {
                    (args, Vec::new())
                };
                let opts = migrate::ImportOptions {
                    branches,
                    everything,
                    include,
                    exclude,
                    above: above_bytes,
                    no_rewrite,
                    message,
                    paths,
                };
                let _ = install::try_install_hooks(&cwd);
                migrate::import(&cwd, &opts)?;
            }
            MigrateCmd::Info {
                branches,
                everything,
                include,
                exclude,
                above,
                top,
                pointers,
            } => {
                let pointer_mode = match pointers.as_str() {
                    "follow" => migrate::PointerMode::Follow,
                    "no-follow" => migrate::PointerMode::NoFollow,
                    "ignore" => migrate::PointerMode::Ignore,
                    other => return Err(format!("--pointers: unknown value {other:?}").into()),
                };
                let above_bytes = migrate::parse_size(&above)?;
                let opts = migrate::InfoOptions {
                    branches,
                    everything,
                    include,
                    exclude,
                    above: above_bytes,
                    top,
                    pointers: pointer_mode,
                };
                migrate::info(&cwd, &opts)?;
            }
        },
        Command::Checkout { paths } => {
            let opts = checkout::Options { paths };
            checkout::run(&cwd, &opts)?;
        }
        Command::Prune { dry_run, verbose } => {
            let opts = prune::Options { dry_run, verbose };
            prune::run(&cwd, &opts)?;
        }
        Command::Fsck {
            refspec,
            objects,
            pointers,
            dry_run,
        } => {
            let _ = install::try_install_hooks(&cwd);
            let mode = match (objects, pointers) {
                (true, false) => fsck::Mode::Objects,
                (false, true) => fsck::Mode::Pointers,
                _ => fsck::Mode::Both,
            };
            let opts = fsck::Options { mode, dry_run };
            let code = fsck::run(&cwd, refspec.as_deref(), &opts)?;
            return Ok(code as u8);
        }
        Command::Status { porcelain, json } => {
            let format = if json {
                status::Format::Json
            } else if porcelain {
                status::Format::Porcelain
            } else {
                status::Format::Default
            };
            status::run(&cwd, format)?;
        }
        Command::Lock {
            paths,
            remote,
            refspec,
            json,
        } => {
            let opts = lock::LockOptions {
                remote,
                refspec,
                json,
            };
            let ok = lock::lock(&cwd, &paths, &opts)?;
            if !ok {
                return Err("one or more locks failed".into());
            }
        }
        Command::Locks {
            remote,
            path,
            id,
            limit,
            refspec,
            verify,
            json,
        } => {
            let opts = lock::LocksOptions {
                remote,
                refspec,
                path,
                id,
                limit,
                verify,
                json,
            };
            lock::locks(&cwd, &opts)?;
        }
        Command::Unlock {
            paths,
            id,
            force,
            remote,
            refspec,
            json,
        } => {
            let opts = lock::UnlockOptions {
                remote,
                refspec,
                id,
                force,
                json,
            };
            let ok = lock::unlock(&cwd, &paths, &opts)?;
            if !ok {
                return Err("one or more unlocks failed".into());
            }
        }
        Command::LsFiles {
            refspec,
            long,
            size,
            name_only,
            all,
            debug,
            json,
        } => {
            let format = if json {
                ls_files::Format::Json
            } else if debug {
                ls_files::Format::Debug
            } else {
                ls_files::Format::Default
            };
            let opts = ls_files::Options {
                long,
                show_size: size,
                name_only,
                all,
                format,
            };
            ls_files::run(&cwd, refspec.as_deref(), &opts)?;
        }
        Command::Untrack { patterns } => {
            if patterns.is_empty() {
                return Err("git lfs untrack <pattern> [pattern...]".into());
            }
            let _ = install::try_install_hooks(&cwd);
            let outcome = track::untrack(&cwd, &patterns)?;
            for p in &outcome.removed {
                println!("Untracking \"{p}\"");
            }
            for p in &outcome.missing {
                println!("\"{p}\" was not tracked");
            }
        }
    }
    Ok(0)
}
