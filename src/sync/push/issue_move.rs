//! Pushing an issue move to another team.

use rusqlite::Connection;

use crate::api::{ApiClient, ApiError, issues};
use crate::api::dto::IssueDto;
use crate::db::queries;
use crate::sync::engine::open;
use super::outcome::{Outcome, Prep, classify_in_team};

// ── Issue move ────────────────────────────────────────────────────────────

struct IssueMovePrep {
    source_team_remote_id: i64,
    issue_remote_id: i64,
    target_team_remote_id: i64,
}

/// Resolve the move push from the payload captured at enqueue (the issue's row
/// has already left the source team). A remote id that was unknown then is
/// looked up again from the local row; the entry waits until all three exist.
fn prep_issue_move_in(
    conn: &Connection,
    entity_id: &str,
    payload: Option<&str>,
) -> Prep<IssueMovePrep> {
    let Some(raw) = payload else {
        return Prep::Done(Outcome::DropAndLog("issue move: no payload".into()));
    };
    let p: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => return Prep::Done(Outcome::DropAndLog(format!("issue move: bad payload: {e}"))),
    };
    match queries::get_issue(conn, entity_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "issue {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    }
    // Captured remote id if present, else the current remote id of the local row.
    let resolve = |key: &str, table: &str, local_id: &str, what: &str| -> Result<i64, Outcome> {
        if let Some(id) = p.get(key).and_then(|v| v.as_i64()) {
            return Ok(id);
        }
        match queries::remote_id_for(conn, table, local_id) {
            Ok(Some(id)) => Ok(id),
            Ok(None) => Err(Outcome::LeavePending(format!("{what} not synced yet"))),
            Err(e) => Err(Outcome::LeavePending(format!("db: {e}"))),
        }
    };
    let local = |key: &str| p.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let source = resolve("source_team_remote_id", "teams", &local("source_team_id"), "source team");
    let issue = resolve("issue_remote_id", "issues", entity_id, "issue");
    let target = resolve("target_team_remote_id", "teams", &local("target_team_id"), "target team");
    match (source, issue, target) {
        (Ok(s), Ok(i), Ok(t)) => Prep::Ready(IssueMovePrep {
            source_team_remote_id: s,
            issue_remote_id: i,
            target_team_remote_id: t,
        }),
        (Err(o), _, _) | (_, Err(o), _) | (_, _, Err(o)) => Prep::Done(o),
    }
}

pub(super) async fn issue_move(client: &ApiClient, entity_id: &str, payload: Option<&str>) -> Outcome {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Outcome::LeavePending(e),
    };
    let prep = match prep_issue_move_in(&conn, entity_id, payload) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    let result = issues::move_to_team(
        client,
        prep.source_team_remote_id,
        prep.issue_remote_id,
        prep.target_team_remote_id,
    )
    .await;
    issue_move_outcome(&conn, entity_id, prep.source_team_remote_id, result)
}

/// 200 applies the response; 404 (target outside the token, or issue gone) and
/// 422 (already there / not a member of both) drop the entry with the server's
/// message. The next full pull restores the issue's server state.
fn issue_move_outcome(
    conn: &Connection,
    entity_id: &str,
    source_team_remote_id: i64,
    result: Result<IssueDto, ApiError>,
) -> Outcome {
    match result {
        Ok(dto) => match apply_move_response(conn, entity_id, &dto) {
            Ok(()) => Outcome::Success,
            Err(e) => Outcome::LeavePending(format!("apply move: {e}")),
        },
        Err(ApiError::NotFound(m)) => Outcome::DropAndLog(format!("not found: {m}")),
        Err(e) => classify_in_team(&e, source_team_remote_id),
    }
}

/// Apply a move response as a pull would, but only the fields the move
/// decides: the server's team number (from `identifier`), lane, assignee and
/// parent. Other local edits are left alone. A ref that isn't known locally
/// keeps the local value.
fn apply_move_response(conn: &Connection, entity_id: &str, dto: &IssueDto) -> Result<(), String> {
    let issue = queries::get_issue(conn, entity_id)
        .map_err(|e| format!("db: {e}"))?
        .ok_or("issue missing locally")?;
    let team_number = dto
        .identifier
        .rsplit_once('-')
        .and_then(|(_, n)| n.parse::<i32>().ok())
        .or(issue.team_number);
    let lane_id = match &dto.lane {
        Some(r) => queries::find_lane_by_remote_id(conn, r.id)
            .map_err(|e| format!("db: {e}"))?
            .map(|l| l.id)
            .unwrap_or_else(|| issue.lane_id.clone()),
        None => issue.lane_id.clone(),
    };
    let assignee_id = match &dto.assignee {
        Some(r) => queries::find_member_by_remote_id(conn, r.id)
            .map_err(|e| format!("db: {e}"))?
            .map(|m| m.id)
            .or(issue.assignee_id.clone()),
        None => None,
    };
    let parent_issue_id = match &dto.parent_issue {
        Some(r) => queries::find_issue_by_remote_id(conn, r.id)
            .map_err(|e| format!("db: {e}"))?
            .map(|i| i.id)
            .or(issue.parent_issue_id.clone()),
        None => None,
    };
    conn.execute(
        "UPDATE issues SET team_number = ?1, lane_id = ?2, assignee_id = ?3, parent_issue_id = ?4
         WHERE id = ?5",
        rusqlite::params![team_number, lane_id, assignee_id, parent_issue_id, entity_id],
    )
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sync_queue;
    use crate::sync::push::fixtures::{is_drop, is_retry, moved_locally};
    use crate::sync::test_support::{moved_dto, setup_move_db};

    #[test]
    fn move_prep_ready_with_the_ids_captured_at_enqueue() {
        let (conn, payload) = moved_locally();
        match prep_issue_move_in(&conn, "i2", Some(&payload)) {
            Prep::Ready(p) => {
                assert_eq!(
                    (p.source_team_remote_id, p.issue_remote_id, p.target_team_remote_id),
                    (700, 2, 701)
                );
            }
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
    }

    #[test]
    fn move_prep_waits_until_the_target_team_has_a_remote_id() {
        let conn = setup_move_db();
        conn.execute("UPDATE teams SET remote_id = NULL WHERE id = 't2'", []).unwrap();
        queries::move_issue_to_team(&conn, "i2", "t2").unwrap();
        queries::queue_issue_move(&conn, "i2", "t1").unwrap();
        let payload = sync_queue::pending_entries(&conn).unwrap().remove(0).payload.unwrap();
        match prep_issue_move_in(&conn, "i2", Some(&payload)) {
            Prep::Done(o) => assert!(is_retry(&o)),
            Prep::Ready(_) => panic!("target team has no remote id yet"),
        }
        // The team create drains; the same payload now resolves it.
        conn.execute("UPDATE teams SET remote_id = 701 WHERE id = 't2'", []).unwrap();
        match prep_issue_move_in(&conn, "i2", Some(&payload)) {
            Prep::Ready(p) => assert_eq!(p.target_team_remote_id, 701),
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
    }

    #[test]
    fn move_prep_drops_a_missing_payload_or_issue() {
        let (conn, payload) = moved_locally();
        assert!(matches!(prep_issue_move_in(&conn, "i2", None), Prep::Done(o) if is_drop(&o)));
        assert!(
            matches!(prep_issue_move_in(&conn, "gone", Some(&payload)), Prep::Done(o) if is_drop(&o))
        );
    }

    #[test]
    fn move_response_sets_the_server_identifier_and_lane() {
        let (conn, _) = moved_locally();
        let before = queries::get_issue(&conn, "i2").unwrap().unwrap();
        assert_ne!(before.team_number, Some(9));
        let out = issue_move_outcome(&conn, "i2", 700, Ok(moved_dto()));
        assert!(matches!(out, Outcome::Success), "{out:?}");
        let i = queries::get_issue(&conn, "i2").unwrap().unwrap();
        assert_eq!((i.team_id.as_str(), i.lane_id.as_str(), i.team_number), ("t2", "l2", Some(9)));
        // Only identifier/lane/assignee/parent come from the response.
        assert_eq!(i.title, before.title);
    }

    #[test]
    fn move_rejections_drop_the_entry_and_server_errors_retry() {
        let (conn, _) = moved_locally();
        let nf = ApiError::NotFound("Team not found or access denied".into());
        assert!(is_drop(&issue_move_outcome(&conn, "i2", 700, Err(nf))));
        let inv = ApiError::Validation { errors: vec!["Issue is already in that team".into()] };
        match issue_move_outcome(&conn, "i2", 700, Err(inv)) {
            Outcome::DropAndLog(m) => assert!(m.contains("already in that team"), "{m}"),
            o => panic!("{o:?}"),
        }
        let down = ApiError::Server { status: 503, message: "x".into() };
        assert!(is_retry(&issue_move_outcome(&conn, "i2", 700, Err(down))));
    }
}
