//! Markdown document module: a body string goes in, a tree of blocks and
//! inline runs comes out. Pure: no Iced, no SQLite.
//!
//! Matches the web `MarkdownRenderer` (CommonMark; hard breaks off, raw HTML
//! dropped). Extension points for later tickets: add variants to `Block` and
//! `Inline`, enable more `pulldown_cmark::Options` in `parse_options`, and
//! handle the new events in `Builder::event`.
//!
//! Containers use a block stack (`Builder::stack`). To add a container
//! (quote, ordered list, ...): add a `Frame` variant for it, open it in
//! `Builder::event` on its `Event::Start` with `open`, and close it on its
//! `Event::End` via `close`, which builds the `Block` and hands it to the
//! enclosing frame. Everything inside it (paragraphs, images, other
//! containers) lands in the frame's blocks through `push_block` with no
//! further work. Add the `Block` variant and one renderer arm too.

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
    /// A bulleted or numbered list.
    List(List),
    /// A `>` quote: any blocks, including lists and further quotes.
    Quote(Vec<Block>),
    /// A heading, level 1 to 6.
    Heading(u8, Vec<Inline>),
    /// A horizontal rule (`---`).
    Rule,
}

/// A list: each item is its own block sequence, so an item can hold
/// several paragraphs, images and nested lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    /// `Some(n)` for a numbered list whose first item is numbered `n` (the
    /// number the author wrote, so `3.` starts at 3); `None` for bullets.
    pub start: Option<u64>,
    pub items: Vec<Vec<Block>>,
    /// One entry per item: `Some(done)` for a task item (`- [ ]` / `- [x]`),
    /// `None` for a plain item. Same length as `items`.
    pub checked: Vec<Option<bool>>,
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
    /// A link with an openable target. Text is flat (no tokens inside).
    /// Links with any other scheme are not produced: their text stays plain.
    Link { text: String, url: String },
    /// `**strong**` text. Holds the inlines inside, so mentions and
    /// references in bold text stay tokens. Wrapper inlines for other styles
    /// (emphasis, strikethrough) nest the same way.
    Strong(Vec<Inline>),
}

/// Whether a link target may be opened: only `http`, `https` and `mailto`
/// (scheme case-insensitive). Everything else (`file:`, custom app schemes,
/// `javascript:`, relative paths) is inert.
pub fn is_openable_link(url: &str) -> bool {
    let url = url.trim();
    let Some((scheme, rest)) = url.split_once(':') else { return false };
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" => rest.starts_with("//") && rest.len() > 2,
        "mailto" => !rest.is_empty(),
        _ => false,
    }
}

/// Parser options: the web feature set. Hard breaks are never produced from
/// plain newlines, so nothing to switch off.
fn parse_options() -> Options {
    Options::ENABLE_TASKLISTS
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
    /// Open containers, innermost last. Empty means the document root.
    stack: Vec<Frame>,
    /// Finished top-level blocks.
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
    /// The link being read: (url, text so far).
    link: Option<(String, String)>,
    /// Open wrappers (strong), innermost last: the buffer each one wraps into
    /// and its constructor. The wrapper's own content is in `current`.
    wrappers: Vec<(Vec<Inline>, fn(Vec<Inline>) -> Inline)>,
    /// Level of the heading being read; its text flushes as `Block::Heading`.
    heading: Option<u8>,
}

/// An open container and what it has collected so far.
enum Frame {
    /// A list (start number if ordered); collects finished items.
    List(Option<u64>, Vec<Vec<Block>>, Vec<Option<bool>>),
    /// An item of a list.
    Item(Vec<Block>, Option<bool>),
    /// A container with no block of its own (an item outside any list):
    /// children are spliced into the parent when it closes.
    Flat(Vec<Block>),
    /// A block quote; becomes `Block::Quote`.
    Quote(Vec<Block>),
}

impl Builder {
    /// Open a container: text and blocks from here on belong to it.
    fn open(&mut self, frame: Frame) {
        self.flush();
        self.stack.push(frame);
    }

    /// Close the innermost container and give its result to the one around it.
    fn close(&mut self) {
        self.flush();
        match self.stack.pop() {
            Some(Frame::List(start, items, checked)) => {
                self.push_block(Block::List(List { start, items, checked }))
            }
            Some(Frame::Item(blocks, done)) => match self.stack.last_mut() {
                Some(Frame::List(_, items, checked)) => {
                    items.push(blocks);
                    checked.push(done);
                }
                _ => blocks.into_iter().for_each(|b| self.push_block(b)),
            },
            Some(Frame::Flat(blocks)) => blocks.into_iter().for_each(|b| self.push_block(b)),
            Some(Frame::Quote(blocks)) => self.push_block(Block::Quote(blocks)),
            None => {}
        }
    }

    /// Add a finished block to the innermost open container, or the root.
    fn push_block(&mut self, block: Block) {
        match self.stack.last_mut() {
            Some(Frame::Item(blocks, _) | Frame::Flat(blocks) | Frame::Quote(blocks)) => blocks.push(block),
            // A block directly in a list (never from the parser) is kept, not lost.
            Some(Frame::List(_, items, checked)) => {
                items.push(vec![block]);
                checked.push(None);
            }
            None => self.blocks.push(block),
        }
    }

    fn event(&mut self, ev: Event) {
        if let Some((_, text)) = &mut self.link {
            match ev {
                Event::Text(s) | Event::Code(s) => text.push_str(&s),
                Event::SoftBreak | Event::HardBreak => text.push(' '),
                Event::End(TagEnd::Link) => {
                    let (url, text) = self.link.take().unwrap_or_default();
                    self.literal_depth = self.literal_depth.saturating_sub(1);
                    self.flush_pending();
                    if is_openable_link(&url) {
                        self.current.push(Inline::Link { text, url });
                    } else {
                        self.push_plain(&text);
                    }
                }
                _ => {}
            }
            return;
        }
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
                    self.push_block(Block::Image(ImageBlock { alt, src, kind }));
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
            Event::Start(Tag::Link { dest_url, .. }) if self.html_depth == 0 => {
                self.flush_pending();
                self.literal_depth += 1;
                self.link = Some((dest_url.to_string(), String::new()));
            }
            Event::Start(Tag::Strong) if self.html_depth == 0 => {
                self.flush_pending();
                self.wrappers.push((std::mem::take(&mut self.current), Inline::Strong));
            }
            Event::End(TagEnd::Strong) => self.close_wrapper(),
            Event::Html(_) | Event::InlineHtml(_) => {}
            Event::Start(Tag::List(start)) => self.open(Frame::List(start, Vec::new(), Vec::new())),
            Event::Start(Tag::Item) => {
                let frame = match self.stack.last() {
                    Some(Frame::List(..)) => Frame::Item(Vec::new(), None),
                    _ => Frame::Flat(Vec::new()),
                };
                self.open(frame);
            }
            Event::End(TagEnd::List(_) | TagEnd::Item) => self.close(),
            Event::TaskListMarker(done) => {
                if let Some(Frame::Item(_, mark)) = self.stack.last_mut() {
                    *mark = Some(done);
                }
            }
            Event::Text(s) => self.push_text(&s),
            Event::Code(s) => self.push_literal(&s),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => {
                self.flush_pending();
                self.current.push(Inline::HardBreak);
            }
            Event::Start(Tag::Link { .. } | Tag::Image { .. }) => {
                self.flush_pending();
                self.literal_depth += 1;
            }
            // A block start ends the text of a tight list item before it.
            Event::Start(Tag::CodeBlock(_)) => {
                self.flush();
                self.literal_depth += 1;
            }
            Event::Start(Tag::Heading { level, .. }) => {
                self.flush();
                self.heading = Some(level as u8);
            }
            Event::End(TagEnd::Heading(_)) => {
                self.flush();
                self.heading = None;
            }
            Event::Rule => {
                self.flush();
                self.push_block(Block::Rule);
            }
            Event::Start(Tag::BlockQuote(_)) => self.open(Frame::Quote(Vec::new())),
            Event::End(TagEnd::BlockQuote(_)) => self.close(),
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

    /// End the innermost open wrapper: its content becomes one inline in the
    /// buffer it was opened from. An end with nothing open (its start was cut
    /// short by an image) does nothing.
    fn close_wrapper(&mut self) {
        self.flush_pending();
        let Some((outer, wrap)) = self.wrappers.pop() else { return };
        let inner = std::mem::replace(&mut self.current, outer);
        if !inner.is_empty() {
            self.current.push(wrap(inner));
        }
    }

    fn flush(&mut self) {
        self.flush_pending();
        // A block boundary inside a wrapper (an image) ends the wrapper.
        while !self.wrappers.is_empty() {
            self.close_wrapper();
        }
        if !self.current.is_empty() {
            let runs = std::mem::take(&mut self.current);
            self.push_block(match self.heading {
                Some(level) => Block::Heading(level, runs),
                None => Block::Paragraph(runs),
            });
        }
    }

    fn finish(mut self) -> Document {
        while !self.stack.is_empty() {
            self.close();
        }
        self.flush();
        Document { blocks: self.blocks }
    }
}

fn is_block_end(end: &TagEnd) -> bool {
    matches!(
        end,
        TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::CodeBlock
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

    fn heading(level: u8, runs: Vec<Inline>) -> Block {
        Block::Heading(level, runs)
    }

    #[test]
    fn parses_headings_and_rules() {
        let cases: Vec<(&str, &str, Vec<Block>)> = vec![
            ("h1", "# Title", vec![heading(1, vec![text("Title")])]),
            ("h6", "###### Small", vec![heading(6, vec![text("Small")])]),
            (
                "between paragraphs",
                "before\n\n## Mid\n\nafter",
                vec![para(vec![text("before")]), heading(2, vec![text("Mid")]), para(vec![text("after")])],
            ),
            (
                "inline styles kept",
                "### a **b**",
                vec![heading(3, vec![text("a "), Inline::Strong(vec![text("b")])])],
            ),
            ("not a heading without a space", "#nope", vec![para(vec![text("#nope")])]),
            ("heading in a quote", "> # Q", vec![Block::Quote(vec![heading(1, vec![text("Q")])])]),
            ("rule", "a\n\n---\n\nb", vec![para(vec![text("a")]), Block::Rule, para(vec![text("b")])]),
            ("star rule", "***", vec![Block::Rule]),
            ("rule in a quote", "> a\n>\n> ---", vec![Block::Quote(vec![para(vec![text("a")]), Block::Rule])]),
            (
                "text after a heading is a paragraph",
                "# H\ntext",
                vec![heading(1, vec![text("H")]), para(vec![text("text")])],
            ),
        ];
        for (name, body, want) in cases {
            assert_eq!(parse(body).blocks, want, "{name}");
        }
    }

    fn quote(blocks: Vec<Block>) -> Block {
        Block::Quote(blocks)
    }

    #[test]
    fn parses_blockquotes() {
        let cases: Vec<(&str, &str, Vec<Block>)> = vec![
            ("one line", "> hi", vec![quote(vec![para(vec![text("hi")])])]),
            (
                "lazy continuation reflows",
                "> a\nb",
                vec![quote(vec![para(vec![text("a b")])])],
            ),
            (
                "paragraphs inside",
                "> a\n>\n> b",
                vec![quote(vec![para(vec![text("a")]), para(vec![text("b")])])],
            ),
            (
                "text after a blank line leaves the quote",
                "> a\n\nb",
                vec![quote(vec![para(vec![text("a")])]), para(vec![text("b")])],
            ),
            (
                "nested quote",
                "> a\n>> b",
                vec![quote(vec![para(vec![text("a")]), quote(vec![para(vec![text("b")])])])],
            ),
            (
                "list inside",
                "> - a\n> - b",
                vec![quote(vec![list(vec![vec![para(vec![text("a")])], vec![para(vec![text("b")])]])])],
            ),
            (
                "quote then paragraph then quote",
                "> a\n\nb\n\n> c",
                vec![
                    quote(vec![para(vec![text("a")])]),
                    para(vec![text("b")]),
                    quote(vec![para(vec![text("c")])]),
                ],
            ),
            ("empty quote", ">", vec![quote(vec![])]),
        ];
        for (name, body, want) in cases {
            assert_eq!(parse(body).blocks, want, "{name}");
        }
    }

    fn strong(runs: Vec<Inline>) -> Inline {
        Inline::Strong(runs)
    }

    #[test]
    fn parses_strong_runs() {
        let cases: Vec<(&str, &str, Vec<Block>)> = vec![
            ("asterisks", "a **b** c", vec![para(vec![text("a "), strong(vec![text("b")]), text(" c")])]),
            ("underscores", "a __b__ c", vec![para(vec![text("a "), strong(vec![text("b")]), text(" c")])]),
            ("whole paragraph", "**all**", vec![para(vec![strong(vec![text("all")])])]),
            (
                "two runs",
                "**a** and **b**",
                vec![para(vec![strong(vec![text("a")]), text(" and "), strong(vec![text("b")])])],
            ),
            ("unclosed stays literal", "**oops", vec![para(vec![text("**oops")])]),
            ("empty marks stay literal", "a **** b", vec![para(vec![text("a **** b")])]),
            ("single star is not strong", "a *b* c", vec![para(vec![text("a b c")])]),
            (
                "tokens inside strong",
                "**ping @Ann on PROJ-7**",
                vec![para(vec![strong(vec![
                    text("ping "),
                    Inline::Mention("@Ann".into()),
                    text(" on "),
                    Inline::IssueRef { text: "PROJ-7".into(), prefix: "PROJ".into(), number: 7 },
                ])])],
            ),
            (
                "code inside strong stays one literal run",
                "**run `x` now**",
                vec![para(vec![strong(vec![text("run x now")])])],
            ),
            (
                "link inside strong",
                "**see [here](https://a.b)**",
                vec![para(vec![strong(vec![
                    text("see "),
                    Inline::Link { text: "here".into(), url: "https://a.b".into() },
                ])])],
            ),
            (
                "strong spans a soft break",
                "**one\ntwo**",
                vec![para(vec![strong(vec![text("one two")])])],
            ),
            (
                "strong in code is literal",
                "`**x**`",
                vec![para(vec![text("**x**")])],
            ),
            (
                "strong in a second paragraph",
                "one\n\n**two**",
                vec![para(vec![text("one")]), para(vec![strong(vec![text("two")])])],
            ),
        ];
        for (name, body, want) in cases {
            assert_eq!(parse_with(body, "", &["Ann"]).blocks, want, "{name}");
        }
    }

    fn list(items: Vec<Vec<Block>>) -> Block {
        let checked = vec![None; items.len()];
        Block::List(List { start: None, items, checked })
    }

    fn ordered(start: u64, items: Vec<Vec<Block>>) -> Block {
        let checked = vec![None; items.len()];
        Block::List(List { start: Some(start), items, checked })
    }

    fn tasks(marks: &[Option<bool>], items: Vec<Vec<Block>>) -> Block {
        Block::List(List { start: None, items, checked: marks.to_vec() })
    }

    #[test]
    fn parses_bulleted_lists() {
        let names = ["ann"];
        let cases: Vec<(&str, &str, Vec<Block>)> = vec![
            (
                "tight list",
                "- one\n- two\n- three",
                vec![list(vec![vec![para(vec![text("one")])], vec![para(vec![text("two")])], vec![para(vec![text("three")])]])],
            ),
            ("star and plus markers", "* a\n\n+ b", vec![
                list(vec![vec![para(vec![text("a")])]]),
                list(vec![vec![para(vec![text("b")])]]),
            ]),
            (
                "loose list keeps its items",
                "- one\n\n- two",
                vec![list(vec![vec![para(vec![text("one")])], vec![para(vec![text("two")])]])],
            ),
            (
                "long item reflows to one paragraph",
                "- one\n  continued",
                vec![list(vec![vec![para(vec![text("one continued")])]])],
            ),
            (
                "list between paragraphs",
                "before\n\n- a\n- b\n\nafter",
                vec![
                    para(vec![text("before")]),
                    list(vec![vec![para(vec![text("a")])], vec![para(vec![text("b")])]]),
                    para(vec![text("after")]),
                ],
            ),
            (
                "mentions and references inside items",
                "- hi @ann\n- see PROJ-3",
                vec![list(vec![
                    vec![para(vec![text("hi "), mention("@ann")])],
                    vec![para(vec![text("see "), issue("PROJ-3", "PROJ", 3)])],
                ])],
            ),
            (
                "inline code in an item stays literal",
                "- `PROJ-1`",
                vec![list(vec![vec![para(vec![text("PROJ-1")])]])],
            ),
            ("empty item", "- \n- b", vec![list(vec![vec![], vec![para(vec![text("b")])]])]),
            (
                "second paragraph in an item",
                "- a\n\n  b",
                vec![list(vec![vec![para(vec![text("a")]), para(vec![text("b")])]])],
            ),
            (
                "nested list lives in its item",
                "- a\n  - b",
                vec![list(vec![vec![para(vec![text("a")]), list(vec![vec![para(vec![text("b")])]])]])],
            ),
            (
                "ordered list starts at 1",
                "1. a\n2. b",
                vec![ordered(1, vec![vec![para(vec![text("a")])], vec![para(vec![text("b")])]])],
            ),
            (
                "ordered list keeps the written start number",
                "3. a\n4. b",
                vec![ordered(3, vec![vec![para(vec![text("a")])], vec![para(vec![text("b")])]])],
            ),
            (
                "paren delimiter is ordered too",
                "1) a",
                vec![ordered(1, vec![vec![para(vec![text("a")])]])],
            ),
            (
                "ordered start of zero",
                "0. a",
                vec![ordered(0, vec![vec![para(vec![text("a")])]])],
            ),
            (
                "ordered list after a paragraph",
                "intro\n\n1. a",
                vec![para(vec![text("intro")]), ordered(1, vec![vec![para(vec![text("a")])]])],
            ),
            (
                "bullets and numbers are separate lists",
                "- a\n\n1. b",
                vec![list(vec![vec![para(vec![text("a")])]]), ordered(1, vec![vec![para(vec![text("b")])]])],
            ),
            (
                "task items carry their state",
                "- [ ] todo\n- [x] done\n- plain",
                vec![tasks(&[Some(false), Some(true), None], vec![
                    vec![para(vec![text("todo")])],
                    vec![para(vec![text("done")])],
                    vec![para(vec![text("plain")])],
                ])],
            ),
            (
                "uppercase X is checked",
                "- [X] done",
                vec![tasks(&[Some(true)], vec![vec![para(vec![text("done")])]])],
            ),
            (
                "task marker needs a space after the bracket",
                "- [x]nope\n- [] nope",
                vec![list(vec![vec![para(vec![text("[x]nope")])], vec![para(vec![text("[] nope")])]])],
            ),
            (
                "task item nested in a plain item",
                "- a\n  - [x] b",
                vec![list(vec![vec![
                    para(vec![text("a")]),
                    tasks(&[Some(true)], vec![vec![para(vec![text("b")])]]),
                ]])],
            ),
            (
                "task marker in a numbered list",
                "1. [x] a",
                vec![Block::List(List {
                    start: Some(1),
                    items: vec![vec![para(vec![text("a")])]],
                    checked: vec![Some(true)],
                })],
            ),
            (
                "nested bullets three deep",
                "- a\n  - b\n    - c",
                vec![list(vec![vec![
                    para(vec![text("a")]),
                    list(vec![vec![para(vec![text("b")]), list(vec![vec![para(vec![text("c")])]])]]),
                ]])],
            ),
            (
                "numbered list nested in bullets",
                "- a\n  1. x\n  2. y",
                vec![list(vec![vec![
                    para(vec![text("a")]),
                    ordered(1, vec![vec![para(vec![text("x")])], vec![para(vec![text("y")])]]),
                ]])],
            ),
            (
                "bullets nested in a numbered list, siblings after",
                "1. a\n   - b\n2. c",
                vec![ordered(1, vec![
                    vec![para(vec![text("a")]), list(vec![vec![para(vec![text("b")])]])],
                    vec![para(vec![text("c")])],
                ])],
            ),
            (
                "dedent returns to the outer list",
                "- a\n  - b\n- c",
                vec![list(vec![
                    vec![para(vec![text("a")]), list(vec![vec![para(vec![text("b")])]])],
                    vec![para(vec![text("c")])],
                ])],
            ),
            (
                "quote inside an item",
                "- a\n  > q",
                vec![list(vec![vec![para(vec![text("a")]), quote(vec![para(vec![text("q")])])]])],
            ),
        ];
        for (name, body, want) in cases {
            assert_eq!(parse_with(body, "", &names).blocks, want, "{name}");
        }
    }

    #[test]
    fn strong_inside_a_list_item() {
        let want = vec![list(vec![vec![para(vec![text("a "), strong(vec![text("b")])])]])];
        assert_eq!(parse("- a **b**").blocks, want);
    }

    #[test]
    fn an_image_inside_strong_ends_the_run_without_losing_text() {
        let body = format!("**a ![x]({BLOB}) b**");
        let blocks = parse(&body).blocks;
        assert_eq!(blocks[0], para(vec![strong(vec![text("a ")])]));
        assert!(matches!(blocks[1], Block::Image(_)));
        assert_eq!(blocks[2], para(vec![text(" b")]));
    }

    #[test]
    fn image_inside_a_list_item_stays_in_the_item() {
        let body = format!("- see ![a]({BLOB}) now");
        let got = parse(&body).blocks;
        let want = vec![list(vec![vec![
            para(vec![text("see ")]),
            image("a", BLOB, ImageKind::Blob),
            para(vec![text(" now")]),
        ]])];
        assert_eq!(got, want);
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
                vec![Inline::Strong(vec![issue("PROJ-1", "PROJ", 1)]), text(" and "), mention("@ann")],
            ),
            ("inline code not tokenized", "`PROJ-1 @ann` x", vec![text("PROJ-1 @ann x")]),
            (
                "code between tokens",
                "PROJ-1 `PROJ-2` PROJ-3",
                vec![issue("PROJ-1", "PROJ", 1), text(" PROJ-2 "), issue("PROJ-3", "PROJ", 3)],
            ),
            (
                "link text not tokenized",
                "[PROJ-1 @ann](http://x.y)",
                vec![Inline::Link { text: "PROJ-1 @ann".into(), url: "http://x.y".into() }],
            ),
            (
                "autolink not tokenized",
                "<http://a.b/PROJ-1>",
                vec![Inline::Link { text: "http://a.b/PROJ-1".into(), url: "http://a.b/PROJ-1".into() }],
            ),
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

    #[test]
    fn classifies_openable_link_schemes() {
        let cases = [
            ("http://a.b", true),
            ("https://a.b/x?y=1#z", true),
            ("HTTPS://A.B", true),
            ("  https://a.b ", true),
            ("mailto:a@b.c", true),
            ("MAILTO:a@b.c", true),
            ("file:///etc/passwd", false),
            ("javascript:alert(1)", false),
            ("jait://issue/1", false),
            ("slack://open", false),
            ("ftp://a.b", false),
            ("data:text/html,x", false),
            ("http:", false),
            ("http:a.b", false),
            ("mailto:", false),
            ("/relative/path", false),
            ("page.html", false),
            ("", false),
        ];
        for (url, want) in cases {
            assert_eq!(is_openable_link(url), want, "{url}");
        }
    }

    #[test]
    fn parses_links() {
        let link = |t: &str, u: &str| Inline::Link { text: t.into(), url: u.into() };
        let cases: Vec<(&str, &str, Vec<Block>)> = vec![
            (
                "https link",
                "see [docs](https://a.b/x) now",
                vec![para(vec![text("see "), link("docs", "https://a.b/x"), text(" now")])],
            ),
            ("mailto link", "[me](mailto:a@b.c)", vec![para(vec![link("me", "mailto:a@b.c")])]),
            ("file link is inert text", "[x](file:///etc/hosts)", vec![para(vec![text("x")])]),
            ("custom scheme is inert text", "a [x](slack://open) b", vec![para(vec![text("a x b")])]),
            ("relative link is inert text", "[x](/foo)", vec![para(vec![text("x")])]),
            (
                "tokens in link text stay literal",
                "[PROJ-1 @Ann](http://x.y)",
                vec![para(vec![link("PROJ-1 @Ann", "http://x.y")])],
            ),
            (
                "tokens in inert link text stay literal",
                "[PROJ-1](file:///x)",
                vec![para(vec![text("PROJ-1")])],
            ),
            (
                "emphasis in link text is flattened",
                "[**bold** `code`](http://x.y)",
                vec![para(vec![link("bold code", "http://x.y")])],
            ),
            (
                "tokens around a link still tokenize",
                "PROJ-2 [a](http://x.y) PROJ-3",
                vec![para(vec![issue("PROJ-2", "PROJ", 2), text(" "), link("a", "http://x.y"), text(" "), issue("PROJ-3", "PROJ", 3)])],
            ),
            ("link in code stays text", "`[a](http://x.y)`", vec![para(vec![text("[a](http://x.y)")])]),
        ];
        for (name, body, want) in cases {
            assert_eq!(parse(body).blocks, want, "{name}");
        }
    }
}
