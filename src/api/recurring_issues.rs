//! `GET /api/v1/teams/:team_id/recurring_issues`: a team's recurring issues
//! (schedule templates the server spawns issues from). A bare array ordered by
//! id, not paged; `updated_since` filters strictly (`updated_at > since`).

use serde::{Deserialize, Serialize};

use super::{updated_since_query, ApiClient, ApiError, Page, RecordRef};

/// One row of the recurring issues index (Rails `RecurringIssueSerializer`).
/// `schedule_summary` is ignored: the desktop derives its own wording.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecurringIssueDto {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Rails enum name ("urgent", "high", "medium", "low", "no_priority").
    pub priority: String,
    /// "daily" | "weekly" | "monthly".
    pub frequency: String,
    pub interval: i64,
    /// 0 (Sunday) to 6; set for weekly schedules.
    #[serde(default)]
    pub weekday: Option<i64>,
    /// 1 to 31; set for monthly schedules.
    #[serde(default)]
    pub day_of_month: Option<i64>,
    /// ActiveSupport zone name, e.g. "Eastern Time (US & Canada)".
    #[serde(default)]
    pub time_zone: Option<String>,
    pub active: bool,
    /// Date (`YYYY-MM-DD`), server-calculated.
    #[serde(default)]
    pub next_run_on: Option<String>,
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub assignee: Option<RecordRef>,
    #[serde(default)]
    pub project: Option<RecordRef>,
    #[serde(default)]
    pub lane: Option<RecordRef>,
    #[serde(default)]
    pub label_ids: Vec<i64>,
    #[serde(default)]
    pub creator: Option<RecordRef>,
    pub created_at: String,
    pub updated_at: String,
}

/// Request fields for `recurring_issue` writes, sent under the
/// `{recurring_issue: {...}}` wrapper. Every field is optional and skipped when
/// `None`, so a write sends only what it changed (Rails permits a partial
/// update). Later write tickets (edit, create) fill more fields.
/// `starts_on` (`YYYY-MM-DD`) sits inside the wrapper with the others; it is
/// sent only on schedule changes, never on a pause/resume, so the server keeps
/// the stored `next_run_on`.
#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct RecurringIssueParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weekday: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day_of_month: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_ids: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starts_on: Option<String>,
}

#[derive(Debug, Serialize)]
struct RecurringIssueWrap<'a> {
    recurring_issue: &'a RecurringIssueParams,
}

fn item_path(team_id: i64, id: i64) -> String {
    format!("api/v1/teams/{team_id}/recurring_issues/{id}")
}

/// `PATCH .../recurring_issues/:id`; returns the server's updated row.
pub async fn update(
    client: &ApiClient,
    team_id: i64,
    id: i64,
    params: &RecurringIssueParams,
) -> Result<RecurringIssueDto, ApiError> {
    client
        .patch(&item_path(team_id, id), &RecurringIssueWrap { recurring_issue: params })
        .await
}

fn list_path(team_id: i64, updated_since: Option<&str>) -> String {
    format!(
        "api/v1/teams/{team_id}/recurring_issues{}",
        updated_since_query(updated_since)
    )
}

/// One response of the index with `X-Server-Time`. `updated_since` (ISO8601)
/// limits it to rows changed strictly after that time.
pub async fn list_page(
    client: &ApiClient,
    team_id: i64,
    updated_since: Option<&str>,
) -> Result<Page<RecurringIssueDto>, ApiError> {
    client.get_list_page(&list_path(team_id, updated_since)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_path_omits_updated_since_on_the_first_pull() {
        assert_eq!(list_path(7, None), "api/v1/teams/7/recurring_issues");
        assert_eq!(
            list_path(7, Some("2026-01-01T00:00:00Z")),
            "api/v1/teams/7/recurring_issues?updated_since=2026-01-01T00%3A00%3A00Z"
        );
    }

    #[test]
    fn update_body_wraps_and_skips_unset_fields_including_starts_on() {
        let params = RecurringIssueParams { active: Some(false), ..Default::default() };
        let body = serde_json::to_value(RecurringIssueWrap { recurring_issue: &params }).unwrap();
        assert_eq!(body, serde_json::json!({ "recurring_issue": { "active": false } }));
        assert_eq!(item_path(7, 5), "api/v1/teams/7/recurring_issues/5");
    }

    #[test]
    fn decodes_a_full_row() {
        let json = r#"{"id":5,"title":"Deploy","description":"Ship it","priority":"high",
          "frequency":"weekly","interval":2,"weekday":5,"day_of_month":null,
          "time_zone":"Eastern Time (US & Canada)","active":true,"next_run_on":"2026-11-06",
          "last_run_at":"2026-10-23T14:00:00.000Z","schedule_summary":"Every 2 weeks on Friday",
          "assignee":{"id":3,"name":"Ann"},"project":{"id":9,"name":"P"},"lane":{"id":4,"name":"Todo"},
          "label_ids":[1,2],"creator":{"id":3,"name":"Ann"},
          "created_at":"2026-10-01T00:00:00.000Z","updated_at":"2026-10-02T00:00:00.000Z"}"#;
        let dto: RecurringIssueDto = serde_json::from_str(json).unwrap();
        assert_eq!(dto.id, 5);
        assert_eq!(dto.weekday, Some(5));
        assert_eq!(dto.day_of_month, None);
        assert_eq!(dto.assignee.as_ref().unwrap().name, "Ann");
        assert_eq!(dto.project.as_ref().unwrap().id, 9);
        assert_eq!(dto.lane.as_ref().unwrap().id, 4);
        assert_eq!(dto.label_ids, vec![1, 2]);
        assert_eq!(dto.next_run_on.as_deref(), Some("2026-11-06"));
        assert!(dto.active);
    }

    #[test]
    fn decodes_null_assignee_project_lane_and_empty_labels() {
        let json = r#"{"id":6,"title":"Invoice","description":null,"priority":"no_priority",
          "frequency":"monthly","interval":1,"weekday":null,"day_of_month":1,
          "time_zone":null,"active":false,"next_run_on":null,"last_run_at":null,
          "schedule_summary":"Every month on the 1st",
          "assignee":null,"project":null,"lane":null,"label_ids":[],"creator":null,
          "created_at":"2026-10-01T00:00:00.000Z","updated_at":"2026-10-02T00:00:00.000Z"}"#;
        let dto: RecurringIssueDto = serde_json::from_str(json).unwrap();
        assert!(dto.assignee.is_none() && dto.project.is_none() && dto.lane.is_none());
        assert!(dto.label_ids.is_empty());
        assert!(dto.time_zone.is_none() && dto.description.is_none());
        assert!(!dto.active);
        assert_eq!(dto.day_of_month, Some(1));
    }
}
