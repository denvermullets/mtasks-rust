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
    Attachment(Attachment),
    Inline { key: String, filename: String },
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

    /// Open on the Attachment with local id `att_id`, among the image
    /// Attachments in `group` (in order). `None` if it isn't an image there.
    pub fn for_attachment(group: &[Attachment], att_id: &str) -> Option<Self> {
        let images: Vec<LightboxImage> = group
            .iter()
            .filter(|a| is_thumbnailable(&a.content_type))
            .cloned()
            .map(LightboxImage::Attachment)
            .collect();
        let index = images
            .iter()
            .position(|i| matches!(i, LightboxImage::Attachment(a) if a.id == att_id))?;
        Self::new(images, index)
    }

    pub fn current(&self) -> &LightboxImage {
        &self.images[self.index]
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
            owner_kind: "issue".into(),
            owner_id: "i1".into(),
            blob_id: Some(70),
            filename: format!("{id}.bin"),
            content_type: content_type.into(),
            byte_size: 10,
            checksum: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            upload_state: UploadState::Synced,
            failure_reason: None,
        }
    }

    #[test]
    fn opens_on_the_clicked_image_among_images_only() {
        let group = [att("a", "application/pdf"), att("b", "image/png"), att("c", "image/jpeg")];
        let s = LightboxState::for_attachment(&group, "c").unwrap();
        assert_eq!(s.images.len(), 2);
        assert_eq!(s.index, 1);
        assert_eq!(s.current().filename(), "c.bin");
    }

    #[test]
    fn non_image_or_unknown_attachment_does_not_open() {
        let group = [att("a", "application/pdf"), att("b", "image/png")];
        assert!(LightboxState::for_attachment(&group, "a").is_none());
        assert!(LightboxState::for_attachment(&group, "zzz").is_none());
    }

    #[test]
    fn empty_list_does_not_open_and_index_clamps() {
        assert!(LightboxState::new(vec![], 0).is_none());
        let imgs = vec![LightboxImage::Inline { key: "k".into(), filename: "x.png".into() }];
        assert_eq!(LightboxState::new(imgs, 5).unwrap().index, 0);
    }

    #[test]
    fn keys_come_from_the_blob_for_attachments_and_verbatim_for_inline() {
        let a = LightboxImage::Attachment(att("a", "image/png"));
        assert_eq!(a.key().as_deref(), Some("blob-70"));
        let i = LightboxImage::Inline { key: "inline-k".into(), filename: "p.png".into() };
        assert_eq!(i.key().as_deref(), Some("inline-k"));
        assert_eq!(i.filename(), "p.png");
    }
}
