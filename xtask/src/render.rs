//! Page composition for both output formats.
//!
//! Both man and markdown follow the same shape:
//!   - title line / heading
//!   - NAME (one-line summary)
//!   - SYNOPSIS (usage from clap)
//!   - DESCRIPTION (clap's long_about, or override from extras)
//!   - OPTIONS (each flag/arg)
//!   - extra sections (from [`ManContent::extra_sections`])
//!   - VERSION (man only)
//!
//! The two rendering backends share that order; only the surface markup
//! differs (groff macros vs. markdown headings/lists/code).

use git_lfs::man::ManContent;

use crate::{groff, markdown};

/// Build a man page as a byte vec. clap_mangen handles NAME / SYNOPSIS /
/// OPTIONS / VERSION; we splice in the markdown-sourced extras
/// (description override, post-OPTIONS sections) converted to groff.
pub fn render_man(
    page_name: &str,
    cmd: clap::Command,
    extras: &ManContent,
) -> std::io::Result<Vec<u8>> {
    use std::io::Write as _;

    // See `xtask::lib`/the bin docs: clap_mangen needs the page name on
    // the command (titles come from `cmd.get_name()`) and a version
    // (subcommands don't inherit the parent's, so set crate version).
    let cmd = cmd
        .name(page_name.to_owned())
        .version(git_lfs::VERSION);
    let man = clap_mangen::Man::new(cmd);

    let mut out = Vec::<u8>::new();
    man.render_title(&mut out)?;
    man.render_name_section(&mut out)?;
    man.render_synopsis_section(&mut out)?;

    if let Some(md) = extras.description {
        writeln!(out, ".SH DESCRIPTION")?;
        out.extend_from_slice(groff::from_markdown(md).as_bytes());
    } else {
        man.render_description_section(&mut out)?;
    }

    man.render_options_section(&mut out)?;

    for (title, body_md) in extras.extra_sections {
        writeln!(out, ".SH {title}")?;
        out.extend_from_slice(groff::from_markdown(body_md).as_bytes());
    }

    man.render_version_section(&mut out)?;
    Ok(out)
}

/// Build a markdown reference page as a String. Layout mirrors the man
/// page so the two stay legible side-by-side; extras pass through
/// verbatim since they're already markdown.
pub fn render_md(page_name: &str, cmd: &clap::Command, extras: &ManContent) -> String {
    let mut out = String::new();
    markdown::render_title(&mut out, page_name);
    markdown::render_name(&mut out, page_name, cmd);
    markdown::render_synopsis(&mut out, page_name, cmd);

    if let Some(md) = extras.description {
        out.push_str("## Description\n\n");
        out.push_str(md.trim_end());
        out.push_str("\n\n");
    } else {
        markdown::render_description(&mut out, cmd);
    }

    markdown::render_options(&mut out, cmd);

    for (title, body) in extras.extra_sections {
        out.push_str("## ");
        out.push_str(&pretty_title(title));
        out.push_str("\n\n");
        out.push_str(body.trim_end());
        out.push_str("\n\n");
    }
    out
}

/// `EXAMPLES` → `Examples`. groff convention is uppercase headings;
/// markdown reads better in title case.
fn pretty_title(title: &str) -> String {
    let lower = title.to_lowercase();
    let mut chars = lower.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
