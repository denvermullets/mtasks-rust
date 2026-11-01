//! Attachment endpoints. Delete lives here; JRST-197 adds upload/create.

use super::{ApiClient, ApiError};

/// What an attachment hangs off, as remote ids. JRST-216 adds the project and
/// comment variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentOwner {
    Issue { issue_id: i64 },
}

fn member_path(team_id: i64, owner: AttachmentOwner, attachment_id: i64) -> String {
    match owner {
        AttachmentOwner::Issue { issue_id } => {
            format!("api/v1/teams/{team_id}/issues/{issue_id}/attachments/{attachment_id}")
        }
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
    fn issue_attachment_path_nests_under_the_issue() {
        assert_eq!(
            member_path(3, AttachmentOwner::Issue { issue_id: 9 }, 12),
            "api/v1/teams/3/issues/9/attachments/12"
        );
    }
}
