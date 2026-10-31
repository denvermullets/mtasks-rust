//! `GET /api/v1/notifications`: the user's Notification feed.
//!
//! The wire DTOs live next to the mirror (`db::notifications`), which applies
//! them directly; they are re-exported here under API names so callers of the
//! API module need not know about the mirror.

use super::{encode_query_value, ApiClient, ApiError, Conditional};

#[allow(unused_imports)] // public DTO names for callers; the poll uses the envelope
pub use crate::db::notifications::{
    ActorIn as NotificationActorDto, Envelope as NotificationsEnvelope,
    IssueIn as NotificationIssueDto, MetaIn as NotificationsMetaDto,
    NotificationIn as NotificationDto,
};

/// Fetch the feed. `updated_since` (ISO8601) limits it to rows changed since;
/// `etag` is sent as `If-None-Match`. Never pages: the server returns its
/// default 50 newest. A 304 yields `NotModified`.
pub async fn list(
    client: &ApiClient,
    updated_since: Option<&str>,
    etag: Option<&str>,
) -> Result<Conditional<NotificationsEnvelope>, ApiError> {
    client
        .get_conditional(&list_path(updated_since), etag)
        .await
}

/// `PATCH /api/v1/notifications/:id/read`. Idempotent; the returned
/// Notification is not needed (the next poll delivers it).
pub async fn mark_read(client: &ApiClient, remote_id: i64) -> Result<(), ApiError> {
    let _: serde_json::Value = client
        .patch(&read_path(remote_id), &serde_json::json!({}))
        .await?;
    Ok(())
}

/// Response of `PATCH /notifications/read_all`.
#[derive(Debug, serde::Deserialize)]
pub struct ReadAllResponse {
    pub unread_count: i64,
}

/// `PATCH /api/v1/notifications/read_all` with `{"before": ts}`: marks every
/// unread Notification created at or before `before`. Idempotent.
pub async fn mark_all_read(client: &ApiClient, before: &str) -> Result<ReadAllResponse, ApiError> {
    client
        .patch(READ_ALL_PATH, &serde_json::json!({ "before": before }))
        .await
}

const READ_ALL_PATH: &str = "api/v1/notifications/read_all";

fn read_path(remote_id: i64) -> String {
    format!("api/v1/notifications/{remote_id}/read")
}

fn list_path(updated_since: Option<&str>) -> String {
    match updated_since {
        Some(t) => format!(
            "api/v1/notifications?updated_since={}",
            encode_query_value(t)
        ),
        None => "api/v1/notifications".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_path_targets_the_member_route() {
        assert_eq!(read_path(12), "api/v1/notifications/12/read");
    }

    #[test]
    fn read_all_path_and_response_decode() {
        assert_eq!(READ_ALL_PATH, "api/v1/notifications/read_all");
        let r: ReadAllResponse = serde_json::from_str(r#"{"marked": 3, "unread_count": 1}"#).unwrap();
        assert_eq!(r.unread_count, 1);
    }

    const SAMPLE: &str = r#"{
      "notifications": [
        {"id": 12, "action": "mention", "message": "Ann mentioned you",
         "read_at": null, "created_at": "2026-10-30T10:00:00Z",
         "updated_at": "2026-10-30T10:00:00Z",
         "actor": {"id": 3, "name": "Ann"},
         "issue": {"id": 99, "identifier": "ABC-4", "title": "Fix", "team_id": 7},
         "comment_id": null, "version_id": 55},
        {"id": 11, "action": "assigned", "message": "Assigned",
         "read_at": "2026-10-30T11:00:00Z", "created_at": "2026-10-29T10:00:00Z",
         "updated_at": "2026-10-30T11:00:00Z",
         "actor": {"id": 3, "name": "Ann"},
         "issue": {"id": 99, "identifier": "ABC-4", "title": "Fix", "team_id": 7},
         "comment_id": 501, "version_id": null}
      ],
      "meta": {"unread_count": 1, "server_time": "2026-10-30T12:00:00.123Z"}
    }"#;

    #[test]
    fn decodes_envelope_with_null_read_at_and_comment_id() {
        let env: NotificationsEnvelope = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(env.notifications.len(), 2);
        assert_eq!(env.notifications[0].read_at, None);
        assert_eq!(env.notifications[0].comment_id, None);
        assert_eq!(env.notifications[1].comment_id, Some(501));
        assert!(env.notifications[1].read_at.is_some());
        assert_eq!(env.meta.unread_count, 1);
        assert_eq!(env.meta.server_time, "2026-10-30T12:00:00.123Z");
    }

    #[test]
    fn path_encodes_updated_since() {
        assert_eq!(list_path(None), "api/v1/notifications");
        assert_eq!(
            list_path(Some("2026-10-30T12:00:00Z")),
            "api/v1/notifications?updated_since=2026-10-30T12%3A00%3A00Z"
        );
    }
}
