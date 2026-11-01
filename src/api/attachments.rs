//! Attachment downloads: `GET /api/v1/attachments/:id/download`.

use super::{ApiClient, ApiError};

/// Path of the authenticated download endpoint for an Attachment's remote id.
pub fn download_path(remote_id: i64) -> String {
    format!("/api/v1/attachments/{remote_id}/download")
}

/// Download an Attachment's bytes. The endpoint redirects to the blob, which
/// the client follows.
pub async fn download(client: &ApiClient, remote_id: i64) -> Result<Vec<u8>, ApiError> {
    client.get_bytes(&download_path(remote_id)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_path_uses_the_remote_id() {
        assert_eq!(download_path(42), "/api/v1/attachments/42/download");
    }
}
