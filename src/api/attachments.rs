//! Direct uploads and attachments. Other tickets add download (JRST-186) and
//! delete (JRST-215) here.
//!
//! An upload is two steps: [`create_direct_upload`] (API) then [`put_bytes`]
//! (storage target, no bearer token); the resulting `signed_id` is then sent to
//! an attach endpoint ([`attach_to_issue`]).

use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, AttachmentDto};

/// `blob` params of `POST /api/v1/direct_uploads`.
#[derive(Debug, Clone, Serialize)]
pub struct BlobParams {
    pub filename: String,
    pub byte_size: u64,
    /// Base64 of the raw MD5 digest.
    pub checksum: String,
    pub content_type: String,
}

#[derive(Debug, Serialize)]
struct DirectUploadBody<'a> {
    blob: &'a BlobParams,
}

/// Where and how to PUT the bytes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DirectUploadTarget {
    pub url: String,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
}

/// Response of a direct-upload create: the blob plus its upload target.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DirectUploadDto {
    pub signed_id: String,
    #[serde(default)]
    pub filename: Option<String>,
    pub direct_upload: DirectUploadTarget,
}

#[derive(Debug, Serialize)]
struct AttachBody<'a> {
    signed_ids: &'a [String],
}

/// `POST /api/v1/direct_uploads`. 422 lists the refused fields.
pub async fn create_direct_upload(
    client: &ApiClient,
    blob: &BlobParams,
) -> Result<DirectUploadDto, ApiError> {
    client
        .post("api/v1/direct_uploads", &DirectUploadBody { blob })
        .await
}

/// PUT the bytes to the upload target with exactly the returned headers and no
/// API token.
pub async fn put_bytes(
    client: &ApiClient,
    target: &DirectUploadTarget,
    bytes: Vec<u8>,
) -> Result<(), ApiError> {
    let headers: Vec<(String, String)> = target
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    client.put_raw(&target.url, &headers, bytes).await
}

pub(crate) fn issue_attachments_path(team_id: i64, issue_id: i64) -> String {
    format!("api/v1/teams/{team_id}/issues/{issue_id}/attachments")
}

/// `POST .../issues/:issue_id/attachments` with `{signed_ids}`. 201 returns the
/// new attachment rows; a bad id fails the whole batch with a 422.
pub async fn attach_to_issue(
    client: &ApiClient,
    team_id: i64,
    issue_id: i64,
    signed_ids: &[String],
) -> Result<Vec<AttachmentDto>, ApiError> {
    client
        .post(
            &issue_attachments_path(team_id, issue_id),
            &AttachBody { signed_ids },
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_upload_response_parses_target_and_headers() {
        let d: DirectUploadDto = serde_json::from_value(serde_json::json!({
            "id": 5, "signed_id": "eyJ", "filename": "a.pdf", "byte_size": 3,
            "direct_upload": {
                "url": "http://localhost:3005/rails/active_storage/disk/tok",
                "headers": {"Content-Type": "application/pdf", "Content-MD5": "abc=="}
            }
        }))
        .unwrap();
        assert_eq!(d.signed_id, "eyJ");
        assert_eq!(d.direct_upload.headers["Content-MD5"], "abc==");
    }

    #[test]
    fn bodies_match_the_rails_contract() {
        let blob = BlobParams {
            filename: "a.pdf".into(),
            byte_size: 3,
            checksum: "abc==".into(),
            content_type: "application/pdf".into(),
        };
        assert_eq!(
            serde_json::to_value(DirectUploadBody { blob: &blob }).unwrap(),
            serde_json::json!({"blob": {"filename": "a.pdf", "byte_size": 3,
                "checksum": "abc==", "content_type": "application/pdf"}})
        );
        assert_eq!(
            serde_json::to_value(AttachBody { signed_ids: &["s1".to_string()] }).unwrap(),
            serde_json::json!({"signed_ids": ["s1"]})
        );
        assert_eq!(issue_attachments_path(4, 9), "api/v1/teams/4/issues/9/attachments");
    }
}
