//! Markdown events → styled lines. Pure, tested on the host.
//!
//! The output is what the viewer paints: a list of lines, each a list of
//! spans with an optional theme role or an optional colour. Roles are the
//! theme's chrome vocabulary (there are no content roles yet), so headings
//! borrow `title` and code borrows `info`; emphasis and links carry fixed
//! colours, which the reader's theme cannot override — the price of not
//! inventing roles before the demos say which ones are needed.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// One fragment of a rendered line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub role: Option<&'static str>,
    pub fg: Option<(u8, u8, u8)>,
}

/// A rendered line.
pub type Line = Vec<Span>;

const EMPHASIS: (u8, u8, u8) = (190, 190, 255);
const STRONG: (u8, u8, u8) = (255, 210, 120);
const LINK_URL: (u8, u8, u8) = (140, 140, 140);
const RULE: (u8, u8, u8) = (120, 120, 120);

#[derive(Default)]
struct State {
    lines: Vec<Line>,
    current: Line,
    /// Nesting of lists: one entry per open list, `Some(n)` for ordered.
    lists: Vec<Option<u64>>,
    quote_depth: usize,
    in_code_block: bool,
    emphasis: usize,
    strong: usize,
    inline_code: bool,
    /// The URL of the link being rendered, appended at its end.
    link: Option<String>,
    /// Table cells of the row being rendered.
    table_row: Option<Vec<String>>,
    in_table_cell: bool,
}

impl State {
    fn prefix(&self) -> String {
        "│ ".repeat(self.quote_depth)
    }

    fn push_text(&mut self, text: &str) {
        if self.in_table_cell {
            if let Some(row) = self.table_row.as_mut() {
                if let Some(cell) = row.last_mut() {
                    cell.push_str(text);
                }
            }
            return;
        }
        let (role, fg) = if self.inline_code {
            (Some("info"), None)
        } else if self.strong > 0 {
            (None, Some(STRONG))
        } else if self.emphasis > 0 {
            (None, Some(EMPHASIS))
        } else {
            (None, None)
        };
        self.push_span(text, role, fg);
    }

    fn push_span(&mut self, text: &str, role: Option<&'static str>, fg: Option<(u8, u8, u8)>) {
        if text.is_empty() {
            return;
        }
        if self.current.is_empty() && self.quote_depth > 0 {
            let p = self.prefix();
            self.current.push(Span {
                text: p,
                role: None,
                fg: None,
            });
        }
        self.current.push(Span {
            text: text.to_owned(),
            role,
            fg,
        });
    }

    fn flush(&mut self) {
        if !self.current.is_empty() {
            let line = std::mem::take(&mut self.current);
            self.lines.push(line);
        }
    }

    fn blank(&mut self) {
        self.flush();
        if self.lines.last().is_some_and(|l| !l.is_empty()) {
            self.lines.push(Vec::new());
        }
    }
}

/// Render a Markdown document to styled lines.
pub fn render(markdown: &str) -> Vec<Line> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let mut st = State::default();
    for ev in Parser::new_ext(markdown, opts) {
        match ev {
            Event::Start(tag) => start(&mut st, tag),
            Event::End(tag) => end(&mut st, tag),
            Event::Text(t) => {
                if st.in_code_block {
                    // Fenced content keeps its own line breaks.
                    for (i, l) in t.split('\n').enumerate() {
                        if i > 0 {
                            st.flush();
                        }
                        st.push_span(l, Some("info"), None);
                    }
                } else {
                    st.push_text(&t);
                }
            }
            Event::Code(t) => {
                st.inline_code = true;
                st.push_text(&t);
                st.inline_code = false;
            }
            Event::SoftBreak => st.push_text(" "),
            Event::HardBreak => st.flush(),
            Event::Rule => {
                st.blank();
                st.push_span("────────", None, Some(RULE));
                st.blank();
            }
            Event::Html(t) | Event::InlineHtml(t) => st.push_text(&t),
            Event::TaskListMarker(done) => st.push_text(if done { "[x] " } else { "[ ] " }),
            Event::FootnoteReference(_) | Event::InlineMath(_) | Event::DisplayMath(_) => {}
        }
    }
    st.flush();
    // No trailing blank line.
    while st.lines.last().is_some_and(Vec::is_empty) {
        st.lines.pop();
    }
    st.lines
}

fn start(st: &mut State, tag: Tag<'_>) {
    match tag {
        Tag::Paragraph => {}
        Tag::Heading { level, .. } => {
            st.blank();
            let marker = match level {
                HeadingLevel::H1 => "",
                HeadingLevel::H2 => "  ",
                _ => "    ",
            };
            st.push_span(marker, None, None);
        }
        Tag::BlockQuote(_) => {
            st.blank();
            st.quote_depth += 1;
        }
        Tag::CodeBlock(kind) => {
            st.blank();
            if let CodeBlockKind::Fenced(lang) = kind {
                if !lang.is_empty() {
                    st.push_span(&format!("[{lang}]"), None, Some(LINK_URL));
                    st.flush();
                }
            }
            st.in_code_block = true;
        }
        Tag::List(first) => {
            if st.lists.is_empty() {
                st.blank();
            }
            st.lists.push(first);
        }
        Tag::Item => {
            st.flush();
            let depth = st.lists.len().saturating_sub(1);
            let bullet = match st.lists.last_mut() {
                Some(Some(n)) => {
                    let b = format!("{n}. ");
                    *n += 1;
                    b
                }
                _ => "• ".to_owned(),
            };
            st.push_span(&format!("{}{bullet}", "  ".repeat(depth)), None, None);
        }
        Tag::Emphasis => st.emphasis += 1,
        Tag::Strong => st.strong += 1,
        Tag::Strikethrough => {}
        Tag::Link { dest_url, .. } => st.link = Some(dest_url.to_string()),
        Tag::Image { dest_url, .. } => {
            st.push_span("[image: ", None, Some(LINK_URL));
            st.push_span(&dest_url, None, Some(LINK_URL));
            st.push_span("]", None, Some(LINK_URL));
        }
        Tag::Table(_) => st.blank(),
        Tag::TableHead | Tag::TableRow => st.table_row = Some(Vec::new()),
        Tag::TableCell => {
            st.in_table_cell = true;
            if let Some(row) = st.table_row.as_mut() {
                row.push(String::new());
            }
        }
        Tag::FootnoteDefinition(_)
        | Tag::DefinitionList
        | Tag::DefinitionListTitle
        | Tag::DefinitionListDefinition
        | Tag::HtmlBlock
        | Tag::MetadataBlock(_)
        | Tag::Superscript
        | Tag::Subscript => {}
    }
}

fn end(st: &mut State, tag: TagEnd) {
    match tag {
        TagEnd::Paragraph => {
            if st.lists.is_empty() {
                st.blank();
            } else {
                st.flush();
            }
        }
        TagEnd::Heading(_) => {
            // The whole heading line carries the title role.
            for s in &mut st.current {
                s.role = Some("title");
                s.fg = None;
            }
            st.blank();
        }
        TagEnd::BlockQuote(_) => {
            st.flush();
            st.quote_depth = st.quote_depth.saturating_sub(1);
            st.blank();
        }
        TagEnd::CodeBlock => {
            st.flush();
            st.in_code_block = false;
            st.blank();
        }
        TagEnd::List(_) => {
            st.flush();
            st.lists.pop();
            if st.lists.is_empty() {
                st.blank();
            }
        }
        TagEnd::Item => st.flush(),
        TagEnd::Emphasis => st.emphasis = st.emphasis.saturating_sub(1),
        TagEnd::Strong => st.strong = st.strong.saturating_sub(1),
        TagEnd::Link => {
            if let Some(url) = st.link.take() {
                st.push_span(&format!(" ({url})"), None, Some(LINK_URL));
            }
        }
        TagEnd::TableCell => st.in_table_cell = false,
        TagEnd::TableHead | TagEnd::TableRow => {
            if let Some(row) = st.table_row.take() {
                st.push_span(&row.join(" | "), None, None);
                st.flush();
            }
        }
        TagEnd::Table => st.blank(),
        TagEnd::Strikethrough
        | TagEnd::Image
        | TagEnd::FootnoteDefinition
        | TagEnd::DefinitionList
        | TagEnd::DefinitionListTitle
        | TagEnd::DefinitionListDefinition
        | TagEnd::HtmlBlock
        | TagEnd::MetadataBlock(_)
        | TagEnd::Superscript
        | TagEnd::Subscript => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(line: &Line) -> String {
        line.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn headings_carry_the_title_role_and_drop_the_hashes() {
        let out = render("# Title\n\nbody\n");
        assert_eq!(text_of(&out[0]), "Title");
        assert!(out[0].iter().all(|s| s.role == Some("title")));
        assert!(out[1].is_empty(), "a blank line after the heading");
        assert_eq!(text_of(&out[2]), "body");
    }

    #[test]
    fn code_blocks_drop_the_fences_and_keep_their_lines() {
        let out = render("```rust\nfn a() {}\nlet b = 1;\n```\n");
        assert_eq!(text_of(&out[0]), "[rust]");
        assert_eq!(text_of(&out[1]), "fn a() {}");
        assert_eq!(out[1][0].role, Some("info"));
        assert_eq!(text_of(&out[2]), "let b = 1;");
        assert!(!out.iter().any(|l| text_of(l).contains("```")));
    }

    #[test]
    fn lists_quotes_links_and_emphasis() {
        let out =
            render("- one\n- *two* and **three** with `c`\n\n> quoted\n\n[norte](https://x.y)\n");
        assert_eq!(text_of(&out[0]), "• one");
        let second = &out[1];
        assert_eq!(text_of(second), "• two and three with c");
        assert!(second
            .iter()
            .any(|s| s.text == "two" && s.fg == Some(EMPHASIS)));
        assert!(second
            .iter()
            .any(|s| s.text == "three" && s.fg == Some(STRONG)));
        assert!(second
            .iter()
            .any(|s| s.text == "c" && s.role == Some("info")));
        let quote = out.iter().find(|l| text_of(l).contains("quoted")).unwrap();
        assert_eq!(text_of(quote), "│ quoted");
        let link = out.iter().find(|l| text_of(l).contains("norte")).unwrap();
        assert_eq!(text_of(link), "norte (https://x.y)");
    }

    #[test]
    fn ordered_lists_count_and_tables_join_cells() {
        let out = render("1. a\n2. b\n\n| h1 | h2 |\n|---|---|\n| x | y |\n");
        assert_eq!(text_of(&out[0]), "1. a");
        assert_eq!(text_of(&out[1]), "2. b");
        let rows: Vec<String> = out
            .iter()
            .map(text_of)
            .filter(|t| t.contains('|'))
            .collect();
        assert_eq!(rows, vec!["h1 | h2", "x | y"]);
    }

    #[test]
    fn empty_input_is_no_lines() {
        assert!(render("").is_empty());
    }
}
