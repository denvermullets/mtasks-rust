//! Markdown document module: a body string goes in, a tree of blocks and
//! inline runs comes out. Pure: no Iced, no SQLite.
//!
//! Matches the web `MarkdownRenderer` (CommonMark; hard breaks off, raw HTML
//! dropped). Extension points for later tickets: add variants to `Block` and
//! `Inline`, enable more `pulldown_cmark::Options` in `parse_options`, and
//! handle the new events in `Builder::event`.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// A parsed body.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Document {
    pub blocks: Vec<Block>,
}

/// A block-level element. Containers (lists, quotes, tables) arrive in later
/// tickets; until then their text content surfaces as paragraphs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Vec<Inline>),
}

/// An inline run inside a block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    /// Plain text. Adjacent text is merged; a soft break (single newline)
    /// reflows to one space.
    Text(String),
    /// An explicit hard break (backslash or two trailing spaces).
    HardBreak,
}

/// Parser options: the web feature set. Hard breaks are never produced from
/// plain newlines, so nothing to switch off.
fn parse_options() -> Options {
    Options::empty()
}

/// Parse `body` into a [`Document`]. Never panics; input that isn't valid
/// markdown stays readable as text.
pub fn parse(body: &str) -> Document {
    let mut b = Builder::default();
    for ev in Parser::new_ext(body, parse_options()) {
        b.event(ev);
    }
    b.finish()
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    current: Vec<Inline>,
    /// Depth inside raw HTML blocks (their text events are dropped).
    html_depth: usize,
}

impl Builder {
    fn event(&mut self, ev: Event) {
        match ev {
            Event::Html(_) | Event::InlineHtml(_) => {}
            Event::Text(s) | Event::Code(s) => self.push_text(&s),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.current.push(Inline::HardBreak),
            Event::Start(Tag::HtmlBlock) => self.html_depth += 1,
            Event::End(TagEnd::HtmlBlock) => self.html_depth = self.html_depth.saturating_sub(1),
            Event::End(end) if is_block_end(&end) => self.flush(),
            _ => {}
        }
    }

    fn push_text(&mut self, s: &str) {
        if self.html_depth > 0 || s.is_empty() {
            return;
        }
        if let Some(Inline::Text(last)) = self.current.last_mut() {
            last.push_str(s);
        } else {
            self.current.push(Inline::Text(s.to_string()));
        }
    }

    fn flush(&mut self) {
        if !self.current.is_empty() {
            self.blocks.push(Block::Paragraph(std::mem::take(&mut self.current)));
        }
    }

    fn finish(mut self) -> Document {
        self.flush();
        Document { blocks: self.blocks }
    }
}

fn is_block_end(end: &TagEnd) -> bool {
    matches!(
        end,
        TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::BlockQuote(_)
            | TagEnd::CodeBlock
            | TagEnd::Item
            | TagEnd::TableCell
            | TagEnd::TableRow
            | TagEnd::TableHead
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        Inline::Text(s.to_string())
    }
    fn para(runs: Vec<Inline>) -> Block {
        Block::Paragraph(runs)
    }

    #[test]
    fn parses_bodies_into_blocks() {
        let cases: Vec<(&str, &str, Vec<Block>)> = vec![
            ("empty", "", vec![]),
            ("whitespace only", "  \n\n \n", vec![]),
            ("one paragraph", "hello world", vec![para(vec![text("hello world")])]),
            (
                "blank line separates paragraphs",
                "one\n\ntwo",
                vec![para(vec![text("one")]), para(vec![text("two")])],
            ),
            (
                "many blank lines still one separation",
                "one\n\n\n\ntwo",
                vec![para(vec![text("one")]), para(vec![text("two")])],
            ),
            (
                "single newline reflows",
                "one\ntwo\nthree",
                vec![para(vec![text("one two three")])],
            ),
            (
                "crlf newline reflows",
                "one\r\ntwo\r\n\r\nthree",
                vec![para(vec![text("one two")]), para(vec![text("three")])],
            ),
            (
                "explicit backslash break kept",
                "one\\\ntwo",
                vec![para(vec![text("one"), Inline::HardBreak, text("two")])],
            ),
            (
                "inline html dropped, text kept",
                "a <b>bold</b> word",
                vec![para(vec![text("a bold word")])],
            ),
            (
                "html block dropped",
                "before\n\n<div>\nsecret\n</div>\n\nafter",
                vec![para(vec![text("before")]), para(vec![text("after")])],
            ),
            (
                "html comment dropped",
                "a <!-- hidden --> b",
                vec![para(vec![text("a  b")])],
            ),
        ];
        for (name, body, want) in cases {
            assert_eq!(parse(body).blocks, want, "{name}");
        }
    }

    #[test]
    fn malformed_input_never_panics_and_stays_readable() {
        let cases = [
            "**unclosed bold",
            "[broken](link",
            "![img",
            "```\nunclosed fence",
            "<div",
            "<<<>>>",
            "| a | b\n|--",
            "\u{0}\u{1}\u{fffd}",
            "> > > > deep",
            "- - - - -",
            "# ",
            "&bogus; &#xFFFFFFFF;",
        ];
        for body in cases {
            let _ = parse(body);
        }
        assert_eq!(parse("**unclosed bold").blocks, vec![para(vec![text("**unclosed bold")])]);
        assert_eq!(parse("[broken](link").blocks, vec![para(vec![text("[broken](link")])]);
    }
}
