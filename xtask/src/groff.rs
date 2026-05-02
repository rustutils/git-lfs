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

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

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
    out
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
}

#[derive(Clone, Copy)]
enum ListKind {
    Bullet,
    Ordered,
}

fn handle(event: Event<'_>, state: &mut State, out: &mut String) {
    match event {
        Event::Start(Tag::Paragraph) => {
            ensure_blank_separator(out);
            out.push_str(".PP\n");
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
            state.list = Some(if start.is_some() {
                ListKind::Ordered
            } else {
                ListKind::Bullet
            });
            ensure_blank_separator(out);
        }
        Event::End(TagEnd::List(_)) => {
            state.list = None;
        }
        Event::Start(Tag::Item) => match state.list {
            Some(ListKind::Bullet) => out.push_str(".IP \\(bu 2\n"),
            Some(ListKind::Ordered) => out.push_str(".IP \\(bu 2\n"),
            None => out.push_str(".PP\n"),
        },
        Event::End(TagEnd::Item) if !out.ends_with('\n') => {
            out.push('\n');
        }

        Event::Start(Tag::DefinitionList) => ensure_blank_separator(out),
        Event::End(TagEnd::DefinitionList) if !out.ends_with('\n') => {
            out.push('\n');
        }
        Event::Start(Tag::DefinitionListTitle) => out.push_str(".TP\n"),
        Event::End(TagEnd::DefinitionListTitle) if !out.ends_with('\n') => {
            out.push('\n');
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

        Event::SoftBreak => out.push('\n'),
        Event::HardBreak => out.push_str("\n.br\n"),

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
