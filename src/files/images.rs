//! In-memory state for lazily fetched images, keyed by cache blob key
//! (see [`super::open::blob_key`]). Shared by attachment thumbnails, inline
//! images and the lightbox: whoever needs an image asks the store, and the
//! bytes live in the file cache, never in memory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where one image is in the fetch pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageState {
    /// A download is in flight.
    Loading,
    /// Cached on disk at this path.
    Ready(PathBuf),
    /// The download or cache write failed; not retried until `forget`.
    Failed,
}

#[derive(Debug, Default)]
pub struct ImageStore {
    states: HashMap<String, ImageState>,
}

impl ImageStore {
    pub fn state(&self, key: &str) -> Option<&ImageState> {
        self.states.get(key)
    }

    /// The cached file for `key`, once ready.
    pub fn path(&self, key: &str) -> Option<&Path> {
        match self.states.get(key) {
            Some(ImageState::Ready(p)) => Some(p),
            _ => None,
        }
    }

    /// Claim `key` for fetching. True only when nothing is known about it yet,
    /// so concurrent requests for one image start a single download.
    pub fn begin(&mut self, key: &str) -> bool {
        if self.states.contains_key(key) {
            return false;
        }
        self.states.insert(key.to_string(), ImageState::Loading);
        true
    }

    pub fn set_ready(&mut self, key: &str, path: PathBuf) {
        self.states.insert(key.to_string(), ImageState::Ready(path));
    }

    pub fn set_failed(&mut self, key: &str) {
        self.states.insert(key.to_string(), ImageState::Failed);
    }

    /// Forget every image (token lost access, or a new token to retry with).
    pub fn clear(&mut self) {
        self.states.clear();
    }

    /// Drop what we know (e.g. after cache eviction) so `begin` can retry.
    #[cfg(test)]
    pub fn forget(&mut self, key: &str) {
        self.states.remove(key);
    }
}

/// Whether a content type is an image Iced can decode for a thumbnail.
pub fn is_thumbnailable(content_type: &str) -> bool {
    matches!(
        content_type.to_ascii_lowercase().as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" | "image/bmp"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_first_request_for_a_key_starts_a_fetch() {
        let mut s = ImageStore::default();
        assert!(s.begin("blob-1"));
        assert!(!s.begin("blob-1"));
        assert_eq!(s.state("blob-1"), Some(&ImageState::Loading));
        assert!(s.begin("blob-2"));
    }

    #[test]
    fn ready_exposes_the_cached_path_and_failed_does_not_refetch() {
        let mut s = ImageStore::default();
        s.begin("k");
        assert_eq!(s.path("k"), None);
        s.set_ready("k", PathBuf::from("/c/k"));
        assert_eq!(s.path("k"), Some(Path::new("/c/k")));
        s.set_failed("j");
        assert!(!s.begin("j"));
        s.forget("j");
        assert!(s.begin("j"));
    }

    #[test]
    fn thumbnailable_types() {
        assert!(is_thumbnailable("image/png"));
        assert!(is_thumbnailable("IMAGE/JPEG"));
        assert!(!is_thumbnailable("image/svg+xml"));
        assert!(!is_thumbnailable("application/pdf"));
    }
}
