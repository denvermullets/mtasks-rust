//! Attachment endpoints: download (JRST-186) and delete (JRST-215).
//! JRST-197 adds upload/create.

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
}
