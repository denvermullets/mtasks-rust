//! Files: local storage for Attachment bytes. No networking.
//!
//! Layout under the files root (`data_local_dir()/mtasks-rusty/files/`):
//! - `cache/`  downloaded blobs, tracked by the `file_cache` index and evicted
//!   least-recently-accessed first ([`cache`]).
//! - `outbox/` pending uploads. Never touched by cache eviction.

#![allow(dead_code)]

pub mod cache;

use std::path::{Path, PathBuf};

/// Root folder for all Files storage.
pub fn default_root() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mtasks-rusty")
        .join("files")
}

/// Outbox folder under a files root. Eviction never touches it.
pub fn outbox_dir(root: &Path) -> PathBuf {
    root.join("outbox")
}
