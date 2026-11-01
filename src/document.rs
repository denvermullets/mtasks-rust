//! Markdown document module: a body string goes in, a tree of blocks and
//! inline runs comes out. Pure: no Iced, no SQLite.
//!
//! Matches the web `MarkdownRenderer` (CommonMark; hard breaks off, raw HTML
//! dropped). Extension points for later tickets: add variants to `Block` and
//! `Inline`, enable more `pulldown_cmark::Options` in `parse_options`, and
//! handle the new events in `Builder::event`.

use crate::mentions::{classify_image_src, find_issue_tokens, find_mentions, ImageKind as MentionKind};
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
    /// An `@Name` mention, `@` included. Only produced for known member names.
    Mention(String),
    /// An `IDENT-123` reference (any prefix; the consumer resolves the team).
    /// `text` is the matched source, including a leading `#` when present.
    IssueRef { text: String, prefix: String, number: i64 },
}

/// Parser options: the web feature set. Hard breaks are never produced from
/// plain newlines, so nothing to switch off.
fn parse_options() -> Options {
    Options::empty()
}

/// Parse `body` into a [`Document`]. Never panics; input that isn't valid
/// markdown stays readable as text.
pub fn parse(body: &str) -> Document {
    parse_with(body, "", &[])
}

/// Full entry point: `host` is the configured JAIT base URL (absolute blob
/// URLs on it are recognised as images), `member_names` are the names whose
/// `@Name` mentions get marked. Tokens are never made inside inline code,
/// links or image alt text.
pub fn parse_with(body: &str, host: &str, member_names: &[&str]) -> Document {
    let mut b = Builder {
        host: host.to_string(),
        names: member_names.iter().map(|n| n.to_string()).collect(),
        ..Builder::default()
    };
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
    /// Member names to mark as mentions.
    names: Vec<String>,
    /// Text not yet tokenized. Kept whole because the parser can split a run
    /// at characters like `_`; it is tokenized when the run ends.
    pending: String,
    /// Depth inside links and images, whose text stays literal.
    literal_depth: usize,
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
            Event::Text(s) => self.push_text(&s),
            Event::Code(s) => self.push_literal(&s),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => {
                self.flush_pending();
                self.current.push(Inline::HardBreak);
            }
            Event::Start(Tag::Link { .. } | Tag::Image { .. } | Tag::CodeBlock(_)) => {
                self.flush_pending();
                self.literal_depth += 1;
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                self.literal_depth = self.literal_depth.saturating_sub(1);
            }
            Event::Start(Tag::HtmlBlock) => self.html_depth += 1,
            Event::End(TagEnd::HtmlBlock) => self.html_depth = self.html_depth.saturating_sub(1),
            Event::End(end) if is_block_end(&end) => {
                if end == TagEnd::CodeBlock {
                    self.literal_depth = self.literal_depth.saturating_sub(1);
                }
                self.flush();
            }
            _ => {}
        }
    }

    fn push_text(&mut self, s: &str) {
        if self.literal_depth > 0 {
            self.push_literal(s);
        } else if self.html_depth == 0 {
            self.pending.push_str(s);
        }
    }

    /// Text that is never tokenized (code, link text).
    fn push_literal(&mut self, s: &str) {
        if self.html_depth > 0 {
            return;
        }
        self.flush_pending();
        self.push_plain(s);
    }

    /// Tokenize the pending run into mentions, references and plain text.
    fn flush_pending(&mut self) {
        let text = std::mem::take(&mut self.pending);
        let names: Vec<&str> = self.names.iter().map(String::as_str).collect();
        // (start, end, token) spans; the earliest start wins an overlap.
        let mut spans: Vec<(usize, usize, Inline)> = Vec::new();
        for t in find_issue_tokens(&text) {
            let src = text[t.start..t.end].to_string();
            spans.push((t.start, t.end, Inline::IssueRef { text: src, prefix: t.prefix, number: t.number }));
        }
        for m in find_mentions(&text, &names) {
            spans.push((m.start, m.end, Inline::Mention(text[m.start..m.end].to_string())));
        }
        spans.sort_by_key(|s| s.0);
        let mut pos = 0;
        for (start, end, token) in spans {
            if start < pos {
                continue;
            }
            self.push_plain(&text[pos..start]);
            self.current.push(token);
            pos = end;
        }
        self.push_plain(&text[pos..]);
    }

    fn push_plain(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if let Some(Inline::Text(last)) = self.current.last_mut() {
            last.push_str(s);
        } else {
            self.current.push(Inline::Text(s.to_string()));
        }
    }

    fn flush(&mut self) {
        self.flush_pending();
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
            assert_eq!(parse_with(&body, HOST, &[]).blocks, want, "{name}");
        }
    }

    fn mention(s: &str) -> Inline {
        Inline::Mention(s.to_string())
    }
    fn issue(text: &str, prefix: &str, number: i64) -> Inline {
        Inline::IssueRef { text: text.into(), prefix: prefix.into(), number }
    }

    #[test]
    fn marks_mentions_and_issue_references() {
        let names = ["ann", "john_smith", "Ann Lee"];
        let cases: Vec<(&str, &str, Vec<Inline>)> = vec![
            ("mention", "hi @ann!", vec![text("hi "), mention("@ann"), text("!")]),
            (
                "underscore name found whole",
                "ping @john_smith now",
                vec![text("ping "), mention("@john_smith"), text(" now")],
            ),
            (
                "underscore name inside underscore emphasis",
                "ping _@john_smith_ now",
                vec![text("ping "), mention("@john_smith"), text(" now")],
            ),
            ("longest name wins", "@Ann Lee ok", vec![mention("@Ann Lee"), text(" ok")]),
            ("unknown member stays text", "@bob", vec![text("@bob")]),
            (
                "issue reference",
                "see PROJ-12 and JRS-3.",
                vec![text("see "), issue("PROJ-12", "PROJ", 12), text(" and "), issue("JRS-3", "JRS", 3), text(".")],
            ),
            ("hash prefix kept in text", "#ABC-7", vec![issue("#ABC-7", "ABC", 7)]),
            (
                "inside emphasis",
                "**PROJ-1** and *@ann*",
                vec![issue("PROJ-1", "PROJ", 1), text(" and "), mention("@ann")],
            ),
            ("inline code not tokenized", "`PROJ-1 @ann` x", vec![text("PROJ-1 @ann x")]),
            (
                "code between tokens",
                "PROJ-1 `PROJ-2` PROJ-3",
                vec![issue("PROJ-1", "PROJ", 1), text(" PROJ-2 "), issue("PROJ-3", "PROJ", 3)],
            ),
            ("link text not tokenized", "[PROJ-1 @ann](http://x.y)", vec![text("PROJ-1 @ann")]),
            ("autolink not tokenized", "<http://a.b/PROJ-1>", vec![text("http://a.b/PROJ-1")]),
            ("code block not tokenized", "```\nPROJ-1 @ann\n```", vec![text("PROJ-1 @ann\n")]),
        ];
        for (name, body, want) in cases {
            assert_eq!(parse_with(body, "", &names).blocks, vec![para(want)], "{name}");
        }
        assert_eq!(parse("@ann PROJ-1").blocks, vec![para(vec![text("@ann "), issue("PROJ-1", "PROJ", 1)])]);
    }

    #[test]
    fn mention_image_reference_keeps_tokens_around_the_image() {
        let body = format!("hi @ann ![see PROJ-9 @ann]({BLOB}) then PROJ-2");
        let got = parse_with(&body, HOST, &["ann"]).blocks;
        let want = vec![
            para(vec![text("hi "), mention("@ann"), text(" ")]),
            image("see PROJ-9 @ann", BLOB, ImageKind::Blob),
            para(vec![text(" then "), issue("PROJ-2", "PROJ", 2)]),
        ];
        assert_eq!(got, want);
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
