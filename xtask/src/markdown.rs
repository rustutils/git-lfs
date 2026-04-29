//! Render clap's auto sections (NAME / SYNOPSIS / DESCRIPTION /
//! OPTIONS) as markdown.
//!
//! Clap exposes everything we need via introspection (`get_about`,
//! `get_arguments`, …). We don't pull in `clap-markdown` because the
//! formatting we want is small enough to produce directly and we need
//! the resulting markdown to interleave cleanly with hand-authored
//! extras.

use std::fmt::Write as _;

pub fn render_title(out: &mut String, page_name: &str) {
    let _ = writeln!(out, "# {page_name}\n");
}

pub fn render_name(out: &mut String, page_name: &str, cmd: &clap::Command) {
    let _ = writeln!(out, "## Name\n");
    let about = cmd.get_about().map(|s| s.to_string()).unwrap_or_default();
    if about.is_empty() {
        let _ = writeln!(out, "`{page_name}`\n");
    } else {
        let _ = writeln!(out, "`{page_name}` — {about}\n");
    }
}

pub fn render_synopsis(out: &mut String, page_name: &str, cmd: &clap::Command) {
    let _ = writeln!(out, "## Synopsis\n");
    let _ = writeln!(out, "```");
    let _ = writeln!(out, "{}", build_synopsis(page_name, cmd));
    let _ = writeln!(out, "```\n");
}

pub fn render_description(out: &mut String, cmd: &clap::Command) {
    let body = cmd
        .get_long_about()
        .map(|s| s.to_string())
        .or_else(|| cmd.get_about().map(|s| s.to_string()))
        .unwrap_or_default();
    if body.is_empty() {
        return;
    }
    let _ = writeln!(out, "## Description\n");
    let _ = writeln!(out, "{}\n", body.trim_end());
}

pub fn render_options(out: &mut String, cmd: &clap::Command) {
    let positionals: Vec<_> = cmd.get_arguments().filter(|a| a.is_positional()).collect();
    let flags: Vec<_> = cmd
        .get_arguments()
        .filter(|a| !a.is_positional() && !a.is_hide_set())
        .collect();
    let subs: Vec<_> = cmd.get_subcommands().filter(|s| !s.is_hide_set()).collect();

    if positionals.is_empty() && flags.is_empty() && subs.is_empty() {
        return;
    }

    let _ = writeln!(out, "## Options\n");

    if !positionals.is_empty() {
        let _ = writeln!(out, "### Arguments\n");
        for a in &positionals {
            render_arg(out, a);
        }
    }
    if !flags.is_empty() {
        let _ = writeln!(out, "### Flags\n");
        for a in &flags {
            render_arg(out, a);
        }
    }
    if !subs.is_empty() {
        let _ = writeln!(out, "### Subcommands\n");
        for s in &subs {
            let name = s.get_name();
            let about = s.get_about().map(|s| s.to_string()).unwrap_or_default();
            if about.is_empty() {
                let _ = writeln!(out, "- `{name}`");
            } else {
                let _ = writeln!(out, "- `{name}` — {about}");
            }
        }
        out.push('\n');
    }
}

/// Format one argument as a markdown definition-list-ish entry. Item
/// header on one line (bold names + value placeholder), help text
/// indented under it.
fn render_arg(out: &mut String, arg: &clap::Arg) {
    let id = arg.get_id().as_str();
    let header = if arg.is_positional() {
        format!("`<{}>`", id.to_uppercase())
    } else {
        let mut parts: Vec<String> = Vec::new();
        if let Some(short) = arg.get_short() {
            parts.push(format!("`-{short}`"));
        }
        if let Some(long) = arg.get_long() {
            parts.push(format!("`--{long}`"));
        }
        let names = parts.join(", ");
        let placeholder = arg
            .get_value_names()
            .filter(|_| arg.get_action().takes_values())
            .and_then(|n| n.first())
            .map(|n| format!(" `<{n}>`"))
            .unwrap_or_default();
        format!("{names}{placeholder}")
    };

    let help = arg
        .get_long_help()
        .map(|s| s.to_string())
        .or_else(|| arg.get_help().map(|s| s.to_string()))
        .unwrap_or_default();

    let _ = writeln!(out, "- {header}");
    for line in help.lines() {
        let _ = writeln!(out, "    {line}");
    }
    out.push('\n');
}

/// Best-effort synopsis line. Mirrors clap's own usage one-liner;
/// shipping it as plain text inside a code block keeps it copy-pasteable.
fn build_synopsis(page_name: &str, cmd: &clap::Command) -> String {
    // clap renders "Usage: <prog> [OPTIONS] [ARGS]...". Steal that and
    // swap in `page_name` so subcommand pages read e.g.
    // `git-lfs-fetch [OPTIONS] [ARGS]` rather than just `fetch`.
    let mut clone = cmd.clone();
    clone = clone
        .name(page_name.to_owned())
        .bin_name(page_name.to_owned());
    let usage = clone.render_usage().to_string();
    // clap emits "Usage: <line>" — strip the leading "Usage: " for
    // the code block.
    usage
        .strip_prefix("Usage: ")
        .unwrap_or(&usage)
        .trim()
        .to_owned()
}
