use serde::Serialize;

use super::{ApiClient, ApiError, IssueDto};

#[derive(Debug, Default, Clone)]
pub struct IssueFilters {
    pub lane_id: Option<i64>,
    pub assignee_id: Option<i64>,
    pub project_id: Option<i64>,
    /// Use [`crate::db::models::Priority::as_rails_str`] for valid values.
    pub priority: Option<String>,
    /// ISO8601; only issues updated at or after this instant.
    pub updated_since: Option<String>,
    /// Page size (Rails caps at 500).
    pub limit: Option<i64>,
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
        if let Some(v) = &self.updated_since {
            parts.push(format!("updated_since={v}"));
        }
        if let Some(v) = self.limit {
            parts.push(format!("limit={v}"));
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
            ..Default::default()
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
