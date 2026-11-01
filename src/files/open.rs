//! Opening a cached Attachment in the system's default app.
//!
//! Cache blobs have no file extension, so the default app couldn't pick a
//! handler. [`materialize`] copies the blob to `<root>/open/<key>/<filename>`
//! first; that folder is not part of the cache index or the outbox.

use std::path::{Path, PathBuf};

use crate::db::attachments::Attachment;

/// Cache key for an Attachment: its blob id when known, else its remote id.
/// `None` for an Attachment that was never uploaded (nothing to download).
pub fn blob_key(a: &Attachment) -> Option<String> {
    match (a.blob_id, a.remote_id) {
        (Some(b), _) => Some(format!("blob-{b}")),
        (None, Some(r)) => Some(format!("att-{r}")),
        (None, None) => None,
    }
}

/// A filename safe to create on disk: path separators and control characters
/// become `_`; empty or dot-only names fall back to `file`.
pub fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == ':' || c.is_control() { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        "file".to_string()
    } else {
        cleaned
    }
}

/// Copy a cached blob to `<root>/open/<key>/<safe filename>` and return that path.
pub fn materialize(root: &Path, key: &str, filename: &str, cached: &Path) -> std::io::Result<PathBuf> {
    let dir = root.join("open").join(key);
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(safe_filename(filename));
    std::fs::copy(cached, &dest)?;
    Ok(dest)
}

/// Hand `path` to the OS default app (macOS `open`).
pub fn open_in_default_app(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("open").arg(path).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::attachments::UploadState;

    fn att(blob_id: Option<i64>, remote_id: Option<i64>) -> Attachment {
        Attachment {
            id: "l".into(),
            remote_id,
            owner_kind: "issue".into(),
            owner_id: "o".into(),
            blob_id,
            filename: "a.pdf".into(),
            content_type: "application/pdf".into(),
            byte_size: 1,
            checksum: None,
            created_at: "t".into(),
            upload_state: UploadState::Synced,
            failure_reason: None,
        }
    }

    #[test]
    fn key_prefers_blob_id_then_remote_id() {
        assert_eq!(blob_key(&att(Some(7), Some(9))).as_deref(), Some("blob-7"));
        assert_eq!(blob_key(&att(None, Some(9))).as_deref(), Some("att-9"));
        assert_eq!(blob_key(&att(None, None)), None);
    }

    #[test]
    fn filenames_cannot_escape_the_folder() {
        assert_eq!(safe_filename("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(safe_filename("Q3 report.pdf"), "Q3 report.pdf");
        assert_eq!(safe_filename(".."), "file");
        assert_eq!(safe_filename("  "), "file");
    }

    #[test]
    fn materialize_copies_under_the_real_filename() {
        let dir = std::env::temp_dir().join(format!("mt-open-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let blob = dir.join("blob");
        std::fs::write(&blob, b"hello").unwrap();
        let out = materialize(&dir, "blob-7", "notes.txt", &blob).unwrap();
        assert_eq!(out, dir.join("open/blob-7/notes.txt"));
        assert_eq!(std::fs::read(&out).unwrap(), b"hello");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
