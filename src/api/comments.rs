use serde::Serialize;

use super::{updated_since_query, ApiClient, ApiError, CommentDto, Page};

#[derive(Debug, Default, Clone, Serialize)]
pub struct CommentParams {
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<i64>,
}

#[derive(Debug, Serialize)]
struct CommentWrap<'a> {
    comment: &'a CommentParams,
}

/// Top-level comments on an issue. The server nests `replies` under each.
pub async fn list(
    client: &ApiClient,
    team_id: i64,
    issue_id: i64,
) -> Result<Vec<CommentDto>, ApiError> {
    let path = format!("api/v1/teams/{team_id}/issues/{issue_id}/comments");
    client.get_list(&path).await
}

/// What a comment is created under: an issue or a project (remote ids).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentTarget {
    Issue(i64),
    Project(i64),
}

impl CommentTarget {
    fn path(self, team_id: i64) -> String {
        match self {
            CommentTarget::Issue(id) => format!("api/v1/teams/{team_id}/issues/{id}/comments"),
            CommentTarget::Project(id) => project_comments_path(team_id, id),
        }
    }
}

fn project_comments_path(team_id: i64, project_id: i64) -> String {
    format!("api/v1/teams/{team_id}/projects/{project_id}/comments")
}

/// Top-level comments on a project, with `replies` nested under each.
pub async fn list_project(
    client: &ApiClient,
    team_id: i64,
    project_id: i64,
) -> Result<Vec<CommentDto>, ApiError> {
    client
        .get_list(&project_comments_path(team_id, project_id))
        .await
}

/// Every comment in the team (issue and project comments, replies as flat rows)
/// updated at or after `updated_since` (ISO8601), oldest update first. Not
/// paginated; the page carries `X-Server-Time` for the next cursor.
pub async fn list_team_updated_since(
    client: &ApiClient,
    team_id: i64,
    updated_since: Option<&str>,
) -> Result<Page<CommentDto>, ApiError> {
    client
        .get_list_page(&team_comments_path(team_id, updated_since))
        .await
}

fn team_comments_path(team_id: i64, updated_since: Option<&str>) -> String {
    format!(
        "api/v1/teams/{team_id}/comments{}",
        updated_since_query(updated_since)
    )
}

/// Create a comment. `idempotency_key` (the local UUID) is sent as the
/// `Idempotency-Key` header so a re-drained create returns the existing comment
/// instead of duplicating it.
pub async fn create(
    client: &ApiClient,
    team_id: i64,
    target: CommentTarget,
    params: &CommentParams,
    idempotency_key: &str,
) -> Result<CommentDto, ApiError> {
    let path = target.path(team_id);
    client
        .post_with_header(
            &path,
            &CommentWrap { comment: params },
            "Idempotency-Key",
            idempotency_key,
        )
        .await
}

/// Delete a comment (and, server-side, its replies). 204 on success; 403 unless
/// the caller is the author or a team admin.
pub async fn delete(
    client: &ApiClient,
    team_id: i64,
    issue_id: i64,
    comment_id: i64,
) -> Result<(), ApiError> {
    client
        .delete(&comment_member_path(team_id, issue_id, comment_id))
        .await
}

fn comment_member_path(team_id: i64, issue_id: i64, comment_id: i64) -> String {
    format!("api/v1/teams/{team_id}/issues/{issue_id}/comments/{comment_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_comments_path_nests_under_the_project() {
        assert_eq!(
            project_comments_path(3, 9),
            "api/v1/teams/3/projects/9/comments"
        );
    }

    #[test]
    fn create_path_follows_the_target() {
        assert_eq!(
            CommentTarget::Issue(5).path(3),
            "api/v1/teams/3/issues/5/comments"
        );
        assert_eq!(
            CommentTarget::Project(9).path(3),
            "api/v1/teams/3/projects/9/comments"
        );
    }

    #[test]
    fn comment_delete_path_nests_under_issue() {
        assert_eq!(
            comment_member_path(3, 50, 9),
            "api/v1/teams/3/issues/50/comments/9"
        );
    }

    #[test]
    fn team_comments_path_includes_cursor_only_when_given() {
        assert_eq!(
            team_comments_path(3, Some("2026-10-30T12:00:00.000000Z")),
            "api/v1/teams/3/comments?updated_since=2026-10-30T12%3A00%3A00.000000Z"
        );
        assert_eq!(team_comments_path(3, None), "api/v1/teams/3/comments");
    }

    #[test]
    fn comment_params_wrap_under_comment_key() {
        let params = CommentParams {
            body: "hello".into(),
            parent_id: None,
        };
        let body = serde_json::to_value(CommentWrap { comment: &params }).unwrap();
        assert_eq!(body, serde_json::json!({ "comment": { "body": "hello" } }));
    }

    #[test]
    fn comment_params_include_parent_when_set() {
        let params = CommentParams {
            body: "reply".into(),
            parent_id: Some(7),
        };
        let body = serde_json::to_value(CommentWrap { comment: &params }).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "comment": { "body": "reply", "parent_id": 7 } })
        );
    }
}
