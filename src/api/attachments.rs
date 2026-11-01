//! Attachment endpoints: download (JRST-186), delete (JRST-215) and upload
//! (JRST-197).
//!
//! An upload is two steps: [`create_direct_upload`] (API) then [`put_bytes`]
//! (storage target, no bearer token); the resulting `signed_id` is then sent to
//! an attach endpoint ([`attach_to_issue`]).

use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, AttachmentDto};

/// Path of the authenticated download endpoint for an Attachment's remote id.
pub fn download_path(remote_id: i64) -> String {
    format!("/api/v1/attachments/{remote_id}/download")
}

/// Download an Attachment's bytes. The endpoint redirects to the blob, which
/// the client follows.
pub async fn download(client: &ApiClient, remote_id: i64) -> Result<Vec<u8>, ApiError> {
    client.get_bytes(&download_path(remote_id)).await
}

/// What an attachment hangs off, as remote ids. Comment variants carry the
/// comment's parent too, since Rails nests comment routes under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentOwner {
    Issue { issue_id: i64 },
    Project { project_id: i64 },
    IssueComment { issue_id: i64, comment_id: i64 },
    ProjectComment { project_id: i64, comment_id: i64 },
}

fn member_path(team_id: i64, owner: AttachmentOwner, attachment_id: i64) -> String {
    match owner {
        AttachmentOwner::Issue { issue_id } => {
            format!("api/v1/teams/{team_id}/issues/{issue_id}/attachments/{attachment_id}")
        }
        AttachmentOwner::Project { project_id } => {
            format!("api/v1/teams/{team_id}/projects/{project_id}/attachments/{attachment_id}")
        }
        AttachmentOwner::IssueComment { issue_id, comment_id } => format!(
            "api/v1/teams/{team_id}/issues/{issue_id}/comments/{comment_id}/attachments/{attachment_id}"
        ),
        AttachmentOwner::ProjectComment { project_id, comment_id } => format!(
            "api/v1/teams/{team_id}/projects/{project_id}/comments/{comment_id}/attachments/{attachment_id}"
        ),
    }
}

/// Delete an attachment. 204 on success; 404 when it is already gone.
pub async fn delete(
    client: &ApiClient,
    team_id: i64,
    owner: AttachmentOwner,
    attachment_id: i64,
) -> Result<(), ApiError> {
    client.delete(&member_path(team_id, owner, attachment_id)).await
}

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
    fn download_path_uses_the_remote_id() {
        assert_eq!(download_path(42), "/api/v1/attachments/42/download");
    }

    /// Live check against local Rails on :3005 (run with `--ignored`). Env:
    /// ATT_TEAM, ATT_ISSUE, ATT_ID (remote ids of an attached throwaway file).
    /// The first delete succeeds, the second is a 404. Token from config.toml.
    #[tokio::test]
    #[ignore]
    async fn live_delete_against_local_rails() {
        let env = |k: &str| std::env::var(k).unwrap().parse::<i64>().unwrap();
        let (cfg, _) = crate::config::load();
        let client = ApiClient::new(cfg.api_base_url.trim(), cfg.api_token.unwrap_or_default()).unwrap();
        let owner = AttachmentOwner::Issue { issue_id: env("ATT_ISSUE") };
        delete(&client, env("ATT_TEAM"), owner, env("ATT_ID")).await.unwrap();
        let again = delete(&client, env("ATT_TEAM"), owner, env("ATT_ID")).await;
        assert!(matches!(again, Err(ApiError::NotFound(_))), "{again:?}");
    }

    #[test]
    fn project_and_comment_attachment_paths_nest_under_their_owner() {
        assert_eq!(
            member_path(3, AttachmentOwner::Project { project_id: 4 }, 12),
            "api/v1/teams/3/projects/4/attachments/12"
        );
        assert_eq!(
            member_path(3, AttachmentOwner::IssueComment { issue_id: 9, comment_id: 5 }, 12),
            "api/v1/teams/3/issues/9/comments/5/attachments/12"
        );
        assert_eq!(
            member_path(3, AttachmentOwner::ProjectComment { project_id: 4, comment_id: 5 }, 12),
            "api/v1/teams/3/projects/4/comments/5/attachments/12"
        );
    }

    #[test]
    fn issue_attachment_path_nests_under_the_issue() {
        assert_eq!(
            member_path(3, AttachmentOwner::Issue { issue_id: 9 }, 12),
            "api/v1/teams/3/issues/9/attachments/12"
        );
    }

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
