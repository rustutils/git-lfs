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
mod render;
mod test;

pub use test::run as run_tests;

/// Generate one `git-lfs-<sub>.1` per subcommand plus a top-level
/// `git-lfs.1`, written to `out`. Creates `out` if missing.
pub fn gen_man(out: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(out)?;
    let root = Cli::command();

    let path = out.join("git-lfs.1");
    let bytes = render::render_man("git-lfs", root.clone(), extras_for(""))?;
    std::fs::write(&path, bytes)?;
    eprintln!("wrote {}", path.display());

    for sub in root.get_subcommands() {
        let name = sub.get_name().to_owned();
        let page_name = format!("git-lfs-{name}");
        let path = out.join(format!("{page_name}.1"));
        let bytes = render::render_man(&page_name, sub.clone(), extras_for(&name))?;
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
    Ok(())
}
