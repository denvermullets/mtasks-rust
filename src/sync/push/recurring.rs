//! Pushing recurring issue creates, updates and deletes.

use rusqlite::Connection;

use crate::api::{ApiClient, ApiError, RecurringIssueParams, recurring_issues};
use crate::api::recurring_issues::RecurringIssueDto;
use crate::db::queries;
use crate::sync::engine::{CycleCtx, open};
use super::outcome::{
    Outcome, Prep, classify_in_team, is_team_access_message, optional_remote_id,
    require_remote_id,
};
use super::reconcile::{find_recurring_issue_on_server, record_attempt};

/// What a recurring issue create sends. `time_zone` is absent when the row's
/// zone is null (the server then uses the creator's preference) and `label_ids`
/// when no label is attached.
struct RecurringIssueCreatePrep {
    team_remote_id: i64,
    local_id: String,
    params: RecurringIssueParams,
}

fn prep_recurring_issue_create(
    entity_id: &str,
    payload: Option<&str>,
) -> Prep<RecurringIssueCreatePrep> {
    match open() {
        Ok(conn) => recurring_issue_create_prep(&conn, entity_id, payload),
        Err(e) => Prep::Done(Outcome::LeavePending(e)),
    }
}

/// Re-reads the recurring issue row so the newest values go out, and waits
/// (leaves the entry pending) while the team, or any assignee, project, lane or
/// label the row references, has no remote id yet. `starts_on` comes from the
/// queued payload.
fn recurring_issue_create_prep(
    conn: &Connection,
    entity_id: &str,
    payload: Option<&str>,
) -> Prep<RecurringIssueCreatePrep> {
    use crate::db::recurring_issues as db;
    let row = match db::get(conn, entity_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "recurring issue {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    if row.remote_id.is_some() {
        // Idempotency: Rails already has this row.
        return Prep::Done(Outcome::Success);
    }
    let team_remote_id = match queries::remote_id_for(conn, "teams", &row.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let refs = (|| -> Result<_, Outcome> {
        Ok((
            recurring_assignee_remote_id(conn, &row)?,
            recurring_project_remote_id(conn, &row)?,
            recurring_lane_remote_id(conn, &row)?,
            recurring_label_remote_ids(conn, &row.id)?,
        ))
    })();
    let (assignee_id, project_id, lane_id, label_ids) = match refs {
        Ok(r) => r,
        Err(o) => return Prep::Done(o),
    };
    let params = RecurringIssueParams {
        title: Some(row.title),
        description: row.description,
        priority: Some(row.priority.as_rails_str().to_string()),
        assignee_id: assignee_id.map(Some),
        project_id: project_id.map(Some),
        lane_id: lane_id.map(Some),
        frequency: Some(row.frequency),
        interval: Some(row.interval),
        weekday: row.weekday,
        day_of_month: row.day_of_month,
        time_zone: row.time_zone,
        label_ids: (!label_ids.is_empty()).then_some(label_ids),
        starts_on: db::payload_starts_on(payload),
        ..Default::default()
    };
    Prep::Ready(RecurringIssueCreatePrep {
        team_remote_id,
        local_id: row.id,
        params,
    })
}

pub(super) async fn recurring_issue_create(
    client: &ApiClient,
    ctx: &CycleCtx,
    entity_id: &str,
    payload: Option<&str>,
    entry_id: i64,
) -> Outcome {
    let prep = match prep_recurring_issue_create(entity_id, payload) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    // A retry (an earlier attempt began but the entry never finished) first
    // looks for the row that attempt created, so a POST that succeeded but
    // whose local commit failed is adopted instead of repeated.
    let attempt = match record_attempt(entry_id, "recurring_issues") {
        Ok(a) => a,
        Err(o) => return o,
    };
    if let Some(since) = &attempt.previous {
        match find_recurring_issue_on_server(
            client,
            ctx,
            prep.team_remote_id,
            &prep.params,
            since,
            &attempt,
        )
        .await
        {
            Ok(Some(dto)) => return commit_recurring_create(&prep.local_id, entry_id, &dto),
            Ok(None) => {}
            Err(o) => return o,
        }
    }
    // A 422 is dropped by `classify`; the row stays visible without a remote id.
    match recurring_issues::create(client, prep.team_remote_id, &prep.params).await {
        Ok(dto) => commit_recurring_create(&prep.local_id, entry_id, &dto),
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

fn commit_recurring_create(local_id: &str, entry_id: i64, dto: &RecurringIssueDto) -> Outcome {
    let committed = open().and_then(|mut conn| {
        crate::db::recurring_issues::commit_created(&mut conn, local_id, entry_id, dto)
            .map_err(|e| format!("commit recurring issue create: {e}"))
    });
    match committed {
        Ok(()) => Outcome::Created,
        Err(e) => Outcome::LeavePending(e),
    }
}

// Remote ids of what a recurring issue references. Each waits (leaves the
// entry pending) while the referenced record has no remote id yet; `None`
// means nothing is referenced. Shared by the create and update preps.

fn recurring_assignee_remote_id(
    conn: &Connection,
    row: &crate::db::recurring_issues::RecurringIssue,
) -> Result<Option<i64>, Outcome> {
    optional_remote_id(conn, "members", row.assignee_id.as_deref())
}

fn recurring_project_remote_id(
    conn: &Connection,
    row: &crate::db::recurring_issues::RecurringIssue,
) -> Result<Option<i64>, Outcome> {
    optional_remote_id(conn, "projects", row.project_id.as_deref())
}

fn recurring_lane_remote_id(
    conn: &Connection,
    row: &crate::db::recurring_issues::RecurringIssue,
) -> Result<Option<i64>, Outcome> {
    optional_remote_id(conn, "lanes", row.lane_id.as_deref())
}

/// Remote ids of a recurring issue's labels; waits (leave pending) while any
/// has none yet.
fn recurring_label_remote_ids(conn: &Connection, id: &str) -> Result<Vec<i64>, Outcome> {
    let local = crate::db::recurring_issues::label_ids(conn, id)
        .map_err(|e| Outcome::LeavePending(format!("db (labels): {e}")))?;
    local
        .iter()
        .map(|l| require_remote_id(conn, "labels", l))
        .collect()
}

struct RecurringIssueUpdatePrep {
    team_remote_id: i64,
    remote_id: i64,
    local_id: String,
    params: RecurringIssueParams,
}

fn prep_recurring_issue_update(
    entity_id: &str,
    payload: Option<&str>,
) -> Prep<RecurringIssueUpdatePrep> {
    match open() {
        Ok(conn) => recurring_issue_update_prep(&conn, entity_id, payload),
        Err(e) => Prep::Done(Outcome::LeavePending(e)),
    }
}

/// Re-reads the recurring issue row so the newest values go out, however many
/// edits were queued. The payload's `changed` list picks which fields are sent
/// (unknown names are ignored), and `starts_on` goes out only if the payload
/// carries one (schedule changes), so a pause/resume leaves the server's
/// `next_run_on` alone. Edit tickets add their fields to the match below.
fn recurring_issue_update_prep(
    conn: &Connection,
    entity_id: &str,
    payload: Option<&str>,
) -> Prep<RecurringIssueUpdatePrep> {
    use db::ChangedField as F;
    use crate::db::recurring_issues as db;
    let row = match db::get(conn, entity_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "recurring issue {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let Some(remote_id) = row.remote_id else {
        return Prep::Done(Outcome::LeavePending(
            "recurring issue create not yet drained".into(),
        ));
    };
    let team_remote_id = match queries::remote_id_for(conn, "teams", &row.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let db::UpdatePayload { changed, starts_on } = db::parse_update_payload(payload);
    let mut params = RecurringIssueParams {
        starts_on,
        ..Default::default()
    };
    // Referenced assignee, project, lane and labels must have remote ids first.
    // A cleared reference is sent as an explicit null.
    for field in changed {
        let r = match field {
            F::Active => {
                params.active = Some(row.active);
                Ok(())
            }
            F::Title => {
                params.title = Some(row.title.clone());
                Ok(())
            }
            F::Description => {
                // Rails treats "" as blank, which is how a cleared description goes out.
                params.description = Some(row.description.clone().unwrap_or_default());
                Ok(())
            }
            F::Priority => {
                params.priority = Some(row.priority.as_rails_str().to_string());
                Ok(())
            }
            F::TimeZone => {
                params.time_zone = row.time_zone.clone();
                Ok(())
            }
            F::AssigneeId => {
                recurring_assignee_remote_id(conn, &row).map(|v| params.assignee_id = Some(v))
            }
            F::ProjectId => {
                recurring_project_remote_id(conn, &row).map(|v| params.project_id = Some(v))
            }
            F::LaneId => recurring_lane_remote_id(conn, &row).map(|v| params.lane_id = Some(v)),
            F::LabelIds => recurring_label_remote_ids(conn, &row.id)
                .map(|v| params.label_ids = Some(v)),
            F::Frequency => {
                params.frequency = Some(row.frequency.clone());
                Ok(())
            }
            F::Interval => {
                params.interval = Some(row.interval);
                Ok(())
            }
            F::Weekday => {
                params.weekday = row.weekday;
                Ok(())
            }
            F::DayOfMonth => {
                params.day_of_month = row.day_of_month;
                Ok(())
            }
        };
        if let Err(o) = r {
            return Prep::Done(o);
        }
    }
    Prep::Ready(RecurringIssueUpdatePrep {
        team_remote_id,
        remote_id,
        local_id: row.id,
        params,
    })
}

pub(super) async fn recurring_issue_update(
    client: &ApiClient,
    entity_id: &str,
    payload: Option<&str>,
) -> Outcome {
    let prep = match prep_recurring_issue_update(entity_id, payload) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    match recurring_issues::update(client, prep.team_remote_id, prep.remote_id, &prep.params).await
    {
        Ok(dto) => {
            let applied = open().and_then(|conn| {
                crate::db::recurring_issues::apply_response(&conn, &prep.local_id, &dto)
                    .map_err(|e| format!("apply recurring issue: {e}"))
            });
            match applied {
                Ok(()) => Outcome::Success,
                Err(e) => Outcome::LeavePending(e),
            }
        }
        // A 422 is dropped by `classify`; the next pull restores the server
        // row. "Recurring issue not found" means it is already gone (the next
        // pull's tombstone removes the local row); a team-access 404 is
        // handled by `classify_in_team`.
        Err(ApiError::NotFound(m)) if !is_team_access_message(&m) => Outcome::Success,
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

/// The local row is gone by drain time, so the payload captured at enqueue time
/// (`{team_remote_id, remote_id}`) is all a recurring issue delete has.
fn prep_recurring_issue_delete(payload: Option<&str>) -> Prep<(i64, i64)> {
    let parsed = payload.and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok());
    let ids = parsed.as_ref().and_then(|v| {
        Some((
            v.get("team_remote_id")?.as_i64()?,
            v.get("remote_id")?.as_i64()?,
        ))
    });
    match ids {
        Some(ids) => Prep::Ready(ids),
        None => Prep::Done(Outcome::DropAndLog(
            "recurring issue delete: missing or bad payload".into(),
        )),
    }
}

pub(super) async fn recurring_issue_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let (team_remote_id, remote_id) = match prep_recurring_issue_delete(payload) {
        Prep::Ready(ids) => ids,
        Prep::Done(o) => return o,
    };
    // 204 and a "not found" 404 both mean it is gone; a team-access 404 is not.
    match recurring_issues::delete(client, team_remote_id, remote_id).await {
        Ok(()) => Outcome::Success,
        Err(ApiError::NotFound(m)) if !is_team_access_message(&m) => Outcome::Success,
        Err(e) => classify_in_team(&e, team_remote_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::recurring_issues::ChangedField as F;
    use crate::sync::test_support::{insert_test_label, setup_labels_db};

    fn insert_test_recurring(conn: &Connection, remote_id: Option<i64>) {
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute(
            "INSERT INTO recurring_issues (id, remote_id, team_id, title, priority, frequency,
                interval, active, created_at, updated_at)
             VALUES ('r1', ?1, 't1', 'R', 0, 'daily', 1, 1, ?2, ?2)",
            rusqlite::params![remote_id, now],
        )
        .unwrap();
    }

    #[test]
    fn recurring_issue_update_prep_rereads_row_and_waits_for_remote_ids() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, None);
        let payload = crate::db::recurring_issues::update_payload(&[crate::db::recurring_issues::ChangedField::Active], None);
        assert!(matches!(
            recurring_issue_update_prep(&conn, "r1", Some(&payload)),
            Prep::Done(Outcome::LeavePending(_))
        ));
        conn.execute("UPDATE recurring_issues SET remote_id = 5 WHERE id = 'r1'", [])
            .unwrap();
        conn.execute("UPDATE teams SET remote_id = NULL WHERE id = 't1'", []).unwrap();
        assert!(matches!(
            recurring_issue_update_prep(&conn, "r1", Some(&payload)),
            Prep::Done(Outcome::LeavePending(_))
        ));
        conn.execute("UPDATE teams SET remote_id = 700 WHERE id = 't1'", []).unwrap();
        // The toggle is read at push time: flip it after queueing.
        crate::db::recurring_issues::set_active(&conn, "r1", false).unwrap();
        match recurring_issue_update_prep(&conn, "r1", Some(&payload)) {
            Prep::Ready(p) => {
                assert_eq!((p.team_remote_id, p.remote_id), (700, 5));
                assert_eq!(p.params.active, Some(false));
                assert_eq!(p.params.starts_on, None);
                let body = serde_json::to_value(&p.params).unwrap();
                assert_eq!(body, serde_json::json!({ "active": false }));
            }
            _ => panic!("expected Ready"),
        }
        assert!(matches!(
            recurring_issue_update_prep(&conn, "nope", Some(&payload)),
            Prep::Done(Outcome::DropAndLog(_))
        ));
    }

    #[test]
    fn recurring_issue_update_prep_sends_starts_on_only_when_the_payload_has_one() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, Some(5));
        let payload = crate::db::recurring_issues::update_payload(&[crate::db::recurring_issues::ChangedField::Active], Some("2026-11-05"));
        match recurring_issue_update_prep(&conn, "r1", Some(&payload)) {
            Prep::Ready(p) => assert_eq!(p.params.starts_on.as_deref(), Some("2026-11-05")),
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn recurring_issue_schedule_prep_sends_schedule_fields_and_starts_on() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, Some(5));
        conn.execute(
            "UPDATE recurring_issues SET frequency = 'weekly', interval = 2, weekday = 3
             WHERE id = 'r1'",
            [],
        )
        .unwrap();
        let payload = crate::db::recurring_issues::update_payload(
            &[F::Frequency, F::Interval, F::Weekday],
            Some("2026-11-10"),
        );
        match recurring_issue_update_prep(&conn, "r1", Some(&payload)) {
            Prep::Ready(p) => assert_eq!(
                serde_json::to_value(&p.params).unwrap(),
                serde_json::json!({
                    "frequency": "weekly", "interval": 2, "weekday": 3,
                    "starts_on": "2026-11-10"
                })
            ),
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn recurring_issue_edit_prep_sends_changed_fields_without_starts_on() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, Some(5));
        conn.execute(
            "UPDATE recurring_issues SET title = 'New', priority = 1, description = NULL,
                time_zone = 'Tokyo' WHERE id = 'r1'",
            [],
        )
        .unwrap();
        let payload = crate::db::recurring_issues::update_payload(
            &[F::Title, F::Priority, F::Description, F::TimeZone],
            None,
        );
        match recurring_issue_update_prep(&conn, "r1", Some(&payload)) {
            Prep::Ready(p) => {
                let body = serde_json::to_value(&p.params).unwrap();
                assert_eq!(
                    body,
                    serde_json::json!({
                        "title": "New", "priority": "high", "description": "",
                        "time_zone": "Tokyo"
                    })
                );
            }
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn recurring_issue_edit_prep_waits_for_references_and_sends_clears_as_null() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, Some(5));
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("UPDATE recurring_issues SET lane_id = 'l1' WHERE id = 'r1'", []).unwrap();
        let payload = crate::db::recurring_issues::update_payload(
            &[F::LaneId, F::AssigneeId, F::ProjectId, F::LabelIds],
            None,
        );
        let waits = |c: &Connection| {
            matches!(
                recurring_issue_update_prep(c, "r1", Some(&payload)),
                Prep::Done(Outcome::LeavePending(_))
            )
        };
        // The lane has no remote id yet.
        assert!(waits(&conn));
        conn.execute("UPDATE lanes SET remote_id = 11 WHERE id = 'l1'", []).unwrap();
        assert!(!waits(&conn));

        insert_test_label(&conn, "lb1", None);
        conn.execute(
            "INSERT INTO recurring_issue_labels (recurring_issue_id, label_id) VALUES ('r1', 'lb1')",
            [],
        )
        .unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE labels SET remote_id = 14 WHERE id = 'lb1'", []).unwrap();

        conn.execute(
            "INSERT INTO members (id, team_id, name, email, created_at, updated_at)
             VALUES ('m1', 't1', 'Ann', 'a@x.io', ?1, ?1)",
            [now],
        )
        .unwrap();
        conn.execute("UPDATE recurring_issues SET assignee_id = 'm1' WHERE id = 'r1'", []).unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE recurring_issues SET assignee_id = NULL WHERE id = 'r1'", []).unwrap();
        match recurring_issue_update_prep(&conn, "r1", Some(&payload)) {
            Prep::Ready(p) => {
                let body = serde_json::to_value(&p.params).unwrap();
                assert_eq!(
                    body,
                    serde_json::json!({
                        "lane_id": 11, "assignee_id": null, "project_id": null,
                        "label_ids": [14]
                    })
                );
            }
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn recurring_issue_create_prep_rereads_row_and_includes_starts_on() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, None);
        conn.execute("UPDATE recurring_issues SET title = 'Edited' WHERE id = 'r1'", [])
            .unwrap();
        let payload = crate::db::recurring_issues::create_payload("2026-11-04");
        match recurring_issue_create_prep(&conn, "r1", Some(&payload)) {
            Prep::Ready(p) => {
                assert_eq!(p.team_remote_id, 700);
                assert_eq!(p.local_id, "r1");
                let body = serde_json::to_value(&p.params).unwrap();
                assert_eq!(
                    body,
                    serde_json::json!({
                        "title": "Edited", "priority": "urgent", "frequency": "daily",
                        "interval": 1, "starts_on": "2026-11-04"
                    })
                );
            }
            _ => panic!("expected Ready"),
        }
        assert!(matches!(
            recurring_issue_create_prep(&conn, "nope", Some(&payload)),
            Prep::Done(Outcome::DropAndLog(_))
        ));
    }

    #[test]
    fn recurring_issue_create_prep_sends_time_zone_only_when_the_row_has_one() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, None);
        let body = |conn: &Connection| match recurring_issue_create_prep(conn, "r1", None) {
            Prep::Ready(p) => serde_json::to_value(&p.params).unwrap(),
            _ => panic!("expected Ready"),
        };
        assert!(body(&conn).get("time_zone").is_none());
        conn.execute("UPDATE recurring_issues SET time_zone = 'UTC' WHERE id = 'r1'", [])
            .unwrap();
        assert_eq!(body(&conn)["time_zone"], "UTC");
    }

    #[test]
    fn recurring_issue_create_prep_waits_for_team_and_referenced_remote_ids() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, None);
        let waits = |conn: &Connection| {
            matches!(
                recurring_issue_create_prep(conn, "r1", None),
                Prep::Done(Outcome::LeavePending(_))
            )
        };
        conn.execute("UPDATE teams SET remote_id = NULL WHERE id = 't1'", []).unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE teams SET remote_id = 700 WHERE id = 't1'", []).unwrap();

        // Lane, project, assignee and label without remote ids each block.
        conn.execute("UPDATE recurring_issues SET lane_id = 'l1' WHERE id = 'r1'", []).unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE lanes SET remote_id = 11 WHERE id = 'l1'", []).unwrap();
        assert!(!waits(&conn));

        conn.execute(
            "INSERT INTO projects (id, team_id, name, created_at, updated_at)
             VALUES ('p1', 't1', 'P', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute("UPDATE recurring_issues SET project_id = 'p1' WHERE id = 'r1'", []).unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE projects SET remote_id = 12 WHERE id = 'p1'", []).unwrap();
        assert!(!waits(&conn));

        conn.execute(
            "INSERT INTO members (id, team_id, name, email, created_at, updated_at)
             VALUES ('m1', 't1', 'Ann', 'a@x.io', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute("UPDATE recurring_issues SET assignee_id = 'm1' WHERE id = 'r1'", []).unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE members SET remote_id = 13 WHERE id = 'm1'", []).unwrap();
        assert!(!waits(&conn));

        insert_test_label(&conn, "lb1", None);
        conn.execute(
            "INSERT INTO recurring_issue_labels (recurring_issue_id, label_id) VALUES ('r1', 'lb1')",
            [],
        )
        .unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE labels SET remote_id = 14 WHERE id = 'lb1'", []).unwrap();
        match recurring_issue_create_prep(&conn, "r1", None) {
            Prep::Ready(p) => {
                assert_eq!(p.params.lane_id, Some(Some(11)));
                assert_eq!(p.params.project_id, Some(Some(12)));
                assert_eq!(p.params.assignee_id, Some(Some(13)));
                assert_eq!(p.params.label_ids, Some(vec![14]));
            }
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn recurring_issue_create_prep_is_a_no_op_once_the_row_has_a_remote_id() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, Some(5));
        assert!(matches!(
            recurring_issue_create_prep(&conn, "r1", None),
            Prep::Done(Outcome::Success)
        ));
    }

    #[test]
    fn recurring_issue_delete_prep_reads_only_the_payload() {
        match prep_recurring_issue_delete(Some(r#"{"team_remote_id":700,"remote_id":11}"#)) {
            Prep::Ready(ids) => assert_eq!(ids, (700, 11)),
            _ => panic!("expected Ready"),
        }
        for bad in [None, Some("not json"), Some(r#"{"remote_id":11}"#)] {
            assert!(matches!(
                prep_recurring_issue_delete(bad),
                Prep::Done(Outcome::DropAndLog(_))
            ));
        }
    }
}
