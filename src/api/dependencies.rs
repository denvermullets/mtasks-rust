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
    /// `blocks` | `relates` | `duplicates`.
    pub kind: String,
    /// Relative to the issue in the URL: `blocking` | `blocked_by` | `relates`
    /// | `duplicates` | `duplicated_by`.
    pub direction: String,
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
    let path = collection_path(team_id, issue_id);
    let body = CreateBody {
        target_issue_id,
        direction,
    };
    client.post(&path, &body).await
}

pub async fn delete(
    client: &ApiClient,
    team_id: i64,
    issue_id: i64,
    dep_id: i64,
) -> Result<(), ApiError> {
    let path = member_path(team_id, issue_id, dep_id);
    client.delete(&path).await
}

fn collection_path(team_id: i64, issue_id: i64) -> String {
    format!("api/v1/teams/{team_id}/issues/{issue_id}/issue_dependencies")
}

fn member_path(team_id: i64, issue_id: i64, dep_id: i64) -> String {
    format!("{}/{dep_id}", collection_path(team_id, issue_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_delete_use_issue_dependencies_path() {
        assert_eq!(
            collection_path(3, 5),
            "api/v1/teams/3/issues/5/issue_dependencies"
        );
        assert_eq!(
            member_path(3, 5, 9),
            "api/v1/teams/3/issues/5/issue_dependencies/9"
        );
    }

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
            "kind": "blocks",
            "direction": "blocking",
            "blocking_issue": { "id": 1, "identifier": "TEAM-1", "title": "A" },
            "blocked_issue":  { "id": 2, "identifier": "TEAM-2", "title": "B" },
            "created_at": "2026-04-27T10:00:00Z"
        });
        let parsed: IssueDependencyResponse = serde_json::from_value(raw).unwrap();
        assert_eq!(parsed.id, 9);
        assert_eq!(parsed.kind, "blocks");
        assert_eq!(parsed.direction, "blocking");
        assert_eq!(parsed.blocking_issue.identifier, "TEAM-1");
        assert_eq!(parsed.blocked_issue.identifier, "TEAM-2");
    }
}
