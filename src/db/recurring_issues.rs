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
        // A queued delete means the local row is already gone; don't bring it back.
        if has_pending_delete(&tx, row.id)? {
            applied.skipped_pending += 1;
            continue;
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

/// True while a `recurring_issue` delete for `remote_id` is queued and unpushed.
fn has_pending_delete(conn: &Connection, remote_id: i64) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_queue
         WHERE entity_type = ?1 AND action = 'delete' AND synced_at IS NULL
           AND json_extract(payload, '$.remote_id') = ?2)",
        params![ENTITY_TYPE, remote_id],
        |r| r.get(0),
    )
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

/// Record which recurring issue spawned `issue_id`, from the server's
/// `recurring_issue_id` on the pulled issue. `None` clears it (hand-made
/// issue). A remote id with no local row yet leaves the stored value alone.
pub fn set_issue_origin(
    conn: &Connection,
    issue_id: &str,
    recurring_remote_id: Option<i64>,
) -> Result<()> {
    let local: Option<String> = match recurring_remote_id {
        None => None,
        Some(remote) => match find_by_remote_id(conn, remote)? {
            Some(r) => Some(r.id),
            None => return Ok(()),
        },
    };
    conn.execute(
        "UPDATE issues SET recurring_issue_id = ?1 WHERE id = ?2",
        params![local, issue_id],
    )?;
    Ok(())
}

/// Local id of the recurring issue that spawned `issue_id`, if any.
pub fn origin_id(conn: &Connection, issue_id: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT recurring_issue_id FROM issues WHERE id = ?1",
            params![issue_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// The recurring issue that spawned `issue_id`, if any (raw row).
pub fn origin_of_issue(conn: &Connection, issue_id: &str) -> Result<Option<RecurringIssue>> {
    match origin_id(conn, issue_id)? {
        Some(id) => get(conn, &id),
        None => Ok(None),
    }
}

/// Current title of the recurring issue that spawned `issue_id`, for the
/// issue detail's "From recurring issue" line. `None` when unlinked or the
/// recurring issue is gone.
pub fn origin_title(conn: &Connection, issue_id: &str) -> Option<String> {
    origin_of_issue(conn, issue_id)
        .ok()
        .flatten()
        .map(|r| r.title)
}

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
pub fn list_for_team(conn: &Connection, local_team_id: &str) -> Result<Vec<RecurringIssue>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM recurring_issues WHERE team_id = ?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map(params![local_team_id], RecurringIssue::from_row)?;
    rows.collect()
}

/// One display-ready row of the Recurring issues view. Column tickets add
/// fields here (schedule summary, next run, assignee, project, lane, labels)
/// and fill them in [`row_from`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurringIssueRow {
    /// Local uuid.
    pub id: String,
    pub title: String,
    pub active: bool,
    /// e.g. "Every 2 weeks on Wednesday".
    pub schedule: String,
    /// Server-calculated next run date (`YYYY-MM-DD`), in the schedule's zone.
    pub next_run_on: Option<String>,
    /// Zone abbreviation (or the zone name if unknown); `None` = unresolved.
    pub time_zone_label: Option<String>,
    /// True when a local write is queued for this row, so `next_run_on` may be
    /// stale until the server answers.
    pub next_run_pending: bool,
    /// Who spawned issues are assigned to: the local member's name, else the
    /// server's assignee name, else `None`.
    pub assignee: Option<String>,
    /// Current local name of the linked project; `None` with no project.
    pub project_name: Option<String>,
    /// Local name of the lane spawned issues start in, or [`FIRST_LANE`] when
    /// none is set (the server then uses the team's first lane).
    pub lane: String,
    /// Attached labels as `(name, hex color)`, sorted by name.
    pub labels: Vec<(String, Option<String>)>,
}

/// Lane cell text for a recurring issue with no lane set.
pub const FIRST_LANE: &str = "First lane";

/// Abbreviation for an ActiveSupport zone name (US zones), else `None`.
pub fn time_zone_abbreviation(name: &str) -> Option<&'static str> {
    Some(match name {
        "Eastern Time (US & Canada)" => "ET",
        "Central Time (US & Canada)" => "CT",
        "Mountain Time (US & Canada)" => "MT",
        "Arizona" => "MST",
        "Pacific Time (US & Canada)" => "PT",
        "Alaska" => "AKT",
        "Hawaii" => "HST",
        "UTC" => "UTC",
        _ => return None,
    })
}

/// What a row shows for a stored zone name: the abbreviation, else the name.
pub fn time_zone_label(name: &str) -> String {
    time_zone_abbreviation(name).unwrap_or(name).to_string()
}

/// Build the display row for one mirrored recurring issue. Column tickets
/// extend this (they may use `conn` to resolve names).
fn row_from(conn: &Connection, r: &RecurringIssue) -> Result<RecurringIssueRow> {
    let local_assignee = match &r.assignee_id {
        Some(mid) => conn
            .query_row("SELECT name FROM members WHERE id = ?1", params![mid], |x| {
                x.get::<_, String>(0)
            })
            .optional()?,
        None => None,
    };
    let project_name = match &r.project_id {
        Some(pid) => conn
            .query_row("SELECT name FROM projects WHERE id = ?1", params![pid], |x| x.get(0))
            .optional()?,
        None => None,
    };
    let lane = match &r.lane_id {
        Some(id) => conn
            .query_row("SELECT name FROM lanes WHERE id = ?1", params![id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?,
        None => None,
    };
    Ok(RecurringIssueRow {
        id: r.id.clone(),
        title: r.title.clone(),
        active: r.active,
        schedule: crate::schedule_summary::schedule_summary(
            &r.frequency,
            r.interval,
            r.weekday,
            r.day_of_month,
        ),
        next_run_on: r.next_run_on.clone(),
        time_zone_label: r.time_zone.as_deref().map(time_zone_label),
        next_run_pending: next_run_pending(conn, &r.id)?,
        assignee: local_assignee.or_else(|| r.assignee_name.clone()),
        project_name,
        lane: lane.unwrap_or_else(|| FIRST_LANE.to_string()),
        labels: label_chips(conn, &r.id)?,
    })
}

/// `(name, color)` of a recurring issue's labels, sorted by name.
fn label_chips(conn: &Connection, id: &str) -> Result<Vec<(String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT l.name, l.color FROM recurring_issue_labels ril
         JOIN labels l ON l.id = ril.label_id
         WHERE ril.recurring_issue_id = ?1 ORDER BY l.name, l.id",
    )?;
    let rows = stmt.query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

#[cfg(test)]
mod time_zone_tests {
    use super::*;

    #[test]
    fn us_zone_names_map_to_abbreviations_with_name_fallback() {
        for (name, abbr) in [
            ("Eastern Time (US & Canada)", "ET"),
            ("Central Time (US & Canada)", "CT"),
            ("Mountain Time (US & Canada)", "MT"),
            ("Arizona", "MST"),
            ("Pacific Time (US & Canada)", "PT"),
            ("Alaska", "AKT"),
            ("Hawaii", "HST"),
        ] {
            assert_eq!(time_zone_label(name), abbr);
        }
        assert_eq!(time_zone_label("Tokyo"), "Tokyo");
        assert_eq!(time_zone_abbreviation("Tokyo"), None);
    }
}

/// A team's display-ready rows for the Recurring issues view, in a stable
/// order (oldest first, id as tiebreak).
pub fn list_rows(conn: &Connection, local_team_id: &str) -> Result<Vec<RecurringIssueRow>> {
    list_for_team(conn, local_team_id)?
        .iter()
        .map(|r| row_from(conn, r))
        .collect()
}

/// Run a local recurring issue write and enqueue its push in ONE transaction,
/// so the row and its `recurring_issue` queue entry commit together or not at
/// all. Returns the new sync-queue entry id. Every local write (set active,
/// edit, delete, create) goes through this; `action` is the queue action and
/// `payload` its JSON (see [`update_payload`] for `update`).
pub fn write_queued(
    conn: &Connection,
    id: &str,
    action: &str,
    payload: Option<&str>,
    edit: impl FnOnce(&Connection) -> Result<()>,
) -> Result<i64> {
    let tx = conn.unchecked_transaction()?;
    edit(&tx)?;
    let entry = sync_queue::enqueue_returning_id(&tx, ENTITY_TYPE, id, action, payload)?;
    tx.commit()?;
    Ok(entry)
}

/// Payload of a `recurring_issue` / `update` queue entry:
/// `{"changed":["active",...]}` names the fields the push sends (values are
/// re-read from the row at push time), plus `"starts_on"` (`YYYY-MM-DD`) only
/// when the change moves the schedule. Without `starts_on` the server keeps
/// the stored `next_run_on` (a pause/resume must not reset the schedule).
pub fn update_payload(changed: &[&str], starts_on: Option<&str>) -> String {
    let mut v = serde_json::json!({ "changed": changed });
    if let Some(d) = starts_on {
        v["starts_on"] = serde_json::Value::String(d.to_string());
    }
    v.to_string()
}

/// Decode an update payload into (changed field names, starts_on). A missing
/// or unparseable payload is "no known fields", no `starts_on`.
pub fn parse_update_payload(payload: Option<&str>) -> (Vec<String>, Option<String>) {
    let Some(v) = payload.and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok()) else {
        return (Vec::new(), None);
    };
    let changed = v
        .get("changed")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let starts_on = v.get("starts_on").and_then(|s| s.as_str()).map(String::from);
    (changed, starts_on)
}

/// Pause (`false`) or resume (`true`) a recurring issue locally and queue the
/// `update` push, atomically. Returns the queue entry id, or `None` when the
/// row doesn't exist (nothing written). The schedule is untouched, so the
/// payload carries no `starts_on`.
pub fn set_active(conn: &Connection, id: &str, active: bool) -> Result<Option<i64>> {
    if get(conn, id)?.is_none() {
        return Ok(None);
    }
    let payload = update_payload(&["active"], None);
    write_queued(conn, id, "update", Some(&payload), |c| {
        c.execute(
            "UPDATE recurring_issues SET active = ?1, updated_at = ?2 WHERE id = ?3",
            params![active, crate::db::now_iso(), id],
        )?;
        Ok(())
    })
    .map(Some)
}

/// A recurring issue as the create form hands it over, already validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRecurringIssue {
    pub title: String,
    pub description: Option<String>,
    pub priority: Priority,
    /// "daily" | "weekly" | "monthly".
    pub frequency: String,
    pub interval: i64,
    /// Set only for weekly (0 = Sunday).
    pub weekday: Option<i64>,
    /// Set only for monthly.
    pub day_of_month: Option<i64>,
    /// First run date, `YYYY-MM-DD`.
    pub starts_on: String,
    /// Local member, project and lane ids; `None` = unassigned / no project /
    /// the team's first lane.
    pub assignee_id: Option<String>,
    pub project_id: Option<String>,
    pub lane_id: Option<String>,
    /// Local label ids to attach.
    pub label_ids: Vec<String>,
}

/// Payload of a `recurring_issue` / `create` queue entry: `{"starts_on":...}`.
/// The other fields are re-read from the row at push time.
pub fn create_payload(starts_on: &str) -> String {
    serde_json::json!({ "starts_on": starts_on }).to_string()
}

/// The `starts_on` carried by a create or update payload, if any.
pub fn payload_starts_on(payload: Option<&str>) -> Option<String> {
    let v = serde_json::from_str::<serde_json::Value>(payload?).ok()?;
    v.get("starts_on")?.as_str().map(String::from)
}

/// Create a recurring issue locally (time zone `NULL`, i.e. the creator's
/// preference; active; no next run until the server answers) and queue the
/// `create` push carrying `starts_on`, atomically. Returns the new local id.
pub fn create(conn: &Connection, local_team_id: &str, new: &NewRecurringIssue) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let payload = create_payload(&new.starts_on);
    write_queued(conn, &id, "create", Some(&payload), |c| {
        let now = crate::db::now_iso();
        c.execute(
            "INSERT INTO recurring_issues (id, team_id, title, description, priority, frequency,
                interval, weekday, day_of_month, time_zone, active, created_at, updated_at,
                assignee_id, project_id, lane_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, 1, ?10, ?10, ?11, ?12, ?13)",
            params![
                id,
                local_team_id,
                new.title,
                new.description,
                new.priority.to_i32(),
                new.frequency,
                new.interval,
                new.weekday,
                new.day_of_month,
                now,
                new.assignee_id,
                new.project_id,
                new.lane_id
            ],
        )?;
        for label_id in &new.label_ids {
            c.execute(
                "INSERT OR IGNORE INTO recurring_issue_labels (recurring_issue_id, label_id)
                 VALUES (?1, ?2)",
                params![id, label_id],
            )?;
        }
        Ok(())
    })?;
    Ok(id)
}

/// True while a local write that moves the schedule is unsynced, so the shown
/// next run is not trustworthy: a queued `create`, or a queued `update` whose
/// payload carries `starts_on`. Pause/resume and content-only edits don't count.
pub fn next_run_pending(conn: &Connection, id: &str) -> Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT action, payload FROM sync_queue
         WHERE entity_type = ?1 AND entity_id = ?2 AND synced_at IS NULL",
    )?;
    let entries = stmt.query_map(params![ENTITY_TYPE, id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    for e in entries {
        let (action, payload) = e?;
        if action == "create"
            || (action == "update" && payload_starts_on(payload.as_deref()).is_some())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Delete a recurring issue locally and queue the server delete, atomically.
/// Pending entries for the row (an earlier edit or pause) are retired first.
/// A row the server never saw (no remote id, or its team unsynced) is just
/// removed along with its queued create, so nothing is queued. Returns the
/// queue entry id when a delete was queued; `None` otherwise (including a
/// missing row). The payload carries `team_remote_id` and `remote_id` because
/// the row is gone by push time.
pub fn delete_local(conn: &Connection, id: &str) -> Result<Option<i64>> {
    let Some(row) = get(conn, id)? else {
        return Ok(None);
    };
    let team_remote = super::queries::remote_id_for(conn, "teams", &row.team_id)?;
    let remove = |c: &Connection| -> Result<()> {
        c.execute(
            "DELETE FROM recurring_issue_labels WHERE recurring_issue_id = ?1",
            params![id],
        )?;
        c.execute("DELETE FROM recurring_issues WHERE id = ?1", params![id])?;
        super::queries::retire_pending_sync(c, ENTITY_TYPE, id)
    };
    match (row.remote_id, team_remote) {
        (Some(remote_id), Some(team_remote_id)) => {
            let payload = serde_json::json!({
                "team_remote_id": team_remote_id,
                "remote_id": remote_id,
            })
            .to_string();
            write_queued(conn, id, "delete", Some(&payload), remove).map(Some)
        }
        _ => {
            let tx = conn.unchecked_transaction()?;
            remove(&tx)?;
            tx.commit()?;
            Ok(None)
        }
    }
}

/// Overwrite the local row `id` with a push response (the server's version
/// wins, including `next_run_on` and the label join). Unlike [`apply_pulled`]
/// it ignores queued entries: it runs while the pushed entry is still queued.
pub fn apply_response(conn: &Connection, id: &str, dto: &RecurringIssueDto) -> Result<()> {
    let Some(local) = get(conn, id)? else {
        return Ok(());
    };
    let tx = conn.unchecked_transaction()?;
    upsert_row(&tx, id, &local.team_id, dto)?;
    replace_labels(&tx, id, &dto.label_ids)?;
    tx.commit()
}

/// Finish a successful create push in one transaction: save the remote id,
/// mark the queue entry synced, and overwrite the row and labels with the
/// response (next run, resolved time zone, last run, timestamps). Any failure
/// rolls all of it back, so the entry stays pending. Marking the entry synced
/// is what clears "next run pending".
pub fn commit_created(
    conn: &mut Connection,
    id: &str,
    entry_id: i64,
    dto: &RecurringIssueDto,
) -> Result<()> {
    let tx = conn.transaction()?;
    // The remote id goes in first so the upsert's ON CONFLICT(remote_id) hits
    // this row instead of inserting a second one.
    tx.execute(
        "UPDATE recurring_issues SET remote_id = ?1 WHERE id = ?2",
        params![dto.id, id],
    )?;
    let team_id: String = tx.query_row(
        "SELECT team_id FROM recurring_issues WHERE id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    upsert_row(&tx, id, &team_id, dto)?;
    replace_labels(&tx, id, &dto.label_ids)?;
    super::sync_queue::mark_synced(&tx, entry_id)?;
    tx.commit()
}

/// Local label ids joined to a recurring issue, sorted.
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

    fn add_issue(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at)
             VALUES (?1, 'x', 4, 'l1', 't1', ?2, ?2)",
            params![id, NOW],
        )
        .unwrap();
    }

    #[test]
    fn issue_origin_is_stored_cleared_and_kept_when_unresolved() {
        let conn = setup();
        apply_pulled(&conn, "t1", &[rec(1, "Weekly", NOW)], Some("s")).unwrap();
        add_issue(&conn, "i1");
        assert_eq!(origin_id(&conn, "i1").unwrap(), None);

        set_issue_origin(&conn, "i1", Some(1)).unwrap();
        let r = origin_of_issue(&conn, "i1").unwrap().unwrap();
        assert_eq!(r.title, "Weekly");

        // Unknown remote id: keep what we have.
        set_issue_origin(&conn, "i1", Some(99)).unwrap();
        assert!(origin_id(&conn, "i1").unwrap().is_some());

        set_issue_origin(&conn, "i1", None).unwrap();
        assert_eq!(origin_id(&conn, "i1").unwrap(), None);
    }

    #[test]
    fn origin_title_follows_rename_and_disappears_on_delete() {
        let conn = setup();
        apply_pulled(&conn, "t1", &[rec(1, "Weekly", NOW)], Some("s")).unwrap();
        add_issue(&conn, "i1");
        assert_eq!(origin_title(&conn, "i1"), None);
        set_issue_origin(&conn, "i1", Some(1)).unwrap();
        assert_eq!(origin_title(&conn, "i1").as_deref(), Some("Weekly"));
        apply_pulled(&conn, "t1", &[rec(1, "Fortnightly", NOW)], Some("s")).unwrap();
        assert_eq!(origin_title(&conn, "i1").as_deref(), Some("Fortnightly"));
        assert!(apply_tombstone(&conn, "t1", 1).unwrap());
        assert_eq!(origin_title(&conn, "i1"), None);
    }

    #[test]
    fn tombstoning_a_recurring_issue_nulls_the_origin() {
        let conn = setup();
        apply_pulled(&conn, "t1", &[rec(1, "Weekly", NOW)], Some("s")).unwrap();
        add_issue(&conn, "i1");
        set_issue_origin(&conn, "i1", Some(1)).unwrap();
        assert!(apply_tombstone(&conn, "t1", 1).unwrap());
        assert_eq!(origin_id(&conn, "i1").unwrap(), None);
    }

    fn rref(id: i64, name: &str) -> Option<RecordRef> {
        Some(RecordRef { id, name: name.into() })
    }

    fn count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM recurring_issues", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn rows_show_local_member_then_server_name_then_nothing() {
        let conn = setup();
        let mut local = rec(1, "Local", "2026-01-01T00:00:01.000Z");
        local.assignee = rref(3, "Server Ann");
        let mut gone = rec(2, "Gone", "2026-01-01T00:00:02.000Z");
        gone.assignee = rref(77, "Left Team");
        let none = rec(3, "None", "2026-01-01T00:00:03.000Z");
        apply_pulled(&conn, "t1", &[local, gone, none], None).unwrap();

        let rows = list_rows(&conn, "t1").unwrap();
        let by = |t: &str| rows.iter().find(|r| r.title == t).unwrap().assignee.clone();
        assert_eq!(by("Local").as_deref(), Some("Ann"));
        assert_eq!(by("Gone").as_deref(), Some("Left Team"));
        assert_eq!(by("None"), None);
    }

    #[test]
    fn rows_carry_label_names_and_colors_sorted_by_name() {
        let conn = setup();
        conn.execute("UPDATE labels SET name = 'Zed', color = '#ff0000' WHERE id = 'lab1'", [])
            .unwrap();
        conn.execute("UPDATE labels SET name = 'Alpha' WHERE id = 'lab2'", []).unwrap();
        let mut with = rec(5, "With", NOW);
        with.label_ids = vec![1, 2];
        apply_pulled(&conn, "t1", &[with, rec(6, "Without", NOW)], None).unwrap();
        let rows = list_rows(&conn, "t1").unwrap();
        let by = |t: &str| rows.iter().find(|r| r.title == t).unwrap().labels.clone();
        assert_eq!(
            by("With"),
            vec![
                ("Alpha".to_string(), None),
                ("Zed".to_string(), Some("#ff0000".to_string()))
            ]
        );
        assert!(by("Without").is_empty());
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

    fn pending_entries(conn: &Connection) -> Vec<(String, String, Option<String>)> {
        let mut stmt = conn
            .prepare("SELECT entity_type, action, payload FROM sync_queue WHERE synced_at IS NULL")
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    fn new_weekly() -> NewRecurringIssue {
        NewRecurringIssue {
            title: "Water plants".into(),
            description: Some("all of them".into()),
            priority: Priority::High,
            frequency: "weekly".into(),
            interval: 2,
            weekday: Some(3),
            day_of_month: None,
            starts_on: "2026-11-04".into(),
            assignee_id: None,
            project_id: None,
            lane_id: None,
            label_ids: vec![],
        }
    }

    #[test]
    fn create_stores_the_routing_and_the_row_shows_it() {
        let conn = setup();
        let mut new = new_weekly();
        new.lane_id = Some("l1".into());
        new.project_id = Some("p1".into());
        new.assignee_id = Some("m1".into());
        let id = create(&conn, "t1", &new).unwrap();
        let r = get(&conn, &id).unwrap().unwrap();
        assert_eq!(r.lane_id.as_deref(), Some("l1"));
        assert_eq!(r.project_id.as_deref(), Some("p1"));
        assert_eq!(r.assignee_id.as_deref(), Some("m1"));
        let row = list_rows(&conn, "t1").unwrap().remove(0);
        assert!(row.assignee.is_some() && row.project_name.is_some());
        assert_ne!(row.lane, FIRST_LANE);

        let id2 = create(&conn, "t1", &new_weekly()).unwrap();
        let r2 = get(&conn, &id2).unwrap().unwrap();
        assert_eq!((r2.lane_id, r2.project_id, r2.assignee_id), (None, None, None));
    }

    #[test]
    fn create_writes_the_chosen_labels() {
        let conn = setup();
        let mut new = new_weekly();
        new.label_ids = vec!["lab2".into(), "lab1".into()];
        let id = create(&conn, "t1", &new).unwrap();
        assert_eq!(label_ids(&conn, &id).unwrap(), vec!["lab1", "lab2"]);
        let id2 = create(&conn, "t1", &new_weekly()).unwrap();
        assert!(label_ids(&conn, &id2).unwrap().is_empty());
    }

    #[test]
    fn commit_created_saves_remote_id_marks_synced_and_clears_next_run_pending() {
        let mut conn = setup();
        let id = create(&conn, "t1", &new_weekly()).unwrap();
        let entry_id = sync_queue::pending_entries(&conn).unwrap()[0].id.unwrap();
        assert!(next_run_pending(&conn, &id).unwrap());

        let mut dto = rec(41, "Water plants", "2026-02-01T00:00:00.000Z");
        dto.time_zone = Some("Eastern Time (US & Canada)".into());
        dto.next_run_on = Some("2026-11-04".into());
        commit_created(&mut conn, &id, entry_id, &dto).unwrap();

        let row = get(&conn, &id).unwrap().unwrap();
        assert_eq!(row.remote_id, Some(41));
        assert_eq!(row.next_run_on.as_deref(), Some("2026-11-04"));
        assert_eq!(row.time_zone.as_deref(), Some("Eastern Time (US & Canada)"));
        assert_eq!(list_for_team(&conn, "t1").unwrap().len(), 1);
        assert!(sync_queue::pending_entries(&conn).unwrap().is_empty());
        assert!(!next_run_pending(&conn, &id).unwrap());
    }

    #[test]
    fn commit_created_rolls_back_when_the_entry_cannot_be_marked() {
        let mut conn = setup();
        let id = create(&conn, "t1", &new_weekly()).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER no_mark BEFORE UPDATE ON sync_queue
             BEGIN SELECT RAISE(ABORT, 'nope'); END;",
        )
        .unwrap();
        let entry_id = sync_queue::pending_entries(&conn).unwrap()[0].id.unwrap();
        assert!(commit_created(&mut conn, &id, entry_id, &rec(41, "X", NOW)).is_err());
        let row = get(&conn, &id).unwrap().unwrap();
        assert_eq!(row.remote_id, None);
        assert_eq!(row.title, "Water plants");
    }

    #[test]
    fn create_inserts_the_row_and_queues_one_create_carrying_starts_on() {
        let conn = setup();
        let id = create(&conn, "t1", &new_weekly()).unwrap();
        let row = get(&conn, &id).unwrap().unwrap();
        assert_eq!(row.title, "Water plants");
        assert_eq!(row.priority, Priority::High);
        assert_eq!((row.weekday, row.day_of_month, row.interval), (Some(3), None, 2));
        assert_eq!(row.time_zone, None);
        assert!(row.active);
        assert_eq!(row.remote_id, None);
        let pending = pending_entries(&conn);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, "create");
        assert_eq!(payload_starts_on(pending[0].2.as_deref()), Some("2026-11-04".into()));
        let rows = list_rows(&conn, "t1").unwrap();
        assert_eq!(rows[0].schedule, "Every 2 weeks on Wednesday");
        assert!(rows[0].next_run_pending);
    }

    #[test]
    fn next_run_pending_ignores_pause_and_content_edits_but_counts_starts_on() {
        let conn = setup();
        let id = create(&conn, "t1", &new_weekly()).unwrap();
        conn.execute("UPDATE sync_queue SET synced_at = ?1", params![NOW]).unwrap();
        assert!(!next_run_pending(&conn, &id).unwrap());
        set_active(&conn, &id, false).unwrap();
        assert!(!next_run_pending(&conn, &id).unwrap());
        let p = update_payload(&["frequency"], Some("2026-11-11"));
        sync_queue::enqueue(&conn, ENTITY_TYPE, &id, "update", Some(&p)).unwrap();
        assert!(next_run_pending(&conn, &id).unwrap());
    }

    #[test]
    fn set_active_enqueues_one_update_without_starts_on() {
        let conn = setup();
        apply_pulled(&conn, "t1", &[rec(5, "R", NOW)], None).unwrap();
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;

        assert!(set_active(&conn, &id, false).unwrap().is_some());

        assert!(!get(&conn, &id).unwrap().unwrap().active);
        let entries = pending_entries(&conn);
        assert_eq!(entries.len(), 1);
        assert_eq!((entries[0].0.as_str(), entries[0].1.as_str()), (ENTITY_TYPE, "update"));
        let payload: serde_json::Value =
            serde_json::from_str(entries[0].2.as_deref().unwrap()).unwrap();
        assert_eq!(payload, serde_json::json!({ "changed": ["active"] }));
        assert!(payload.get("starts_on").is_none());
    }

    #[test]
    fn set_active_on_a_missing_row_writes_nothing() {
        let conn = setup();
        assert_eq!(set_active(&conn, "nope", true).unwrap(), None);
        assert!(pending_entries(&conn).is_empty());
    }

    #[test]
    fn write_queued_rolls_back_the_edit_when_the_edit_fails() {
        let conn = setup();
        apply_pulled(&conn, "t1", &[rec(5, "R", NOW)], None).unwrap();
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;
        let r = write_queued(&conn, &id, "update", None, |c| {
            c.execute("UPDATE recurring_issues SET active = 0 WHERE id = ?1", params![id])?;
            c.execute("UPDATE no_such_table SET x = 1", [])?;
            Ok(())
        });
        assert!(r.is_err());
        assert!(get(&conn, &id).unwrap().unwrap().active);
        assert!(pending_entries(&conn).is_empty());
    }

    #[test]
    fn update_payload_round_trips_and_only_schedule_changes_carry_starts_on() {
        let p = update_payload(&["frequency", "interval"], Some("2026-11-05"));
        assert_eq!(
            parse_update_payload(Some(&p)),
            (
                vec!["frequency".to_string(), "interval".to_string()],
                Some("2026-11-05".to_string())
            )
        );
        assert_eq!(parse_update_payload(None), (vec![], None));
        assert_eq!(parse_update_payload(Some("junk")), (vec![], None));
    }

    #[test]
    fn a_pull_during_a_pending_toggle_does_not_revert_the_row() {
        let conn = setup();
        apply_pulled(&conn, "t1", &[rec(5, "R", NOW)], None).unwrap();
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;
        set_active(&conn, &id, false).unwrap();

        let applied =
            apply_pulled(&conn, "t1", &[rec(5, "R", "2026-02-01T00:00:00.000Z")], None).unwrap();

        assert_eq!(applied.skipped_pending, 1);
        assert!(!get(&conn, &id).unwrap().unwrap().active);
    }

    #[test]
    fn apply_response_overwrites_the_row_even_while_its_entry_is_queued() {
        let conn = setup();
        apply_pulled(&conn, "t1", &[rec(5, "R", NOW)], None).unwrap();
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;
        set_active(&conn, &id, false).unwrap();

        let mut resp = rec(5, "R server", "2026-02-01T00:00:00.000Z");
        resp.active = false;
        resp.next_run_on = Some("2026-11-09".into());
        resp.label_ids = vec![1];
        apply_response(&conn, &id, &resp).unwrap();

        let row = get(&conn, &id).unwrap().unwrap();
        assert_eq!(row.title, "R server");
        assert_eq!(row.next_run_on.as_deref(), Some("2026-11-09"));
        assert_eq!(label_ids(&conn, &id).unwrap(), vec!["lab1".to_string()]);
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
    fn list_rows_carries_the_schedule_summary() {
        let conn = setup();
        let mut weekly = rec(1, "W", NOW);
        weekly.frequency = "weekly".into();
        weekly.interval = 2;
        weekly.weekday = Some(3);
        weekly.created_at = "2026-01-01T00:00:00.000Z".into();
        let mut daily = rec(2, "D", "2026-01-02T00:00:00.000Z");
        daily.created_at = "2026-01-02T00:00:00.000Z".into();
        apply_pulled(&conn, "t1", &[weekly, daily], None).unwrap();
        let schedules: Vec<String> = list_rows(&conn, "t1")
            .unwrap()
            .into_iter()
            .map(|r| r.schedule)
            .collect();
        assert_eq!(schedules, ["Every 2 weeks on Wednesday", "Every day"]);
    }

    #[test]
    fn list_rows_returns_the_teams_rows_with_title_and_active_in_stable_order() {
        let conn = setup();
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES ('t2', 'U', 'U', 701, ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        let mut paused = rec(6, "Second", "2026-02-01T00:00:00.000Z");
        paused.active = false;
        paused.created_at = "2026-01-02T00:00:00.000Z".into();
        apply_pulled(&conn, "t1", &[paused, rec(5, "First", NOW)], None).unwrap();
        apply_pulled(&conn, "t2", &[rec(7, "Other team", NOW)], None).unwrap();

        let rows = list_rows(&conn, "t1").unwrap();
        let got: Vec<(&str, bool)> = rows.iter().map(|r| (r.title.as_str(), r.active)).collect();
        assert_eq!(got, [("First", true), ("Second", false)]);
        assert!(list_rows(&conn, "nope").unwrap().is_empty());
    }

    #[test]
    fn list_rows_shows_the_projects_current_name_and_clears_when_deleted() {
        let conn = setup();
        let mut row = rec(5, "R", NOW);
        row.project = rref(9, "P");
        row.created_at = "2026-01-01T00:00:00.000Z".into();
        let mut other = rec(6, "None", "2026-02-01T00:00:00.000Z");
        other.created_at = "2026-01-02T00:00:00.000Z".into();
        apply_pulled(&conn, "t1", &[row, other], None).unwrap();
        let names = |c: &Connection| -> Vec<Option<String>> {
            list_rows(c, "t1").unwrap().into_iter().map(|r| r.project_name).collect()
        };
        assert_eq!(names(&conn), [Some("P".to_string()), None]);

        conn.execute("UPDATE projects SET name = 'Renamed' WHERE id = 'p1'", []).unwrap();
        assert_eq!(names(&conn), [Some("Renamed".to_string()), None]);

        conn.execute("DELETE FROM projects WHERE id = 'p1'", []).unwrap();
        assert_eq!(names(&conn), [None, None]);
    }

    #[test]
    fn list_rows_shows_the_lane_name_or_first_lane() {
        let conn = setup();
        let mut with = rec(5, "With", NOW);
        with.lane = rref(4, "Todo");
        with.created_at = "2026-01-01T00:00:00.000Z".into();
        let mut without = rec(6, "Without", "2026-02-01T00:00:00.000Z");
        without.created_at = "2026-01-02T00:00:00.000Z".into();
        apply_pulled(&conn, "t1", &[with, without], None).unwrap();
        conn.execute("UPDATE lanes SET name = 'Renamed' WHERE id = 'l1'", []).unwrap();
        let rows = list_rows(&conn, "t1").unwrap();
        let got: Vec<(&str, &str)> = rows.iter().map(|r| (r.title.as_str(), r.lane.as_str())).collect();
        assert_eq!(got, [("With", "Renamed"), ("Without", "First lane")]);
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

    #[test]
    fn deleting_a_synced_row_removes_it_and_queues_a_delete_with_ids() {
        let conn = setup();
        let mut row = rec(5, "Weekly", NOW);
        row.label_ids = vec![1];
        apply_pulled(&conn, "t1", &[row], None).unwrap();
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;
        set_active(&conn, &id, false).unwrap();

        assert!(delete_local(&conn, &id).unwrap().is_some());
        assert!(get(&conn, &id).unwrap().is_none());
        assert!(label_ids(&conn, &id).unwrap().is_empty());
        let pending = sync_queue::pending_entries(&conn).unwrap();
        // The earlier update is retired; only the delete is left.
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].action, "delete");
        assert_eq!(pending[0].entity_type, ENTITY_TYPE);
        let v: serde_json::Value =
            serde_json::from_str(pending[0].payload.as_deref().unwrap()).unwrap();
        assert_eq!(v["team_remote_id"], 700);
        assert_eq!(v["remote_id"], 5);
    }

    #[test]
    fn deleting_an_unsynced_row_queues_nothing() {
        let conn = setup();
        conn.execute(
            "INSERT INTO recurring_issues (id, team_id, title, priority, frequency, interval,
               active, created_at, updated_at)
             VALUES ('r1', 't1', 'New', 0, 'daily', 1, 1, ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        sync_queue::enqueue_returning_id(&conn, ENTITY_TYPE, "r1", "create", None).unwrap();
        assert_eq!(delete_local(&conn, "r1").unwrap(), None);
        assert!(get(&conn, "r1").unwrap().is_none());
        assert!(sync_queue::pending_entries(&conn).unwrap().is_empty());
    }

    #[test]
    fn deleting_a_missing_row_is_a_no_op() {
        let conn = setup();
        assert_eq!(delete_local(&conn, "nope").unwrap(), None);
        assert!(sync_queue::pending_entries(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_pull_does_not_resurrect_a_row_with_a_queued_delete() {
        let conn = setup();
        apply_pulled(&conn, "t1", &[rec(5, "Weekly", NOW)], None).unwrap();
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;
        delete_local(&conn, &id).unwrap();

        let applied = apply_pulled(
            &conn,
            "t1",
            &[rec(5, "Weekly", "2026-02-01T00:00:00.000Z"), rec(6, "Other", NOW)],
            Some("s"),
        )
        .unwrap();
        assert_eq!(applied.skipped_pending, 1);
        assert_eq!(applied.upserted, 1);
        assert!(find_by_remote_id(&conn, 5).unwrap().is_none());
        assert!(find_by_remote_id(&conn, 6).unwrap().is_some());
    }
}
