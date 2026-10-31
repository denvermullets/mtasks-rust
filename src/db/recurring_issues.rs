//! Local mirror of a team's Recurring issues (schedule templates the server
//! spawns issues from). Pure over a `rusqlite::Connection`: no Iced, no HTTP.
//! The Data sync feeds it decoded index rows via [`apply_pulled`].
//!
//! The per-team pull cursor lives in `pull_cursors` under [`CURSOR_RESOURCE`].
//! Later tickets add the local writes (create/update/set active/delete), the
//! display-ready list read and tombstones to this module.
use rusqlite::{params, Connection, OptionalExtension, Result, Row};
use uuid::Uuid;

use super::models::Priority;
use super::queries::{
    find_label_by_remote_id, find_lane_by_remote_id, find_member_by_remote_id,
    find_project_by_remote_id, get_pull_cursor, set_pull_cursor,
};
use super::sync_queue;
use crate::api::recurring_issues::RecurringIssueDto;

/// `pull_cursors.resource` for a team's recurring issues index.
pub const CURSOR_RESOURCE: &str = "recurring_issues";
/// `sync_queue.entity_type` for recurring issue writes.
pub const ENTITY_TYPE: &str = "recurring_issue";

/// A mirrored recurring issue row (raw: references are local ids, names are
/// not resolved; the display-ready read comes in a later ticket).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurringIssue {
    /// Local uuid.
    pub id: String,
    pub remote_id: Option<i64>,
    /// Local team uuid.
    pub team_id: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: Priority,
    /// "daily" | "weekly" | "monthly".
    pub frequency: String,
    pub interval: i64,
    pub weekday: Option<i64>,
    pub day_of_month: Option<i64>,
    /// ActiveSupport zone name; `None` = the creator's preference.
    pub time_zone: Option<String>,
    pub active: bool,
    /// Server-calculated date (`YYYY-MM-DD`).
    pub next_run_on: Option<String>,
    pub last_run_at: Option<String>,
    /// Local member uuid, if the server assignee is mirrored.
    pub assignee_id: Option<String>,
    /// The server's assignee name, a display fallback.
    pub assignee_name: Option<String>,
    pub project_id: Option<String>,
    pub lane_id: Option<String>,
    pub creator_remote_id: Option<i64>,
    pub creator_name: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

const COLUMNS: &str = "id, remote_id, team_id, title, description, priority, frequency, \
    interval, weekday, day_of_month, time_zone, active, next_run_on, last_run_at, \
    assignee_id, assignee_name, project_id, lane_id, creator_remote_id, creator_name, \
    created_at, updated_at";

impl RecurringIssue {
    fn from_row(r: &Row) -> Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            remote_id: r.get(1)?,
            team_id: r.get(2)?,
            title: r.get(3)?,
            description: r.get(4)?,
            priority: Priority::from_i32(r.get(5)?),
            frequency: r.get(6)?,
            interval: r.get(7)?,
            weekday: r.get(8)?,
            day_of_month: r.get(9)?,
            time_zone: r.get(10)?,
            active: r.get::<_, i64>(11)? != 0,
            next_run_on: r.get(12)?,
            last_run_at: r.get(13)?,
            assignee_id: r.get(14)?,
            assignee_name: r.get(15)?,
            project_id: r.get(16)?,
            lane_id: r.get(17)?,
            creator_remote_id: r.get(18)?,
            creator_name: r.get(19)?,
            created_at: r.get(20)?,
            updated_at: r.get(21)?,
        })
    }
}

/// What an apply did, for the sync log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub upserted: usize,
    /// Rows left alone because a local write is queued for them.
    pub skipped_pending: usize,
}

/// The stored pull cursor (`X-Server-Time` of the last successful pull) for a
/// local team, or `None` if never pulled.
pub fn cursor(conn: &Connection, local_team_id: &str) -> Result<Option<String>> {
    get_pull_cursor(conn, local_team_id, CURSOR_RESOURCE)
}

/// Upsert a pulled batch for one team by `remote_id` (idempotent, so re-delivery
/// at the cursor overlap is safe), resolving the remote assignee, project, lane
/// and label ids to local ones, then advance the cursor to `server_time` if
/// given. All-or-nothing: an error leaves rows and cursor untouched. A reference
/// that isn't mirrored yet resolves to `None` (the assignee name is kept). A row
/// with a queued local write keeps its local values. Duplicate ids in the batch:
/// the last one wins.
pub fn apply_pulled(
    conn: &Connection,
    local_team_id: &str,
    rows: &[RecurringIssueDto],
    server_time: Option<&str>,
) -> Result<Applied> {
    let tx = conn.unchecked_transaction()?;
    let mut applied = Applied::default();
    for row in rows {
        let existing = find_by_remote_id(&tx, row.id)?;
        if let Some(local) = &existing {
            if sync_queue::has_pending_entry(&tx, ENTITY_TYPE, &local.id)? {
                applied.skipped_pending += 1;
                continue;
            }
        }
        let id = existing
            .map(|r| r.id)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        upsert_row(&tx, &id, local_team_id, row)?;
        replace_labels(&tx, &id, &row.label_ids)?;
        applied.upserted += 1;
    }
    if let Some(t) = server_time {
        set_pull_cursor(&tx, local_team_id, CURSOR_RESOURCE, t)?;
    }
    tx.commit()?;
    Ok(applied)
}

fn upsert_row(conn: &Connection, id: &str, team_id: &str, row: &RecurringIssueDto) -> Result<()> {
    let assignee = match &row.assignee {
        Some(r) => find_member_by_remote_id(conn, r.id)?.map(|m| m.id),
        None => None,
    };
    let project = match &row.project {
        Some(r) => find_project_by_remote_id(conn, r.id)?.map(|p| p.id),
        None => None,
    };
    let lane = match &row.lane {
        Some(r) => find_lane_by_remote_id(conn, r.id)?.map(|l| l.id),
        None => None,
    };
    let priority = Priority::from_rails_str(&row.priority)
        .unwrap_or(Priority::None)
        .to_i32();
    conn.execute(
        "INSERT INTO recurring_issues (id, remote_id, team_id, title, description, priority,
            frequency, interval, weekday, day_of_month, time_zone, active, next_run_on,
            last_run_at, assignee_id, assignee_name, project_id, lane_id, creator_remote_id,
            creator_name, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
            ?18, ?19, ?20, ?21, ?22)
         ON CONFLICT(remote_id) DO UPDATE SET
            team_id = excluded.team_id, title = excluded.title,
            description = excluded.description, priority = excluded.priority,
            frequency = excluded.frequency, interval = excluded.interval,
            weekday = excluded.weekday, day_of_month = excluded.day_of_month,
            time_zone = excluded.time_zone, active = excluded.active,
            next_run_on = excluded.next_run_on, last_run_at = excluded.last_run_at,
            assignee_id = excluded.assignee_id, assignee_name = excluded.assignee_name,
            project_id = excluded.project_id, lane_id = excluded.lane_id,
            creator_remote_id = excluded.creator_remote_id,
            creator_name = excluded.creator_name, created_at = excluded.created_at,
            updated_at = excluded.updated_at",
        params![
            id,
            row.id,
            team_id,
            row.title,
            row.description,
            priority,
            row.frequency,
            row.interval,
            row.weekday,
            row.day_of_month,
            row.time_zone,
            row.active,
            row.next_run_on,
            row.last_run_at,
            assignee,
            row.assignee.as_ref().map(|r| r.name.as_str()),
            project,
            lane,
            row.creator.as_ref().map(|r| r.id),
            row.creator.as_ref().map(|r| r.name.as_str()),
            row.created_at,
            row.updated_at,
        ],
    )?;
    Ok(())
}

/// Replace a recurring issue's label join with the labels `remote_ids` resolve
/// to locally (unmirrored labels are skipped).
fn replace_labels(conn: &Connection, id: &str, remote_ids: &[i64]) -> Result<()> {
    conn.execute(
        "DELETE FROM recurring_issue_labels WHERE recurring_issue_id = ?1",
        params![id],
    )?;
    for remote in remote_ids {
        if let Some(label) = find_label_by_remote_id(conn, *remote)? {
            conn.execute(
                "INSERT OR IGNORE INTO recurring_issue_labels (recurring_issue_id, label_id)
                 VALUES (?1, ?2)",
                params![id, label.id],
            )?;
        }
    }
    Ok(())
}

/// Apply a server tombstone: remove the team's local row for `remote_id`, its
/// label join rows (cascade) and any queued local write for it. Idempotent;
/// returns whether a local row existed. An unknown id or another team's row is
/// a no-op.
pub fn apply_tombstone(conn: &Connection, local_team_id: &str, remote_id: i64) -> Result<bool> {
    let local: Option<String> = conn
        .query_row(
            "SELECT id FROM recurring_issues WHERE remote_id = ?1 AND team_id = ?2",
            params![remote_id, local_team_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(local_id) = local else {
        return Ok(false);
    };
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM recurring_issue_labels WHERE recurring_issue_id = ?1",
        params![local_id],
    )?;
    tx.execute("DELETE FROM recurring_issues WHERE id = ?1", params![local_id])?;
    super::queries::retire_pending_sync(&tx, ENTITY_TYPE, &local_id)?;
    tx.commit()?;
    Ok(true)
}

#[allow(dead_code)] // used by the view/write tickets that build on this module
pub fn get(conn: &Connection, id: &str) -> Result<Option<RecurringIssue>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM recurring_issues WHERE id = ?1"),
        params![id],
        RecurringIssue::from_row,
    )
    .optional()
}

pub fn find_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<RecurringIssue>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM recurring_issues WHERE remote_id = ?1"),
        params![remote_id],
        RecurringIssue::from_row,
    )
    .optional()
}

/// A team's recurring issues, oldest first (raw rows; no name resolution).
#[allow(dead_code)] // used by the Recurring issues view ticket
pub fn list_for_team(conn: &Connection, local_team_id: &str) -> Result<Vec<RecurringIssue>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM recurring_issues WHERE team_id = ?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map(params![local_team_id], RecurringIssue::from_row)?;
    rows.collect()
}

/// Local label ids joined to a recurring issue, sorted.
#[allow(dead_code)] // used by the view/push tickets
pub fn label_ids(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT label_id FROM recurring_issue_labels WHERE recurring_issue_id = ?1
         ORDER BY label_id",
    )?;
    let rows = stmt.query_map(params![id], |r| r.get(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RecordRef;

    const NOW: &str = "2026-01-01T00:00:00.000Z";

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES ('t1', 'T', 'T', 700, ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, remote_id, team_id, name, email, created_at, updated_at)
             VALUES ('m1', 3, 't1', 'Ann', 'a@x.io', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lanes (id, remote_id, name, position, team_id, created_at, updated_at)
             VALUES ('l1', 4, 'Todo', 0, 't1', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO projects (id, remote_id, team_id, name, created_at, updated_at)
             VALUES ('p1', 9, 't1', 'P', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        for (id, remote) in [("lab1", 1), ("lab2", 2)] {
            conn.execute(
                "INSERT INTO labels (id, remote_id, team_id, name, created_at, updated_at)
                 VALUES (?1, ?2, 't1', ?1, ?3, ?3)",
                params![id, remote, NOW],
            )
            .unwrap();
        }
        conn
    }

    fn rec(id: i64, title: &str, updated: &str) -> RecurringIssueDto {
        RecurringIssueDto {
            id,
            title: title.into(),
            description: None,
            priority: "no_priority".into(),
            frequency: "daily".into(),
            interval: 1,
            weekday: None,
            day_of_month: None,
            time_zone: None,
            active: true,
            next_run_on: Some("2026-11-01".into()),
            last_run_at: None,
            assignee: None,
            project: None,
            lane: None,
            label_ids: vec![],
            creator: None,
            created_at: NOW.into(),
            updated_at: updated.into(),
        }
    }

    fn rref(id: i64, name: &str) -> Option<RecordRef> {
        Some(RecordRef { id, name: name.into() })
    }

    fn count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM recurring_issues", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn tombstone_removes_the_row_and_its_labels_and_unknown_ids_are_noops() {
        let conn = setup();
        let mut row = rec(5, "R", NOW);
        row.label_ids = vec![1, 2];
        apply_pulled(&conn, "t1", &[row, rec(6, "Keep", NOW)], None).unwrap();
        let id5 = find_by_remote_id(&conn, 5).unwrap().unwrap().id;
        sync_queue::enqueue(&conn, ENTITY_TYPE, &id5, "update", Some("{}")).unwrap();

        assert!(!apply_tombstone(&conn, "t1", 999).unwrap());
        assert!(!apply_tombstone(&conn, "other-team", 5).unwrap());
        assert_eq!(count(&conn), 2);

        assert!(apply_tombstone(&conn, "t1", 5).unwrap());
        assert!(!apply_tombstone(&conn, "t1", 5).unwrap(), "repeat is safe");
        assert_eq!(count(&conn), 1);
        assert!(find_by_remote_id(&conn, 6).unwrap().is_some());
        assert!(label_ids(&conn, &id5).unwrap().is_empty());
        assert!(!sync_queue::has_pending_entry(&conn, ENTITY_TYPE, &id5).unwrap());
    }

    #[test]
    fn applying_the_same_batch_twice_leaves_one_row_per_remote_id_with_latest_values() {
        let conn = setup();
        let first = [rec(5, "Old", "2026-02-01T00:00:00.000Z"), rec(6, "Six", NOW)];
        apply_pulled(&conn, "t1", &first, Some("c1")).unwrap();
        let id5 = find_by_remote_id(&conn, 5).unwrap().unwrap().id;

        // Re-delivery at the overlap: row 5 changed, row 6 repeated.
        let mut changed = rec(5, "New", "2026-03-01T00:00:00.000Z");
        changed.active = false;
        let again = [changed, rec(6, "Six", NOW)];
        let a = apply_pulled(&conn, "t1", &again, Some("c2")).unwrap();
        apply_pulled(&conn, "t1", &again, Some("c2")).unwrap();

        assert_eq!(a.upserted, 2);
        assert_eq!(count(&conn), 2);
        let r5 = find_by_remote_id(&conn, 5).unwrap().unwrap();
        assert_eq!(r5.id, id5, "local id is stable across re-apply");
        assert_eq!(r5.title, "New");
        assert!(!r5.active);
        assert_eq!(cursor(&conn, "t1").unwrap().as_deref(), Some("c2"));
    }

    #[test]
    fn remote_references_resolve_to_local_ids_and_the_label_join_is_replaced() {
        let conn = setup();
        let mut row = rec(5, "R", NOW);
        row.assignee = rref(3, "Ann");
        row.project = rref(9, "P");
        row.lane = rref(4, "Todo");
        row.label_ids = vec![1, 2, 99];
        row.creator = rref(3, "Ann");
        row.priority = "high".into();
        apply_pulled(&conn, "t1", &[row.clone()], None).unwrap();

        let r = find_by_remote_id(&conn, 5).unwrap().unwrap();
        assert_eq!(r.assignee_id.as_deref(), Some("m1"));
        assert_eq!(r.assignee_name.as_deref(), Some("Ann"));
        assert_eq!(r.project_id.as_deref(), Some("p1"));
        assert_eq!(r.lane_id.as_deref(), Some("l1"));
        assert_eq!(r.creator_name.as_deref(), Some("Ann"));
        assert_eq!(r.priority, Priority::High);
        assert_eq!(label_ids(&conn, &r.id).unwrap(), vec!["lab1", "lab2"]);

        // Re-apply with fewer labels, no lane, and an unknown assignee.
        row.label_ids = vec![2];
        row.lane = None;
        row.assignee = rref(77, "Gone");
        apply_pulled(&conn, "t1", &[row], None).unwrap();
        let r = find_by_remote_id(&conn, 5).unwrap().unwrap();
        assert_eq!(label_ids(&conn, &r.id).unwrap(), vec!["lab2"]);
        assert_eq!(r.lane_id, None);
        assert_eq!(r.assignee_id, None);
        assert_eq!(r.assignee_name.as_deref(), Some("Gone"));
    }

    #[test]
    fn a_row_with_a_queued_local_write_keeps_its_local_values() {
        let conn = setup();
        apply_pulled(&conn, "t1", &[rec(5, "Local", NOW)], None).unwrap();
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;
        sync_queue::enqueue(&conn, ENTITY_TYPE, &id, "update", None).unwrap();
        let a = apply_pulled(&conn, "t1", &[rec(5, "Server", NOW)], None).unwrap();
        assert_eq!((a.upserted, a.skipped_pending), (0, 1));
        assert_eq!(get(&conn, &id).unwrap().unwrap().title, "Local");
    }

    #[test]
    fn deleting_a_target_nulls_the_reference_and_deleting_the_team_cascades() {
        let conn = setup();
        let mut row = rec(5, "R", NOW);
        row.lane = rref(4, "Todo");
        row.project = rref(9, "P");
        row.label_ids = vec![1];
        apply_pulled(&conn, "t1", &[row], None).unwrap();
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;

        conn.execute("DELETE FROM projects WHERE id = 'p1'", []).unwrap();
        conn.execute("DELETE FROM labels WHERE id = 'lab1'", []).unwrap();
        let r = get(&conn, &id).unwrap().unwrap();
        assert_eq!(r.project_id, None);
        assert!(label_ids(&conn, &id).unwrap().is_empty());

        conn.execute("DELETE FROM lanes WHERE id = 'l1'", []).unwrap();
        assert_eq!(get(&conn, &id).unwrap().unwrap().lane_id, None);

        // Other tables reference the team without cascading; clear them first.
        for t in ["labels", "members", "projects"] {
            conn.execute(&format!("DELETE FROM {t}"), []).unwrap();
        }
        conn.execute("DELETE FROM teams WHERE id = 't1'", []).unwrap();
        assert_eq!(count(&conn), 0);
    }

    #[test]
    fn list_for_team_returns_only_that_teams_rows() {
        let conn = setup();
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES ('t2', 'U', 'U', 701, ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        apply_pulled(&conn, "t1", &[rec(5, "A", NOW)], None).unwrap();
        apply_pulled(&conn, "t2", &[rec(6, "B", NOW)], None).unwrap();
        let rows = list_for_team(&conn, "t1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "A");
        assert_eq!(cursor(&conn, "t2").unwrap(), None);
    }
}
