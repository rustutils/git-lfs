//! Build automation for git-lfs.
//!
//! Generates man pages (groff) and reference docs (markdown) from a
//! single source: the clap definition in [`git_lfs::args`] plus the
//! hand-authored extras in [`git_lfs::man`]. Both formats share the
//! same input — markdown-authored extras are rendered verbatim for
//! mdbook and converted to groff for `man(1)`.
//!
//! The bin is a thin CLI wrapper over the [`gen_man`] and [`gen_md`]
//! entry points exposed here, so the snapshot test under `tests/` can
//! call them directly without spawning a subprocess.

use std::path::Path;

use clap::CommandFactory;
use git_lfs::args::Cli;
use git_lfs::man::extras_for;

mod groff;
mod markdown;
mod post_process;
mod render;
mod test;

pub use test::run as run_tests;

/// A page that isn't backed by a clap subcommand — e.g. config-file
/// reference pages in section 5. Driven entirely by the hand-authored
/// extras in [`git_lfs::man`]; clap is just a vehicle for the NAME
/// line and uniform layout.
struct SyntheticPage {
    /// Full page name as it appears on disk and in the NAME line, e.g.
    /// `"git-lfs-config"`.
    name: &'static str,
    /// Man-page section number, e.g. `"5"` for file-format pages.
    section: &'static str,
    /// Short summary used as clap's `about` (shows up in NAME).
    about: &'static str,
    /// Lookup key handed to [`extras_for`] for the body sections.
    extras_key: &'static str,
}

const SYNTHETIC_PAGES: &[SyntheticPage] = &[SyntheticPage {
    name: "git-lfs-config",
    section: "5",
    about: "Configuration options for git-lfs",
    extras_key: "config",
}];

/// Build a `clap::Command` skeleton for a synthetic page. No
/// subcommands, no flags — just enough metadata for `render_man` /
/// `render_md` to produce NAME and (an empty) DESCRIPTION; everything
/// substantive comes from [`extras_for`]. We disable the `--help` /
/// `--version` flags clap auto-injects so the OPTIONS section stays
/// empty (config-file pages have nothing to say there).
fn synthetic_command(page: &SyntheticPage) -> clap::Command {
    clap::Command::new(page.name)
        .about(page.about)
        .disable_help_flag(true)
        .disable_version_flag(true)
}

/// Generate one `git-lfs-<sub>.1` per subcommand plus a top-level
/// `git-lfs.1`, written to `out`. Creates `out` if missing.
pub fn gen_man(out: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(out)?;
    let root = Cli::command();

    let path = out.join("git-lfs.1");
    let bytes = render::render_man("git-lfs", root.clone(), extras_for(""), "1")?;
    std::fs::write(&path, bytes)?;
    eprintln!("wrote {}", path.display());

    for sub in root.get_subcommands() {
        let name = sub.get_name().to_owned();
        let page_name = format!("git-lfs-{name}");
        let path = out.join(format!("{page_name}.1"));
        let bytes = render::render_man(&page_name, sub.clone(), extras_for(&name), "1")?;
        std::fs::write(&path, bytes)?;
        eprintln!("wrote {}", path.display());
    }

    for page in SYNTHETIC_PAGES {
        let path = out.join(format!("{}.{}", page.name, page.section));
        let bytes = render::render_man(
            page.name,
            synthetic_command(page),
            extras_for(page.extras_key),
            page.section,
        )?;
        std::fs::write(&path, bytes)?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}

/// Generate one `git-lfs-<sub>.md` per subcommand plus a top-level
/// `git-lfs.md`, written to `out`. Creates `out` if missing.
pub fn gen_md(out: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(out)?;
    let root = Cli::command();

    let path = out.join("git-lfs.md");
    let body = render::render_md("git-lfs", &root, extras_for(""));
    std::fs::write(&path, body)?;
    eprintln!("wrote {}", path.display());

    for sub in root.get_subcommands() {
        let name = sub.get_name().to_owned();
        let page_name = format!("git-lfs-{name}");
        let path = out.join(format!("{page_name}.md"));
        let body = render::render_md(&page_name, sub, extras_for(&name));
        std::fs::write(&path, body)?;
        eprintln!("wrote {}", path.display());
    }

    for page in SYNTHETIC_PAGES {
        let path = out.join(format!("{}.md", page.name));
        let body = render::render_md(
            page.name,
            &synthetic_command(page),
            extras_for(page.extras_key),
        );
        std::fs::write(&path, body)?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}
