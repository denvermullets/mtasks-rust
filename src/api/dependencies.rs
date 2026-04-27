use serde::{Deserialize, Serialize};

use super::{ApiClient, ApiError, IssueRef};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Current issue blocks the target issue.
    Blocking,
    /// Current issue is blocked by the target issue.
    BlockedBy,
}

/// Rails returns dependency rows with embedded issue refs (not flat ids).
/// This is the wire shape from `Api::V1::IssueDependenciesController#create`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct IssueDependencyResponse {
    pub id: i64,
    pub blocking_issue: IssueRef,
    pub blocked_issue: IssueRef,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
struct CreateBody {
    target_issue_id: i64,
    direction: Direction,
}

pub async fn create(
    client: &ApiClient,
    team_id: i64,
    issue_id: i64,
    target_issue_id: i64,
    direction: Direction,
) -> Result<IssueDependencyResponse, ApiError> {
    let path = format!("api/v1/teams/{team_id}/issues/{issue_id}/dependencies");
    let body = CreateBody { target_issue_id, direction };
    client.post(&path, &body).await
}

pub async fn delete(
    client: &ApiClient,
    team_id: i64,
    issue_id: i64,
    dep_id: i64,
) -> Result<(), ApiError> {
    let path = format!("api/v1/teams/{team_id}/issues/{issue_id}/dependencies/{dep_id}");
    client.delete(&path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_body_is_flat_with_snake_case_direction() {
        let body = CreateBody {
            target_issue_id: 42,
            direction: Direction::BlockedBy,
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({ "target_issue_id": 42, "direction": "blocked_by" })
        );

        let body = CreateBody {
            target_issue_id: 7,
            direction: Direction::Blocking,
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({ "target_issue_id": 7, "direction": "blocking" })
        );
    }

    #[test]
    fn response_deserializes_from_rails_shape() {
        let raw = serde_json::json!({
            "id": 9,
            "blocking_issue": { "id": 1, "identifier": "TEAM-1", "title": "A" },
            "blocked_issue":  { "id": 2, "identifier": "TEAM-2", "title": "B" },
            "created_at": "2026-04-27T10:00:00Z"
        });
        let parsed: IssueDependencyResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.id, 9);
        assert_eq!(parsed.blocking_issue.identifier, "TEAM-1");
        assert_eq!(parsed.blocked_issue.identifier, "TEAM-2");
    }
}
