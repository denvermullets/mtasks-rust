//! Outbox: a private copy of a file queued for upload, so it survives the
//! source being moved or deleted. No networking here.

use base64::Engine;
use md5::{Digest, Md5};
use std::io;
use std::path::{Path, PathBuf};

/// A staged file ready for upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEntry {
    pub path: PathBuf,
    pub byte_size: u64,
    /// Base64 of the raw MD5 digest (Active Storage's format, not hex).
    pub checksum: String,
}

/// Default root: `<data_local_dir>/<APP_DIR>/files/outbox`.
pub fn default_root() -> PathBuf {
    super::outbox_dir(&super::default_root())
}

/// Active Storage checksum: base64 of the raw MD5 digest.
pub fn checksum_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(Md5::digest(bytes))
}

/// Copy `source` into `root` (created if needed) and describe the copy.
pub fn stage(root: &Path, source: &Path) -> io::Result<OutboxEntry> {
    let bytes = std::fs::read(source)?;
    stage_bytes(root, &bytes)
}

/// Stage in-memory bytes (e.g. a pasted image) into `root`.
pub fn stage_bytes(root: &Path, bytes: &[u8]) -> io::Result<OutboxEntry> {
    std::fs::create_dir_all(root)?;
    let path = root.join(uuid::Uuid::new_v4().to_string());
    std::fs::write(&path, bytes)?;
    Ok(OutboxEntry {
        path,
        byte_size: bytes.len() as u64,
        checksum: checksum_base64(bytes),
    })
}

/// Delete the outbox copy (after a successful upload or when the user clicks
/// *Remove*). A copy that is already gone is not an error.
pub fn remove(entry_path: &Path) -> io::Result<()> {
    match std::fs::remove_file(entry_path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("outbox-test-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn checksum_matches_known_vectors() {
        // Active Storage: Digest::MD5.base64digest("")
        assert_eq!(checksum_base64(b""), "1B2M2Y8AsgTpgAmY7PhCfg==");
        // Digest::MD5.base64digest("hello world")
        assert_eq!(checksum_base64(b"hello world"), "XrY7u+Ae7tCTyyK7j1rNww==");
    }

    #[test]
    fn copy_survives_source_deletion() {
        let dir = tmp("survive");
        let src = dir.join("a.txt");
        std::fs::write(&src, b"hello world").unwrap();
        let e = stage(&dir.join("outbox"), &src).unwrap();
        std::fs::remove_file(&src).unwrap();
        assert_eq!(std::fs::read(&e.path).unwrap(), b"hello world");
        assert_eq!(e.byte_size, 11);
        assert_eq!(e.checksum, "XrY7u+Ae7tCTyyK7j1rNww==");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn remove_deletes_file_and_is_idempotent() {
        let dir = tmp("remove");
        let e = stage_bytes(&dir, b"xyz").unwrap();
        assert!(e.path.exists());
        remove(&e.path).unwrap();
        assert!(!e.path.exists());
        remove(&e.path).unwrap();
        std::fs::remove_dir_all(dir).ok();
    }
}
