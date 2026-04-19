use std::io::{self, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use git_lfs_filter::{clean, smudge};
use git_lfs_store::Store;

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
    let lfs_dir = git_lfs_git::lfs_dir(&cwd)?;
    let store = Store::new(lfs_dir);

    let stdin = io::stdin().lock();
    let stdout = io::stdout().lock();
    let mut input: Box<dyn Read> = Box::new(stdin);
    let mut output: Box<dyn Write> = Box::new(BufWriter::new(stdout));

    match cmd {
        Command::Clean { path: _ } => {
            clean(&store, &mut input, &mut output)?;
        }
        Command::Smudge { path: _ } => {
            smudge(&store, &mut input, &mut output)?;
        }
    }
    output.flush()?;
    Ok(())
}
