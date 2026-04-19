use std::io::{self, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use git_lfs_filter::{clean, smudge};
use git_lfs_git::ConfigScope;
use git_lfs_store::Store;

mod install;

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
            let stdin = io::stdin().lock();
            let mut input: Box<dyn Read> = Box::new(stdin);
            let mut output: Box<dyn Write> = Box::new(BufWriter::new(io::stdout().lock()));
            smudge(&store, &mut input, &mut output)?;
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
    }
    Ok(())
}
