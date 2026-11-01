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

use iced::widget::{button, column, container, image, row, text};
use iced::{Border, Color, ContentFit, Element, Theme};

use crate::app::Message;
use crate::db::models::{Issue, Member, Team};
use crate::files::images::{ImageState, ImageStore};
use crate::files::inline;
use crate::mentions::{body_segments, BodySegment, ImageBlock, ImageKind};
use crate::theme as t;

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
    body_segments(body, host)
        .iter()
        .filter_map(|s| match s {
            BodySegment::Image(b) => Some(InlineImage::from_block(b)),
            BodySegment::Text(_) => None,
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
    body_segments(body, host)
        .iter()
        .any(|s| matches!(s, BodySegment::Image(_)))
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

/// Render `body`: text runs via `issue_detail::rich_body`, images as blocks.
pub fn view<'a>(body: &str, size: f32, plain: Color, ctx: &BodyCtx<'a>) -> Element<'a, Message> {
    let mut col = column![].spacing(8);
    for seg in body_segments(body, ctx.host) {
        match seg {
            BodySegment::Text(run) => {
                let run = run.trim_matches('\n');
                if run.trim().is_empty() {
                    continue;
                }
                col = col.push(crate::views::issue_detail::rich_body(
                    run,
                    size,
                    plain,
                    ctx.team,
                    ctx.teams,
                    ctx.issues,
                    ctx.members,
                ));
            }
            BodySegment::Image(block) => col = col.push(image_block(&block, ctx)),
        }
    }
    col.into()
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

fn image_block<'a>(block: &ImageBlock<'_>, ctx: &BodyCtx<'a>) -> Element<'a, Message> {
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
