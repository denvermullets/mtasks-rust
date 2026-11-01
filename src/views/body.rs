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

use iced::widget::{button, column, container, image, row, span, text};
use iced::{Border, Color, ContentFit, Element, Theme};

use crate::app::Message;
use crate::document::{self, Block};
use crate::db::models::{Issue, Member, Team};
use crate::files::images::{ImageState, ImageStore};
use crate::files::inline;
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
    document::parse_with(body, host, &[])
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Image(img) => InlineImage::from_document(img),
            _ => None,
        })
        .collect()
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
    let doc = document::parse_with(body, ctx.host, &names);
    let mut col = column![].spacing(t::space::S2);
    for b in &doc.blocks {
        col = col.push(block(b, size, plain, ctx));
    }
    col.into()
}

/// True when the body has anything beyond plain text (a token or an image).
pub fn has_rich_content(body: &str, host: &str, member_names: &[&str]) -> bool {
    let doc = document::parse_with(body, host, member_names);
    doc.blocks.iter().any(|b| match b {
        Block::Paragraph(inlines) => inlines
            .iter()
            .any(|i| !matches!(i, Inline::Text(_) | Inline::HardBreak)),
        _ => true,
    })
}

/// Draw one block.
fn block<'a>(b: &Block, size: f32, plain: Color, ctx: &BodyCtx<'a>) -> Element<'a, Message> {
    match b {
        Block::Paragraph(inlines) => paragraph(inlines, size, plain, ctx),
        Block::Image(img) => image_block(img, ctx),
        Block::List(list) => bullet_list(list, size, plain, ctx),
    }
}

/// Gutter holding the bullet; item content sits in its own column beside it,
/// so wrapped lines hang under the first line rather than under the bullet.
fn bullet_list<'a>(list: &document::List, size: f32, plain: Color, ctx: &BodyCtx<'a>) -> Element<'a, Message> {
    let mut col = column![].spacing(t::space::S1);
    for item in &list.items {
        let mut content = column![].spacing(t::space::S1).width(iced::Length::Fill);
        for b in item {
            content = content.push(block(b, size, plain, ctx));
        }
        col = col.push(
            row![container(text("•").size(size).color(plain)).width(size * 1.25), content]
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
}

/// Map inlines to runs. `resolve` turns an `IDENT-n` token into a link target.
fn runs(inlines: &[Inline], resolve: impl Fn(&str) -> Option<LinkTarget>) -> Vec<Run> {
    inlines
        .iter()
        .map(|i| match i {
            Inline::Text(s) => Run::Plain(s.clone()),
            Inline::HardBreak => Run::Plain("\n".to_string()),
            Inline::Mention(s) => Run::Mention(s.clone()),
            Inline::IssueRef { text, prefix, number } => Run::Reference {
                text: text.clone(),
                target: resolve(&format!("{prefix}-{number}")),
            },
            Inline::Link { text, url } => Run::Link { text: text.clone(), url: url.clone() },
        })
        .collect()
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
    fn rich_content_means_tokens_or_images_and_raw_html_is_gone() {
        for (body, want) in [
            ("plain\ntext", false),
            ("<b>hi</b>", false),
            ("ping @Ann", true),
            ("see PROJ-7", true),
            ("![a](https://x.test/a.png)", true),
        ] {
            assert_eq!(has_rich_content(body, "", &["Ann"]), want, "{body}");
        }
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
