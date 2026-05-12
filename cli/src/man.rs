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

const ROOT: ManContent = ManContent {
    description: None,
    extra_sections: &[("EXAMPLES", include_str!("../man/root/examples.md"))],
};

/// Markdown body for the `REPORTING BUGS` section that xtask appends
/// to every generated man / mdbook page. Single source of truth for
/// the project URL and the "this is the Rust implementation" framing
/// — change here and every page picks it up on the next regen.
pub const REPORTING_BUGS: &str = include_str!("../man/reporting_bugs.md");

const SMUDGE: ManContent = ManContent {
    description: None,
    extra_sections: &[
        ("ENVIRONMENT", include_str!("../man/smudge/environment.md")),
        ("KNOWN BUGS", include_str!("../man/smudge/known_bugs.md")),
    ],
};

const CHECKOUT: ManContent = ManContent {
    description: None,
    extra_sections: &[("EXAMPLES", include_str!("../man/checkout/examples.md"))],
};

const FETCH: ManContent = ManContent {
    description: None,
    extra_sections: &[
        (
            "DEFAULT REMOTE",
            include_str!("../man/fetch/default_remote.md"),
        ),
        ("DEFAULT REFS", include_str!("../man/fetch/default_refs.md")),
        (
            "INCLUDE AND EXCLUDE",
            include_str!("../man/fetch/include_and_exclude.md"),
        ),
        ("EXAMPLES", include_str!("../man/fetch/examples.md")),
        ("SEE ALSO", include_str!("../man/fetch/see_also.md")),
    ],
};

const PULL: ManContent = ManContent {
    description: None,
    extra_sections: &[
        (
            "DEFAULT REMOTE",
            include_str!("../man/pull/default_remote.md"),
        ),
        (
            "INCLUDE AND EXCLUDE",
            include_str!("../man/pull/include_and_exclude.md"),
        ),
        ("SEE ALSO", include_str!("../man/pull/see_also.md")),
    ],
};

const PUSH: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/push/see_also.md"))],
};

const INSTALL: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/install/see_also.md"))],
};

const UNINSTALL: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/uninstall/see_also.md"))],
};

const TRACK: ManContent = ManContent {
    description: None,
    extra_sections: &[
        ("EXAMPLES", include_str!("../man/track/examples.md")),
        ("SEE ALSO", include_str!("../man/track/see_also.md")),
    ],
};

const UNTRACK: ManContent = ManContent {
    description: None,
    extra_sections: &[
        ("EXAMPLES", include_str!("../man/untrack/examples.md")),
        ("SEE ALSO", include_str!("../man/untrack/see_also.md")),
    ],
};

const LOCK: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/lock/see_also.md"))],
};

const LOCKS: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/locks/see_also.md"))],
};

const UNLOCK: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/unlock/see_also.md"))],
};

const STATUS: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/status/see_also.md"))],
};

const LS_FILES: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/ls-files/see_also.md"))],
};

const PRUNE: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/prune/see_also.md"))],
};

const FSCK: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/fsck/see_also.md"))],
};

const CLEAN: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/clean/see_also.md"))],
};

const FILTER_PROCESS: ManContent = ManContent {
    description: None,
    extra_sections: &[(
        "SEE ALSO",
        include_str!("../man/filter-process/see_also.md"),
    )],
};

const CLONE: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/clone/see_also.md"))],
};

const PRE_PUSH: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/pre-push/see_also.md"))],
};

const POST_CHECKOUT: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/post-checkout/see_also.md"))],
};

const POST_COMMIT: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/post-commit/see_also.md"))],
};

const POST_MERGE: ManContent = ManContent {
    description: None,
    extra_sections: &[("SEE ALSO", include_str!("../man/post-merge/see_also.md"))],
};

const MIGRATE: ManContent = ManContent {
    description: None,
    extra_sections: &[
        (
            "INCLUDE AND EXCLUDE",
            include_str!("../man/migrate/include_and_exclude.md"),
        ),
        (
            "INCLUDE AND EXCLUDE REFERENCES",
            include_str!("../man/migrate/include_and_exclude_references.md"),
        ),
        ("EXAMPLES", include_str!("../man/migrate/examples.md")),
        ("SEE ALSO", include_str!("../man/migrate/see_also.md")),
    ],
};

/// Look up the doc extras for `subcommand` (e.g. `"fetch"`,
/// `"checkout"`). Pass `""` for the top-level `git-lfs` page.
/// Returns a reference to [`ManContent::empty`] when there's no entry,
/// so the caller can always splice unconditionally.
pub fn extras_for(subcommand: &str) -> &'static ManContent {
    match subcommand {
        "smudge" => &SMUDGE,
        "checkout" => &CHECKOUT,
        "fetch" => &FETCH,
        "pull" => &PULL,
        "push" => &PUSH,
        "install" => &INSTALL,
        "uninstall" => &UNINSTALL,
        "track" => &TRACK,
        "untrack" => &UNTRACK,
        "lock" => &LOCK,
        "locks" => &LOCKS,
        "unlock" => &UNLOCK,
        "status" => &STATUS,
        "ls-files" => &LS_FILES,
        "prune" => &PRUNE,
        "fsck" => &FSCK,
        "clean" => &CLEAN,
        "filter-process" => &FILTER_PROCESS,
        "clone" => &CLONE,
        "pre-push" => &PRE_PUSH,
        "post-checkout" => &POST_CHECKOUT,
        "post-commit" => &POST_COMMIT,
        "post-merge" => &POST_MERGE,
        "migrate" => &MIGRATE,
        "" => &ROOT,
        _ => &EMPTY,
    }
}
