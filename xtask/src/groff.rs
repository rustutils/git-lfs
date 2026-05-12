//! Convert the small markdown subset we use for doc extras into groff.
//!
//! Vocabulary supported (anything beyond this is ignored or passed
//! through best-effort):
//!   - Paragraphs
//!   - **bold** and *italic* spans
//!   - `code` spans and fenced code blocks
//!   - Bulleted and numbered lists (one level; nesting renders flat)
//!   - Definition lists (Pandoc-style: `term` / `:   def`) → `.TP`
//!
//! Keeping the surface small means [`man.rs`] authors can rely on
//! predictable groff output without us needing to track every corner of
//! CommonMark. Anything richer goes in the markdown extras directly and
//! just won't render to groff prettily — fine, the markdown side is the
//! primary docs target anyway.

use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};

/// Render `md` as groff macros suitable for splicing inside a `.SH`
/// section. Always returns text terminated with `\n`.
pub fn from_markdown(md: &str) -> String {
    let mut out = String::new();
    let mut state = State::default();
    let parser = Parser::new_ext(
        md,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_DEFINITION_LIST,
    );
    for event in parser {
        handle(event, &mut state, &mut out);
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    fold_punct_after_ue(out)
}

/// Fold sentence-ending punctuation that lands on the line after `.UE`
/// into the macro's optional argument (`.UE .` is the canonical groff
/// idiom that places the punctuation right after the angle-bracketed
/// URL, so the sentence reads naturally instead of orphaning the
/// punctuation on its own line). Pulldown-cmark emits trailing
/// punctuation as a separate `Text` event, after `TagEnd::Link`, so we
/// can't catch it at link-end time without lookahead.
fn fold_punct_after_ue(mut s: String) -> String {
    // `.` and `'` get a leading `\&` from `escape` so groff doesn't
    // interpret them as macro-call lines; we have to match either form.
    let patterns: &[(&str, &str)] = &[
        (".UE\n\\&.", ".UE .\n"),
        (".UE\n\\&'", ".UE '\n"),
        (".UE\n,", ".UE ,\n"),
        (".UE\n;", ".UE ;\n"),
        (".UE\n:", ".UE :\n"),
        (".UE\n!", ".UE !\n"),
        (".UE\n?", ".UE ?\n"),
        (".UE\n)", ".UE )\n"),
    ];
    for (from, to) in patterns {
        s = s.replace(from, to);
    }
    s
}

#[derive(Default)]
struct State {
    /// We're inside a list (`Some(LIST)` until `End`); tracks ordered/
    /// unordered so each item's prefix matches.
    list: Option<ListKind>,
    /// True between `CodeBlock` start/end. Suppresses inline conversion
    /// and emits `.nf`/`.fi` to keep code block whitespace.
    in_code_block: bool,
    /// True between heading start/end. Used to emit a `.SS` header.
    in_heading: bool,
    /// True between `DefinitionListTitle` start/end. Lets us promote a
    /// `<br/>` inside a multi-line term to `.TQ` (tagged-paragraph
    /// continuation) instead of `.br` — `.TP` accepts only one tag
    /// line, so plain `.br` would fold the second line into the body.
    in_def_title: bool,
    /// Set after we emit a break-style macro (`.br` / `.TQ`); causes
    /// the immediately following SoftBreak to be skipped, since we've
    /// already produced the line ending and a stray `\n` would render
    /// as a blank line (and groff treats blanks as implicit `.PP`).
    suppress_next_soft_break: bool,
    /// Set on `Start(Item)`; consumed by the next `Start(Paragraph)`.
    /// Suppresses the `.PP` for the item's first paragraph so the
    /// list marker (`1.` / `•`) sits on the same logical line as
    /// the first line of body text. Without this, `.IP "1." 4`
    /// followed by `.PP` resets the paragraph and pushes the body
    /// onto its own line below the marker.
    suppress_next_paragraph: bool,
    /// True between an inline-style `Tag::Link` start and end. We use
    /// this to remember whether to emit the matching `.UE` (only for
    /// inline / reference / shortcut links — autolinks render the URL
    /// as text directly and don't get the `.UR` / `.UE` wrapper).
    in_inline_link: bool,
}

#[derive(Clone, Copy)]
enum ListKind {
    Bullet,
    /// Carries the *next* item number — incremented each `Start(Item)`
    /// so the rendered markers (`1.`, `2.`, …) match the source's
    /// ordering even when the markdown starts the list at a non-1
    /// number (pulldown-cmark threads the start number through
    /// `Tag::List(Some(n))`).
    Ordered(u64),
}

fn handle(event: Event<'_>, state: &mut State, out: &mut String) {
    match event {
        Event::Start(Tag::Paragraph) => {
            if state.suppress_next_paragraph {
                state.suppress_next_paragraph = false;
            } else {
                ensure_blank_separator(out);
                out.push_str(".PP\n");
            }
        }
        Event::End(TagEnd::Paragraph) if !out.ends_with('\n') => {
            out.push('\n');
        }

        Event::Start(Tag::Heading { .. }) => {
            ensure_blank_separator(out);
            out.push_str(".SS ");
            state.in_heading = true;
        }
        Event::End(TagEnd::Heading(_)) => {
            state.in_heading = false;
            out.push('\n');
        }

        Event::Start(Tag::Emphasis) => out.push_str("\\fI"),
        Event::End(TagEnd::Emphasis) => out.push_str("\\fR"),
        Event::Start(Tag::Strong) => out.push_str("\\fB"),
        Event::End(TagEnd::Strong) => out.push_str("\\fR"),

        Event::Start(Tag::List(start)) => {
            state.list = Some(match start {
                Some(n) => ListKind::Ordered(n),
                None => ListKind::Bullet,
            });
            ensure_blank_separator(out);
        }
        Event::End(TagEnd::List(_)) => {
            state.list = None;
        }
        Event::Start(Tag::Item) => {
            match state.list {
                Some(ListKind::Bullet) => out.push_str(".IP \\(bu 2\n"),
                Some(ListKind::Ordered(n)) => {
                    // `.IP "1." 4` — quoted marker so the literal `.`
                    // doesn't get parsed as a macro start; indent width
                    // 4 fits double-digit markers comfortably.
                    out.push_str(&format!(".IP \"{n}.\" 4\n"));
                    state.list = Some(ListKind::Ordered(n + 1));
                }
                None => out.push_str(".PP\n"),
            }
            state.suppress_next_paragraph = true;
        }
        Event::End(TagEnd::Item) => {
            // Clear the marker-pairing flag set on Start(Item). In a
            // tight list (no blank lines between items) pulldown-cmark
            // skips the per-item Paragraph events entirely, so the
            // flag never gets consumed and would otherwise leak onto
            // the next "real" paragraph after the list — eating the
            // .PP that should separate it from the trailing item.
            state.suppress_next_paragraph = false;
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }

        Event::Start(Tag::DefinitionList) => ensure_blank_separator(out),
        Event::End(TagEnd::DefinitionList) if !out.ends_with('\n') => {
            out.push('\n');
        }
        Event::Start(Tag::DefinitionListTitle) => {
            out.push_str(".TP\n");
            state.in_def_title = true;
        }
        Event::End(TagEnd::DefinitionListTitle) => {
            state.in_def_title = false;
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        Event::Start(Tag::DefinitionListDefinition) => {}
        Event::End(TagEnd::DefinitionListDefinition) if !out.ends_with('\n') => {
            out.push('\n');
        }

        Event::Start(Tag::CodeBlock(_)) => {
            ensure_blank_separator(out);
            out.push_str(".PP\n.RS 4\n.nf\n");
            state.in_code_block = true;
        }
        Event::End(TagEnd::CodeBlock) => {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(".fi\n.RE\n");
            state.in_code_block = false;
        }

        Event::Code(code) => {
            // Inline code: bold mono-ish styling. groff `\fB...\fR`
            // is the closest universally-rendered equivalent.
            out.push_str("\\fB");
            out.push_str(&escape(&code));
            out.push_str("\\fR");
        }

        Event::Text(text) => {
            if state.in_code_block {
                // Don't escape backslashes in code blocks the same way
                // — but still mask the `.` at line start that groff
                // would interpret as a macro.
                for line in text.split_inclusive('\n') {
                    if line.starts_with('.') {
                        out.push('\\');
                        out.push('&');
                    }
                    out.push_str(line);
                }
            } else {
                out.push_str(&escape(&text));
            }
        }

        Event::SoftBreak => {
            if state.suppress_next_soft_break {
                state.suppress_next_soft_break = false;
            } else {
                out.push('\n');
            }
        }
        Event::HardBreak => {
            out.push_str("\n.br\n");
            state.suppress_next_soft_break = true;
        }

        // Markdown links → groff's `.UR` / `.UE` macros. In a modern
        // terminal that supports OSC-8, the link text becomes
        // clickable and is styled (typically blue/underlined); the
        // URL gets appended in `< >` as a fallback for non-OSC-8
        // viewers. Autolinks (`<https://...>`) and email autolinks
        // skip the wrapper because their text *is* the URL — wrapping
        // would render the URL twice.
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            ..
        }) => match link_type {
            LinkType::Autolink | LinkType::Email => {}
            _ => {
                out.push_str("\n.UR ");
                out.push_str(&dest_url);
                out.push('\n');
                state.in_inline_link = true;
            }
        },
        Event::End(TagEnd::Link) if state.in_inline_link => {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(".UE\n");
            state.in_inline_link = false;
        }

        // Inline HTML: only `<br/>` / `<br>` is meaningful for our
        // surface. Inside a definition-list title, promote it to
        // `.TQ` so the next line stacks above the body at tag-level
        // indent. Anywhere else, treat it like a hard break.
        Event::InlineHtml(html) => {
            let trimmed = html.trim().to_ascii_lowercase();
            if trimmed == "<br/>" || trimmed == "<br>" || trimmed == "<br />" {
                if state.in_def_title {
                    out.push_str("\n.TQ\n");
                } else {
                    out.push_str("\n.br\n");
                }
                state.suppress_next_soft_break = true;
            }
        }

        // Best-effort: ignore everything else (links, images, html,
        // tables, footnotes, …). Authors writing the extras stick to
        // the documented vocabulary.
        _ => {}
    }
}

/// Make sure we're at the start of a fresh line with a blank line
/// before the next macro, so e.g. paragraphs don't run into the prior
/// content.
fn ensure_blank_separator(out: &mut String) {
    if out.is_empty() {
        return;
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Escape characters that have meaning in groff. The only ones that
/// matter for the small vocabulary we support: leading `.` (macro
/// trigger) and bare `\\` (escape).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for line in s.split_inclusive('\n') {
        if line.starts_with('.') || line.starts_with('\'') {
            // `\&` is a zero-width space — prevents groff from
            // interpreting the leading `.` or `'` as a macro.
            out.push_str("\\&");
        }
        for c in line.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '-' => out.push_str("\\-"),
                _ => out.push(c),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraph_renders_pp() {
        let g = from_markdown("hello world");
        assert!(g.contains(".PP\nhello world\n"));
    }

    #[test]
    fn bold_and_italic() {
        let g = from_markdown("**bold** and *italic*");
        assert!(g.contains("\\fBbold\\fR"));
        assert!(g.contains("\\fIitalic\\fR"));
    }

    #[test]
    fn inline_code() {
        let g = from_markdown("run `git lfs fetch`");
        assert!(g.contains("\\fBgit lfs fetch\\fR"));
    }

    #[test]
    fn code_block() {
        let g = from_markdown("```\ngit lfs fetch\n```");
        assert!(g.contains(".nf\ngit lfs fetch\n"));
        assert!(g.contains(".fi\n"));
    }

    #[test]
    fn bullet_list() {
        let g = from_markdown("- foo\n- bar");
        assert!(g.contains(".IP \\(bu 2\n"));
        assert!(g.contains("foo"));
        assert!(g.contains("bar"));
    }

    #[test]
    fn inline_link_uses_ur_ue_macros() {
        // Inline `[text](url)` becomes a `.UR` / `.UE` block so modern
        // terminals can render it as a clickable hyperlink. Trailing
        // sentence punctuation gets folded into the `.UE` argument so
        // it doesn't orphan onto its own line.
        let g = from_markdown("Report at our [issue tracker](https://example.com/issues).");
        assert!(g.contains(".UR https://example.com/issues\n"), "got: {g}");
        assert!(g.contains("issue tracker\n.UE .\n"), "got: {g}");
    }

    #[test]
    fn autolink_skips_ur_ue_wrapper() {
        // Autolinks (`<https://...>`) don't get the wrapper because the
        // text is the URL — wrapping would render the URL twice.
        let g = from_markdown("See <https://example.com/foo>");
        assert!(g.contains("https://example.com/foo"), "got: {g}");
        assert!(!g.contains(".UR"), "got: {g}");
        assert!(!g.contains(".UE"), "got: {g}");
    }

    #[test]
    fn definition_list_multiline_title_via_br() {
        // `<br/>` inside a def-list title becomes `.TQ` so the second
        // line stacks at tag-level indent rather than folding into
        // the body. The SoftBreak that pulldown-cmark emits after
        // the inline HTML must be suppressed so we don't render a
        // blank line between `.TQ` and the next term line.
        let g = from_markdown(
            "`first line`<br/>\n\
             `second line`\n\
             :   body text here\n",
        );
        assert!(
            g.contains(".TP\n\\fBfirst line\\fR\n.TQ\n\\fBsecond line\\fR\n"),
            "got: {g}"
        );
        // No blank line between .TQ and the second tag line.
        assert!(!g.contains(".TQ\n\n"), "got: {g}");
        assert!(g.contains("body text here"), "got: {g}");
    }

    #[test]
    fn definition_list() {
        let g = from_markdown(
            "GIT_LFS_SKIP_SMUDGE\n\
             :   When set, behaves as `--skip`.\n\
             \n\
             GIT_LFS_PROGRESS\n\
             :   Path to a progress log.\n",
        );
        // Each title becomes a `.TP` block; body follows on the next line.
        assert!(g.contains(".TP\nGIT_LFS_SKIP_SMUDGE\n"), "got: {g}");
        assert!(g.contains(".TP\nGIT_LFS_PROGRESS\n"), "got: {g}");
        // Body text appears, with inline-code rendered as `\fB...\fR`.
        assert!(g.contains("When set, behaves as"), "got: {g}");
        assert!(g.contains("\\fB\\-\\-skip\\fR"), "got: {g}");
    }

    #[test]
    fn leading_dot_in_text_is_escaped() {
        let g = from_markdown(".gitignore is special");
        // Leading `.` would otherwise look like a groff macro; we
        // prepend `\&` to neutralize it.
        assert!(g.contains("\\&.gitignore"));
    }
}
