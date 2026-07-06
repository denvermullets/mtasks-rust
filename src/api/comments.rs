use serde::Serialize;

use super::{ApiClient, ApiError, CommentDto};

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

/// Top-level comments on an issue. The server nests `replies` under each; v1
/// ignores them (flat thread).
pub async fn list(
    client: &ApiClient,
    team_id: i64,
    issue_id: i64,
) -> Result<Vec<CommentDto>, ApiError> {
    let path = format!("api/v1/teams/{team_id}/issues/{issue_id}/comments");
    client.get(&path).await
}

/// Create a comment. `idempotency_key` (the local UUID) is sent as the
/// `Idempotency-Key` header so a re-drained create returns the existing comment
/// instead of duplicating it.
pub async fn create(
    client: &ApiClient,
    team_id: i64,
    issue_id: i64,
    params: &CommentParams,
    idempotency_key: &str,
) -> Result<CommentDto, ApiError> {
    let path = format!("api/v1/teams/{team_id}/issues/{issue_id}/comments");
    client
        .post_with_header(
            &path,
            &CommentWrap { comment: params },
            "Idempotency-Key",
            idempotency_key,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

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
