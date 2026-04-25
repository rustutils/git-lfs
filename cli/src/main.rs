use std::io::{self, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use git_lfs_filter::{clean, filter_process, smudge_with_fetch};
use git_lfs_git::ConfigScope;
use git_lfs_store::Store;

mod env;
mod fetch;
mod fetcher;
mod install;
mod ls_files;
mod pre_push;
mod pull;
mod push;
mod status;
mod track;

use fetcher::LfsFetcher;

#[derive(Parser)]
#[command(name = "git-lfs", version, about = "Git LFS — large file storage for git")]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
        /// Refs to scan for LFS pointers. Defaults to `HEAD`.
        refs: Vec<String>,
    },
    /// `fetch` then re-run the smudge filter so the working tree contains
    /// real LFS file contents instead of pointer text. Requires
    /// `git lfs install` to have wired up the smudge filter.
    Pull {
        /// Refs to scan for LFS pointers. Defaults to `HEAD`.
        refs: Vec<String>,
    },
    /// Upload every LFS object reachable from the given refs that the
    /// remote doesn't already have. The "doesn't have" set is approximated
    /// by `refs/remotes/<remote>/*`; the LFS server's batch API also
    /// dedupes server-side so missing exclusions don't waste bandwidth.
    Push {
        /// Name of the remote (e.g. "origin") whose tracking refs are
        /// excluded from the upload set.
        remote: String,
        /// Refs to push LFS objects for. Defaults to `HEAD`.
        refs: Vec<String>,
    },
    /// Git pre-push hook entry point — not typically invoked by hand.
    /// Reads `<local-ref> <local-sha> <remote-ref> <remote-sha>` lines
    /// from stdin and uploads the LFS objects newly reachable from each
    /// `<local-sha>`.
    PrePush {
        /// Name of the remote being pushed to.
        remote: String,
        /// URL of the remote (informational; we use `lfs.url` config).
        url: Option<String>,
    },
    /// Show the LFS environment: version, endpoints, on-disk paths, and
    /// the three `filter.lfs.*` config values.
    Env,
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
    match dispatch(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("git-lfs: {e}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cmd: Command) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;

    match cmd {
        Command::Clean { path: _ } => {
            let store = Store::new(git_lfs_git::lfs_dir(&cwd)?);
            let stdin = io::stdin().lock();
            let mut input: Box<dyn Read> = Box::new(stdin);
            let mut output: Box<dyn Write> = Box::new(BufWriter::new(io::stdout().lock()));
            clean(&store, &mut input, &mut output)?;
            output.flush()?;
        }
        Command::Smudge { path: _ } => {
            let store = Store::new(git_lfs_git::lfs_dir(&cwd)?);
            let fetcher = LfsFetcher::from_repo(&cwd, &store)?;
            let stdin = io::stdin().lock();
            let mut input: Box<dyn Read> = Box::new(stdin);
            let mut output: Box<dyn Write> = Box::new(BufWriter::new(io::stdout().lock()));
            smudge_with_fetch(&store, &mut input, &mut output, |p| fetcher.fetch(p))?;
            output.flush()?;
        }
        Command::Install { local, force, skip_repo } => {
            let opts = install::InstallOptions {
                scope: if local { ConfigScope::Local } else { ConfigScope::Global },
                force,
                skip_repo,
            };
            install::install(&cwd, &opts)?;
            println!("Git LFS initialized.");
        }
        Command::Uninstall { local, skip_repo } => {
            let opts = install::UninstallOptions {
                scope: if local { ConfigScope::Local } else { ConfigScope::Global },
                skip_repo,
            };
            install::uninstall(&cwd, &opts)?;
            if local {
                println!("Local Git LFS configuration has been removed.");
            } else {
                println!("Global Git LFS configuration has been removed.");
            }
        }
        Command::FilterProcess => {
            let store = Store::new(git_lfs_git::lfs_dir(&cwd)?);
            let fetcher = LfsFetcher::from_repo(&cwd, &store)?;
            let stdin = io::stdin().lock();
            let stdout = io::stdout().lock();
            filter_process(&store, stdin, stdout, |p| fetcher.fetch(p))?;
        }
        Command::Fetch { refs } => {
            let report = fetch::fetch(&cwd, &refs)?;
            if !report.failed.is_empty() {
                return Err("one or more objects failed to download".into());
            }
        }
        Command::Pull { refs } => {
            pull::pull(&cwd, &refs)?;
        }
        Command::Push { remote, refs } => {
            let report = push::push(&cwd, &remote, &refs)?;
            if !report.failed.is_empty() {
                return Err("one or more objects failed to upload".into());
            }
        }
        Command::PrePush { remote, url: _ } => {
            let stdin = io::stdin().lock();
            let report = pre_push::pre_push(&cwd, &remote, stdin)?;
            if !report.failed.is_empty() {
                return Err("pre-push: one or more objects failed to upload".into());
            }
        }
        Command::Track { patterns } => {
            if patterns.is_empty() {
                println!("Listing tracked patterns");
                for p in track::list(&cwd)? {
                    println!("    {p} (.gitattributes)");
                }
            } else {
                let outcome = track::track(&cwd, &patterns)?;
                for p in &outcome.added {
                    println!("Tracking \"{p}\"");
                }
                for p in &outcome.already {
                    println!("\"{p}\" already supported");
                }
            }
        }
        Command::Env => {
            env::run(&cwd)?;
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
            let outcome = track::untrack(&cwd, &patterns)?;
            for p in &outcome.removed {
                println!("Untracking \"{p}\"");
            }
            for p in &outcome.missing {
                println!("\"{p}\" was not tracked");
            }
        }
    }
    Ok(())
}
