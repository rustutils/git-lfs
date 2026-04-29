//! Per-subcommand man-page extras.
//!
//! `clap_mangen` autogenerates NAME / SYNOPSIS / OPTIONS / VERSION from
//! the clap derive in [`crate::cli_def`]. Anything richer — DESCRIPTION
//! prose, EXAMPLES, NOTES, FILES, SEE ALSO — needs to be authored by
//! hand and stitched into the rendered output by xtask.
//!
//! Each subcommand exposes its extras here as a [`ManContent`] entry in
//! [`extras_for`]. Bodies live as raw groff under `cli/man/<sub>/*.man`
//! and are pulled in via [`include_str!`], so xtask can write them
//! verbatim between the auto-generated sections.
//!
//! Onboarding a new section is two-step:
//! 1. Drop one or more `.man` files into `cli/man/<subcommand>/`.
//! 2. Add a match arm in [`extras_for`] referencing them.
//!
//! Subcommands without an entry get the auto-generated page with no
//! extras — still useful, just shorter.

/// Hand-authored extras for a single command's man page. Returned by
/// [`extras_for`] keyed on the subcommand name (or `""` for the top-
/// level `git-lfs` page).
#[derive(Debug)]
pub struct ManContent {
    /// Replaces clap's auto-generated DESCRIPTION (which is just the
    /// short `about` from the derive). Plain text — xtask wraps it in
    /// `.SH DESCRIPTION`.
    pub description: Option<&'static str>,

    /// Sections appended after OPTIONS, in order. Each entry is
    /// `(uppercase title, raw groff body)`. Conventional ordering:
    /// EXAMPLES, FILES, ENVIRONMENT, NOTES, BUGS, SEE ALSO.
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

/// Look up the man-page extras for `subcommand` (e.g. `"fetch"`,
/// `"checkout"`). Pass `""` for the top-level `git-lfs` page.
/// Returns a reference to [`ManContent::empty`] when there's no entry,
/// so the caller can always splice unconditionally.
pub fn extras_for(_subcommand: &str) -> &'static ManContent {
    // No entries yet. As we author content, replace this with a
    // match on `subcommand`:
    //
    //     match subcommand {
    //         "fetch" => &FETCH_MAN,
    //         "checkout" => &CHECKOUT_MAN,
    //         _ => &EMPTY,
    //     }
    &EMPTY
}
