use serde::Serialize;

use super::{encode_query_value, ApiClient, ApiError, IssueDto, Page};

/// Body for create/update — fields match Rails `IssuesController` strong params.
/// Use [`crate::db::models::Priority::as_rails_str`] to populate `priority`.
#[derive(Debug, Default, Clone, Serialize)]
pub struct IssueParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// `estimate`, `due_date`, `assignee_id` and `project_id` are tri-state
    /// like `parent_issue_id`: `None` omits the key, `Some(None)` sends `null`
    /// (Rails clears it), `Some(Some(v))` sets it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimate: Option<Option<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<Option<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Option<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Tri-state: `None` omits the key (Rails keeps the parent), `Some(None)`
    /// sends `null` (clears it), `Some(Some(id))` sets it.
    pub parent_issue_id: Option<Option<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_ids: Option<Vec<i64>>,
}

#[derive(Debug, Serialize)]
struct IssueWrap<'a> {
    issue: &'a IssueParams,
}

/// Query for the incremental, keyset-paged issue index.
#[derive(Debug, Default, Clone)]
pub struct IssuePageQuery {
    /// ISO8601 lower bound on `updated_at` (inclusive). `None` = everything.
    pub updated_since: Option<String>,
    /// Ask for the detailed serializer on every row.
    pub include_detail: bool,
    /// Page size; Rails caps at 500 and defaults to 200.
    pub limit: Option<u32>,
    /// Resume after this issue id (from the previous page's `X-Next-After-Id`).
    pub after_id: Option<i64>,
}

impl IssuePageQuery {
    pub(crate) fn query_string(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(s) = &self.updated_since {
            parts.push(format!("updated_since={}", encode_query_value(s)));
        }
        if self.include_detail {
            parts.push("include=detail".to_string());
        }
        if let Some(l) = self.limit {
            parts.push(format!("limit={l}"));
        }
        if let Some(a) = self.after_id {
            parts.push(format!("after_id={a}"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.join("&"))
        }
    }
}

/// One page of the paged issue index plus `X-Next-After-Id` / `X-Server-Time`.
pub async fn list_page(
    client: &ApiClient,
    team_id: i64,
    query: &IssuePageQuery,
) -> Result<Page<IssueDto>, ApiError> {
    let path = format!("api/v1/teams/{team_id}/issues{}", query.query_string());
    client.get_list_page(&path).await
}

pub async fn get(client: &ApiClient, team_id: i64, id: i64) -> Result<IssueDto, ApiError> {
    let path = format!("api/v1/teams/{team_id}/issues/{id}");
    client.get(&path).await
}

pub async fn create(
    client: &ApiClient,
    team_id: i64,
    params: &IssueParams,
) -> Result<IssueDto, ApiError> {
    let path = format!("api/v1/teams/{team_id}/issues");
    client.post(&path, &IssueWrap { issue: params }).await
}

pub async fn update(
    client: &ApiClient,
    team_id: i64,
    id: i64,
    params: &IssueParams,
) -> Result<IssueDto, ApiError> {
    let path = format!("api/v1/teams/{team_id}/issues/{id}");
    client.patch(&path, &IssueWrap { issue: params }).await
}

pub async fn delete(client: &ApiClient, team_id: i64, id: i64) -> Result<(), ApiError> {
    let path = format!("api/v1/teams/{team_id}/issues/{id}");
    client.delete(&path).await
}

#[derive(Debug, Serialize)]
struct MoveBody {
    target_team_id: i64,
}

fn move_path(team_id: i64, id: i64) -> String {
    format!("api/v1/teams/{team_id}/issues/{id}/move")
}

/// Move an issue to another team. `team_id` is the SOURCE team; Rails answers
/// with the detailed issue (new `identifier`). 404 = target outside the token,
/// 422 = already in that team or not a member of both.
pub async fn move_to_team(
    client: &ApiClient,
    team_id: i64,
    id: i64,
    target_team_id: i64,
) -> Result<IssueDto, ApiError> {
    client
        .patch(&move_path(team_id, id), &MoveBody { target_team_id })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::Priority;

    #[test]
    fn move_path_and_body_target_the_source_team() {
        assert_eq!(move_path(3, 5), "api/v1/teams/3/issues/5/move");
        assert_eq!(
            serde_json::to_value(MoveBody { target_team_id: 9 }).unwrap(),
            serde_json::json!({ "target_team_id": 9 })
        );
    }

    #[test]
    fn page_query_encodes_cursor_and_paging() {
        let q = IssuePageQuery {
            updated_since: Some("2026-10-30T12:00:00Z".into()),
            include_detail: true,
            limit: Some(500),
            after_id: Some(9),
        };
        assert_eq!(
            q.query_string(),
            "?updated_since=2026-10-30T12%3A00%3A00Z&include=detail&limit=500&after_id=9"
        );
        assert_eq!(IssuePageQuery::default().query_string(), "");
    }

    #[test]
    fn issue_params_wrap_under_issue_key() {
        let params = IssueParams {
            title: Some("Bug".into()),
            priority: Some(Priority::High.as_rails_str().into()),
            label_ids: Some(vec![1, 2]),
            ..Default::default()
        };
        let body = serde_json::to_value(IssueWrap { issue: &params }).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "issue": {
                    "title": "Bug",
                    "priority": "high",
                    "label_ids": [1, 2]
                }
            })
        );
    }

    fn parent_body(parent: Option<Option<i64>>) -> serde_json::Value {
        let params = IssueParams {
            parent_issue_id: parent,
            ..Default::default()
        };
        serde_json::to_value(IssueWrap { issue: &params }).unwrap()
    }

    #[test]
    fn parent_issue_id_is_tri_state() {
        assert_eq!(
            parent_body(Some(None)),
            serde_json::json!({ "issue": { "parent_issue_id": null } })
        );
        assert_eq!(
            parent_body(Some(Some(42))),
            serde_json::json!({ "issue": { "parent_issue_id": 42 } })
        );
        assert_eq!(parent_body(None), serde_json::json!({ "issue": {} }));
    }

    #[test]
    fn issue_params_skip_unset_fields() {
        let params = IssueParams {
            title: Some("Just a title".into()),
            ..Default::default()
        };
        let body = serde_json::to_value(IssueWrap { issue: &params }).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "issue": { "title": "Just a title" } })
        );
    }
}
