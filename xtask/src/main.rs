//! Build automation. Currently: generate man pages and reference docs.
//!
//! Usage:
//! - `cargo run -p xtask -- gen-man [<out-dir>]` (default: `target/man/`)
//! - `cargo run -p xtask -- gen-md [<out-dir>]` (default: `docs/`)
//! - `cargo xtask test [<suite>...] [--failures]` (runs the upstream
//!   shell suites via `make` and prints a clean per-suite summary).
//!   Suite names accept `pull`, `t-pull`, or `t-pull.sh`. With no
//!   names, every `t-*.sh` runs.
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
    /// Run upstream shell suites and print a clean per-suite summary.
    /// Streams `make`'s output verbatim during the run, then groups
    /// TAP results by suite at the end. With no suite names, runs the
    /// full set under one setup/shutdown; otherwise runs only the
    /// listed suites.
    Test {
        /// Suite names. Accepts `pull`, `t-pull`, or `t-pull.sh`.
        suites: Vec<String>,
        /// Tests directory containing the Makefile and `t-*.sh`
        /// suites.
        #[arg(long, default_value = "tests")]
        dir: PathBuf,
        /// Also list the per-test failure descriptions under each
        /// failing suite.
        #[arg(long)]
        failures: bool,
    },
}

fn main() -> ExitCode {
    match Args::parse().cmd {
        Cmd::GenMan { out } => to_exit(xtask::gen_man(&out)),
        Cmd::GenMd { out } => to_exit(xtask::gen_md(&out)),
        Cmd::Test {
            suites,
            dir,
            failures,
        } => match xtask::run_tests(&dir, &suites, failures) {
            Ok(0) => ExitCode::SUCCESS,
            Ok(code) => ExitCode::from(code.clamp(1, 255) as u8),
            Err(e) => {
                eprintln!("xtask: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

fn to_exit(r: std::io::Result<()>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}
