//! What pushing one queue entry can come to, and how API errors map onto it.

use rusqlite::Connection;

use crate::api::ApiError;
use crate::db::queries;
use crate::sync::engine::PushPause;

#[derive(Debug)]
pub(super) enum Outcome {
    /// Entry pushed successfully, or didn't need pushing.
    Success,
    /// Create pushed; the remote id and the synced mark were already saved in
    /// one transaction, so `record` must not mark the entry again.
    Created,
    /// Permanent failure for this entry — mark synced so we don't retry forever.
    DropAndLog(String),
    /// Transient failure — leave pending so the next drain retries.
    LeavePending(String),
    /// Keep this entry pending AND skip the remaining pushes this drain.
    PausePushes(PushPause, String),
    /// Team-access 404: keep the entry pending, flag the team (by Rails id) as
    /// inaccessible, and carry on with other teams' entries.
    TeamAccessLost(i64, String),
}

/// `Done` short-circuits the API call (e.g. idempotency hit, missing FK).
/// `Ready` carries the prepared payload for the API call.
pub(super) enum Prep<T> {
    Ready(T),
    Done(Outcome),
}

pub(super) fn require_remote_id(conn: &Connection, table: &str, id: &str) -> Result<i64, Outcome> {
    match queries::remote_id_for(conn, table, id) {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Err(Outcome::LeavePending(format!(
            "{table} {id} not synced yet"
        ))),
        Err(e) => Err(Outcome::LeavePending(format!("db: {e}"))),
    }
}

pub(super) fn optional_remote_id(
    conn: &Connection,
    table: &str,
    id: Option<&str>,
) -> Result<Option<i64>, Outcome> {
    match id {
        Some(uuid) => require_remote_id(conn, table, uuid).map(Some),
        None => Ok(None),
    }
}

/// Outcome for a write whose target may already be gone on the server: success
/// (running `on_ok` with the response) and a 404 both mean done; anything else
/// is classified as usual.
pub(super) fn done_if_gone<T>(result: Result<T, ApiError>, on_ok: impl FnOnce(T)) -> Outcome {
    match result {
        Ok(v) => {
            on_ok(v);
            Outcome::Success
        }
        Err(ApiError::NotFound(_)) => Outcome::Success,
        Err(e) => classify(&e),
    }
}

// ── Error classification ──────────────────────────────────────────────────

/// Rails' message for a 404 on the team scope (unknown, archived, or outside
/// the token). Resource 404s ("Issue not found", ...) use different messages.
const TEAM_ACCESS_MESSAGE: &str = "Team not found or access denied";

pub(super) fn is_team_access_message(m: &str) -> bool {
    m.trim() == TEAM_ACCESS_MESSAGE
}

/// Rails' 403 message when the API token lacks the scope for the request
/// (base_controller `authorize_api_token_scope!`). Other 403s carry
/// per-record messages.
const TOKEN_SCOPE_MESSAGE: &str = "Token lacks required scope";

pub(super) fn is_token_scope_message(m: &str) -> bool {
    m.trim() == TOKEN_SCOPE_MESSAGE
}

/// Like [`classify`], but a team-access 404 flags `team_remote_id` as lost.
pub(super) fn classify_in_team(err: &ApiError, team_remote_id: i64) -> Outcome {
    match err {
        ApiError::NotFound(m) if is_team_access_message(m) => {
            Outcome::TeamAccessLost(team_remote_id, format!("team access lost: {m}"))
        }
        other => classify(other),
    }
}

pub(super) fn classify(err: &ApiError) -> Outcome {
    match err {
        ApiError::Server { status, message } => {
            if *status >= 500 {
                Outcome::LeavePending(format!("server {status}: {message}"))
            } else {
                Outcome::DropAndLog(format!("server {status}: {message}"))
            }
        }
        ApiError::Network(e) => Outcome::LeavePending(format!("network: {e}")),
        ApiError::MissingToken => Outcome::LeavePending("missing api token".into()),
        ApiError::Validation { errors } => {
            Outcome::DropAndLog(format!("validation: {}", errors.join("; ")))
        }
        // Keep the entry pending: an expired/revoked token is recoverable — the
        // user re-enters one in Settings (sync pauses meanwhile) and the next
        // drain pushes the still-queued edit rather than silently losing it.
        ApiError::Unauthorized(m) => Outcome::LeavePending(format!("unauthorized: {m}")),
        // Only Rails' "Token lacks required scope" 403 means the whole token
        // cannot write: keep it queued and stop pushing; a better token can
        // send it later. Any other 403 (e.g. "Only the author or a team admin
        // can ...") is about this one entry: keep it pending, never drop it,
        // and carry on with the rest of the queue.
        ApiError::Forbidden(m) if is_token_scope_message(m) => {
            Outcome::PausePushes(PushPause::Forbidden, format!("forbidden: {m}"))
        }
        ApiError::Forbidden(m) => Outcome::LeavePending(format!("forbidden: {m}")),
        ApiError::NotFound(m) => Outcome::DropAndLog(format!("not found: {m}")),
        ApiError::Decode(m) => Outcome::DropAndLog(format!("decode: {m}")),
        ApiError::EnumMapping(m) => Outcome::DropAndLog(format!("enum: {m}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::SyncQueueEntry;
    use crate::sync::engine::SyncReport;
    use crate::sync::push::record;
    use crate::sync::push::fixtures::{is_drop, is_retry};

    #[test]
    fn classify_5xx_is_retry() {
        assert!(is_retry(&classify(&ApiError::Server {
            status: 500,
            message: "boom".into()
        })));
        assert!(is_retry(&classify(&ApiError::Server {
            status: 503,
            message: "boom".into()
        })));
    }

    #[test]
    fn classify_4xx_is_drop() {
        assert!(is_drop(&classify(&ApiError::Server {
            status: 418,
            message: "teapot".into()
        })));
        assert!(is_drop(&classify(&ApiError::NotFound("gone".into()))));
    }

    #[test]
    fn classify_non_scope_forbidden_stays_pending_without_pausing() {
        for m in [
            "Only the author or a team admin can delete this",
            "Bootstrap token required",
        ] {
            match classify(&ApiError::Forbidden(m.into())) {
                Outcome::LeavePending(r) => assert_eq!(r, format!("forbidden: {m}")),
                other => panic!("expected leave-pending for {m:?}, got {other:?}"),
            }
        }
        let entry = SyncQueueEntry {
            id: Some(1),
            entity_type: "comment".into(),
            entity_id: "c1".into(),
            action: "delete".into(),
            payload: None,
            created_at: String::new(),
            synced_at: None,
        };
        let mut report = SyncReport::default();
        let outcome = classify(&ApiError::Forbidden("Only the author can".into()));
        assert!(!record(&mut report, &entry, 1, outcome));
        assert_eq!(report.push_paused, None);
    }

    #[test]
    fn classify_forbidden_keeps_pending_and_pauses() {
        match classify(&ApiError::Forbidden("Token lacks required scope".into())) {
            Outcome::PausePushes(PushPause::Forbidden, m) => assert!(m.starts_with("forbidden:")),
            other => panic!("expected pause, got {other:?}"),
        }
    }

    #[test]
    fn team_access_404_is_told_apart_from_resource_404() {
        let team = ApiError::NotFound("Team not found or access denied".into());
        match classify_in_team(&team, 42) {
            Outcome::TeamAccessLost(42, m) => assert!(m.starts_with("team access lost:")),
            other => panic!("expected team access lost, got {other:?}"),
        }
        // Resource 404s keep today's drop-and-log.
        for m in ["Issue not found", "Project not found", "Label not found"] {
            let o = classify_in_team(&ApiError::NotFound(m.into()), 42);
            assert!(is_drop(&o), "{m} should drop, got {o:?}");
        }
    }

    #[test]
    fn classify_unauthorized_is_retry() {
        // A 401 is recoverable: the user re-enters a token in Settings and the
        // still-pending entry drains on the next sync, so we must not drop it.
        assert!(is_retry(&classify(&ApiError::Unauthorized("nope".into()))));
    }

    #[test]
    fn classify_validation_is_drop() {
        let e = ApiError::Validation {
            errors: vec!["title can't be blank".into()],
        };
        match classify(&e) {
            Outcome::DropAndLog(m) => assert!(m.contains("title can't be blank")),
            other => panic!("expected drop, got {other:?}"),
        }
    }

    #[test]
    fn classify_missing_token_is_retry() {
        // MissingToken is recoverable: user can fix the token in settings and
        // the next drain will pick up.
        assert!(is_retry(&classify(&ApiError::MissingToken)));
    }

    #[test]
    fn done_if_gone_treats_success_and_404_as_done_and_classifies_the_rest() {
        let mut ran = false;
        assert!(matches!(done_if_gone(Ok(1), |_| ran = true), Outcome::Success));
        assert!(ran);
        assert!(matches!(
            done_if_gone(Err::<(), _>(ApiError::NotFound("gone".into())), |_| {}),
            Outcome::Success
        ));
        assert!(matches!(
            done_if_gone(Err::<(), _>(ApiError::MissingToken), |_| {}),
            Outcome::LeavePending(_)
        ));
    }
}
