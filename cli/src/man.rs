//! Per-subcommand documentation extras (man pages + mdbook).
//!
//! The clap derive in [`crate::args`] is the source of truth for the
//! NAME / SYNOPSIS / OPTIONS surface — xtask renders that automatically
//! into both groff (man pages) and markdown (mdbook). This module owns
//! everything richer: DESCRIPTION prose, EXAMPLES, NOTES, FILES,
//! SEE ALSO.
//!
//! **Bodies are authored in markdown.** xtask passes them through
//! verbatim for the markdown output and converts them to groff for the
//! man pages, so a single source feeds both formats. The supported
//! markdown vocabulary is intentionally small — paragraphs, bold/italic,
//! code spans / fenced code blocks, bulleted and numbered lists. Stick
//! to that and the groff conversion stays predictable.
//!
//! Each subcommand exposes its extras here as a [`ManContent`] entry in
//! [`extras_for`]. Bodies live under `cli/man/<sub>/*.md` and are pulled
//! in via [`include_str!`], keeping prose out of `man.rs`.
//!
//! Onboarding a new section is two-step:
//! 1. Drop one or more `.md` files into `cli/man/<subcommand>/`.
//! 2. Add a match arm in [`extras_for`] referencing them.
//!
//! Subcommands without an entry get the auto-generated page with no
//! extras — still useful, just shorter.

/// Hand-authored extras for a single command's documentation. Returned
/// by [`extras_for`] keyed on the subcommand name (or `""` for the top-
/// level `git-lfs` page). Both fields are markdown — xtask renders them
/// to either groff or markdown depending on output format.
#[derive(Debug)]
pub struct ManContent {
    /// Replaces the auto-generated DESCRIPTION (which is just the short
    /// `about` from the clap derive). Markdown.
    pub description: Option<&'static str>,

    /// Sections appended after OPTIONS, in order. Each entry is
    /// `(title, markdown body)`. Conventional titles: `EXAMPLES`,
    /// `FILES`, `ENVIRONMENT`, `NOTES`, `BUGS`, `SEE ALSO`. The title
    /// becomes a `.SH` in groff and a top-level `##` in markdown.
    pub extra_sections: &'static [(&'static str, &'static str)],
}

impl ManContent {
    pub const fn empty() -> Self {
        Self {
            description: None,
            extra_sections: &[],
        }
    }
}

const EMPTY: ManContent = ManContent::empty();

/// Look up the doc extras for `subcommand` (e.g. `"fetch"`,
/// `"checkout"`). Pass `""` for the top-level `git-lfs` page.
/// Returns a reference to [`ManContent::empty`] when there's no entry,
/// so the caller can always splice unconditionally.
pub fn extras_for(_subcommand: &str) -> &'static ManContent {
    // No entries yet. As we author content, replace this with a
    // match on `subcommand`:
    //
    //     match subcommand {
    //         "fetch" => &FETCH_DOCS,
    //         "checkout" => &CHECKOUT_DOCS,
    //         _ => &EMPTY,
    //     }
    &EMPTY
}
