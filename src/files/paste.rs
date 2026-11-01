//! Paste an image from the clipboard into a body editor. The clipboard read is
//! native (arboard); everything after it is pure and shared by every editor
//! (issue description today; comments and project descriptions reuse it).

use std::path::Path;

use rusqlite::Connection;

use crate::db::attachments::{self, NewPending, OwnerKind, UploadKind};
use crate::files::outbox;
use crate::files::validate::{validate_upload, Validation};
use crate::mentions::PENDING_SCHEME;

/// What the clipboard holds, as far as paste cares.
pub enum ClipboardPaste {
    /// Plain text (or nothing usable): the editor's default paste applies.
    Default,
    /// Image only, already encoded as PNG.
    Image(Vec<u8>),
}

/// Encode RGBA pixels as PNG.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(rgba).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// Classify the system clipboard. Text wins over an image.
pub fn read_clipboard() -> ClipboardPaste {
    let Ok(mut cb) = arboard::Clipboard::new() else {
        return ClipboardPaste::Default;
    };
    if cb.get_text().map(|t| !t.is_empty()).unwrap_or(false) {
        return ClipboardPaste::Default;
    }
    match cb.get_image() {
        Ok(img) => match encode_png(img.width as u32, img.height as u32, &img.bytes) {
            Ok(png) => ClipboardPaste::Image(png),
            Err(_) => ClipboardPaste::Default,
        },
        Err(_) => ClipboardPaste::Default,
    }
}

/// Stage PNG `png` as a pending inline image of `owner` and return the text to
/// insert at the cursor (`![image](jait-pending://<uuid>)`). Err is toast-ready;
/// nothing is left behind on failure.
pub fn paste_image_placeholder(
    conn: &Connection,
    outbox_root: &Path,
    owner_kind: OwnerKind,
    owner_id: &str,
    png: &[u8],
) -> Result<String, String> {
    let filename = "image.png";
    if let Validation::Refused(reason) = validate_upload(
        filename,
        png.len() as u64,
        Some("image/png"),
        UploadKind::InlineImage,
    ) {
        return Err(reason);
    }
    let staged =
        outbox::stage_bytes(outbox_root, png).map_err(|e| format!("Can't stage image ({e})."))?;
    let uuid = uuid::Uuid::new_v4().to_string();
    let new = NewPending {
        owner_kind,
        owner_id: owner_id.to_string(),
        filename: filename.to_string(),
        content_type: "image/png".to_string(),
        byte_size: staged.byte_size as i64,
        checksum: staged.checksum.clone(),
        outbox_path: staged.path.to_string_lossy().into_owned(),
        kind: UploadKind::InlineImage,
        placeholder_uuid: Some(uuid.clone()),
    };
    match attachments::add_pending(conn, &new) {
        Ok(_) => Ok(format!("![image]({PENDING_SCHEME}{uuid})")),
        Err(e) => {
            let _ = outbox::remove(&staged.path);
            Err(format!("Can't queue image: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&c).unwrap();
        c
    }

    fn root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("paste-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn pasted_png_becomes_a_placeholder_backed_by_a_pending_row() {
        let c = conn();
        let r = root();
        let png = encode_png(1, 1, &[255, 0, 0, 255]).unwrap();
        let text = paste_image_placeholder(&c, &r, OwnerKind::Issue, "i1", &png).unwrap();
        let uuid = text
            .strip_prefix("![image](jait-pending://")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap();
        let rows = attachments::list_for_owner(&c, OwnerKind::Issue, "i1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].placeholder_uuid.as_deref(), Some(uuid));
        assert_eq!(rows[0].upload_kind, UploadKind::InlineImage);
        let path = rows[0].outbox_path.clone().unwrap();
        assert_eq!(std::fs::read(path).unwrap(), png);
        // Only the upload is queued; the body write attaches the blob.
        let q: Vec<String> = c
            .prepare("SELECT entity_type || '/' || action FROM sync_queue")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(q, vec!["upload/create"]);
    }

    #[test]
    fn empty_image_is_refused_and_leaves_nothing() {
        let c = conn();
        let r = root();
        assert!(paste_image_placeholder(&c, &r, OwnerKind::Issue, "i1", &[]).is_err());
        assert!(attachments::list_for_owner(&c, OwnerKind::Issue, "i1")
            .unwrap()
            .is_empty());
    }
}
