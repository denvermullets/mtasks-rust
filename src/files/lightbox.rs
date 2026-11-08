//! State for the image lightbox: an ordered list of images plus the one
//! being shown. Pure data so stepping and list-building are unit-testable;
//! the modal view and app messages live in `app.rs` / `views::lightbox`.
//!
//! The list is deliberately general: later tickets append inline images
//! (description blobs) to it and step through it in reading order.

use crate::db::attachments::Attachment;
use crate::files::images::is_thumbnailable;

/// One entry in the lightbox: an image Attachment, or an inline image whose
/// bytes are cached under `key` (see `files::images::ImageStore`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LightboxImage {
    Attachment(Box<Attachment>),
    Inline { key: String, src: String, filename: String },
}

impl LightboxImage {
    /// `ImageStore` key for this image; `None` for an Attachment that was
    /// never uploaded.
    pub fn key(&self) -> Option<String> {
        match self {
            LightboxImage::Attachment(a) => crate::files::open::blob_key(a),
            LightboxImage::Inline { key, .. } => Some(key.clone()),
        }
    }

    pub fn filename(&self) -> &str {
        match self {
            LightboxImage::Attachment(a) => &a.filename,
            LightboxImage::Inline { filename, .. } => filename,
        }
    }
}

/// One comment's contribution to the reading-order list.
pub struct CommentImages<'a> {
    pub body: &'a str,
    pub attachments: &'a [Attachment],
}

/// Every lightbox-able image of an issue or project, in reading order:
/// the description's inline images, the description-level image attachments,
/// then for each comment (caller passes them in thread order) its inline
/// images followed by its image attachments. Pending inline placeholders
/// (no blob yet) and non-image attachments are skipped. Pure; shared by issue
/// and project detail.
pub fn reading_order_images(
    description: &str,
    attachments: &[Attachment],
    comments: &[CommentImages<'_>],
    host: &str,
) -> Vec<LightboxImage> {
    let mut out = Vec::new();
    let mut push_group = |body: &str, atts: &[Attachment]| {
        out.extend(crate::views::body::image_list(body, host).into_iter().filter_map(|i| {
            let key = i.key?;
            let filename = if i.alt.trim().is_empty() {
                i.src.rsplit('/').next().unwrap_or("image").to_string()
            } else {
                i.alt
            };
            Some(LightboxImage::Inline { key, src: i.src, filename })
        }));
        out.extend(
            atts.iter()
                .filter(|a| is_thumbnailable(&a.content_type))
                .map(|a| LightboxImage::Attachment(Box::new(a.clone()))),
        );
    };
    push_group(description, attachments);
    for c in comments {
        push_group(c.body, c.attachments);
    }
    out
}

/// `Modal::Lightbox` payload: `images[index]` is on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightboxState {
    pub images: Vec<LightboxImage>,
    pub index: usize,
}

impl LightboxState {
    /// Open on `images[index]`; `None` when the list is empty. An
    /// out-of-range index clamps to the last image.
    pub fn new(images: Vec<LightboxImage>, index: usize) -> Option<Self> {
        if images.is_empty() {
            return None;
        }
        let index = index.min(images.len() - 1);
        Some(Self { images, index })
    }

    /// Open on the first entry matching `pick`, `None` if none does.
    pub fn at(images: Vec<LightboxImage>, pick: impl Fn(&LightboxImage) -> bool) -> Option<Self> {
        let index = images.iter().position(pick)?;
        Self::new(images, index)
    }

    /// Move `delta` images forward (negative = back), stopping at the ends.
    pub fn step(&mut self, delta: isize) {
        let last = self.images.len() as isize - 1;
        self.index = (self.index as isize + delta).clamp(0, last) as usize;
    }

    /// "3 of 7"; `None` when there is only one image.
    pub fn position_label(&self) -> Option<String> {
        (self.images.len() > 1).then(|| format!("{} of {}", self.index + 1, self.images.len()))
    }

    pub fn current(&self) -> &LightboxImage {
        &self.images[self.index]
    }

    /// `(blob key, filename, cached path)` of the current image, for handing
    /// to the OS. `None` until the image has loaded into `images`.
    pub fn open_target(
        &self,
        images: &crate::files::images::ImageStore,
    ) -> Option<(String, &str, std::path::PathBuf)> {
        let cur = self.current();
        let key = cur.key()?;
        let path = images.path(&key)?.to_path_buf();
        Some((key, cur.filename(), path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::attachments::UploadState;

    fn att(id: &str, content_type: &str) -> Attachment {
        Attachment {
            id: id.into(),
            remote_id: Some(7),
            owner_kind: crate::db::attachments::OwnerKind::Issue,
            owner_id: "i1".into(),
            blob_id: Some(70),
            filename: format!("{id}.bin"),
            content_type: content_type.into(),
            byte_size: 10,
            checksum: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            upload_state: UploadState::Synced,
            failure_reason: None,
            outbox_path: None,
            upload_kind: crate::files::validate::UploadKind::Attachment,
            placeholder_uuid: None,
            signed_id: None,
            blob_filename: None,
        }
    }

    #[test]
    fn open_target_needs_a_loaded_image_for_either_variant() {
        use crate::files::images::ImageStore;
        let mut store = ImageStore::default();
        let a = LightboxState::new(vec![LightboxImage::Attachment(Box::new(att("a", "image/png")))], 0).unwrap();
        let i = LightboxState::new(
            vec![LightboxImage::Inline { key: "inline-k".into(), src: "s".into(), filename: "p.png".into() }],
            0,
        )
        .unwrap();
        assert!(a.open_target(&store).is_none());
        assert!(i.open_target(&store).is_none());
        store.set_ready("blob-70", "/c/blob-70".into());
        store.set_ready("inline-k", "/c/inline-k".into());
        let (k, name, path) = a.open_target(&store).unwrap();
        assert_eq!((k.as_str(), name, path.to_str()), ("blob-70", "a.bin", Some("/c/blob-70")));
        let (k, name, path) = i.open_target(&store).unwrap();
        assert_eq!((k.as_str(), name, path.to_str()), ("inline-k", "p.png", Some("/c/inline-k")));
    }

    #[test]
    fn opens_on_the_clicked_image_among_images_only() {
        let group = [att("a", "application/pdf"), att("b", "image/png"), att("c", "image/jpeg")];
        let s = LightboxState::at(reading_order_images("", &group, &[], ""), |i| {
            matches!(i, LightboxImage::Attachment(a) if a.id == "c")
        })
        .unwrap();
        assert_eq!(s.images.len(), 2);
        assert_eq!(s.index, 1);
        assert_eq!(s.current().filename(), "c.bin");
    }

    #[test]
    fn non_image_or_unknown_attachment_does_not_open() {
        let group = [att("a", "application/pdf"), att("b", "image/png")];
        let pick = |id: &'static str| {
            LightboxState::at(reading_order_images("", &group, &[], ""), move |i| {
                matches!(i, LightboxImage::Attachment(a) if a.id == id)
            })
        };
        assert!(pick("a").is_none());
        assert!(pick("zzz").is_none());
    }

    #[test]
    fn empty_list_does_not_open_and_index_clamps() {
        assert!(LightboxState::new(vec![], 0).is_none());
        let imgs = vec![LightboxImage::Inline { key: "k".into(), src: "s".into(), filename: "x.png".into() }];
        assert_eq!(LightboxState::new(imgs, 5).unwrap().index, 0);
    }

    #[test]
    fn keys_come_from_the_blob_for_attachments_and_verbatim_for_inline() {
        let a = LightboxImage::Attachment(Box::new(att("a", "image/png")));
        assert_eq!(a.key().as_deref(), Some("blob-70"));
        let i = LightboxImage::Inline { key: "inline-k".into(), src: "s".into(), filename: "p.png".into() };
        assert_eq!(i.key().as_deref(), Some("inline-k"));
        assert_eq!(i.filename(), "p.png");
    }

    #[test]
    fn step_clamps_at_both_ends_and_labels_position() {
        let imgs: Vec<_> = ["a", "b", "c"]
            .iter()
            .map(|k| LightboxImage::Inline { key: (*k).into(), src: "s".into(), filename: "f".into() })
            .collect();
        let mut s = LightboxState::new(imgs, 1).unwrap();
        assert_eq!(s.position_label().as_deref(), Some("2 of 3"));
        s.step(1);
        s.step(1);
        assert_eq!(s.index, 2);
        s.step(-5);
        assert_eq!(s.index, 0);
        let one = LightboxState::new(vec![LightboxImage::Inline { key: "a".into(), src: "s".into(), filename: "f".into() }], 0).unwrap();
        assert!(one.position_label().is_none());
    }

    #[test]
    fn reading_order_is_description_then_comments_inline_before_attachments() {
        const HOST: &str = "http://localhost:3005";
        let img = |n: &str| format!("![{n}](/rails/active_storage/blobs/redirect/{n}--1/{n}.png)");
        let desc = format!("{} text {}", img("d1"), "![p](jait-pending://abc)");
        let da = [att("da", "image/png"), att("pdf", "application/pdf")];
        let c1 = [att("c1a", "image/png")];
        let b1 = img("c1i");
        let b2 = img("c2i");
        let comments = [
            CommentImages { body: &b1, attachments: &c1 },
            CommentImages { body: &b2, attachments: &[] },
        ];
        let list = reading_order_images(&desc, &da, &comments, HOST);
        let names: Vec<_> = list.iter().map(|i| i.filename().to_string()).collect();
        assert_eq!(names, ["d1", "da.bin", "c1i", "c1a.bin", "c2i"]);
    }
}
