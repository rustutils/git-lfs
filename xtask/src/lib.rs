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

/// One generated page from the clap tree: top-level `git-lfs`, a
/// first-level subcommand like `git-lfs-fetch`, or a nested
/// subcommand like `git-lfs-migrate-import`.
struct CommandPage {
    /// On-disk page name and the value used in the `.TH` title /
    /// markdown title — e.g. `"git-lfs-migrate-import"`.
    page_name: String,
    /// Lookup key handed to [`extras_for`]. Empty for the top-level
    /// `git-lfs` page; otherwise the dash-joined chain after
    /// `git-lfs-` (e.g. `"migrate-import"`). Mirrors the on-disk
    /// layout of `cli/man/<key>/...`.
    extras_key: String,
    /// Owned clone of the clap subcommand this page renders.
    cmd: clap::Command,
}

/// Walk the clap tree and collect every page we should emit.
/// Skips the auto-injected `help` subcommand clap appends to any
/// command that has subcommands — it's an interaction noun, not a
/// real command, and clap_mangen would render an unhelpful page
/// for it.
fn command_pages(root: &clap::Command) -> Vec<CommandPage> {
    let mut out = Vec::new();
    out.push(CommandPage {
        page_name: "git-lfs".to_owned(),
        extras_key: String::new(),
        cmd: root.clone(),
    });
    collect_subcommands(root, "git-lfs", "", &mut out);
    out
}

fn collect_subcommands(
    cmd: &clap::Command,
    page_name: &str,
    extras_key: &str,
    out: &mut Vec<CommandPage>,
) {
    for sub in cmd.get_subcommands() {
        let sub_name = sub.get_name();
        if sub_name == "help" {
            continue;
        }
        let child_page = format!("{page_name}-{sub_name}");
        let child_key = if extras_key.is_empty() {
            sub_name.to_owned()
        } else {
            format!("{extras_key}-{sub_name}")
        };
        out.push(CommandPage {
            page_name: child_page.clone(),
            extras_key: child_key.clone(),
            cmd: sub.clone(),
        });
        collect_subcommands(sub, &child_page, &child_key, out);
    }
}

/// Generate one `git-lfs-<sub>.1` per subcommand (recursing into
/// nested subcommands like `git-lfs-migrate-import.1`), plus a
/// top-level `git-lfs.1`, plus the synthetic pages from
/// `SYNTHETIC_PAGES`. Creates `out` if missing.
pub fn gen_man(out: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(out)?;
    let root = Cli::command();

    for page in command_pages(&root) {
        let path = out.join(format!("{}.1", page.page_name));
        let bytes =
            render::render_man(&page.page_name, page.cmd, extras_for(&page.extras_key), "1")?;
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

/// Generate one `git-lfs-<sub>.md` per subcommand (recursing into
/// nested subcommands like `git-lfs-migrate-import.md`), plus a
/// top-level `git-lfs.md`, plus the synthetic pages from
/// `SYNTHETIC_PAGES`. Creates `out` if missing.
pub fn gen_md(out: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(out)?;
    let root = Cli::command();

    for page in command_pages(&root) {
        let path = out.join(format!("{}.md", page.page_name));
        let body = render::render_md(&page.page_name, &page.cmd, extras_for(&page.extras_key));
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
