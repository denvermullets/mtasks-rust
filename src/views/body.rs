//! Segmented-body renderer: a description/comment body drawn as text runs
//! (mention and issue-reference highlighting) interleaved with inline images,
//! in the order they were written. Reused by issue descriptions (JRST-189),
//! comments (JRST-190) and project descriptions (JRST-191).
//!
//! Images load lazily: the App fetches blob URLs into the file cache and
//! records progress in the `ImageStore` (see `App::request_inline_images`).
//! While loading a fixed-size placeholder shows; with nothing cached, offline,
//! after a failure, or for a pending upload with no known outbox path, a card
//! with the alt text shows.

use std::collections::HashMap;
use std::path::PathBuf;

use iced::widget::{button, column, container, image, row, scrollable, span, text};
use iced::{Border, Color, ContentFit, Element, Theme};

use crate::app::Message;
use crate::body_cache::Parsed;
use crate::document::{self, Block};
use crate::highlight::Role;
use crate::db::models::{Issue, Member, Team};
use crate::files::images::{ImageState, ImageStore};
use crate::files::inline;
use crate::fonts;
use crate::document::Inline;
use crate::mentions::{self, body_segments, LinkTarget, BodySegment, ImageBlock, ImageKind};
use crate::theme as t;
use crate::views::hover_rich::hover_rich;

/// Widest an inline image is drawn.
const MAX_IMAGE_WIDTH: f32 = 560.0;
/// Placeholder size while an image loads.
const PLACEHOLDER: (f32, f32) = (240.0, 140.0);

/// A not-yet-uploaded inline image (pending or failed), found by placeholder uuid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInline {
    /// Local attachment id (target of Retry / Remove).
    pub id: String,
    /// Outbox copy of the pasted image.
    pub path: PathBuf,
    /// `Some(reason)` when the upload failed for good.
    pub failure: Option<String>,
}

/// Pending inline images by placeholder uuid.
pub type PendingInlineMap = HashMap<String, PendingInline>;

/// One image block in reading order. `key` is the file-cache/`ImageStore` key
/// (`None` for pending placeholders, which have no blob yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImage {
    pub key: Option<String>,
    pub src: String,
    pub alt: String,
}

impl InlineImage {
    /// `None` for foreign images, which are never fetched or paged.
    fn from_document(b: &document::ImageBlock) -> Option<Self> {
        let key = match b.kind {
            document::ImageKind::Blob => inline::blob_key(&b.src),
            document::ImageKind::Pending(_) => None,
            document::ImageKind::Foreign => return None,
        };
        Some(InlineImage { key, src: b.src.clone(), alt: b.alt.clone() })
    }

    fn from_block(b: &ImageBlock<'_>) -> Self {
        InlineImage {
            key: match b.kind {
                ImageKind::Blob => inline::blob_key(b.src),
                ImageKind::Pending(_) => None,
            },
            src: b.src.to_string(),
            alt: b.alt.to_string(),
        }
    }
}

/// Every image block of `body`, in reading order (JRST-193 lightbox paging).
pub fn image_list(body: &str, host: &str) -> Vec<InlineImage> {
    let mut out = Vec::new();
    collect_images(&crate::body_cache::parsed(body, host, &[]).doc.blocks, &mut out);
    out
}

/// Walk `blocks` depth first, descending into list items and quotes.
fn collect_images(blocks: &[Block], out: &mut Vec<InlineImage>) {
    for b in blocks {
        match b {
            Block::Image(img) => out.extend(InlineImage::from_document(img)),
            Block::List(list) => list.items.iter().for_each(|item| collect_images(item, out)),
            Block::Quote(inner) => collect_images(inner, out),
            Block::Table(table) => table
                .header
                .iter()
                .chain(table.rows.iter().flatten())
                .for_each(|cell| collect_images(cell, out)),
            Block::Paragraph(_) | Block::Heading(..) | Block::Rule | Block::Code(_) => {}
        }
    }
}

/// `src` of every image in ONE `body` that can be fetched (blob images; pending skipped).
pub fn fetchable_srcs_of_body(body: &str, host: &str) -> Vec<String> {
    image_list(body, host)
        .into_iter()
        .filter(|i| i.key.is_some())
        .map(|i| i.src)
        .collect()
}

/// [`fetchable_srcs_of_body`] across MANY `bodies`, in order.
pub fn fetchable_srcs_of_bodies<'b>(
    bodies: impl IntoIterator<Item = &'b str>,
    host: &str,
) -> Vec<String> {
    bodies
        .into_iter()
        .flat_map(|b| fetchable_srcs_of_body(b, host))
        .collect()
}

/// True when `body` has at least one image block.
pub fn has_images(body: &str, host: &str) -> bool {
    !image_list(body, host).is_empty()
}

/// Everything the renderer needs besides the body text itself.
pub struct BodyCtx<'a> {
    pub team: Option<&'a Team>,
    pub teams: &'a [Team],
    pub issues: &'a [Issue],
    pub members: &'a [Member],
    pub images: &'a ImageStore,
    /// Configured JAIT base URL (resolves blob-redirect URLs).
    pub host: &'a str,
    /// Outbox file per pending placeholder uuid. Pending uploads (JRST-197)
    /// fill this; a uuid with no entry renders as a card.
    /// `None` = no pending uploads known.
    pub pending_paths: Option<&'a PendingInlineMap>,
    /// Click on an image (lightbox, JRST-192). `None` = not clickable.
    pub on_image: Option<fn(InlineImage) -> Message>,
}

/// Render `body` by parsing it into a [`document::Document`] and drawing each
/// block. Mentions and references resolve against `ctx` here, at draw time.
///
/// Adding a block type: add the `Block` variant and parse it in `document.rs`,
/// then add one arm to [`block`]. Adding an inline: add the `Inline` variant,
/// one arm to [`runs`] (and a `Run` variant if it needs new styling), and one
/// arm to [`span_for`].
pub fn view<'a>(body: &str, size: f32, plain: Color, ctx: &BodyCtx<'a>) -> Element<'a, Message> {
    let names: Vec<&str> = ctx.members.iter().map(|m| m.name.as_str()).collect();
    let parsed = crate::body_cache::parsed(body, ctx.host, &names);
    let doc = &parsed.doc;
    let mut col = column![].spacing(t::space::S2);
    for (i, b) in doc.blocks.iter().enumerate() {
        col = col.push(match b {
            Block::Heading(level, inlines) if i == 0 => heading_block(*level, inlines, plain, ctx, true),
            _ => block(b, size, plain, ctx, &parsed),
        });
    }
    col.into()
}

/// Draw one block.
fn block<'a>(b: &Block, size: f32, plain: Color, ctx: &BodyCtx<'a>, parsed: &Parsed) -> Element<'a, Message> {
    match b {
        Block::Paragraph(inlines) => paragraph(inlines, size, plain, ctx),
        // Never fetched: an alt-text link that opens in the browser.
        Block::Image(img) if img.kind == document::ImageKind::Foreign => {
            paragraph(&[document::foreign_image_inline(img, ctx.host)], size, plain, ctx)
        }
        Block::Image(img) => image_block(img, ctx),
        Block::List(list) => list_block(list, size, plain, ctx, parsed),
        Block::Code(code) => code_block(parsed, code, size),
        Block::Quote(blocks) => quote_block(blocks, size, t::text_muted(), ctx, parsed),
        Block::Table(table) => table_block(table, size, plain, ctx, parsed),
        Block::Heading(level, inlines) => heading_block(*level, inlines, plain, ctx, false),
        Block::Rule => rule_block(size),
    }
}

/// Font size of a heading, as the web's `.markdown h1..h6` (1.5, 1.25, 1.125
/// and 1 rem). Absolute, like `rem`: it ignores the body size.
fn heading_size(level: u8) -> f32 {
    match level {
        1 => t::type_scale::XXL,
        2 => t::type_scale::XL,
        3 => t::type_scale::LG,
        _ => t::type_scale::BASE,
    }
}

/// Space above and below a heading beyond the column's `S2` gap. The web
/// gives `margin: 1.25em 0 0.5em`; the gap already supplies `S2`, and a
/// margin smaller than the gap adds nothing. `lead` is the first block of
/// the body, whose top margin is zero (`.markdown > *:first-child`).
fn heading_padding(size: f32, lead: bool) -> (f32, f32) {
    let extra = |margin: f32| (margin - t::space::S2).max(0.0);
    (if lead { 0.0 } else { extra(1.25 * size) }, extra(0.5 * size))
}

/// A heading: its runs drawn bold where the font has a bold face (`fonts::bold`
/// leaves other families regular).
fn heading_block<'a>(
    level: u8,
    inlines: &[Inline],
    plain: Color,
    ctx: &BodyCtx<'a>,
    lead: bool,
) -> Element<'a, Message> {
    let size = heading_size(level);
    let (top, bottom) = heading_padding(size, lead);
    let bold = [Inline::Strong(inlines.to_vec())];
    container(paragraph(&bold, size, plain, ctx))
        .padding(iced::Padding { top, bottom, ..Default::default() })
        .into()
}

/// Space above and below the web's `hr` (`margin: 1.5em 0`) beyond the column gap.
fn rule_margin(size: f32) -> f32 {
    (1.5 * size - t::space::S2).max(0.0)
}

/// A horizontal rule: one pixel in the border color across the column.
fn rule_block<'a>(size: f32) -> Element<'a, Message> {
    let line = container(iced::widget::Space::new(0, 0))
        .width(iced::Length::Fill)
        .height(1)
        .style(|_| container::Style { background: Some(t::border().into()), ..Default::default() });
    container(line)
        .padding(iced::Padding { top: rule_margin(size), bottom: rule_margin(size), ..Default::default() })
        .into()
}

/// A piece of code text and its color. The seam for syntax highlighting:
/// [`code_spans`] is the only place that decides colors.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CodeSpan {
    text: String,
    color: CodeColor,
}

/// A color role inside a code block, resolved to a real color at draw time.
/// Monokai, as on the web; the frame is the same dark on every theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodeColor {
    Plain,
    Comment,
    Keyword,
    Str,
    Number,
    Function,
    Type,
    Parameter,
}

impl CodeColor {
    fn color(self) -> Color {
        match self {
            CodeColor::Plain => t::CODE_BLOCK_TEXT,
            CodeColor::Comment => Color::from_rgb8(0x75, 0x71, 0x5e),
            CodeColor::Keyword => Color::from_rgb8(0xf9, 0x26, 0x72),
            CodeColor::Str => Color::from_rgb8(0xe6, 0xdb, 0x74),
            CodeColor::Number => Color::from_rgb8(0xae, 0x81, 0xff),
            CodeColor::Function => Color::from_rgb8(0xa6, 0xe2, 0x2e),
            CodeColor::Type => Color::from_rgb8(0x66, 0xd9, 0xef),
            CodeColor::Parameter => Color::from_rgb8(0xfd, 0x97, 0x1f),
        }
    }
}

impl From<Role> for CodeColor {
    fn from(r: Role) -> Self {
        match r {
            Role::Plain => CodeColor::Plain,
            Role::Comment => CodeColor::Comment,
            Role::Keyword => CodeColor::Keyword,
            Role::String => CodeColor::Str,
            Role::Number => CodeColor::Number,
            Role::Function => CodeColor::Function,
            Role::Type => CodeColor::Type,
            Role::Parameter => CodeColor::Parameter,
        }
    }
}

/// Split a code block into colored spans from the cached highlight. Concatenating
/// the span texts gives back `text` exactly; a block the cache doesn't know is plain.
fn code_spans(parsed: &Parsed, lang: Option<&str>, text: &str) -> Vec<CodeSpan> {
    match parsed.code_spans(lang, text) {
        Some(runs) => runs
            .iter()
            .map(|r| CodeSpan { text: r.text.clone(), color: r.role.into() })
            .collect(),
        None => vec![CodeSpan { text: text.to_string(), color: CodeColor::Plain }],
    }
}

/// A code block: monospace text in a fixed dark frame. Lines never wrap.
fn code_block<'a>(parsed: &Parsed, code: &document::CodeBlock, size: f32) -> Element<'a, Message> {
    let spans: Vec<text::Span<'a, Message, iced::Font>> = code_spans(parsed, code.lang.as_deref(), &code.text)
        .into_iter()
        .map(|s| span(s.text).color(s.color.color()))
        .collect();
    let body = iced::widget::rich_text(spans)
        .font(fonts::code())
        .size(size)
        .wrapping(text::Wrapping::None);
    // Long lines scroll sideways inside the frame. `Both` (not `Horizontal`)
    // on purpose: a horizontal-only scrollable turns the vertical wheel into
    // sideways motion and captures it, so the page would stop scrolling over
    // a code block. With `Both`, vertical wheel input goes to the vertical
    // axis, which never overflows (height shrinks to the text), so the event
    // is left uncaptured and the page scrolls. Sideways trackpad motion and
    // shift+wheel scroll the block. The padding is inside the scrollable so
    // the scrollbar sits in the bottom padding instead of over the text.
    let scroll = scrollable(container(body).padding(t::space::S3))
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::new().width(0).scroller_width(0),
            horizontal: scrollable::Scrollbar::new().width(6).scroller_width(6),
        })
        .style(|theme: &Theme, status| {
            let mut s = scrollable::default(theme, status);
            s.horizontal_rail.scroller.color = Color { a: 0.35, ..t::CODE_BLOCK_TEXT };
            s
        });
    container(scroll)
        .width(iced::Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::CODE_BLOCK_BG)),
            border: Border { radius: t::radius::MD.into(), ..Border::default() },
            ..Default::default()
        })
        .into()
}

/// A quote: a muted left rule beside the content column. Nested quotes get
/// their own rule, so they read as stacked bars. Callers pass the muted text
/// color (web: `.markdown blockquote { color: gray-400 }`, token `text_muted`).
fn quote_block<'a>(blocks: &[Block], size: f32, plain: Color, ctx: &BodyCtx<'a>, parsed: &Parsed) -> Element<'a, Message> {
    let mut content = column![].spacing(t::space::S2).width(iced::Length::Fill);
    for b in blocks {
        content = content.push(block(b, size, plain, ctx, parsed));
    }
    let rule = container(iced::widget::Space::new(0, 0))
        .width(3)
        .height(iced::Length::Fill)
        .style(|_| container::Style { background: Some(t::border().into()), ..Default::default() });
    row![rule, content].spacing(t::space::S3).height(iced::Length::Shrink).into()
}

/// A bordered grid of equal-width columns. The border is the table's
/// background showing through a 1px gap between cells, so adjacent cells share
/// one line. The header row is filled with the context-foreground token.
fn table_block<'a>(table: &document::Table, size: f32, plain: Color, ctx: &BodyCtx<'a>, parsed: &Parsed) -> Element<'a, Message> {
    let cells = |cells: &[Vec<Block>], header: bool| -> Element<'a, Message> {
        // Cells are `Fill` x `Fill` inside a `Shrink` row: the row is as tall
        // as its tallest wrapped cell and every cell stretches to it.
        let mut r = row![].spacing(1).height(iced::Length::Shrink);
        for cell in cells {
            let mut content = column![].spacing(t::space::S1);
            for b in cell {
                content = content.push(block(b, size, plain, ctx, parsed));
            }
            r = r.push(
                container(content)
                    .padding([t::space::S1, t::space::S2])
                    .width(iced::Length::Fill)
                    .height(iced::Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(if header { t::bg_card() } else { t::bg_primary() }.into()),
                        ..Default::default()
                    }),
            );
        }
        r.into()
    };
    let mut grid = column![].spacing(1);
    grid = grid.push(cells(&table.header, true));
    for r in &table.rows {
        grid = grid.push(cells(r, false));
    }
    container(grid)
        .padding(1)
        .width(iced::Length::Fill)
        .style(|_| container::Style { background: Some(t::border().into()), ..Default::default() })
        .into()
}

/// Marker for item `index` (0-based): a bullet, or the number counted from
/// the list's start.
fn list_marker(start: Option<u64>, index: usize) -> String {
    match start {
        Some(n) => format!("{}.", n + index as u64),
        None => "•".to_string(),
    }
}

/// Marker for a task item, replacing its bullet or number; `None` for a
/// plain item. Read-only: toggling a box is not wired.
fn task_marker(done: Option<bool>) -> Option<&'static str> {
    done.map(|d| if d { "☑" } else { "☐" })
}

/// Gutter holding the marker; item content sits in its own column beside it,
/// so wrapped lines hang under the first line rather than under the marker.
/// A numbered gutter is as wide as its widest label so items stay aligned.
fn list_block<'a>(list: &document::List, size: f32, plain: Color, ctx: &BodyCtx<'a>, parsed: &Parsed) -> Element<'a, Message> {
    let mut col = column![].spacing(t::space::S1);
    let labels: Vec<String> = (0..list.items.len()).map(|i| list_marker(list.start, i)).collect();
    let gutter = match list.start {
        Some(_) => labels.iter().map(|l| l.chars().count()).max().unwrap_or(1) as f32 * size * 0.65 + size * 0.5,
        None => size * 1.25,
    };
    for ((item, label), done) in list.items.iter().zip(labels).zip(&list.checked) {
        let mut content = column![].spacing(t::space::S1).width(iced::Length::Fill);
        for b in item {
            content = content.push(block(b, size, plain, ctx, parsed));
        }
        col = col.push(
            row![container(text(task_marker(*done).map(str::to_string).unwrap_or(label)).size(size).color(plain)).width(gutter), content]
                .spacing(t::space::S1),
        );
    }
    container(col).padding(iced::Padding { left: t::space::S2, ..Default::default() }).into()
}

/// A reflowing run of inlines.
fn paragraph<'a>(inlines: &[Inline], size: f32, plain: Color, ctx: &BodyCtx<'a>) -> Element<'a, Message> {
    let resolve = |token: &str| mentions::issue_link_target(ctx.issues, ctx.teams, token);
    let spans: Vec<text::Span<'a, LinkTarget, iced::Font>> = runs(inlines, resolve)
        .into_iter()
        .map(|r| span_for(r, plain))
        .collect();
    hover_rich(spans, Message::OpenBodyLink)
        .size(size)
        .on_hover(|owner, h| Message::BodyHover(owner, h))
        .into()
}

/// A styled stretch of text, before it becomes an Iced span. Pure, so the
/// inline-to-run mapping is testable without a widget tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Run {
    Plain(String),
    Mention(String),
    /// An issue reference; `target` is set when it resolves to a local issue.
    Reference { text: String, target: Option<LinkTarget> },
    /// An external link.
    Link { text: String, url: String },
    /// A run drawn bold. Wraps the run it styles, so it composes with
    /// other styles (a bold link, a bold mention).
    Bold(Box<Run>),
    /// A run drawn italic; wraps like `Bold`.
    Italic(Box<Run>),
    /// A run drawn with a line through it; wraps like `Bold`.
    Struck(Box<Run>),
    /// Inline code: monospace on a subtle background.
    Code(String),
}

/// Map inlines to runs. `resolve` turns an `IDENT-n` token into a link target.
fn runs(inlines: &[Inline], resolve: impl Fn(&str) -> Option<LinkTarget>) -> Vec<Run> {
    let mut out = Vec::new();
    push_runs(inlines, &resolve, &mut out);
    out
}

fn push_runs(inlines: &[Inline], resolve: &impl Fn(&str) -> Option<LinkTarget>, out: &mut Vec<Run>) {
    for i in inlines {
        match i {
            Inline::Text(s) => out.push(Run::Plain(s.clone())),
            Inline::HardBreak => out.push(Run::Plain("\n".to_string())),
            Inline::Mention(s) => out.push(Run::Mention(s.clone())),
            Inline::IssueRef { text, prefix, number } => out.push(Run::Reference {
                text: text.clone(),
                target: resolve(&format!("{prefix}-{number}")),
            }),
            Inline::Link { text, url } => out.push(Run::Link { text: text.clone(), url: url.clone() }),
            Inline::Code(s) => out.push(Run::Code(s.clone())),
            Inline::Strong(inner) => {
                let mut runs = Vec::new();
                push_runs(inner, resolve, &mut runs);
                out.extend(runs.into_iter().map(|r| Run::Bold(Box::new(r))));
            }
            Inline::Emphasis(inner) => {
                let mut runs = Vec::new();
                push_runs(inner, resolve, &mut runs);
                out.extend(runs.into_iter().map(|r| Run::Italic(Box::new(r))));
            }
            Inline::Strikethrough(inner) => {
                let mut runs = Vec::new();
                push_runs(inner, resolve, &mut runs);
                out.extend(runs.into_iter().map(|r| Run::Struck(Box::new(r))));
            }
        }
    }
}

fn span_for<'a>(r: Run, plain: Color) -> text::Span<'a, LinkTarget, iced::Font> {
    match r {
        Run::Plain(s) => span(s).color(plain),
        // Tagged so a mention hover card (JRST-310) can identify the span.
        Run::Mention(s) => span(s.clone()).color(t::highlight()).link(LinkTarget::Mention(s)),
        Run::Reference { text, target: Some(target) } => {
            span(text).color(t::accent()).underline(true).link(target)
        }
        Run::Reference { text, target: None } => span(text).color(t::accent()),
        Run::Link { text, url } => span(text).color(t::accent()).link(LinkTarget::External(url)),
        Run::Code(s) => span(s)
            .color(plain)
            .font(fonts::code())
            .background(t::bg_card())
            .border(Border { radius: t::radius::SM.into(), ..Border::default() })
            .padding(iced::Padding { top: 0.0, bottom: 0.0, left: t::space::S1, right: t::space::S1 }),
        Run::Bold(inner) => {
            let mut s = span_for(*inner, plain);
            s.font = Some(fonts::bold(s.font.unwrap_or_else(fonts::active)));
            s
        }
        Run::Italic(inner) => {
            let mut s = span_for(*inner, plain);
            s.font = Some(fonts::italic(s.font.unwrap_or_else(fonts::active)));
            s
        }
        Run::Struck(inner) => span_for(*inner, plain).strikethrough(true),
    }
}

/// The document image in the shape the image helpers below take.
fn legacy(img: &document::ImageBlock) -> ImageBlock<'_> {
    ImageBlock {
        alt: &img.alt,
        src: &img.src,
        kind: match &img.kind {
            document::ImageKind::Blob => ImageKind::Blob,
            document::ImageKind::Pending(id) => ImageKind::Pending(id),
            // Not fetchable: an empty pending id has no path, so it draws as the alt card.
            document::ImageKind::Foreign => ImageKind::Pending(""),
        },
    }
}

/// Where the image's bytes are on disk, if they are.
fn image_path(block: &ImageBlock<'_>, ctx: &BodyCtx<'_>) -> Option<PathBuf> {
    match block.kind {
        ImageKind::Pending(uuid) => ctx.pending_paths
            .and_then(|m| m.get(uuid))
            .map(|p| &p.path)
            .filter(|p| p.exists())
            .cloned(),
        ImageKind::Blob => inline::blob_key(block.src)
            .and_then(|k| ctx.images.path(&k).map(|p| p.to_path_buf())),
    }
}

fn image_block<'a>(img: &document::ImageBlock, ctx: &BodyCtx<'a>) -> Element<'a, Message> {
    let block = &legacy(img);
    let loading = matches!(block.kind, ImageKind::Blob)
        && inline::blob_key(block.src)
            .and_then(|k| ctx.images.state(&k).cloned())
            .is_some_and(|s| s == ImageState::Loading);
    let el: Element<'a, Message> = match image_path(block, ctx) {
        Some(path) => container(image(path).content_fit(ContentFit::ScaleDown))
            .max_width(MAX_IMAGE_WIDTH)
            .into(),
        None if loading => placeholder(),
        None => alt_card(block.alt),
    };
    let el: Element<'a, Message> = match ctx.on_image {
        Some(on_image) => button(el)
            .padding(0)
            .style(|_theme: &Theme, _status| button::Style::default())
            .on_press(on_image(InlineImage::from_block(block)))
            .into(),
        None => el,
    };
    match failed_inline(block, ctx) {
        Some((id, reason)) => column![el, failed_row(id, reason)].spacing(4).into(),
        None => el,
    }
}

/// The failed upload behind a pending-placeholder block: (attachment id, reason).
fn failed_inline<'a>(block: &ImageBlock<'_>, ctx: &BodyCtx<'a>) -> Option<(String, String)> {
    let ImageKind::Pending(uuid) = block.kind else { return None };
    let p = ctx.pending_paths?.get(uuid)?;
    Some((p.id.clone(), p.failure.clone()?))
}

/// "Upload failed: reason" with the two ways out.
fn failed_row<'a>(id: String, reason: String) -> Element<'a, Message> {
    row![]
        .spacing(8)
        .push(text(format!("Upload failed: {reason}")).size(11).color(t::danger()))
        .push(
            button(text("Retry").size(11).color(t::accent()))
                .on_press(Message::RetryUpload(id.clone()))
                .padding(0)
                .style(button::text),
        )
        .push(
            button(text("Remove").size(11).color(t::text_muted()))
                .on_press(Message::RemoveFailedUpload(id))
                .padding(0)
                .style(button::text),
        )
        .into()
}

fn placeholder<'a>() -> Element<'a, Message> {
    container(text("Loading image…").size(12).color(t::text_muted()))
        .width(PLACEHOLDER.0)
        .height(PLACEHOLDER.1)
        .center_x(PLACEHOLDER.0)
        .center_y(PLACEHOLDER.1)
        .style(frame_style)
        .into()
}

/// Offline, failed or pending-without-file: a card carrying the alt text.
fn alt_card<'a>(alt: &str) -> Element<'a, Message> {
    let label = if alt.trim().is_empty() { "Image" } else { alt };
    container(text(format!("🖼  {label}")).size(12).color(t::text_secondary()))
        .padding([8, 12])
        .style(frame_style)
        .into()
}

fn frame_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(t::bg_secondary())),
        border: Border {
            color: t::border(),
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "http://localhost:3005";

    #[test]
    fn images_inside_table_cells_are_listed_in_reading_order() {
        let a = "/rails/active_storage/blobs/redirect/aaa/a.png";
        let b = "/rails/active_storage/blobs/redirect/bbb/b.png";
        let body = format!("| h ![a]({a}) |\n|---|\n| ![b]({b}) |");
        let alts: Vec<String> = image_list(&body, HOST).into_iter().map(|i| i.alt).collect();
        assert_eq!(alts, ["a", "b"]);
        assert_eq!(fetchable_srcs_of_body(&body, HOST), [a, b]);
    }

    #[test]
    fn heading_sizes_and_spacing_follow_the_web_stylesheet() {
        // (level, size in px, top, bottom) from `.markdown h1..h6`; extra
        // space is margin minus the S2 column gap, never negative.
        let cases = [
            (1u8, 24.0, 22.0, 4.0),
            (2, 20.0, 17.0, 2.0),
            (3, 18.0, 14.5, 1.0),
            (4, 16.0, 12.0, 0.0),
            (6, 16.0, 12.0, 0.0),
        ];
        for (level, size, top, bottom) in cases {
            assert_eq!(heading_size(level), size, "size h{level}");
            assert_eq!(heading_padding(size, false), (top, bottom), "padding h{level}");
        }
        assert_eq!(heading_padding(24.0, true), (0.0, 4.0), "first block has no top margin");
    }

    #[test]
    fn rule_margin_follows_the_web_stylesheet() {
        assert_eq!(rule_margin(16.0), 16.0);
        assert_eq!(rule_margin(14.0), 13.0);
        assert_eq!(rule_margin(4.0), 0.0);
    }

    #[test]
    fn images_inside_lists_and_quotes_are_listed_in_reading_order() {
        let a = "/rails/active_storage/blobs/redirect/aaa/a.png";
        let b = "/rails/active_storage/blobs/redirect/bbb/b.png";
        let c = "/rails/active_storage/blobs/redirect/ccc/c.png";
        let d = "/rails/active_storage/blobs/redirect/ddd/d.png";
        let body = format!(
            "![a]({a})\n\n- item ![b]({b})\n  - nested ![c]({c})\n\n> quoted ![d]({d})\n>\n> > ![e](https://x.test/e.png)"
        );
        let alts: Vec<String> = image_list(&body, HOST).into_iter().map(|i| i.alt).collect();
        assert_eq!(alts, ["a", "b", "c", "d"]);
        assert_eq!(fetchable_srcs_of_body(&body, HOST), [a, b, c, d]);
        assert!(has_images(&format!("> ![q]({a})"), HOST));
        assert!(has_images(&format!("- ![q]({a})"), HOST));
        assert!(!has_images("> - plain", HOST));
    }

    #[test]
    fn task_markers_show_state() {
        assert_eq!(task_marker(None), None);
        assert_eq!(task_marker(Some(false)), Some("☐"));
        assert_eq!(task_marker(Some(true)), Some("☑"));
    }

    #[test]
    fn list_markers_count_from_the_start() {
        let cases = [
            (None, 2, "•"),
            (Some(1), 0, "1."),
            (Some(1), 9, "10."),
            (Some(3), 1, "4."),
            (Some(0), 0, "0."),
        ];
        for (start, index, want) in cases {
            assert_eq!(list_marker(start, index), want, "{start:?} {index}");
        }
    }

    fn resolve(token: &str) -> Option<LinkTarget> {
        (token == "PROJ-7").then(|| LinkTarget::Issue("id-7".into()))
    }

    #[test]
    fn inlines_map_to_runs_resolving_references_at_draw_time() {
        let doc = document::parse_with("hi @Ann see PROJ-7 and OTH-1\nnext", "", &["Ann"]);
        let Block::Paragraph(inl) = &doc.blocks[0] else { panic!("paragraph") };
        assert_eq!(
            runs(inl, resolve),
            [
                Run::Plain("hi ".into()),
                Run::Mention("@Ann".into()),
                Run::Plain(" see ".into()),
                Run::Reference { text: "PROJ-7".into(), target: Some(LinkTarget::Issue("id-7".into())) },
                Run::Plain(" and ".into()),
                Run::Reference { text: "OTH-1".into(), target: None },
                Run::Plain(" next".into()),
            ]
        );
    }

    #[test]
    fn strong_inlines_map_to_bold_runs_keeping_their_tokens() {
        let doc = document::parse_with("a **b @Ann [l](https://a.b)** c", "", &["Ann"]);
        let Block::Paragraph(inl) = &doc.blocks[0] else { panic!("paragraph") };
        let bold = |r| Run::Bold(Box::new(r));
        assert_eq!(
            runs(inl, resolve),
            [
                Run::Plain("a ".into()),
                bold(Run::Plain("b ".into())),
                bold(Run::Mention("@Ann".into())),
                bold(Run::Plain(" ".into())),
                bold(Run::Link { text: "l".into(), url: "https://a.b".into() }),
                Run::Plain(" c".into()),
            ]
        );
    }

    #[test]
    fn emphasis_maps_to_italic_runs() {
        let doc = document::parse_with("*a* ***c***", "", &[]);
        let Block::Paragraph(inl) = &doc.blocks[0] else { panic!("paragraph") };
        let it = |r| Run::Italic(Box::new(r));
        let bold = |r| Run::Bold(Box::new(r));
        assert_eq!(
            runs(inl, resolve),
            [it(Run::Plain("a".into())), Run::Plain(" ".into()), it(bold(Run::Plain("c".into())))]
        );
    }

    #[test]
    fn italic_spans_use_the_italic_style_and_compose_with_bold() {
        let s = span_for(Run::Italic(Box::new(Run::Plain("x".into()))), Color::WHITE);
        assert_eq!(s.font.map(|f| f.style), Some(iced::font::Style::Italic));
        assert_eq!(s.color, Some(Color::WHITE));
        let both = span_for(Run::Italic(Box::new(Run::Bold(Box::new(Run::Plain("x".into()))))), Color::WHITE);
        let f = both.font.unwrap();
        assert_eq!((f.style, f.weight), (iced::font::Style::Italic, iced::font::Weight::Bold));
    }

    #[test]
    fn strikethrough_maps_to_struck_runs_keeping_their_tokens() {
        let doc = document::parse_with("~~a @Ann~~ ~~*b*~~", "", &["Ann"]);
        let Block::Paragraph(inl) = &doc.blocks[0] else { panic!("paragraph") };
        let st = |r| Run::Struck(Box::new(r));
        assert_eq!(
            runs(inl, resolve),
            [
                st(Run::Plain("a ".into())),
                st(Run::Mention("@Ann".into())),
                Run::Plain(" ".into()),
                st(Run::Italic(Box::new(Run::Plain("b".into())))),
            ]
        );
    }

    #[test]
    fn struck_spans_set_strikethrough_and_keep_other_styling() {
        let s = span_for(Run::Struck(Box::new(Run::Plain("x".into()))), Color::WHITE);
        assert!(s.strikethrough);
        assert_eq!(s.color, Some(Color::WHITE));
        let link = span_for(
            Run::Struck(Box::new(Run::Link { text: "l".into(), url: "https://a.b".into() })),
            Color::WHITE,
        );
        assert!(link.strikethrough && link.link.is_some());
        assert!(!span_for(Run::Plain("x".into()), Color::WHITE).strikethrough);
    }

    #[test]
    fn bold_spans_use_the_bold_weight_and_keep_their_color() {
        let s = span_for(Run::Bold(Box::new(Run::Plain("x".into()))), Color::WHITE);
        assert_eq!(s.font.map(|f| f.weight), Some(iced::font::Weight::Bold));
        assert_eq!(s.color, Some(Color::WHITE));
        let plain = span_for(Run::Plain("x".into()), Color::WHITE);
        assert_eq!(plain.font, None);
    }

    #[test]
    fn inline_code_maps_to_code_runs_without_tokens() {
        let doc = document::parse_with("see `PROJ-7 @Ann` and PROJ-7", "", &["Ann"]);
        let Block::Paragraph(inl) = &doc.blocks[0] else { panic!("paragraph") };
        assert_eq!(
            runs(inl, resolve),
            [
                Run::Plain("see ".into()),
                Run::Code("PROJ-7 @Ann".into()),
                Run::Plain(" and ".into()),
                Run::Reference { text: "PROJ-7".into(), target: Some(LinkTarget::Issue("id-7".into())) },
            ]
        );
    }

    #[test]
    fn code_spans_use_the_code_font_on_the_card_background() {
        let s = span_for(Run::Code("x".into()), Color::WHITE);
        assert_eq!(s.font, Some(fonts::code()));
        assert_eq!(s.highlight.map(|h| h.background), Some(iced::Background::Color(t::bg_card())));
    }

    #[test]
    fn code_spans_keep_the_text_exactly() {
        for text in ["", "a", "if a {\n    b();\n\n\tc();\n}", "ABC-12 @Ann ![x](y)"] {
            let joined: String = code_spans(&crate::body_cache::parsed("", "", &[]), Some("rust"), text).into_iter().map(|s| s.text).collect();
            assert_eq!(joined, text);
        }
    }

    #[test]
    fn code_frame_is_dark_with_legible_text_on_every_theme() {
        // Relative luminance, sRGB approximated by the channel mean.
        let lum = |c: Color| (c.r + c.g + c.b) / 3.0;
        assert!(lum(t::CODE_BLOCK_BG) < 0.2);
        assert!(lum(CodeColor::Plain.color()) > 0.7);
    }

    #[test]
    fn links_map_to_link_runs() {
        let doc = document::parse_with("go [here](https://a.b) or [no](file:///x)", "", &[]);
        let Block::Paragraph(inl) = &doc.blocks[0] else { panic!("paragraph") };
        assert_eq!(
            runs(inl, resolve),
            [
                Run::Plain("go ".into()),
                Run::Link { text: "here".into(), url: "https://a.b".into() },
                Run::Plain(" or no".into()),
            ]
        );
    }

    #[test]
    fn image_list_is_in_reading_order_and_skips_text_and_foreign_urls() {
        let body = "a ![one](/rails/active_storage/blobs/redirect/AA--1/one.png) b \
                    ![x](https://evil.example/x.png) \
                    ![two](http://localhost:3005/rails/active_storage/blobs/redirect/BB--2/two.png)";
        let list = image_list(body, HOST);
        assert_eq!(
            list.iter().map(|i| i.alt.as_str()).collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(list[0].key.as_deref(), Some("inline-AA--1"));
        assert_eq!(list[1].key.as_deref(), Some("inline-BB--2"));
    }

    #[test]
    fn images_in_code_are_never_listed_fetched_or_counted() {
        let blob = "![a](/rails/active_storage/blobs/redirect/AA--1/a.png)";
        let body = format!("```\n{blob}\n```\n\n`{blob}`");
        assert!(image_list(&body, HOST).is_empty());
        assert!(fetchable_srcs_of_body(&body, HOST).is_empty());
        assert!(!has_images(&body, HOST));
        let mixed = format!("```\n{blob}\n```\n\n![b](/rails/active_storage/blobs/redirect/BB--2/b.png)");
        assert_eq!(image_list(&mixed, HOST).len(), 1);
        assert!(has_images(&mixed, HOST));
    }

    #[test]
    fn fetchable_srcs_of_body_lists_only_blob_images() {
        let body = "![a](/rails/active_storage/blobs/redirect/AA--1/a.png) \
                    ![p](jait-pending://123e4567-e89b-12d3-a456-426614174000)";
        assert_eq!(
            fetchable_srcs_of_body(body, HOST),
            ["/rails/active_storage/blobs/redirect/AA--1/a.png"]
        );
    }

    #[test]
    fn fetchable_srcs_of_bodies_collects_fetchable_images_across_bodies() {
        let a = "![one](/rails/active_storage/blobs/redirect/AA--1/one.png)";
        let b = "text ![p](jait-pending://123e4567-e89b-12d3-a456-426614174000) \
                 ![two](/rails/active_storage/blobs/redirect/BB--2/two.png)";
        let srcs = fetchable_srcs_of_bodies([a, b, "none"], HOST);
        assert_eq!(
            srcs,
            [
                "/rails/active_storage/blobs/redirect/AA--1/one.png",
                "/rails/active_storage/blobs/redirect/BB--2/two.png"
            ]
        );
    }

    #[test]
    fn pending_placeholders_are_listed_without_a_key() {
        let body = "![p](jait-pending://123e4567-e89b-12d3-a456-426614174000)";
        let list = image_list(body, HOST);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].key, None);
        assert!(has_images(body, HOST));
        assert!(!has_images("plain text", HOST));
    }
}
