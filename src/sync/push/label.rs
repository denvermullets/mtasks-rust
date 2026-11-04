//! Pushing label creates, updates and deletes.

use rusqlite::Connection;

use crate::api::{ApiClient, ApiError, LabelParams, labels};
use crate::db::queries;
use crate::sync::engine::open;
use super::outcome::{Outcome, Prep, classify, classify_in_team, is_team_access_message};
use super::reconcile::{commit_create, find_label_on_server, record_attempt};

// ── Labels ────────────────────────────────────────────────────────────────

struct LabelCreatePrep {
    team_remote_id: i64,
    params: LabelParams,
    local_id: String,
}

fn prep_label_create(entity_id: &str) -> Prep<LabelCreatePrep> {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Prep::Done(Outcome::LeavePending(e)),
    };
    let label = match queries::get_label(&conn, entity_id) {
        Ok(Some(l)) => l,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "label {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    if label.remote_id.is_some() {
        return Prep::Done(Outcome::Success);
    }
    let team_remote_id = match queries::remote_id_for(&conn, "teams", &label.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    Prep::Ready(LabelCreatePrep {
        team_remote_id,
        params: LabelParams {
            name: label.name,
            color: label.color,
        },
        local_id: label.id,
    })
}

pub(super) async fn label_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match prep_label_create(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    let attempt = match record_attempt(entry_id, "labels") {
        Ok(a) => a,
        Err(o) => return o,
    };
    if let Some(since) = &attempt.previous {
        match find_label_on_server(client, prep.team_remote_id, &prep.params, since, &attempt)
            .await
        {
            Ok(Some(remote_id)) => {
                return commit_create(entry_id, "labels", &prep.local_id, remote_id)
            }
            Ok(None) => {}
            Err(e) => return classify(&e),
        }
    }
    match labels::create(client, prep.team_remote_id, &prep.params).await {
        Ok(dto) => commit_create(entry_id, "labels", &prep.local_id, dto.id),
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

struct LabelUpdatePrep {
    team_remote_id: i64,
    remote_id: i64,
    params: LabelParams,
}

fn prep_label_update(entity_id: &str) -> Prep<LabelUpdatePrep> {
    match open() {
        Ok(conn) => label_update_prep(&conn, entity_id),
        Err(e) => Prep::Done(Outcome::LeavePending(e)),
    }
}

/// Re-reads the label row so the newest name and color go out, however many
/// edits were queued.
fn label_update_prep(conn: &Connection, entity_id: &str) -> Prep<LabelUpdatePrep> {
    let label = match queries::get_label(conn, entity_id) {
        Ok(Some(l)) => l,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "label {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let Some(remote_id) = label.remote_id else {
        return Prep::Done(Outcome::LeavePending("label create not yet drained".into()));
    };
    let team_remote_id = match queries::remote_id_for(conn, "teams", &label.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    Prep::Ready(LabelUpdatePrep {
        team_remote_id,
        remote_id,
        params: LabelParams {
            name: label.name,
            color: label.color,
        },
    })
}

pub(super) async fn label_update(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match prep_label_update(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    // A 422 (duplicate name on the server) is dropped; the next label pull
    // restores the server value.
    match labels::update(client, prep.team_remote_id, prep.remote_id, &prep.params).await {
        Ok(_) => Outcome::Success,
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

/// The local row is gone by drain time, so the payload captured at enqueue time
/// (`{team_remote_id, remote_id}`) is all a label delete has.
fn prep_label_delete(payload: Option<&str>) -> Prep<(i64, i64)> {
    let Some(raw) = payload else {
        return Prep::Done(Outcome::DropAndLog(
            "label delete: no payload (legacy entry)".into(),
        ));
    };
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "label delete: bad payload: {e}"
            )))
        }
    };
    match (
        parsed.get("team_remote_id").and_then(|v| v.as_i64()),
        parsed.get("remote_id").and_then(|v| v.as_i64()),
    ) {
        (Some(team), Some(id)) => Prep::Ready((team, id)),
        _ => Prep::Done(Outcome::DropAndLog(
            "label delete: missing ids in payload".into(),
        )),
    }
}

pub(super) async fn label_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let (team_remote_id, remote_id) = match prep_label_delete(payload) {
        Prep::Ready(ids) => ids,
        Prep::Done(o) => return o,
    };
    // 204 and 404 both mean the label is gone.
    match labels::delete(client, team_remote_id, remote_id).await {
        Ok(_) => Outcome::Success,
        Err(ApiError::NotFound(m)) if !is_team_access_message(&m) => Outcome::Success,
        Err(e) => classify_in_team(&e, team_remote_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::{insert_test_label, setup_labels_db};

    #[test]
    fn label_update_prep_rereads_row_and_waits_for_remote_id() {
        let conn = setup_labels_db();
        insert_test_label(&conn, "lab1", None);
        assert!(matches!(
            label_update_prep(&conn, "lab1"),
            Prep::Done(Outcome::LeavePending(_))
        ));
        queries::set_label_remote_id(&conn, "lab1", 11).unwrap();
        queries::edit_label(&conn, "lab1", "renamed", "#3b82f6").unwrap();
        match label_update_prep(&conn, "lab1") {
            Prep::Ready(p) => {
                assert_eq!((p.team_remote_id, p.remote_id), (700, 11));
                assert_eq!(p.params.name, "renamed");
                assert_eq!(p.params.color.as_deref(), Some("#3b82f6"));
            }
            _ => panic!("expected Ready"),
        }
        assert!(matches!(
            label_update_prep(&conn, "nope"),
            Prep::Done(Outcome::DropAndLog(_))
        ));
    }

    #[test]
    fn label_delete_prep_reads_only_the_payload() {
        match prep_label_delete(Some(r#"{"team_remote_id":700,"remote_id":11}"#)) {
            Prep::Ready(ids) => assert_eq!(ids, (700, 11)),
            _ => panic!("expected Ready"),
        }
        for bad in [None, Some("not json"), Some(r#"{"remote_id":11}"#)] {
            assert!(matches!(
                prep_label_delete(bad),
                Prep::Done(Outcome::DropAndLog(_))
            ));
        }
    }
}
