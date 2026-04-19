use std::io::{self, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use git_lfs_filter::{clean, filter_process, smudge_with_fetch};
use git_lfs_git::ConfigScope;
use git_lfs_store::Store;

mod fetch;
mod fetcher;
mod install;
mod pull;
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
    /// Track a file pattern with git-lfs by adding it to .gitattributes.
    /// With no patterns, lists currently-tracked patterns.
    Track {
        /// File patterns to track (e.g. "*.jpg", "data/*.bin").
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
    }
    Ok(())
}
