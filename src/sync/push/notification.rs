//! Pushing notification read state.

use crate::api::{ApiClient, ApiError};
use crate::sync::engine::open;
use super::outcome::{Outcome, done_if_gone};

/// `PATCH /notifications/:id/read`. Idempotent, so a retry is safe.
pub(super) async fn notification_read(client: &ApiClient, entity_id: &str) -> Outcome {
    let Ok(remote_id) = entity_id.parse::<i64>() else {
        return Outcome::DropAndLog(format!("bad notification id {entity_id}"));
    };
    notification_read_outcome(crate::api::notifications::mark_read(client, remote_id).await)
}

/// `PATCH /notifications/read_all` with the queued `before` timestamp, then
/// correct the badge from the response.
pub(super) async fn notification_read_all(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let Some(before) = notification_read_all_before(payload) else {
        return Outcome::DropAndLog("read_all without a before timestamp".into());
    };
    let result = crate::api::notifications::mark_all_read(client, &before).await;
    if let Ok(resp) = &result {
        if let Ok(conn) = open() {
            let _ = crate::db::notifications::set_unread_count(&conn, resp.unread_count);
        }
    }
    notification_read_outcome(result.map(|_| ()))
}

fn notification_read_all_before(payload: Option<&str>) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(payload?).ok()?;
    v.get("before")?.as_str().map(str::to_string)
}

/// A 404 (not yours, or gone) means there is nothing left to mark: done.
fn notification_read_outcome(result: Result<(), ApiError>) -> Outcome {
    done_if_gone(result, |()| {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_all_before_parses_payload() {
        assert_eq!(
            notification_read_all_before(Some(r#"{"before":"2026-10-31T09:10:00.000Z"}"#)).as_deref(),
            Some("2026-10-31T09:10:00.000Z")
        );
        assert_eq!(notification_read_all_before(Some("{}")), None);
        assert_eq!(notification_read_all_before(None), None);
    }

    #[test]
    fn notification_read_404_counts_as_done() {
        assert!(matches!(notification_read_outcome(Ok(())), Outcome::Success));
        assert!(matches!(
            notification_read_outcome(Err(ApiError::NotFound("Not found".into()))),
            Outcome::Success
        ));
        assert!(matches!(
            notification_read_outcome(Err(ApiError::Unauthorized("x".into()))),
            Outcome::LeavePending(_)
        ));
    }
}
