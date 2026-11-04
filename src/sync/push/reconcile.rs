//! Finding and adopting a record an earlier create already made on the server.

use std::collections::HashSet;

use rusqlite::Connection;

use crate::api::{
    ApiClient, ApiError, Conditional, IssueParams, LabelParams, ProjectParams,
    RecurringIssueParams, issues, labels, projects, recurring_issues,
};
use crate::api::dto::{IssueDto, LabelDto, ProjectDto};
use crate::api::recurring_issues::RecurringIssueDto;
use crate::db::{queries, sync_queue};
use crate::sync::engine::{CycleCtx, open};
use super::outcome::{Outcome, classify};

/// Save a freshly created record's server id and mark its queue entry synced
/// in one SQLite transaction. If either write fails both roll back, so the
/// entry stays pending and is retried on the next drain.
pub(super) fn save_remote_id_and_mark_synced(
    conn: &mut Connection,
    table: &str,
    local_id: &str,
    remote_id: i64,
    entry_id: i64,
) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| format!("begin: {e}"))?;
    match table {
        "issues" => queries::set_issue_remote_id(&tx, local_id, remote_id),
        "projects" => queries::set_project_remote_id(&tx, local_id, remote_id),
        "labels" => queries::set_label_remote_id(&tx, local_id, remote_id),
        "comments" => queries::set_comment_remote_id(&tx, local_id, remote_id),
        "issue_dependencies" => queries::set_issue_dependency_remote_id(&tx, local_id, remote_id),
        other => return Err(format!("unknown create table {other}")),
    }
    .map_err(|e| format!("write remote_id: {e}"))?;
    sync_queue::mark_synced(&tx, entry_id).map_err(|e| format!("mark_synced: {e}"))?;
    tx.commit().map_err(|e| format!("commit: {e}"))
}

pub(super) fn commit_create(entry_id: i64, table: &str, local_id: &str, remote_id: i64) -> Outcome {
    let mut conn = match open() {
        Ok(c) => c,
        Err(e) => return Outcome::LeavePending(e),
    };
    match save_remote_id_and_mark_synced(&mut conn, table, local_id, remote_id, entry_id) {
        Ok(()) => Outcome::Created,
        Err(e) => Outcome::LeavePending(e),
    }
}

// ── Create reconciliation (JRST-23) ───────────────────────────────────────
//
// If a create's POST reached Rails but we never saw the response (crash, lost
// connection), the entry is still pending. Before re-POSTing we look for the
// record the earlier attempt made and adopt its id. `sync_queue.attempted_at`
// is written before the first POST and never moved forward, so a retry's
// window covers every earlier attempt.
//
// Rails has no creator/created_at filter and no server-side idempotency for
// issue/project/label creates, so each type uses the strongest rule its
// payload allows. Every rule also skips server ids already linked to another
// local row, so two identical local creates cannot adopt the same record.
//
//  - Issues:   `GET issues?updated_since=<attempt>`; match `creator.id` ==
//              token user (`/me`), exact `title`, and `created_at` >= attempt.
//              Strongest (creator + title + time).
//  - Projects: `GET projects?updated_since=<attempt>`; rows carry no creator,
//              so match exact `name` and `created_at` >= attempt. Cannot
//              verify the creator.
//  - Recurring issues: `GET recurring_issues?updated_since=<attempt>`; match
//              `creator.id` == token user (`/me`), exact `title`, the same
//              frequency, interval, weekday and day of month, and
//              `created_at` >= attempt. The adopted row is applied whole
//              (`commit_created`), so a create whose POST succeeded but whose
//              local commit failed is finished, not repeated.
//  - Labels:   rows carry only `updated_at` (no created_at/creator, no
//              updated_since filter), which says nothing about creation. List
//              the team's labels and match `name` case-insensitively (Rails
//              enforces per-team case-insensitive uniqueness, so the match is
//              unambiguous, and a plain POST would 422 on it anyway).
//
// `created_at` is compared with CLOCK_SKEW_SLACK_SECS of tolerance because it
// is the server's clock and the attempt time is ours; a small slack beats a
// missed adoption (which would duplicate). Issue lookups fetch one page of up
// to RECONCILE_PAGE_LIMIT rows. A lookup error leaves the entry pending
// rather than POSTing blind.

const CLOCK_SKEW_SLACK_SECS: i64 = 60;

const RECONCILE_PAGE_LIMIT: u32 = 500;

/// State read before a create's POST: when an earlier attempt began (if any)
/// and the server ids already linked locally for the entity's table.
pub(super) struct Attempt {
    pub(super) previous: Option<String>,
    taken: HashSet<i64>,
}

pub(super) fn record_attempt(entry_id: i64, table: &str) -> Result<Attempt, Outcome> {
    let conn = open().map_err(Outcome::LeavePending)?;
    record_attempt_on(&conn, entry_id, table).map_err(Outcome::LeavePending)
}

fn record_attempt_on(conn: &Connection, entry_id: i64, table: &str) -> Result<Attempt, String> {
    let taken = linked_remote_ids(conn, table)?;
    let previous =
        sync_queue::begin_attempt(conn, entry_id).map_err(|e| format!("begin_attempt: {e}"))?;
    Ok(Attempt { previous, taken })
}

fn linked_remote_ids(conn: &Connection, table: &str) -> Result<HashSet<i64>, String> {
    if !matches!(table, "issues" | "projects" | "labels" | "recurring_issues") {
        return Err(format!("unknown reconcile table {table}"));
    }
    let mut stmt = conn
        .prepare(&format!(
            "SELECT remote_id FROM {table} WHERE remote_id IS NOT NULL"
        ))
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, i64>(0))
        .map_err(|e| format!("db: {e}"))?;
    rows.collect::<Result<_, _>>().map_err(|e| format!("db: {e}"))
}

/// True when `created_at` is at or after `attempted_at` (minus clock slack).
/// Unparseable timestamps never match.
fn created_after_attempt(created_at: &str, attempted_at: &str) -> bool {
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();
    match (parse(created_at), parse(attempted_at)) {
        (Some(c), Some(a)) => c >= a - chrono::Duration::seconds(CLOCK_SKEW_SLACK_SECS),
        _ => false,
    }
}

/// Earliest-created matching issue: same creator, exact title, created after
/// the attempt, not already linked to a local row.
fn match_issue(
    candidates: &[IssueDto],
    me_id: i64,
    title: &str,
    attempted_at: &str,
    taken: &HashSet<i64>,
) -> Option<i64> {
    candidates
        .iter()
        .filter(|d| {
            d.title == title
                && d.creator.as_ref().map(|c| c.id) == Some(me_id)
                && created_after_attempt(&d.created_at, attempted_at)
                && !taken.contains(&d.id)
        })
        .min_by(|a, b| a.created_at.cmp(&b.created_at))
        .map(|d| d.id)
}

/// Earliest-created project with the exact name created after the attempt.
fn match_project(
    candidates: &[ProjectDto],
    name: &str,
    attempted_at: &str,
    taken: &HashSet<i64>,
) -> Option<i64> {
    candidates
        .iter()
        .filter(|d| {
            d.name == name
                && created_after_attempt(&d.created_at, attempted_at)
                && !taken.contains(&d.id)
        })
        .min_by(|a, b| a.created_at.cmp(&b.created_at))
        .map(|d| d.id)
}

/// Label with the same name (case-insensitive), last written after the
/// attempt, not already linked. Labels carry no creator, so the timestamp is
/// what tells our own create from an older label that shares the name.
fn match_label(
    candidates: &[LabelDto],
    name: &str,
    attempted_at: &str,
    taken: &HashSet<i64>,
) -> Option<i64> {
    let wanted = name.trim().to_lowercase();
    candidates
        .iter()
        .find(|d| {
            d.name.trim().to_lowercase() == wanted
                && d.updated_at
                    .as_deref()
                    .is_some_and(|u| created_after_attempt(u, attempted_at))
                && !taken.contains(&d.id)
        })
        .map(|d| d.id)
}

pub(super) async fn find_issue_on_server(
    client: &ApiClient,
    ctx: &CycleCtx,
    team_remote_id: i64,
    params: &IssueParams,
    attempted_at: &str,
    attempt: &Attempt,
) -> Result<Option<i64>, Outcome> {
    let Some(title) = params.title.as_deref() else {
        return Ok(None);
    };
    // The cycle's one `/me` result. Without it the creator can't be matched,
    // so keep the entry pending rather than POSTing blind.
    let me = match &ctx.me {
        Ok(me) => me,
        Err(e) => return Err(Outcome::LeavePending(format!("/me unavailable: {e}"))),
    };
    let query = issues::IssuePageQuery {
        updated_since: Some(attempted_at.to_string()),
        limit: Some(RECONCILE_PAGE_LIMIT),
        ..Default::default()
    };
    let candidates = issues::list_page(client, team_remote_id, &query)
        .await
        .map_err(|e| classify(&e))?
        .rows;
    Ok(match_issue(
        &candidates,
        me.id,
        title,
        attempted_at,
        &attempt.taken,
    ))
}

/// Earliest-created recurring issue by this user with the same title and
/// schedule, created after the attempt, not already linked to a local row.
fn match_recurring_issue<'a>(
    candidates: &'a [RecurringIssueDto],
    me_id: i64,
    params: &RecurringIssueParams,
    attempted_at: &str,
    taken: &HashSet<i64>,
) -> Option<&'a RecurringIssueDto> {
    candidates
        .iter()
        .filter(|d| {
            Some(&d.title) == params.title.as_ref()
                && Some(&d.frequency) == params.frequency.as_ref()
                && Some(d.interval) == params.interval
                && d.weekday == params.weekday
                && d.day_of_month == params.day_of_month
                && d.creator.as_ref().map(|c| c.id) == Some(me_id)
                && created_after_attempt(&d.created_at, attempted_at)
                && !taken.contains(&d.id)
        })
        .min_by(|a, b| a.created_at.cmp(&b.created_at))
}

pub(super) async fn find_recurring_issue_on_server(
    client: &ApiClient,
    ctx: &CycleCtx,
    team_remote_id: i64,
    params: &RecurringIssueParams,
    attempted_at: &str,
    attempt: &Attempt,
) -> Result<Option<RecurringIssueDto>, Outcome> {
    // Without `/me` the creator can't be matched: stay pending, don't POST blind.
    let me = match &ctx.me {
        Ok(me) => me,
        Err(e) => return Err(Outcome::LeavePending(format!("/me unavailable: {e}"))),
    };
    let candidates = recurring_issues::list_page(client, team_remote_id, Some(attempted_at))
        .await
        .map_err(|e| classify(&e))?
        .rows;
    Ok(match_recurring_issue(&candidates, me.id, params, attempted_at, &attempt.taken).cloned())
}

pub(super) async fn find_project_on_server(
    client: &ApiClient,
    team_remote_id: i64,
    params: &ProjectParams,
    attempted_at: &str,
    attempt: &Attempt,
) -> Result<Option<i64>, ApiError> {
    let Some(name) = params.name.as_deref() else {
        return Ok(None);
    };
    let candidates = projects::list_page(client, team_remote_id, Some(attempted_at))
        .await?
        .rows;
    Ok(match_project(&candidates, name, attempted_at, &attempt.taken))
}

pub(super) async fn find_label_on_server(
    client: &ApiClient,
    team_remote_id: i64,
    params: &LabelParams,
    attempted_at: &str,
    attempt: &Attempt,
) -> Result<Option<i64>, ApiError> {
    let candidates = match labels::list_conditional(client, team_remote_id, None).await? {
        Conditional::Modified { data, .. } => data,
        Conditional::NotModified => return Ok(None),
    };
    Ok(match_label(
        &candidates,
        &params.name,
        attempted_at,
        &attempt.taken,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::push::fixtures::none;
    use crate::sync::test_support::{insert_test_label, project_dto, setup_labels_db};

    fn queue_create(conn: &Connection, entity_type: &str, entity_id: &str) -> i64 {
        sync_queue::enqueue(conn, entity_type, entity_id, "create", None).unwrap();
        conn.last_insert_rowid()
    }

    fn issue_remote(conn: &Connection) -> Option<i64> {
        conn.query_row("SELECT remote_id FROM issues WHERE id='i1'", [], |r| {
            r.get(0)
        })
        .unwrap()
    }

    #[test]
    fn create_saves_remote_id_and_marks_synced_together() {
        let mut conn = setup_labels_db();
        let entry = queue_create(&conn, "issue", "i1");
        save_remote_id_and_mark_synced(&mut conn, "issues", "i1", 99, entry).unwrap();
        assert_eq!(issue_remote(&conn), Some(99));
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn create_rolls_back_remote_id_when_mark_synced_fails() {
        let mut conn = setup_labels_db();
        let entry = queue_create(&conn, "issue", "i1");
        conn.execute_batch(
            "CREATE TRIGGER fail_mark BEFORE UPDATE ON sync_queue
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();
        assert!(save_remote_id_and_mark_synced(&mut conn, "issues", "i1", 99, entry).is_err());
        assert_eq!(issue_remote(&conn), None);
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 1);
    }

    #[test]
    fn create_rolls_back_mark_synced_when_remote_id_write_fails() {
        let mut conn = setup_labels_db();
        let entry = queue_create(&conn, "issue", "i1");
        conn.execute_batch(
            "CREATE TRIGGER fail_rid BEFORE UPDATE OF remote_id ON issues
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();
        assert!(save_remote_id_and_mark_synced(&mut conn, "issues", "i1", 99, entry).is_err());
        assert_eq!(issue_remote(&conn), None);
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 1);
    }

    #[test]
    fn create_commit_covers_label_table_and_rejects_unknown() {
        let mut conn = setup_labels_db();
        insert_test_label(&conn, "lab1", None);
        let entry = queue_create(&conn, "label", "lab1");
        save_remote_id_and_mark_synced(&mut conn, "labels", "lab1", 5, entry).unwrap();
        assert_eq!(
            queries::remote_id_for(&conn, "labels", "lab1").unwrap(),
            Some(5)
        );
        assert!(save_remote_id_and_mark_synced(&mut conn, "bogus", "x", 1, entry).is_err());
    }

    // ── Create reconciliation (JRST-23) ──

    const ATTEMPT: &str = "2026-10-30T12:00:00.000Z";

    fn issue_dto(id: i64, title: &str, creator: Option<i64>, created_at: &str) -> IssueDto {
        serde_json::from_value(serde_json::json!({
            "id": id, "identifier": format!("X-{id}"), "title": title,
            "priority": "no_priority", "estimate": null, "due_date": null,
            "lane": null, "assignee": null,
            "creator": creator.map(|c| serde_json::json!({"id": c, "name": "n"})),
            "project": null, "labels": [],
            "created_at": created_at, "updated_at": created_at,
        }))
        .unwrap()
    }

    fn label_dto(id: i64, name: &str) -> LabelDto {
        serde_json::from_value(serde_json::json!({"id": id, "name": name, "color": null}))
            .unwrap()
    }

    fn label_dto_at(id: i64, name: &str, updated_at: &str) -> LabelDto {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "color": null, "updated_at": updated_at
        }))
        .unwrap()
    }

    #[test]
    fn issue_match_requires_creator_title_and_time() {
        let after = "2026-10-30T12:00:05.000Z";
        let c = vec![
            issue_dto(1, "Fix", Some(8), after),
            issue_dto(2, "Other", Some(7), after),
            issue_dto(3, "Fix", Some(7), "2026-10-30T11:00:00.000Z"),
            issue_dto(4, "Fix", None, after),
            issue_dto(5, "Fix", Some(7), after),
        ];
        assert_eq!(match_issue(&c, 7, "Fix", ATTEMPT, &none()), Some(5));
        assert_eq!(match_issue(&c[..4], 7, "Fix", ATTEMPT, &none()), None);
    }

    #[test]
    fn issue_match_tolerates_small_clock_skew_only() {
        let near = issue_dto(1, "Fix", Some(7), "2026-10-30T11:59:30.000Z");
        let far = issue_dto(2, "Fix", Some(7), "2026-10-30T11:55:00.000Z");
        assert_eq!(match_issue(&[near], 7, "Fix", ATTEMPT, &none()), Some(1));
        assert_eq!(match_issue(&[far], 7, "Fix", ATTEMPT, &none()), None);
    }

    #[test]
    fn issue_match_picks_earliest_and_skips_linked_ids() {
        let c = vec![
            issue_dto(10, "Fix", Some(7), "2026-10-30T12:00:09.000Z"),
            issue_dto(11, "Fix", Some(7), "2026-10-30T12:00:02.000Z"),
        ];
        assert_eq!(match_issue(&c, 7, "Fix", ATTEMPT, &none()), Some(11));
        let taken: HashSet<i64> = [11].into_iter().collect();
        assert_eq!(match_issue(&c, 7, "Fix", ATTEMPT, &taken), Some(10));
    }

    fn recurring_dto(id: i64, title: &str, creator: Option<i64>, created_at: &str) -> RecurringIssueDto {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": title, "priority": "no_priority",
            "frequency": "weekly", "interval": 2, "weekday": 5, "day_of_month": null,
            "active": true,
            "creator": creator.map(|c| serde_json::json!({"id": c, "name": "n"})),
            "created_at": created_at, "updated_at": created_at,
        }))
        .unwrap()
    }

    fn recurring_params(title: &str) -> RecurringIssueParams {
        RecurringIssueParams {
            title: Some(title.into()),
            frequency: Some("weekly".into()),
            interval: Some(2),
            weekday: Some(5),
            ..Default::default()
        }
    }

    #[test]
    fn recurring_match_requires_creator_title_schedule_and_time() {
        let after = "2026-10-30T12:00:05.000Z";
        let mut other_schedule = recurring_dto(3, "Water", Some(7), after);
        other_schedule.interval = 1;
        let c = vec![
            recurring_dto(1, "Water", Some(8), after),
            recurring_dto(2, "Other", Some(7), after),
            other_schedule,
            recurring_dto(4, "Water", Some(7), "2026-10-30T11:00:00.000Z"),
            recurring_dto(5, "Water", None, after),
            recurring_dto(6, "Water", Some(7), after),
        ];
        let p = recurring_params("Water");
        assert_eq!(match_recurring_issue(&c, 7, &p, ATTEMPT, &none()).map(|d| d.id), Some(6));
        assert!(match_recurring_issue(&c[..5], 7, &p, ATTEMPT, &none()).is_none());
    }

    #[test]
    fn recurring_match_picks_earliest_and_skips_linked_ids() {
        let c = vec![
            recurring_dto(10, "Water", Some(7), "2026-10-30T12:00:09.000Z"),
            recurring_dto(11, "Water", Some(7), "2026-10-30T12:00:02.000Z"),
        ];
        let p = recurring_params("Water");
        assert_eq!(match_recurring_issue(&c, 7, &p, ATTEMPT, &none()).map(|d| d.id), Some(11));
        let taken: HashSet<i64> = [11].into_iter().collect();
        assert_eq!(match_recurring_issue(&c, 7, &p, ATTEMPT, &taken).map(|d| d.id), Some(10));
    }

    #[test]
    fn project_match_is_exact_name_after_attempt() {
        let after = "2026-10-30T12:01:00.000Z";
        let c = vec![
            project_dto(1, "Roadmap", "2026-10-29T00:00:00.000Z"),
            project_dto(2, "roadmap", after),
            project_dto(3, "Roadmap", after),
        ];
        assert_eq!(match_project(&c, "Roadmap", ATTEMPT, &none()), Some(3));
        let taken: HashSet<i64> = [3].into_iter().collect();
        assert_eq!(match_project(&c, "Roadmap", ATTEMPT, &taken), None);
    }

    #[test]
    fn label_match_is_case_insensitive_name_and_skips_linked() {
        let at = "2026-10-30T12:00:00Z";
        let c = vec![
            label_dto_at(1, "Bug", "2026-10-30T12:00:05Z"),
            label_dto_at(2, "Feature", "2026-10-30T12:00:05Z"),
        ];
        assert_eq!(match_label(&c, "bug", at, &none()), Some(1));
        assert_eq!(match_label(&c, " FEATURE ", at, &none()), Some(2));
        assert_eq!(match_label(&c, "Chore", at, &none()), None);
        let taken: HashSet<i64> = [1].into_iter().collect();
        assert_eq!(match_label(&c, "Bug", at, &taken), None);
    }

    #[test]
    fn label_match_requires_update_after_the_attempt() {
        let at = "2026-10-30T12:00:00Z";
        // Same name, but the label was last touched well before the attempt:
        // an unrelated pre-existing label, not our create.
        let old = vec![label_dto_at(1, "Bug", "2026-10-29T08:00:00Z")];
        assert_eq!(match_label(&old, "Bug", at, &none()), None);
        // Within the clock-skew slack still matches.
        let skewed = vec![label_dto_at(2, "Bug", "2026-10-30T11:59:30Z")];
        assert_eq!(match_label(&skewed, "Bug", at, &none()), Some(2));
        // No timestamp at all never matches.
        assert_eq!(match_label(&[label_dto(3, "Bug")], "Bug", at, &none()), None);
    }

    fn stored_attempt(conn: &Connection, entry: i64) -> String {
        conn.query_row(
            "SELECT attempted_at FROM sync_queue WHERE id=?1",
            [entry],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn record_attempt_records_time_once_and_reports_earlier_attempt() {
        let conn = setup_labels_db();
        let entry = queue_create(&conn, "issue", "i1");
        let first = record_attempt_on(&conn, entry, "issues").unwrap();
        assert!(first.previous.is_none());
        let stored = stored_attempt(&conn, entry);
        let second = record_attempt_on(&conn, entry, "issues").unwrap();
        assert_eq!(second.previous.as_deref(), Some(stored.as_str()));
        assert_eq!(stored_attempt(&conn, entry), stored);
    }

    #[test]
    fn record_attempt_collects_linked_remote_ids() {
        let conn = setup_labels_db();
        insert_test_label(&conn, "a", Some(41));
        insert_test_label(&conn, "b", None);
        let entry = queue_create(&conn, "label", "b");
        let a = record_attempt_on(&conn, entry, "labels").unwrap();
        assert_eq!(a.taken, [41].into_iter().collect::<HashSet<_>>());
        assert!(record_attempt_on(&conn, entry, "comments").is_err());
    }

    #[test]
    fn adopting_a_match_links_and_marks_synced_without_posting() {
        let mut conn = setup_labels_db();
        let entry = queue_create(&conn, "issue", "i1");
        let _ = record_attempt_on(&conn, entry, "issues").unwrap();
        let retry = record_attempt_on(&conn, entry, "issues").unwrap();
        let since = retry.previous.unwrap();
        let c = vec![issue_dto(55, "Issue", Some(7), "2999-01-01T00:00:00.000Z")];
        let id = match_issue(&c, 7, "Issue", &since, &retry.taken).unwrap();
        save_remote_id_and_mark_synced(&mut conn, "issues", "i1", id, entry).unwrap();
        assert_eq!(issue_remote(&conn), Some(55));
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 0);
    }
}
