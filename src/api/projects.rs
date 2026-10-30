use serde::Serialize;

use super::{updated_since_query, ApiClient, ApiError, Page, ProjectDto};

/// Body for create/update — fields match Rails `ProjectsController` strong params.
/// Use [`crate::db::models::Priority::as_rails_str`] to populate `priority`.
/// `status` is a string ("backlog" | "started" | "paused" | "completed" | "cancelled").
/// `roadmap_commitment` is a string ("now" | "next" | "later").
#[derive(Debug, Default, Clone, Serialize)]
pub struct ProjectParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lead_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roadmap_commitment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_ids: Option<Vec<i64>>,
}

#[derive(Debug, Serialize)]
struct ProjectWrap<'a> {
    project: &'a ProjectParams,
}

/// One response of the project index (not paged) with `X-Server-Time`.
/// `updated_since` (ISO8601) limits it to rows changed at or after that time.
pub async fn list_page(
    client: &ApiClient,
    team_id: i64,
    updated_since: Option<&str>,
) -> Result<Page<ProjectDto>, ApiError> {
    let path = format!(
        "api/v1/teams/{team_id}/projects{}",
        updated_since_query(updated_since)
    );
    client.get_list_page(&path).await
}

pub async fn get(client: &ApiClient, team_id: i64, id: i64) -> Result<ProjectDto, ApiError> {
    let path = format!("api/v1/teams/{team_id}/projects/{id}");
    client.get(&path).await
}

pub async fn create(
    client: &ApiClient,
    team_id: i64,
    params: &ProjectParams,
) -> Result<ProjectDto, ApiError> {
    let path = format!("api/v1/teams/{team_id}/projects");
    client.post(&path, &ProjectWrap { project: params }).await
}

pub async fn update(
    client: &ApiClient,
    team_id: i64,
    id: i64,
    params: &ProjectParams,
) -> Result<ProjectDto, ApiError> {
    let path = format!("api/v1/teams/{team_id}/projects/{id}");
    client.patch(&path, &ProjectWrap { project: params }).await
}

pub async fn delete(client: &ApiClient, team_id: i64, id: i64) -> Result<(), ApiError> {
    let path = format!("api/v1/teams/{team_id}/projects/{id}");
    client.delete(&path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updated_since_query_is_encoded_and_optional() {
        assert_eq!(updated_since_query(None), "");
        assert_eq!(
            updated_since_query(Some("2026-10-30T12:00:00+00:00")),
            "?updated_since=2026-10-30T12%3A00%3A00%2B00%3A00"
        );
    }

    #[test]
    fn project_params_wrap_under_project_key() {
        let params = ProjectParams {
            name: Some("Q3 push".into()),
            status: Some("started".into()),
            label_ids: Some(vec![7]),
            ..Default::default()
        };
        let body = serde_json::to_value(ProjectWrap { project: &params }).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "project": {
                    "name": "Q3 push",
                    "status": "started",
                    "label_ids": [7]
                }
            })
        );
    }
}
