//! Markdown document module: a body string goes in, a tree of blocks and
//! inline runs comes out. Pure: no Iced, no SQLite.
//!
//! Matches the web `MarkdownRenderer` (CommonMark; hard breaks off, raw HTML
//! dropped). Extension points for later tickets: add variants to `Block` and
//! `Inline`, enable more `pulldown_cmark::Options` in `parse_options`, and
//! handle the new events in `Builder::event`.

use crate::mentions::{classify_image_src, ImageKind as MentionKind};
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
    /// An image on its own line in reading order. An image inside a paragraph
    /// splits it into paragraph, image, paragraph.
    Image(ImageBlock),
}

/// `![alt](src)` with its allowlist verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBlock {
    pub alt: String,
    pub src: String,
    pub kind: ImageKind,
}

/// Whether an image may load. Only JAIT blob URLs and `jait-pending://`
/// placeholders do; anything else is never fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageKind {
    /// JAIT blob-redirect URL (relative or on the configured host).
    Blob,
    /// Local placeholder; carries the uuid.
    Pending(String),
    /// Anything else: not fetched, shown as an alt-text link.
    Foreign,
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
    parse_with_host(body, "")
}

/// Like [`parse`], with the configured JAIT base URL so absolute blob URLs on
/// that host are recognised.
pub fn parse_with_host(body: &str, host: &str) -> Document {
    let mut b = Builder { host: host.to_string(), ..Builder::default() };
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
    host: String,
    /// The image being read: (src, alt so far).
    image: Option<(String, String)>,
}

impl Builder {
    fn event(&mut self, ev: Event) {
        if let Some((src, alt)) = &mut self.image {
            match ev {
                Event::Text(s) | Event::Code(s) => alt.push_str(&s),
                Event::SoftBreak | Event::HardBreak => alt.push(' '),
                Event::End(TagEnd::Image) => {
                    let (src, alt) = (std::mem::take(src), std::mem::take(alt));
                    self.image = None;
                    self.flush();
                    let kind = match classify_image_src(&src, &self.host) {
                        Some(MentionKind::Blob) => ImageKind::Blob,
                        Some(MentionKind::Pending(id)) => ImageKind::Pending(id.to_string()),
                        None => ImageKind::Foreign,
                    };
                    self.blocks.push(Block::Image(ImageBlock { alt, src, kind }));
                }
                _ => {}
            }
            return;
        }
        match ev {
            Event::Start(Tag::Image { dest_url, .. }) if self.html_depth == 0 => {
                self.flush();
                self.image = Some((dest_url.to_string(), String::new()));
            }
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

    const HOST: &str = "https://jait.example.com";
    const BLOB: &str = "/rails/active_storage/blobs/redirect/abc123/shot.png";
    const UUID: &str = "123e4567-e89b-12d3-a456-426614174000";

    fn image(alt: &str, src: &str, kind: ImageKind) -> Block {
        Block::Image(ImageBlock { alt: alt.to_string(), src: src.to_string(), kind })
    }

    #[test]
    fn parses_inline_images() {
        let pending = format!("jait-pending://{UUID}");
        let abs = format!("{HOST}{BLOB}");
        let paren = "/rails/active_storage/blobs/redirect/abc/shot (1).png";
        let paren2 = "/rails/active_storage/blobs/redirect/abc/shot(1).png";
        let cases: Vec<(&str, String, Vec<Block>)> = vec![
            ("lone blob image", format!("![shot]({BLOB})"), vec![image("shot", BLOB, ImageKind::Blob)]),
            (
                "image splits a paragraph into text, image, text",
                format!("before ![a]({BLOB}) after"),
                vec![para(vec![text("before ")]), image("a", BLOB, ImageKind::Blob), para(vec![text(" after")])],
            ),
            (
                "adjacent images in order",
                format!("![a]({BLOB})![b]({pending})"),
                vec![image("a", BLOB, ImageKind::Blob), image("b", &pending, ImageKind::Pending(UUID.to_string()))],
            ),
            (
                "images across paragraphs keep reading order",
                format!("![a]({BLOB})\n\ntext\n\n![b]({pending})"),
                vec![
                    image("a", BLOB, ImageKind::Blob),
                    para(vec![text("text")]),
                    image("b", &pending, ImageKind::Pending(UUID.to_string())),
                ],
            ),
            ("absolute blob url on host", format!("![a]({abs})"), vec![image("a", &abs, ImageKind::Blob)]),
            (
                "foreign host blob url is foreign",
                format!("![a](https://evil.example.org{BLOB})"),
                vec![image("a", &format!("https://evil.example.org{BLOB}"), ImageKind::Foreign)],
            ),
            (
                "non-jait url is foreign",
                "![cat](https://example.com/cat.png)".to_string(),
                vec![image("cat", "https://example.com/cat.png", ImageKind::Foreign)],
            ),
            (
                "pending with bad uuid is foreign",
                "![a](jait-pending://nope)".to_string(),
                vec![image("a", "jait-pending://nope", ImageKind::Foreign)],
            ),
            ("empty alt", format!("![]({BLOB})"), vec![image("", BLOB, ImageKind::Blob)]),
            (
                "filename with space and parens",
                format!("![a](<{paren}>)"),
                vec![image("a", paren, ImageKind::Blob)],
            ),
            (
                "filename with unescaped parens",
                format!("![a]({paren2})"),
                vec![image("a", paren2, ImageKind::Blob)],
            ),
            (
                "fenced code is not an image",
                format!("```\n![a]({BLOB})\n```"),
                vec![para(vec![text(&format!("![a]({BLOB})\n"))])],
            ),
            (
                "inline code is not an image",
                format!("see `![a]({BLOB})` here"),
                vec![para(vec![text(&format!("see ![a]({BLOB}) here"))])],
            ),
            (
                "indented code is not an image",
                format!("    ![a]({BLOB})"),
                vec![para(vec![text(&format!("![a]({BLOB})"))])],
            ),
            (
                "broken image syntax stays text",
                "![alt](".to_string(),
                vec![para(vec![text("![alt](")])],
            ),
        ];
        for (name, body, want) in cases {
            assert_eq!(parse_with_host(&body, HOST).blocks, want, "{name}");
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
