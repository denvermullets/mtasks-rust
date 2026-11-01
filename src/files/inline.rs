//! Inline images: bare blobs reached through a JAIT blob-redirect URL
//! (`/rails/active_storage/blobs/redirect/<signed_id>/<filename>`) rather than
//! an Attachment row. The URL is signed, so fetching needs no auth. Cached
//! under a key derived from the signed id (see [`blob_key`]).

const BLOB_PATH_PREFIX: &str = "/rails/active_storage/blobs/redirect/";
const KEY_PREFIX: &str = "inline-";

/// The signed id inside a blob-redirect `src` (relative or absolute).
pub fn signed_id(src: &str) -> Option<&str> {
    let at = src.find(BLOB_PATH_PREFIX)?;
    let rest = &src[at + BLOB_PATH_PREFIX.len()..];
    let id = rest.split('/').next()?;
    (!id.is_empty()).then_some(id)
}

/// File-cache key for an inline image: `inline-<signed id>` with anything
/// outside `[A-Za-z0-9_-]` (e.g. base64 `=`, `+`) replaced by `_`. `None` when
/// `src` isn't a blob URL or the key would exceed the cache's 255-char limit.
pub fn blob_key(src: &str) -> Option<String> {
    let id = signed_id(src)?;
    let mut key = String::from(KEY_PREFIX);
    key.extend(id.chars().map(|c| {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '_'
        }
    }));
    (key.len() <= 255).then_some(key)
}

/// Absolute URL to GET for `src`: relative paths are resolved against `host`
/// (the configured JAIT base URL); absolute ones pass through.
pub fn fetch_url(src: &str, host: &str) -> String {
    if src.starts_with('/') {
        format!("{}{}", host.trim().trim_end_matches('/'), src)
    } else {
        src.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_derived_from_the_signed_id() {
        let src = "/rails/active_storage/blobs/redirect/eyJfcmFpbHMi--abc123/shot.png";
        assert_eq!(blob_key(src).as_deref(), Some("inline-eyJfcmFpbHMi--abc123"));
    }

    #[test]
    fn key_is_the_same_for_relative_and_absolute_forms() {
        let rel = "/rails/active_storage/blobs/redirect/AbC--1/a.png";
        let abs = "http://localhost:3005/rails/active_storage/blobs/redirect/AbC--1/a.png";
        assert_eq!(blob_key(rel), blob_key(abs));
    }

    #[test]
    fn key_replaces_characters_the_cache_rejects() {
        let src = "/rails/active_storage/blobs/redirect/ab=+/x.png";
        assert_eq!(blob_key(src).as_deref(), Some("inline-ab__"));
    }

    #[test]
    fn key_is_none_for_non_blob_urls_and_oversized_ids() {
        assert_eq!(blob_key("https://example.com/a.png"), None);
        let long = format!("/rails/active_storage/blobs/redirect/{}/a.png", "a".repeat(300));
        assert_eq!(blob_key(&long), None);
    }

    #[test]
    fn relative_urls_resolve_against_the_host() {
        assert_eq!(
            fetch_url("/rails/x", "http://localhost:3005/"),
            "http://localhost:3005/rails/x"
        );
        assert_eq!(fetch_url("http://h/rails/x", "http://other"), "http://h/rails/x");
    }
}
