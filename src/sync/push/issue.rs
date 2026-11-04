//! Pushing issue creates, updates and deletes.

use rusqlite::Connection;

use crate::api::{ApiClient, ApiError, IssueParams, issues};
use crate::db::{attachments, queries, sync_queue};
use crate::db::models::{Issue, Priority};
use crate::sync::engine::{CycleCtx, open};
use super::attachment::{InlineImages, prepare_body, settle_inline};
use super::outcome::{
    Outcome, Prep, classify_in_team, is_team_access_message, optional_remote_id,
    require_remote_id,
};
use super::reconcile::{commit_create, find_issue_on_server, record_attempt};

// ── Issues ────────────────────────────────────────────────────────────────

struct IssueCreatePrep {
    team_remote_id: i64,
    params: IssueParams,
    local_id: String,
    inline: InlineImages,
}

struct IssueUpdatePrep {
    team_remote_id: i64,
    remote_id: i64,
    params: IssueParams,
    local_id: String,
    inline: InlineImages,
}

fn prep_issue_create(entity_id: &str) -> Prep<IssueCreatePrep> {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Prep::Done(Outcome::LeavePending(e)),
    };
    let issue = match queries::get_issue(&conn, entity_id) {
        Ok(Some(i)) => i,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "issue {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };

    if issue.remote_id.is_some() {
        // Idempotency: Rails already has this row.
        return Prep::Done(Outcome::Success);
    }

    let team_remote_id = match queries::remote_id_for(&conn, "teams", &issue.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return Prep::Done(Outcome::LeavePending(format!(
                "team {} not synced yet",
                issue.team_id
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };

    let mut params = match build_issue_params(&conn, &issue) {
        Ok(p) => p,
        Err(o) => return Prep::Done(o),
    };
    let inline = match prepare_body(&conn, attachments::OwnerKind::Issue, &issue.id, params.description.take()) {
        Ok((d, i)) => {
            params.description = d;
            i
        }
        Err(o) => return Prep::Done(o),
    };

    Prep::Ready(IssueCreatePrep {
        team_remote_id,
        params,
        local_id: issue.id,
        inline,
    })
}

fn prep_issue_update(entity_id: &str, entry_id: i64) -> Prep<IssueUpdatePrep> {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Prep::Done(Outcome::LeavePending(e)),
    };
    prep_issue_update_in(&conn, entity_id, entry_id)
}

fn prep_issue_update_in(conn: &Connection, entity_id: &str, entry_id: i64) -> Prep<IssueUpdatePrep> {
    let issue = match queries::get_issue(conn, entity_id) {
        Ok(Some(i)) => i,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "issue {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };

    let remote_id = match issue.remote_id {
        Some(id) => id,
        None => {
            return Prep::Done(Outcome::LeavePending(format!(
                "issue {entity_id} has no remote_id — create not yet drained"
            )))
        }
    };

    let source_team = match move_order(conn, entity_id, entry_id) {
        Ok(s) => s,
        Err(o) => return Prep::Done(o),
    };

    let team_remote_id = match source_team {
        Some(src) => src,
        None => match queries::remote_id_for(conn, "teams", &issue.team_id) {
            Ok(Some(id)) => id,
            Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
            Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
        },
    };

    let mut params = match build_issue_params(conn, &issue) {
        Ok(p) => p,
        Err(o) => return Prep::Done(o),
    };
    if source_team.is_some() {
        // Queued before a move: the row already shows the post-move lane,
        // project, labels, assignee and parent, which the source team doesn't
        // know. The move resets those server-side anyway, so send only the
        // team-independent fields.
        params.lane_id = None;
        params.project_id = None;
        params.assignee_id = None;
        params.parent_issue_id = None;
        params.label_ids = None;
    }
    let inline = match prepare_body(conn, attachments::OwnerKind::Issue, &issue.id, params.description.take()) {
        Ok((d, i)) => {
            params.description = d;
            i
        }
        Err(o) => return Prep::Done(o),
    };

    Prep::Ready(IssueUpdatePrep {
        team_remote_id,
        remote_id,
        params,
        local_id: issue.id,
        inline,
    })
}

/// Where an entry for `issue_id` stands relative to the issue's queued
/// `issue/move` entries. The drain is FIFO but does not stop at a pending
/// entry, and the issue's local row already shows the post-move team, so
/// per-issue entries must check:
/// - an earlier move is still pending: wait (`LeavePending`), so the entry goes
///   to the new team's URL only after the server has moved the issue;
/// - a later move is pending: `Ok(Some(source team remote id))`, so the entry
///   still goes to the team the server has the issue in;
/// - otherwise `Ok(None)`: use the row's team.
///
/// A move that was dropped (404/422) is no longer pending, so later entries
/// proceed against the local row's team; if the server still has the issue
/// elsewhere they 404 and are dropped, and the next full pull restores the
/// issue to its server state.
pub(super) fn move_order(conn: &Connection, issue_id: &str, entry_id: i64) -> Result<Option<i64>, Outcome> {
    let pending = sync_queue::pending_entries(conn)
        .map_err(|e| Outcome::LeavePending(format!("db: {e}")))?;
    let mut moves = pending
        .iter()
        .filter(|e| e.entity_type == "issue" && e.action == "move" && e.entity_id == issue_id);
    if moves.clone().any(|m| m.id.is_some_and(|id| id < entry_id)) {
        return Err(Outcome::LeavePending(
            "waiting for an earlier move of this issue".into(),
        ));
    }
    let Some(next) = moves.find(|m| m.id.is_some_and(|id| id > entry_id)) else {
        return Ok(None);
    };
    let payload: serde_json::Value = next
        .payload
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    if let Some(id) = payload.get("source_team_remote_id").and_then(|v| v.as_i64()) {
        return Ok(Some(id));
    }
    let source_local = payload
        .get("source_team_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    match queries::remote_id_for(conn, "teams", source_local) {
        Ok(Some(id)) => Ok(Some(id)),
        Ok(None) => Err(Outcome::LeavePending("source team not synced yet".into())),
        Err(e) => Err(Outcome::LeavePending(format!("db: {e}"))),
    }
}

pub(super) async fn issue_create(
    client: &ApiClient,
    ctx: &CycleCtx,
    entity_id: &str,
    entry_id: i64,
) -> Outcome {
    let prep = match prep_issue_create(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };

    let attempt = match record_attempt(entry_id, "issues") {
        Ok(a) => a,
        Err(o) => return o,
    };
    if let Some(since) = &attempt.previous {
        match find_issue_on_server(
            client,
            ctx,
            prep.team_remote_id,
            &prep.params,
            since,
            &attempt,
        )
        .await
        {
            Ok(Some(remote_id)) => {
                return commit_create(entry_id, "issues", &prep.local_id, remote_id)
            }
            Ok(None) => {}
            Err(o) => return o,
        }
    }

    match issues::create(client, prep.team_remote_id, &prep.params).await {
        Ok(dto) => {
            let out = commit_create(entry_id, "issues", &prep.local_id, dto.id);
            settle_inline(attachments::OwnerKind::Issue, &prep.local_id, &prep.inline);
            out
        }
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

pub(super) async fn issue_update(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match prep_issue_update(entity_id, entry_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };

    match issues::update(client, prep.team_remote_id, prep.remote_id, &prep.params).await {
        Ok(_) => {
            settle_inline(attachments::OwnerKind::Issue, &prep.local_id, &prep.inline);
            Outcome::Success
        }
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

pub(super) async fn issue_delete(
    client: &ApiClient,
    local_id: &str,
    payload: Option<&str>,
    entry_id: i64,
) -> Outcome {
    // The payload's team was captured at enqueue; after a pending move it is
    // the new team, which the server doesn't have the issue in yet.
    match open().map(|c| move_order(&c, local_id, entry_id)) {
        Ok(Err(o)) => return o,
        Ok(Ok(_)) => {}
        Err(e) => return Outcome::LeavePending(e),
    }
    let raw = match payload {
        Some(s) => s,
        None => return Outcome::DropAndLog("issue delete: no payload (legacy entry)".into()),
    };
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => return Outcome::DropAndLog(format!("issue delete: bad payload: {e}")),
    };
    let remote_id = match parsed.get("remote_id").and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => return Outcome::DropAndLog("issue delete: missing remote_id in payload".into()),
    };
    let team_remote_id = match parsed.get("team_remote_id").and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => {
            return Outcome::DropAndLog("issue delete: missing team_remote_id in payload".into())
        }
    };
    let result = issues::delete(client, team_remote_id, remote_id).await;
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Outcome::LeavePending(e),
    };
    issue_delete_outcome(&conn, local_id, team_remote_id, result)
}

/// Map the DELETE result to an outcome; on success (or already gone) purge the
/// local tombstone and its attached rows before the entry is marked synced.
/// A resource 404 ("Issue not found") means the issue is already gone, which
/// is what the delete wanted. A team-access 404 keeps the tombstone pending
/// and flags the team (via `classify_in_team`).
fn issue_delete_outcome(
    conn: &Connection,
    local_id: &str,
    team_remote_id: i64,
    result: Result<(), ApiError>,
) -> Outcome {
    match result {
        Ok(_) => purge_local_issue(conn, local_id),
        Err(ApiError::NotFound(m)) if !is_team_access_message(&m) => {
            purge_local_issue(conn, local_id)
        }
        Err(e) => classify_in_team(&e, team_remote_id),
    }
}

fn purge_local_issue(conn: &Connection, local_id: &str) -> Outcome {
    match queries::purge_tombstoned_issue(conn, local_id) {
        Ok(()) => Outcome::Success,
        Err(e) => Outcome::LeavePending(format!("purge issue: {e}")),
    }
}

fn build_issue_params(conn: &Connection, issue: &Issue) -> Result<IssueParams, Outcome> {
    let lane_id = require_remote_id(conn, "lanes", &issue.lane_id)?;
    let project_id = optional_remote_id(conn, "projects", issue.project_id.as_deref())?;
    let assignee_id = optional_remote_id(conn, "members", issue.assignee_id.as_deref())?;
    let parent_issue_id = optional_remote_id(conn, "issues", issue.parent_issue_id.as_deref())?;
    let label_ids = build_label_ids(conn, &issue.id)?;

    Ok(IssueParams {
        title: Some(issue.title.clone()),
        description: issue.description.clone(),
        lane_id: Some(lane_id),
        priority: Some(
            Priority::from_i32(issue.priority)
                .as_rails_str()
                .to_string(),
        ),
        estimate: issue.estimate.map(|e| e as i64),
        due_date: issue.due_date.clone(),
        assignee_id,
        project_id,
        parent_issue_id,
        label_ids,
    })
}

/// Resolve the issue's locally-attached labels to their Rails remote ids.
///
/// Always returns `Some(..)` (possibly empty) so the push asserts the full
/// label set — Rails' `label_ids=` setter treats `[]` as "detach all", which is
/// exactly what we want when the user clears an issue's labels locally. If any
/// attached label hasn't been pushed yet (no `remote_id`), we leave the entry
/// pending so the label create drains first and ordering is preserved.
fn build_label_ids(conn: &Connection, issue_id: &str) -> Result<Option<Vec<i64>>, Outcome> {
    let local_ids = queries::label_ids_for_issue(conn, issue_id)
        .map_err(|e| Outcome::LeavePending(format!("db (issue labels): {e}")))?;
    let mut remote_ids = Vec::with_capacity(local_ids.len());
    for local in &local_ids {
        remote_ids.push(require_remote_id(conn, "labels", local)?);
    }
    Ok(Some(remote_ids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::engine::PushPause;
    use crate::sync::push::fixtures::{is_retry, moved_locally};
    use crate::sync::test_support::{
        assert_purged, count, insert_test_label, setup_labels_db, setup_move_db,
        setup_tombstoned_issue,
    };

    #[test]
    fn build_label_ids_resolves_remote_ids() {
        let conn = setup_labels_db();
        insert_test_label(&conn, "lab1", Some(11));
        insert_test_label(&conn, "lab2", Some(22));
        queries::set_issue_labels(&conn, "i1", &["lab1".into(), "lab2".into()]).unwrap();

        let mut ids = build_label_ids(&conn, "i1").unwrap().unwrap();
        ids.sort();
        assert_eq!(ids, vec![11, 22]);
    }

    #[test]
    fn issue_delete_204_purges_tombstone_and_attachments() {
        let conn = setup_tombstoned_issue();
        assert!(matches!(
            issue_delete_outcome(&conn, "i1", 42, Ok(())),
            Outcome::Success
        ));
        assert_purged(&conn);
    }

    #[test]
    fn issue_delete_resource_404_counts_as_gone() {
        let conn = setup_tombstoned_issue();
        let r = Err(ApiError::NotFound("Issue not found".into()));
        assert!(matches!(issue_delete_outcome(&conn, "i1", 42, r), Outcome::Success));
        assert_purged(&conn);
    }

    #[test]
    fn issue_delete_403_pauses_and_keeps_tombstone() {
        let conn = setup_tombstoned_issue();
        let r = Err(ApiError::Forbidden("Token lacks required scope".into()));
        assert!(matches!(
            issue_delete_outcome(&conn, "i1", 42, r),
            Outcome::PausePushes(PushPause::Forbidden, _)
        ));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issues WHERE id='i1'"), 1);
    }

    #[test]
    fn issue_delete_team_access_404_is_not_success_and_keeps_tombstone() {
        let conn = setup_tombstoned_issue();
        let r = Err(ApiError::NotFound("Team not found or access denied".into()));
        assert!(matches!(
            issue_delete_outcome(&conn, "i1", 42, r),
            Outcome::TeamAccessLost(42, _)
        ));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issues WHERE id='i1'"), 1);
    }

    #[test]
    fn build_label_ids_empty_when_no_labels() {
        let conn = setup_labels_db();
        // No labels attached → assert an empty set (Rails detaches all).
        assert_eq!(build_label_ids(&conn, "i1").unwrap(), Some(vec![]));
    }

    #[test]
    fn build_label_ids_leaves_pending_for_unsynced_label() {
        let conn = setup_labels_db();
        // Label exists locally but hasn't been pushed yet (no remote_id).
        insert_test_label(&conn, "lab1", None);
        queries::set_issue_labels(&conn, "i1", &["lab1".into()]).unwrap();

        match build_label_ids(&conn, "i1") {
            Err(Outcome::LeavePending(msg)) => assert!(msg.contains("labels lab1")),
            other => panic!("expected LeavePending, got {other:?}"),
        }
    }

    // ── Per-issue ordering around a queued move ──────────────────────────

    fn queue_update(conn: &Connection) -> i64 {
        sync_queue::enqueue(conn, "issue", "i2", "update", None).unwrap();
        conn.last_insert_rowid()
    }

    fn move_entry_id(conn: &Connection) -> i64 {
        sync_queue::pending_entries(conn)
            .unwrap()
            .into_iter()
            .find(|e| e.action == "move")
            .and_then(|e| e.id)
            .unwrap()
    }

    #[test]
    fn update_queued_after_a_move_waits_until_the_move_drains() {
        let (conn, _) = moved_locally();
        let update = queue_update(&conn);
        match prep_issue_update_in(&conn, "i2", update) {
            Prep::Done(o) => assert!(is_retry(&o), "{o:?}"),
            Prep::Ready(_) => panic!("update must wait for the pending move"),
        }
        // Once the move succeeded, the update goes to the new team's URL.
        sync_queue::mark_synced(&conn, move_entry_id(&conn)).unwrap();
        match prep_issue_update_in(&conn, "i2", update) {
            Prep::Ready(p) => assert_eq!((p.team_remote_id, p.remote_id), (701, 2)),
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
    }

    #[test]
    fn update_queued_before_a_move_goes_to_the_source_team() {
        let conn = setup_move_db();
        let update = queue_update(&conn);
        queries::move_issue_to_team(&conn, "i2", "t2").unwrap();
        assert!(queries::queue_issue_move(&conn, "i2", "t1").unwrap());
        assert!(update < move_entry_id(&conn));
        match prep_issue_update_in(&conn, "i2", update) {
            Prep::Ready(p) => {
                assert_eq!((p.team_remote_id, p.remote_id), (700, 2));
                // Post-move lane/labels aren't sent to the old team.
                assert_eq!(p.params.lane_id, None);
                assert_eq!(p.params.label_ids, None);
                assert!(p.params.title.is_some());
            }
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
    }

    #[test]
    fn a_dropped_move_does_not_block_later_updates() {
        let (conn, _) = moved_locally();
        let update = queue_update(&conn);
        // A 422 drops the entry: it is marked synced and no longer pending.
        sync_queue::mark_synced(&conn, move_entry_id(&conn)).unwrap();
        assert!(matches!(prep_issue_update_in(&conn, "i2", update), Prep::Ready(_)));
    }

    #[test]
    fn other_issues_ignore_the_move() {
        let (conn, _) = moved_locally();
        assert_eq!(move_order(&conn, "i1", i64::MAX).unwrap(), None);
    }
}
