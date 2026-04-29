//! Build automation. Currently: generate man pages and reference docs.
//!
//! Usage:
//! - `cargo run -p xtask -- gen-man [<out-dir>]` (default: `target/man/`)
//! - `cargo run -p xtask -- gen-md [<out-dir>]` (default: `docs/`)
//!
//! See [`xtask`] for the rendering details. This bin is a thin wrapper
//! over [`xtask::gen_man`] / [`xtask::gen_md`] so the snapshot test
//! under `tests/` can call the same entry points without spawning a
//! subprocess.
//!
//! [`xtask`]: ../xtask/index.html

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate man pages for `git-lfs` and every subcommand.
    GenMan {
        /// Output directory; created if missing.
        #[arg(default_value = "target/man")]
        out: PathBuf,
    },
    /// Generate markdown reference docs (mdbook-friendly).
    GenMd {
        /// Output directory; created if missing. Defaults to
        /// `docs/cmds/`, which is what the snapshot test compares
        /// against — keeping the auto-generated reference pages in a
        /// dedicated subdirectory leaves the rest of `docs/` (specs,
        /// hand-authored prose, mdbook config, …) untouched.
        #[arg(default_value = "docs/cmds")]
        out: PathBuf,
    },
}

fn main() -> ExitCode {
    let result = match Args::parse().cmd {
        Cmd::GenMan { out } => xtask::gen_man(&out),
        Cmd::GenMd { out } => xtask::gen_md(&out),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}
