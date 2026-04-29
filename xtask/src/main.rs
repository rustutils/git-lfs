//! Build automation. Currently: generate man pages.
//!
//! Usage: `cargo run -p xtask -- gen-man [<out-dir>]`.
//! `out-dir` defaults to `target/man/`. One page per subcommand
//! (`git-lfs-fetch.1`, `git-lfs-checkout.1`, …) plus a top-level
//! `git-lfs.1`.
//!
//! The clap derive in `git_lfs::cli_def` is the source of truth for
//! NAME / SYNOPSIS / OPTIONS / VERSION. Hand-authored extras
//! (DESCRIPTION prose, EXAMPLES, NOTES, …) come from
//! [`git_lfs::man::extras_for`] and are spliced in between the
//! auto-generated sections.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use git_lfs::cli_def::Cli;
use git_lfs::man::{ManContent, extras_for};

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
}

fn main() -> ExitCode {
    match Args::parse().cmd {
        Cmd::GenMan { out } => match gen_man(&out) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("xtask gen-man: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

fn gen_man(out: &Path) -> std::io::Result<()> {
    fs::create_dir_all(out)?;

    let root = Cli::command();

    // Top-level `git-lfs.1`. The empty-string key in the registry
    // is reserved for it; subcommand pages use their own names.
    let path = out.join("git-lfs.1");
    let mut f = fs::File::create(&path)?;
    render_man(&mut f, "git-lfs", root.clone(), extras_for(""))?;
    eprintln!("wrote {}", path.display());

    // One page per subcommand. clap's `get_subcommands` doesn't
    // recurse, which is what we want — each first-level subcommand
    // (`fetch`, `migrate`, …) becomes its own `git-lfs-<name>.1`.
    // For now we don't generate separate pages for nested commands
    // (e.g. `migrate import`); they show up as flags / sections
    // inside `git-lfs-migrate.1`.
    for sub in root.get_subcommands() {
        let name = sub.get_name().to_owned();
        let page_name = format!("git-lfs-{name}");
        let path = out.join(format!("{page_name}.1"));
        let mut f = fs::File::create(&path)?;
        render_man(&mut f, &page_name, sub.clone(), extras_for(&name))?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}

/// Compose a man page: clap_mangen autogenerates NAME / SYNOPSIS /
/// OPTIONS / VERSION, and we splice in a custom DESCRIPTION (if
/// provided) and any extra sections after OPTIONS.
fn render_man(
    out: &mut impl Write,
    page_name: &str,
    cmd: clap::Command,
    extras: &ManContent,
) -> std::io::Result<()> {
    // Override the displayed page name. clap_mangen builds the
    // title from `cmd.get_name()`; for subcommand pages we want
    // the `git-lfs-<sub>` form rather than just `<sub>`. clap's
    // `name(...)` setter wants `Into<Str>` with a 'static-ish
    // lifetime, so hand it an owned String.
    //
    // Also pin the version on the cloned command. clap_mangen's
    // version section unwraps `get_version()`; subcommands don't
    // inherit it from the parent and our top-level `Cli` doesn't
    // declare one (we suppress `--version` and handle it manually
    // in main.rs), so we set the crate version uniformly here.
    let cmd = cmd
        .name(page_name.to_owned())
        .version(git_lfs::VERSION);
    let man = clap_mangen::Man::new(cmd);

    man.render_title(out)?;
    man.render_name_section(out)?;
    man.render_synopsis_section(out)?;

    if let Some(desc) = extras.description {
        writeln!(out, ".SH DESCRIPTION")?;
        writeln!(out, "{desc}")?;
    } else {
        man.render_description_section(out)?;
    }

    man.render_options_section(out)?;

    for (title, body) in extras.extra_sections {
        writeln!(out, ".SH {title}")?;
        writeln!(out, "{body}")?;
    }

    man.render_version_section(out)?;
    Ok(())
}
