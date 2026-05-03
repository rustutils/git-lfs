//! Post-processing passes for already-rendered output.
//!
//! Two transforms, applied in order:
//!   1. Inline code spans `` `code` `` → `\fB code \fR` (groff only;
//!      markdown handles backticks natively).
//!   2. Man-page cross-references like `git-lfs-config(5)`,
//!      `gitignore(5)`, `gitattributes(5)` → bold (groff) or hyperlink
//!      (markdown). Internal pages link to a sibling `.md`; the upstream
//!      git pages link to <https://git-scm.com/docs/...>.
//!
//! Why post-process instead of mutating the clap tree before render:
//! clap_mangen renders OPTIONS internally and we don't get a hook into
//! each arg's help text. Operating on the rendered bytes is brittle but
//! contained — if we add more markdown features we'll need to revisit.
//!
//! Limitation: the markdown man-ref pass uses a simple "skip text inside
//! backtick spans" scan rather than a full pulldown-cmark walk. So a
//! reference inside fenced code (e.g. inside a triple-backtick block)
//! would still get linked. Don't write `git-lfs-config(5)` inside fenced
//! examples — wrap with single backticks if you really want it literal.

use regex::Regex;
use std::sync::OnceLock;

/// Apply groff-targeted transforms to a fully-rendered man page.
pub fn for_groff(input: String) -> String {
    let s = transform_inline_code_groff(&input);
    transform_man_refs_groff(&s)
}

/// Apply markdown-targeted transforms to a fully-rendered markdown page.
pub fn for_markdown(input: String) -> String {
    transform_man_refs_markdown(&input)
}

fn inline_code_re() -> &'static Regex {
    // Match a single-backtick span on one line. Multi-backtick spans
    // (CommonMark allows ``code with ` in it``) aren't used in our
    // sources today; if that changes we'll need a smarter match.
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`([^`\n]+)`").unwrap())
}

fn internal_ref_md_re() -> &'static Regex {
    // Internal pages, raw (unescaped) form — for markdown input where
    // hyphens are literal.
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(git-lfs-[a-z][a-z0-9-]*)\((1|5)\)").unwrap())
}

fn internal_ref_groff_re() -> &'static Regex {
    // Internal pages, post-clap_mangen form — that renderer escapes `-`
    // to `\-`. The capture is the full reference so the bolded
    // substitution preserves the escapes verbatim.
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(git\\-lfs\\-[a-z](?:[a-z0-9]|\\-)*\((?:1|5)\))").unwrap())
}

fn external_ref_re() -> &'static Regex {
    // Upstream git man pages we cross-reference. Add new names here
    // as we link to them.
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(gitignore|gitattributes|gitconfig|git-worktree)\((\d)\)").unwrap()
    })
}

fn transform_inline_code_groff(s: &str) -> String {
    inline_code_re().replace_all(s, r"\fB$1\fR").into_owned()
}

fn transform_man_refs_groff(s: &str) -> String {
    let s = internal_ref_groff_re()
        .replace_all(s, r"\fB$1\fR")
        .into_owned();
    external_ref_re()
        .replace_all(&s, r"\fB$1($2)\fR")
        .into_owned()
}

fn transform_man_refs_markdown(s: &str) -> String {
    map_outside_code_spans(s, |chunk| {
        let chunk = internal_ref_md_re().replace_all(chunk, r"[$1($2)](./$1.md)");
        external_ref_re()
            .replace_all(&chunk, r"[$1($2)](https://git-scm.com/docs/$1)")
            .into_owned()
    })
}

/// Apply `transform` to the parts of `s` that are *not* inside a single-
/// backtick code span. Spans (and their delimiters) are appended to the
/// output unchanged. Used by the markdown man-ref pass so that a literal
/// `` `name(5)` `` doesn't get rewritten into a broken link.
fn map_outside_code_spans(s: &str, transform: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut buffer = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '`' {
            buffer.push(c);
            continue;
        }
        out.push_str(&transform(&buffer));
        buffer.clear();
        out.push('`');
        for c in chars.by_ref() {
            out.push(c);
            if c == '`' {
                break;
            }
        }
    }
    if !buffer.is_empty() {
        out.push_str(&transform(&buffer));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_code_to_groff() {
        let g = for_groff("invoke `git lfs fetch` first".to_owned());
        assert!(g.contains(r"\fBgit lfs fetch\fR"), "got: {g}");
    }

    #[test]
    fn inline_code_unmatched_backtick_passthrough() {
        let g = for_groff("price is $5 ` somewhere".to_owned());
        // Single dangling backtick: no closing match, leave intact.
        assert!(g.contains("price is $5 ` somewhere"), "got: {g}");
    }

    #[test]
    fn man_refs_groff_internal_escaped() {
        // clap_mangen escapes `-` to `\-`; our groff regex must match the
        // escaped form. Bolded substitution preserves the escapes.
        let g = for_groff(r"see git\-lfs\-config(5) and gitignore(5)".to_owned());
        assert!(g.contains(r"\fBgit\-lfs\-config(5)\fR"), "got: {g}");
        assert!(g.contains(r"\fBgitignore(5)\fR"), "got: {g}");
    }

    #[test]
    fn man_refs_markdown_internal() {
        let m = for_markdown("see git-lfs-config(5) and git-lfs-fetch(1)".to_owned());
        assert!(
            m.contains("[git-lfs-config(5)](./git-lfs-config.md)"),
            "got: {m}"
        );
        assert!(
            m.contains("[git-lfs-fetch(1)](./git-lfs-fetch.md)"),
            "got: {m}"
        );
    }

    #[test]
    fn man_refs_markdown_external() {
        let m = for_markdown("see gitignore(5) and gitattributes(5)".to_owned());
        assert!(
            m.contains("[gitignore(5)](https://git-scm.com/docs/gitignore)"),
            "got: {m}"
        );
        assert!(
            m.contains("[gitattributes(5)](https://git-scm.com/docs/gitattributes)"),
            "got: {m}"
        );
    }

    #[test]
    fn man_refs_markdown_skips_code_spans() {
        // The literal `name(5)` inside a code span must not be rewritten.
        let m = for_markdown("literal `git-lfs-config(5)` here".to_owned());
        assert!(m.contains("`git-lfs-config(5)`"), "got: {m}");
        assert!(!m.contains("](./git-lfs-config.md)"), "got: {m}");
    }

    #[test]
    fn man_refs_groff_unknown_section_skipped() {
        // Section 7 isn't in our `(1|5)` whitelist for internal pages.
        // Escaped form, like clap_mangen would produce.
        let g = for_groff(r"imaginary git\-lfs\-thing(7) here".to_owned());
        assert!(g.contains(r"git\-lfs\-thing(7)"), "got: {g}");
        assert!(!g.contains(r"\fBgit\-lfs\-thing(7)\fR"), "got: {g}");
    }
}
