use serde::Serialize;

use super::{ApiClient, ApiError, IssueDto, Page};

#[derive(Debug, Default, Clone)]
pub struct IssueFilters {
    pub lane_id: Option<i64>,
    pub assignee_id: Option<i64>,
    pub project_id: Option<i64>,
    /// Use [`crate::db::models::Priority::as_rails_str`] for valid values.
    pub priority: Option<String>,
}

impl IssueFilters {
    fn query_string(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(v) = self.lane_id {
            parts.push(format!("lane_id={v}"));
        }
        if let Some(v) = self.assignee_id {
            parts.push(format!("assignee_id={v}"));
        }
        if let Some(v) = self.project_id {
            parts.push(format!("project_id={v}"));
        }
        if let Some(v) = &self.priority {
            parts.push(format!("priority={v}"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.join("&"))
        }
    }
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_issue_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_ids: Option<Vec<i64>>,
}

#[derive(Debug, Serialize)]
struct IssueWrap<'a> {
    issue: &'a IssueParams,
}

pub async fn list(
    client: &ApiClient,
    team_id: i64,
    filters: &IssueFilters,
) -> Result<Vec<IssueDto>, ApiError> {
    let path = format!("api/v1/teams/{team_id}/issues{}", filters.query_string());
    client.get_list(&path).await
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

/// Percent-encode a query value (timestamps carry `:` and possibly `+`).
pub(crate) fn encode_query_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::Priority;

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
    fn empty_filters_produce_no_query_string() {
        assert_eq!(IssueFilters::default().query_string(), "");
    }

    #[test]
    fn populated_filters_join_with_ampersand() {
        let f = IssueFilters {
            lane_id: Some(1),
            assignee_id: Some(2),
            project_id: Some(3),
            priority: Some(Priority::High.as_rails_str().into()),
        };
        assert_eq!(
            f.query_string(),
            "?lane_id=1&assignee_id=2&project_id=3&priority=high"
        );
    }

    #[test]
    fn partial_filters_only_emit_set_keys() {
        let f = IssueFilters {
            priority: Some(Priority::Urgent.as_rails_str().into()),
            ..Default::default()
        };
        assert_eq!(
            f.query_string(),
            format!("?priority={}", Priority::Urgent.as_rails_str())
        );
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
