//! Pushing issue dependency creates and deletes.

use crate::api::{ApiClient, Direction, dependencies};
use crate::db::queries;
use crate::sync::engine::open;
use super::issue::move_order;
use super::outcome::{Outcome, Prep, classify_in_team};
use super::reconcile::commit_create;

// ── Issue dependencies ────────────────────────────────────────────────────

struct DependencyCreatePrep {
    team_remote_id: i64,
    issue_remote_id: i64,
    target_remote_id: i64,
    direction: Direction,
    local_id: String,
}

fn prep_dependency_create(entity_id: &str, entry_id: i64) -> Prep<DependencyCreatePrep> {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Prep::Done(Outcome::LeavePending(e)),
    };
    let dep = match queries::get_issue_dependency(&conn, entity_id) {
        Ok(Some(d)) => d,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "dependency {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    if dep.remote_id.is_some() {
        return Prep::Done(Outcome::Success);
    }
    let blocking = match queries::get_issue(&conn, &dep.blocking_issue_id) {
        Ok(Some(i)) => i,
        Ok(None) => return Prep::Done(Outcome::DropAndLog("blocking issue missing".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let blocked = match queries::get_issue(&conn, &dep.blocked_issue_id) {
        Ok(Some(i)) => i,
        Ok(None) => return Prep::Done(Outcome::DropAndLog("blocked issue missing".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let (issue_remote_id, target_remote_id) = match (blocking.remote_id, blocked.remote_id) {
        (Some(b), Some(t)) => (b, t),
        _ => {
            return Prep::Done(Outcome::LeavePending(
                "dependency endpoints not yet synced".into(),
            ))
        }
    };
    let team_remote_id = match move_order(&conn, &blocking.id, entry_id) {
        Ok(Some(src)) => src,
        Ok(None) => match queries::remote_id_for(&conn, "teams", &blocking.team_id) {
            Ok(Some(id)) => id,
            Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
            Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
        },
        Err(o) => return Prep::Done(o),
    };
    Prep::Ready(DependencyCreatePrep {
        team_remote_id,
        issue_remote_id,
        target_remote_id,
        direction: dependencies::direction_for_edge(&dep.kind),
        local_id: dep.id,
    })
}

pub(super) async fn dependency_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match prep_dependency_create(entity_id, entry_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    match dependencies::create(
        client,
        prep.team_remote_id,
        prep.issue_remote_id,
        prep.target_remote_id,
        prep.direction,
    )
    .await
    {
        Ok(resp) => {
            // The create is posted from the source side, so the response
            // direction is already relative to it; store what Rails echoed.
            let conn = match open() {
                Ok(c) => c,
                Err(e) => return Outcome::LeavePending(e),
            };
            if let Err(e) = queries::set_issue_dependency_sync_fields(
                &conn,
                &prep.local_id,
                resp.id,
                &resp.kind,
                &resp.direction,
            ) {
                return Outcome::LeavePending(format!("write dependency fields: {e}"));
            }
            commit_create(entry_id, "issue_dependencies", &prep.local_id, resp.id)
        }
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

/// Deletes a dependency's Rails record from the queue payload.
///
/// The local row (and its remote_id) is gone by drain time, so — like
/// issue_delete — this handler relies entirely on the payload,
/// NOT on re-reading local state. The enqueue site (JAIT-114) must capture, before
/// deleting the local row:
///   { "team_remote_id":  <blocking issue's team remote_id>,
///     "issue_remote_id": <blocking issue's remote_id>,
///     "remote_id":       <dependency row's own remote_id / Rails dep id> }
/// and must skip enqueueing entirely when the dependency has no remote_id.
pub(super) async fn dependency_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let raw = match payload {
        Some(s) => s,
        None => return Outcome::DropAndLog("dependency delete: no payload (legacy entry)".into()),
    };
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => return Outcome::DropAndLog(format!("dependency delete: bad payload: {e}")),
    };
    let team_remote_id = match parsed.get("team_remote_id").and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => {
            return Outcome::DropAndLog(
                "dependency delete: missing team_remote_id in payload".into(),
            )
        }
    };
    let issue_remote_id = match parsed.get("issue_remote_id").and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => {
            return Outcome::DropAndLog(
                "dependency delete: missing issue_remote_id in payload".into(),
            )
        }
    };
    let remote_id = match parsed.get("remote_id").and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => {
            return Outcome::DropAndLog("dependency delete: missing remote_id in payload".into())
        }
    };
    match dependencies::delete(client, team_remote_id, issue_remote_id, remote_id).await {
        Ok(_) => Outcome::Success,
        Err(e) => classify_in_team(&e, team_remote_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::setup_dependency_db;

    #[test]
    fn create_response_fields_are_stored() {
        let conn = setup_dependency_db();
        conn.execute(
            "INSERT INTO issue_dependencies (id, blocking_issue_id, blocked_issue_id, created_at, updated_at)
             VALUES ('d1', 'i1', 'i2', 'x', 'x')",
            [],
        )
        .unwrap();
        queries::set_issue_dependency_sync_fields(&conn, "d1", 77, "blocks", "blocking").unwrap();
        let d = queries::get_issue_dependency(&conn, "d1").unwrap().unwrap();
        assert_eq!(
            (d.remote_id, d.kind.as_str(), d.direction.as_str()),
            (Some(77), "blocks", "blocking")
        );
    }
}
