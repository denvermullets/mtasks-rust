use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Result};

use super::events::{self, Source};
use super::models::{Comment, Issue, IssueDependency, Label, Lane, Member, Project, Team};

// ── Teams ──

pub fn all_teams(conn: &Connection) -> Result<Vec<Team>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, identifier, remote_id, archived_at, created_at, updated_at
         FROM teams ORDER BY name",
    )?;
    let rows = stmt.query_map([], Team::from_row)?;
    rows.collect()
}

// ── Lanes ──

pub fn lanes_for_team(conn: &Connection, team_id: &str) -> Result<Vec<Lane>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, color, position, team_id, remote_id, category, created_at, updated_at
         FROM lanes WHERE team_id = ?1 ORDER BY position",
    )?;
    let rows = stmt.query_map(params![team_id], Lane::from_row)?;
    rows.collect()
}

// ── Issues ──

pub fn issues_for_team(conn: &Connection, team_id: &str) -> Result<Vec<Issue>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, description, priority, team_number, due_date, estimate,
                started_at, completed_at, canceled_at, lane_id, team_id,
                project_id, parent_issue_id, assignee_id, creator_id, archived_at,
                remote_id, created_at, updated_at
         FROM issues WHERE team_id = ?1 AND deleted_at IS NULL ORDER BY priority, created_at",
    )?;
    let rows = stmt.query_map(params![team_id], Issue::from_row)?;
    rows.collect()
}

pub fn issues_for_project(conn: &Connection, project_id: &str) -> Result<Vec<Issue>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, description, priority, team_number, due_date, estimate,
                started_at, completed_at, canceled_at, lane_id, team_id,
                project_id, parent_issue_id, assignee_id, creator_id, archived_at,
                remote_id, created_at, updated_at
         FROM issues WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY priority, created_at",
    )?;
    let rows = stmt.query_map(params![project_id], Issue::from_row)?;
    rows.collect()
}

pub fn next_team_number(conn: &Connection, team_id: &str) -> Result<i32> {
    let max: Option<i32> = conn.query_row(
        "SELECT MAX(team_number) FROM issues WHERE team_id = ?1",
        params![team_id],
        |row| row.get(0),
    )?;
    Ok(max.unwrap_or(0) + 1)
}

pub fn insert_issue(conn: &Connection, issue: &Issue) -> Result<()> {
    conn.execute(
        "INSERT INTO issues (id, title, description, priority, team_number, due_date, estimate, started_at, completed_at, canceled_at, lane_id, team_id, project_id, parent_issue_id, assignee_id, creator_id, remote_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        params![
            issue.id,
            issue.title,
            issue.description,
            issue.priority,
            issue.team_number,
            issue.due_date,
            issue.estimate,
            issue.started_at,
            issue.completed_at,
            issue.canceled_at,
            issue.lane_id,
            issue.team_id,
            issue.project_id,
            issue.parent_issue_id,
            issue.assignee_id,
            issue.creator_id,
            issue.remote_id,
            issue.created_at,
            issue.updated_at,
        ],
    )?;
    events::record_created(conn, issue, Source::Local)?;
    Ok(())
}

fn now_ts() -> String {
    crate::db::now_iso()
}

/// Record local activity events for a write that turned `old` into the
/// current row. No-op when the issue didn't exist beforehand.
fn record_local_diff(conn: &Connection, old: Option<Issue>, issue_id: &str) -> Result<()> {
    if let (Some(old), Some(new)) = (old, get_issue(conn, issue_id)?) {
        events::record_changes(conn, &old, &new, Source::Local, &now_ts())?;
    }
    Ok(())
}

pub fn update_issue(conn: &Connection, issue: &Issue) -> Result<()> {
    let old = get_issue(conn, &issue.id)?;
    conn.execute(
        "UPDATE issues SET title=?1, description=?2, priority=?3, due_date=?4, estimate=?5, started_at=?6, completed_at=?7, canceled_at=?8, lane_id=?9, project_id=?10, assignee_id=?11, updated_at=?12 WHERE id=?13",
        params![
            issue.title,
            issue.description,
            issue.priority,
            issue.due_date,
            issue.estimate,
            issue.started_at,
            issue.completed_at,
            issue.canceled_at,
            issue.lane_id,
            issue.project_id,
            issue.assignee_id,
            issue.updated_at,
            issue.id,
        ],
    )?;
    record_local_diff(conn, old, &issue.id)?;
    Ok(())
}

/// Soft-delete: stamp the local tombstone instead of removing the row, so a
/// later pull can't recreate the issue while the server delete is pending.
pub fn delete_issue(conn: &Connection, issue_id: &str) -> Result<()> {
    let now = crate::db::now_iso();
    conn.execute(
        "UPDATE issues SET deleted_at = ?2 WHERE id = ?1 AND deleted_at IS NULL",
        params![issue_id, now],
    )?;
    Ok(())
}

/// Hard-delete a tombstoned issue and everything attached to it (comments,
/// labels, dependencies, events). Sub-issues are detached, mirroring Rails'
/// nullify. No-op unless the row carries the tombstone.
pub fn purge_tombstoned_issue(conn: &Connection, issue_id: &str) -> Result<()> {
    let tombstoned: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issues WHERE id = ?1 AND deleted_at IS NOT NULL",
        params![issue_id],
        |r| r.get(0),
    )?;
    if tombstoned == 0 {
        return Ok(());
    }
    purge_issue_rows(conn, issue_id)
}

/// Hard-delete an issue and everything attached to it, whether or not it
/// carries the local tombstone. Callers decide that the issue must go
/// (a confirmed server delete, or a local delete the server has accepted).
fn purge_issue_rows(conn: &Connection, issue_id: &str) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE issues SET parent_issue_id = NULL WHERE parent_issue_id = ?1",
        params![issue_id],
    )?;
    tx.execute(
        "DELETE FROM comments WHERE issue_id = ?1",
        params![issue_id],
    )?;
    tx.execute(
        "DELETE FROM issue_labels WHERE issue_id = ?1",
        params![issue_id],
    )?;
    tx.execute(
        "DELETE FROM issue_dependencies WHERE blocking_issue_id = ?1 OR blocked_issue_id = ?1",
        params![issue_id],
    )?;
    tx.execute(
        "DELETE FROM issue_events WHERE issue_id = ?1",
        params![issue_id],
    )?;
    tx.execute("DELETE FROM issues WHERE id = ?1", params![issue_id])?;
    tx.commit()
}

/// Apply a server tombstone for an issue: hard-delete the local issue (with
/// everything attached), clear any local delete marker, and retire its pending
/// sync entries (a queued delete or edit has nothing left to act on). Returns
/// whether a local issue existed in this team. Idempotent.
pub fn apply_server_issue_tombstone(
    conn: &Connection,
    team_id: &str,
    remote_id: i64,
) -> Result<bool> {
    let local: Option<String> = conn
        .query_row(
            "SELECT id FROM issues WHERE remote_id = ?1 AND team_id = ?2",
            params![remote_id, team_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(local_id) = local else {
        return Ok(false);
    };
    // The server delete wins whether or not the user had already deleted it
    // locally, so purge without requiring the local marker.
    purge_issue_rows(conn, &local_id)?;
    conn.execute(
        "UPDATE sync_queue SET synced_at = ?2
         WHERE entity_type = 'issue' AND entity_id = ?1 AND synced_at IS NULL",
        params![
            local_id,
            crate::db::now_iso()
        ],
    )?;
    Ok(true)
}

fn retire_pending_sync(conn: &Connection, entity_type: &str, local_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE sync_queue SET synced_at = ?3
         WHERE entity_type = ?1 AND entity_id = ?2 AND synced_at IS NULL",
        params![
            entity_type,
            local_id,
            crate::db::now_iso()
        ],
    )?;
    Ok(())
}

/// Apply a server tombstone for a comment: hard-delete it and its replies
/// (children first, any depth), and retire pending sync entries for them.
/// Scoped to the team through the comment's issue. Idempotent; returns whether
/// a local comment existed.
pub fn apply_server_comment_tombstone(
    conn: &Connection,
    team_id: &str,
    remote_id: i64,
) -> Result<bool> {
    let local: Option<String> = conn
        .query_row(
            "SELECT c.id FROM comments c JOIN issues i ON i.id = c.issue_id
             WHERE c.remote_id = ?1 AND i.team_id = ?2",
            params![remote_id, team_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(local_id) = local else {
        return Ok(false);
    };
    let tx = conn.unchecked_transaction()?;
    let mut ids = vec![local_id];
    let mut i = 0;
    while i < ids.len() {
        let mut stmt = tx.prepare("SELECT id FROM comments WHERE parent_id = ?1")?;
        let kids: Vec<String> = stmt
            .query_map(params![ids[i]], |r| r.get(0))?
            .collect::<Result<_>>()?;
        drop(stmt);
        ids.extend(kids);
        i += 1;
    }
    for id in ids.iter().rev() {
        tx.execute("DELETE FROM comments WHERE id = ?1", params![id])?;
        retire_pending_sync(&tx, "comment", id)?;
    }
    tx.commit()?;
    Ok(true)
}

/// Apply a server tombstone for an issue dependency (Rails destroys blocking
/// edges when an issue completes): delete the local edge with this remote id
/// and retire its pending sync entries. Edges are team-scoped through their
/// issues. Idempotent; returns whether a local edge existed.
pub fn apply_server_dependency_tombstone(
    conn: &Connection,
    team_id: &str,
    remote_id: i64,
) -> Result<bool> {
    let local: Option<String> = conn
        .query_row(
            "SELECT d.id FROM issue_dependencies d
             JOIN issues i ON i.id = d.blocking_issue_id
             WHERE d.remote_id = ?1 AND i.team_id = ?2",
            params![remote_id, team_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(local_id) = local else {
        return Ok(false);
    };
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM issue_dependencies WHERE id = ?1", params![local_id])?;
    retire_pending_sync(&tx, "dependency", &local_id)?;
    tx.commit()?;
    Ok(true)
}

/// Delete an issue's local dependency edges (either side) that carry a
/// remote id the server's detail payload no longer lists. Edges with a NULL
/// remote id are local creates still pending push and are never touched.
/// Returns how many edges were removed.
pub fn prune_issue_dependencies(
    conn: &Connection,
    issue_id: &str,
    keep_remote_ids: &[i64],
) -> Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT id, remote_id FROM issue_dependencies
         WHERE (blocking_issue_id = ?1 OR blocked_issue_id = ?1) AND remote_id IS NOT NULL",
    )?;
    let stale: Vec<String> = stmt
        .query_map(params![issue_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|(_, remote)| !keep_remote_ids.contains(remote))
        .map(|(id, _)| id)
        .collect();
    drop(stmt);
    let tx = conn.unchecked_transaction()?;
    for id in &stale {
        tx.execute("DELETE FROM issue_dependencies WHERE id = ?1", params![id])?;
        retire_pending_sync(&tx, "dependency", id)?;
    }
    tx.commit()?;
    Ok(stale.len())
}

/// Apply a server tombstone for a label: detach it from issues and projects,
/// then delete it. Idempotent; returns whether a local label existed.
pub fn apply_server_label_tombstone(
    conn: &Connection,
    team_id: &str,
    remote_id: i64,
) -> Result<bool> {
    let local: Option<String> = conn
        .query_row(
            "SELECT id FROM labels WHERE remote_id = ?1 AND team_id = ?2",
            params![remote_id, team_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(local_id) = local else {
        return Ok(false);
    };
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM issue_labels WHERE label_id = ?1", params![local_id])?;
    tx.execute("DELETE FROM project_labels WHERE label_id = ?1", params![local_id])?;
    tx.execute("DELETE FROM labels WHERE id = ?1", params![local_id])?;
    retire_pending_sync(&tx, "label", &local_id)?;
    tx.commit()?;
    Ok(true)
}

/// Apply a server tombstone for a project: issues lose their project link
/// (Rails nullifies), project labels are detached, then the project is
/// deleted. Idempotent; returns whether a local project existed.
pub fn apply_server_project_tombstone(
    conn: &Connection,
    team_id: &str,
    remote_id: i64,
) -> Result<bool> {
    let local: Option<String> = conn
        .query_row(
            "SELECT id FROM projects WHERE remote_id = ?1 AND team_id = ?2",
            params![remote_id, team_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(local_id) = local else {
        return Ok(false);
    };
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE issues SET project_id = NULL WHERE project_id = ?1",
        params![local_id],
    )?;
    tx.execute("DELETE FROM project_labels WHERE project_id = ?1", params![local_id])?;
    tx.execute("DELETE FROM projects WHERE id = ?1", params![local_id])?;
    retire_pending_sync(&tx, "project", &local_id)?;
    tx.commit()?;
    Ok(true)
}

/// True when a local row for this remote id carries the delete tombstone.
pub fn is_issue_tombstoned_by_remote_id(conn: &Connection, remote_id: i64) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issues WHERE remote_id = ?1 AND deleted_at IS NOT NULL",
        params![remote_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn move_issue_to_lane(conn: &Connection, issue_id: &str, lane_id: &str) -> Result<()> {
    let old = get_issue(conn, issue_id)?;
    let now = crate::db::now_iso();
    conn.execute(
        "UPDATE issues SET lane_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![lane_id, now, issue_id],
    )?;
    record_local_diff(conn, old, issue_id)?;
    Ok(())
}

pub fn set_issue_priority(conn: &Connection, issue_id: &str, priority: i32) -> Result<()> {
    let old = get_issue(conn, issue_id)?;
    let now = crate::db::now_iso();
    conn.execute(
        "UPDATE issues SET priority = ?1, updated_at = ?2 WHERE id = ?3",
        params![priority, now, issue_id],
    )?;
    record_local_diff(conn, old, issue_id)?;
    Ok(())
}

pub fn get_issue(conn: &Connection, issue_id: &str) -> Result<Option<Issue>> {
    conn.query_row(
        "SELECT id, title, description, priority, team_number, due_date, estimate,
                started_at, completed_at, canceled_at, lane_id, team_id,
                project_id, parent_issue_id, assignee_id, creator_id, archived_at,
                remote_id, created_at, updated_at
         FROM issues WHERE id = ?1 AND deleted_at IS NULL",
        params![issue_id],
        Issue::from_row,
    )
    .optional()
}

pub fn get_project(conn: &Connection, project_id: &str) -> Result<Option<Project>> {
    conn.query_row(
        "SELECT id, remote_id, team_id, name, description, status, priority,
                lead_id, start_date, due_date, roadmap_commitment,
                velocity_score, completed_issues_count, total_issues_count,
                progress_percentage, behind_schedule,
                created_at, updated_at
         FROM projects WHERE id = ?1",
        params![project_id],
        Project::from_row,
    )
    .optional()
}

pub fn projects_for_team(conn: &Connection, team_id: &str) -> Result<Vec<Project>> {
    let mut stmt = conn.prepare(
        "SELECT id, remote_id, team_id, name, description, status, priority,
                lead_id, start_date, due_date, roadmap_commitment,
                velocity_score, completed_issues_count, total_issues_count,
                progress_percentage, behind_schedule,
                created_at, updated_at
         FROM projects WHERE team_id = ?1 ORDER BY name",
    )?;
    let rows = stmt.query_map(params![team_id], Project::from_row)?;
    rows.collect()
}

pub fn members_for_team(conn: &Connection, team_id: &str) -> Result<Vec<Member>> {
    let mut stmt = conn.prepare(
        "SELECT id, remote_id, team_id, name, email, created_at, updated_at
         FROM members WHERE team_id = ?1 ORDER BY name",
    )?;
    let rows = stmt.query_map(params![team_id], Member::from_row)?;
    rows.collect()
}

// ── Comments ──

/// Flat list of comments on an issue, oldest first. Nesting (parent_id) is not
/// surfaced in v1 — replies are ignored on pull.
pub fn comments_for_issue(conn: &Connection, issue_id: &str) -> Result<Vec<Comment>> {
    let mut stmt = conn.prepare(
        "SELECT id, remote_id, issue_id, project_id, author_id, body, parent_id, created_at, updated_at
         FROM comments WHERE issue_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt.query_map(params![issue_id], Comment::from_row)?;
    rows.collect()
}

/// Every comment on any issue of a team (for local "Referenced In" scanning).
pub fn comments_for_team(conn: &Connection, team_id: &str) -> Result<Vec<Comment>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.remote_id, c.issue_id, c.project_id, c.author_id, c.body, c.parent_id, c.created_at, c.updated_at
         FROM comments c JOIN issues i ON i.id = c.issue_id
         WHERE i.team_id = ?1 AND i.deleted_at IS NULL ORDER BY c.created_at",
    )?;
    let rows = stmt.query_map(params![team_id], Comment::from_row)?;
    rows.collect()
}

pub fn get_comment(conn: &Connection, comment_id: &str) -> Result<Option<Comment>> {
    conn.query_row(
        "SELECT id, remote_id, issue_id, project_id, author_id, body, parent_id, created_at, updated_at
         FROM comments WHERE id = ?1",
        params![comment_id],
        Comment::from_row,
    )
    .optional()
}

pub fn insert_comment(conn: &Connection, comment: &Comment) -> Result<()> {
    conn.execute(
        "INSERT INTO comments
            (id, remote_id, issue_id, project_id, author_id, body, parent_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            comment.id,
            comment.remote_id,
            comment.issue_id,
            comment.project_id,
            comment.author_id,
            comment.body,
            comment.parent_id,
            comment.created_at,
            comment.updated_at,
        ],
    )?;
    Ok(())
}

pub fn find_comment_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<Comment>> {
    conn.query_row(
        "SELECT id, remote_id, issue_id, project_id, author_id, body, parent_id, created_at, updated_at
         FROM comments WHERE remote_id = ?1",
        params![remote_id],
        Comment::from_row,
    )
    .optional()
}

pub fn upsert_comment(conn: &Connection, comment: &Comment) -> Result<()> {
    conn.execute(
        "INSERT INTO comments
            (id, remote_id, issue_id, project_id, author_id, body, parent_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
            remote_id = excluded.remote_id,
            issue_id = excluded.issue_id,
            project_id = excluded.project_id,
            author_id = excluded.author_id,
            body = excluded.body,
            parent_id = excluded.parent_id,
            updated_at = excluded.updated_at",
        params![
            comment.id,
            comment.remote_id,
            comment.issue_id,
            comment.project_id,
            comment.author_id,
            comment.body,
            comment.parent_id,
            comment.created_at,
            comment.updated_at,
        ],
    )?;
    Ok(())
}

pub fn set_comment_remote_id(conn: &Connection, comment_id: &str, remote_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE comments SET remote_id = ?1 WHERE id = ?2",
        params![remote_id, comment_id],
    )?;
    Ok(())
}

pub fn insert_project(conn: &Connection, project: &Project) -> Result<()> {
    conn.execute(
        "INSERT INTO projects
            (id, remote_id, team_id, name, description, status, priority, lead_id,
             start_date, due_date, roadmap_commitment, velocity_score,
             completed_issues_count, total_issues_count, progress_percentage,
             behind_schedule, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            project.id,
            project.remote_id,
            project.team_id,
            project.name,
            project.description,
            project.status,
            project.priority,
            project.lead_id,
            project.start_date,
            project.due_date,
            project.roadmap_commitment,
            project.velocity_score,
            project.completed_issues_count,
            project.total_issues_count,
            project.progress_percentage,
            project.behind_schedule,
            project.created_at,
            project.updated_at,
        ],
    )?;
    Ok(())
}

pub fn update_project(conn: &Connection, project: &Project) -> Result<()> {
    conn.execute(
        "UPDATE projects SET
            name = ?1, description = ?2, status = ?3, priority = ?4, lead_id = ?5,
            start_date = ?6, due_date = ?7, roadmap_commitment = ?8, updated_at = ?9
         WHERE id = ?10",
        params![
            project.name,
            project.description,
            project.status,
            project.priority,
            project.lead_id,
            project.start_date,
            project.due_date,
            project.roadmap_commitment,
            project.updated_at,
            project.id,
        ],
    )?;
    Ok(())
}

pub fn delete_project(conn: &Connection, project_id: &str) -> Result<()> {
    conn.execute("DELETE FROM projects WHERE id = ?1", params![project_id])?;
    Ok(())
}

pub fn get_label(conn: &Connection, label_id: &str) -> Result<Option<Label>> {
    conn.query_row(
        "SELECT id, remote_id, team_id, name, color, created_at, updated_at
         FROM labels WHERE id = ?1",
        params![label_id],
        Label::from_row,
    )
    .optional()
}

pub fn labels_for_team(conn: &Connection, team_id: &str) -> Result<Vec<Label>> {
    let mut stmt = conn.prepare(
        "SELECT id, remote_id, team_id, name, color, created_at, updated_at
         FROM labels WHERE team_id = ?1 ORDER BY name",
    )?;
    let rows = stmt.query_map(params![team_id], Label::from_row)?;
    rows.collect()
}

pub fn insert_label(conn: &Connection, label: &Label) -> Result<()> {
    conn.execute(
        "INSERT INTO labels (id, remote_id, team_id, name, color, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            label.id,
            label.remote_id,
            label.team_id,
            label.name,
            label.color,
            label.created_at,
            label.updated_at,
        ],
    )?;
    Ok(())
}

/// Local UUIDs of the labels currently attached to `issue_id`. Used to preload
/// the multi-select when editing an issue.
pub fn label_ids_for_issue(conn: &Connection, issue_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT label_id FROM issue_labels WHERE issue_id = ?1")?;
    let rows = stmt.query_map(params![issue_id], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// Every (issue_id, Label) pair for a team's issues, so the board can render
/// label chips without a query per card. One join keeps reloads cheap.
pub fn issue_labels_for_team(conn: &Connection, team_id: &str) -> Result<Vec<(String, Label)>> {
    let mut stmt = conn.prepare(
        "SELECT il.issue_id,
                l.id, l.remote_id, l.team_id, l.name, l.color, l.created_at, l.updated_at
         FROM issue_labels il
         JOIN labels l ON l.id = il.label_id
         JOIN issues i ON i.id = il.issue_id
         WHERE i.team_id = ?1 AND i.deleted_at IS NULL
         ORDER BY l.name",
    )?;
    let rows = stmt.query_map(params![team_id], |row| {
        let issue_id: String = row.get(0)?;
        let label = Label {
            id: row.get(1)?,
            remote_id: row.get(2)?,
            team_id: row.get(3)?,
            name: row.get(4)?,
            color: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        };
        Ok((issue_id, label))
    })?;
    rows.collect()
}

/// Replace the full set of labels attached to `issue_id` with `label_ids`.
/// Mirrors Rails' `label_ids=` collection setter (delete + insert to match).
pub fn set_issue_labels(conn: &Connection, issue_id: &str, label_ids: &[String]) -> Result<()> {
    let now = crate::db::now_iso();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM issue_labels WHERE issue_id = ?1",
        params![issue_id],
    )?;
    for label_id in label_ids {
        tx.execute(
            "INSERT INTO issue_labels (issue_id, label_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)",
            params![issue_id, label_id, now],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn get_issue_dependency(conn: &Connection, dep_id: &str) -> Result<Option<IssueDependency>> {
    conn.query_row(
        "SELECT id, remote_id, blocking_issue_id, blocked_issue_id, kind, direction,
                created_at, updated_at
         FROM issue_dependencies WHERE id = ?1",
        params![dep_id],
        IssueDependency::from_row,
    )
    .optional()
}

/// Insert a new local dependency row. Errors on the `UNIQUE(blocking_issue_id,
/// blocked_issue_id)` constraint when the edge already exists — callers surface
/// that as a "already linked" toast rather than a hard failure.
pub fn insert_issue_dependency(conn: &Connection, dep: &IssueDependency) -> Result<()> {
    conn.execute(
        "INSERT INTO issue_dependencies
            (id, remote_id, blocking_issue_id, blocked_issue_id, kind, direction,
             created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            dep.id,
            dep.remote_id,
            dep.blocking_issue_id,
            dep.blocked_issue_id,
            dep.kind,
            dep.direction,
            dep.created_at,
            dep.updated_at,
        ],
    )?;
    events::record_dependency(
        conn,
        &dep.blocking_issue_id,
        &dep.blocked_issue_id,
        true,
        Source::Local,
        &now_ts(),
    )?;
    Ok(())
}

pub fn delete_issue_dependency(conn: &Connection, dep_id: &str) -> Result<()> {
    if let Some(dep) = get_issue_dependency(conn, dep_id)? {
        events::record_dependency(
            conn,
            &dep.blocking_issue_id,
            &dep.blocked_issue_id,
            false,
            Source::Local,
            &now_ts(),
        )?;
    }
    conn.execute(
        "DELETE FROM issue_dependencies WHERE id = ?1",
        params![dep_id],
    )?;
    Ok(())
}

/// A resolved dependency edge for the issue-detail sidebar: the dependency row's
/// own id/kind plus the issue on the far end (for display + navigation +
/// removal).
#[derive(Debug, Clone)]
pub struct DependencyLink {
    pub dep_id: String,
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    /// `blocks` | `relates` | `duplicates` (the stored `kind`).
    pub kind: String,
    /// True when the viewed issue is the edge's source (`blocking_issue_id`).
    pub is_source: bool,
}

/// An issue's dependency edges split into the five sidebar sections.
#[derive(Debug, Clone, Default)]
pub struct DependencyGroups {
    pub blocking: Vec<DependencyLink>,
    pub blocked_by: Vec<DependencyLink>,
    pub related: Vec<DependencyLink>,
    pub duplicates: Vec<DependencyLink>,
    pub duplicated_by: Vec<DependencyLink>,
}

/// Pure grouping of an issue's edges into sections by kind and orientation.
/// Relates is symmetric, so it lands in `related` from either side; unknown
/// kinds are dropped rather than shown under a wrong heading.
pub fn group_dependency_links(links: Vec<DependencyLink>) -> DependencyGroups {
    let mut g = DependencyGroups::default();
    for link in links {
        let bucket = match (link.kind.as_str(), link.is_source) {
            ("blocks", true) => &mut g.blocking,
            ("blocks", false) => &mut g.blocked_by,
            ("relates", _) => &mut g.related,
            ("duplicates", true) => &mut g.duplicates,
            ("duplicates", false) => &mut g.duplicated_by,
            _ => continue,
        };
        bucket.push(link);
    }
    g
}

/// Resolve every dependency edge touching `issue_id` (any kind, either side)
/// and group it for the sidebar.
pub fn dependency_groups_for_issue(conn: &Connection, issue_id: &str) -> Result<DependencyGroups> {
    let mut stmt = conn.prepare(
        "SELECT d.id, i.id, i.team_number, i.title, t.identifier, d.kind,
                d.blocking_issue_id = ?1
         FROM issue_dependencies d
         JOIN issues i ON i.id = CASE WHEN d.blocking_issue_id = ?1
                                      THEN d.blocked_issue_id
                                      ELSE d.blocking_issue_id END
         JOIN teams t ON t.id = i.team_id
         WHERE (d.blocking_issue_id = ?1 OR d.blocked_issue_id = ?1)
           AND i.deleted_at IS NULL
         ORDER BY i.created_at",
    )?;
    let links = stmt
        .query_map(params![issue_id], |row| {
            let team_number: Option<i32> = row.get(2)?;
            let team_identifier: String = row.get(4)?;
            Ok(DependencyLink {
                dep_id: row.get(0)?,
                issue_id: row.get(1)?,
                identifier: format!("{team_identifier}-{}", team_number.unwrap_or(0)),
                title: row.get(3)?,
                kind: row.get(5)?,
                is_source: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>>>()?;
    Ok(group_dependency_links(links))
}

pub fn set_issue_remote_id(conn: &Connection, issue_id: &str, remote_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE issues SET remote_id = ?1 WHERE id = ?2",
        params![remote_id, issue_id],
    )?;
    Ok(())
}

pub fn set_project_remote_id(conn: &Connection, project_id: &str, remote_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE projects SET remote_id = ?1 WHERE id = ?2",
        params![remote_id, project_id],
    )?;
    Ok(())
}

pub fn set_label_remote_id(conn: &Connection, label_id: &str, remote_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE labels SET remote_id = ?1 WHERE id = ?2",
        params![remote_id, label_id],
    )?;
    Ok(())
}

pub fn set_issue_dependency_remote_id(
    conn: &Connection,
    dep_id: &str,
    remote_id: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE issue_dependencies SET remote_id = ?1 WHERE id = ?2",
        params![remote_id, dep_id],
    )?;
    Ok(())
}

/// Record the server's identity for a pushed dependency: its id plus the
/// `kind` / normalized `direction` Rails echoed back.
pub fn set_issue_dependency_sync_fields(
    conn: &Connection,
    dep_id: &str,
    remote_id: i64,
    kind: &str,
    direction: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE issue_dependencies SET remote_id = ?1, kind = ?2, direction = ?3 WHERE id = ?4",
        params![remote_id, kind, direction, dep_id],
    )?;
    Ok(())
}

/// Persist one dependency received from a pull. `source_id` / `target_id` are
/// local issue UUIDs (source = Rails `blocking_issue`).
///
/// - Known `remote_id`: refresh kind/direction only (endpoints keep the
///   orientation they were first stored with).
/// - Same edge already exists locally without a remote id (pushed, response
///   not yet recorded): adopt the remote id.
/// - Otherwise insert. Writes no issue events and no sync_queue entry.
pub fn upsert_pulled_dependency(
    conn: &Connection,
    remote_id: i64,
    source_id: &str,
    target_id: &str,
    kind: &str,
    direction: &str,
) -> Result<()> {
    let now = now_ts();
    let updated = conn.execute(
        "UPDATE issue_dependencies SET kind = ?1, direction = ?2 WHERE remote_id = ?3",
        params![kind, direction, remote_id],
    )?;
    if updated > 0 {
        return Ok(());
    }
    let adopted = conn.execute(
        "UPDATE issue_dependencies SET remote_id = ?1, kind = ?2, direction = ?3
         WHERE blocking_issue_id = ?4 AND blocked_issue_id = ?5 AND remote_id IS NULL",
        params![remote_id, kind, direction, source_id, target_id],
    )?;
    if adopted > 0 {
        return Ok(());
    }
    conn.execute(
        "INSERT OR IGNORE INTO issue_dependencies
            (id, remote_id, blocking_issue_id, blocked_issue_id, kind, direction,
             created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            uuid::Uuid::new_v4().to_string(),
            remote_id,
            source_id,
            target_id,
            kind,
            direction,
            now
        ],
    )?;
    Ok(())
}

// ── Find by remote_id (used by pull-side merge) ──

pub fn find_team_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<Team>> {
    conn.query_row(
        "SELECT id, name, identifier, remote_id, archived_at, created_at, updated_at
         FROM teams WHERE remote_id = ?1",
        params![remote_id],
        Team::from_row,
    )
    .optional()
}

pub fn find_lane_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<Lane>> {
    conn.query_row(
        "SELECT id, name, color, position, team_id, remote_id, category, created_at, updated_at
         FROM lanes WHERE remote_id = ?1",
        params![remote_id],
        Lane::from_row,
    )
    .optional()
}

pub fn find_label_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<Label>> {
    conn.query_row(
        "SELECT id, remote_id, team_id, name, color, created_at, updated_at
         FROM labels WHERE remote_id = ?1",
        params![remote_id],
        Label::from_row,
    )
    .optional()
}

pub fn find_member_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<Member>> {
    conn.query_row(
        "SELECT id, remote_id, team_id, name, email, created_at, updated_at
         FROM members WHERE remote_id = ?1",
        params![remote_id],
        Member::from_row,
    )
    .optional()
}

pub fn find_project_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<Project>> {
    conn.query_row(
        "SELECT id, remote_id, team_id, name, description, status, priority,
                lead_id, start_date, due_date, roadmap_commitment,
                velocity_score, completed_issues_count, total_issues_count,
                progress_percentage, behind_schedule, created_at, updated_at
         FROM projects WHERE remote_id = ?1",
        params![remote_id],
        Project::from_row,
    )
    .optional()
}

pub fn find_issue_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<Issue>> {
    conn.query_row(
        "SELECT id, title, description, priority, team_number, due_date, estimate,
                started_at, completed_at, canceled_at, lane_id, team_id,
                project_id, parent_issue_id, assignee_id, creator_id, archived_at,
                remote_id, created_at, updated_at
         FROM issues WHERE remote_id = ?1",
        params![remote_id],
        Issue::from_row,
    )
    .optional()
}

// ── Upserts (used by pull-side merge) ──
//
// Each `upsert_*` is keyed on the local UUID `id`, so the caller is
// responsible for resolving "this remote_id maps to local UUID X" via
// `find_*_by_remote_id` before calling this. New rows pass a fresh UUID; merges
// reuse the existing local UUID so foreign-key references stay stable.

pub fn upsert_team(conn: &Connection, team: &Team) -> Result<()> {
    conn.execute(
        "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            identifier = excluded.identifier,
            remote_id = excluded.remote_id,
            updated_at = excluded.updated_at",
        params![
            team.id,
            team.name,
            team.identifier,
            team.remote_id,
            team.created_at,
            team.updated_at
        ],
    )?;
    Ok(())
}

pub fn upsert_lane(conn: &Connection, lane: &Lane) -> Result<()> {
    conn.execute(
        "INSERT INTO lanes (id, name, color, position, team_id, remote_id, category, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            color = excluded.color,
            position = excluded.position,
            team_id = excluded.team_id,
            remote_id = excluded.remote_id,
            category = excluded.category,
            updated_at = excluded.updated_at",
        params![
            lane.id,
            lane.name,
            lane.color,
            lane.position,
            lane.team_id,
            lane.remote_id,
            lane.category,
            lane.created_at,
            lane.updated_at
        ],
    )?;
    Ok(())
}

/// Stored ETag for a team's list ("lanes" | "labels" | "members").
pub fn get_list_etag(conn: &Connection, team_id: &str, list: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT etag FROM list_etags WHERE team_id = ?1 AND list = ?2",
        params![team_id, list],
        |r| r.get(0),
    )
    .optional()
}

pub fn set_list_etag(conn: &Connection, team_id: &str, list: &str, etag: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO list_etags (team_id, list, etag) VALUES (?1, ?2, ?3)
         ON CONFLICT(team_id, list) DO UPDATE SET etag = excluded.etag",
        params![team_id, list, etag],
    )?;
    Ok(())
}

pub fn clear_list_etag(conn: &Connection, team_id: &str, list: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM list_etags WHERE team_id = ?1 AND list = ?2",
        params![team_id, list],
    )?;
    Ok(())
}

/// Incremental-pull cursor (a server timestamp) for a team's resource
/// ("issues", ...). `None` means never pulled: do a full pull.
pub fn get_pull_cursor(conn: &Connection, team_id: &str, resource: &str) -> Result<Option<String>> {
    conn.query_row(
        "SELECT cursor FROM pull_cursors WHERE team_id = ?1 AND resource = ?2",
        params![team_id, resource],
        |r| r.get(0),
    )
    .optional()
}

pub fn set_pull_cursor(
    conn: &Connection,
    team_id: &str,
    resource: &str,
    cursor: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pull_cursors (team_id, resource, cursor) VALUES (?1, ?2, ?3)
         ON CONFLICT(team_id, resource) DO UPDATE SET cursor = excluded.cursor",
        params![team_id, resource, cursor],
    )?;
    Ok(())
}

#[cfg(test)]
pub fn clear_pull_cursor(conn: &Connection, team_id: &str, resource: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM pull_cursors WHERE team_id = ?1 AND resource = ?2",
        params![team_id, resource],
    )?;
    Ok(())
}

pub fn upsert_label(conn: &Connection, label: &Label) -> Result<()> {
    conn.execute(
        "INSERT INTO labels (id, remote_id, team_id, name, color, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
            remote_id = excluded.remote_id,
            team_id = excluded.team_id,
            name = excluded.name,
            color = excluded.color,
            updated_at = excluded.updated_at",
        params![
            label.id,
            label.remote_id,
            label.team_id,
            label.name,
            label.color,
            label.created_at,
            label.updated_at
        ],
    )?;
    Ok(())
}

pub fn upsert_member(conn: &Connection, member: &Member) -> Result<()> {
    conn.execute(
        "INSERT INTO members (id, remote_id, team_id, name, email, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET
            remote_id = excluded.remote_id,
            team_id = excluded.team_id,
            name = excluded.name,
            email = excluded.email,
            updated_at = excluded.updated_at",
        params![
            member.id,
            member.remote_id,
            member.team_id,
            member.name,
            member.email,
            member.created_at,
            member.updated_at
        ],
    )?;
    Ok(())
}

pub fn upsert_project(conn: &Connection, project: &Project) -> Result<()> {
    conn.execute(
        "INSERT INTO projects
            (id, remote_id, team_id, name, description, status, priority, lead_id,
             start_date, due_date, roadmap_commitment, velocity_score,
             completed_issues_count, total_issues_count, progress_percentage,
             behind_schedule, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT(id) DO UPDATE SET
            remote_id = excluded.remote_id,
            team_id = excluded.team_id,
            name = excluded.name,
            description = excluded.description,
            status = excluded.status,
            priority = excluded.priority,
            lead_id = excluded.lead_id,
            start_date = excluded.start_date,
            due_date = excluded.due_date,
            roadmap_commitment = excluded.roadmap_commitment,
            velocity_score = excluded.velocity_score,
            completed_issues_count = excluded.completed_issues_count,
            total_issues_count = excluded.total_issues_count,
            progress_percentage = excluded.progress_percentage,
            behind_schedule = excluded.behind_schedule,
            updated_at = excluded.updated_at",
        params![
            project.id,
            project.remote_id,
            project.team_id,
            project.name,
            project.description,
            project.status,
            project.priority,
            project.lead_id,
            project.start_date,
            project.due_date,
            project.roadmap_commitment,
            project.velocity_score,
            project.completed_issues_count,
            project.total_issues_count,
            project.progress_percentage,
            project.behind_schedule,
            project.created_at,
            project.updated_at,
        ],
    )?;
    Ok(())
}

pub fn upsert_issue(conn: &Connection, issue: &Issue) -> Result<()> {
    // Tombstoned rows are never overwritten by pull.
    let tombstoned: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issues WHERE id = ?1 AND deleted_at IS NOT NULL",
        params![issue.id],
        |r| r.get(0),
    )?;
    if tombstoned > 0 {
        return Ok(());
    }
    let old = get_issue(conn, &issue.id)?;
    conn.execute(
        "INSERT INTO issues
            (id, title, description, priority, team_number, due_date, estimate,
             started_at, completed_at, canceled_at, lane_id, team_id, project_id,
             parent_issue_id, assignee_id, creator_id, archived_at, remote_id,
             created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18, ?19, ?20)
         ON CONFLICT(id) DO UPDATE SET
            title = excluded.title,
            description = excluded.description,
            priority = excluded.priority,
            team_number = excluded.team_number,
            due_date = excluded.due_date,
            estimate = excluded.estimate,
            started_at = excluded.started_at,
            completed_at = excluded.completed_at,
            canceled_at = excluded.canceled_at,
            lane_id = excluded.lane_id,
            team_id = excluded.team_id,
            project_id = excluded.project_id,
            parent_issue_id = excluded.parent_issue_id,
            assignee_id = excluded.assignee_id,
            creator_id = excluded.creator_id,
            archived_at = excluded.archived_at,
            remote_id = excluded.remote_id,
            updated_at = excluded.updated_at",
        params![
            issue.id,
            issue.title,
            issue.description,
            issue.priority,
            issue.team_number,
            issue.due_date,
            issue.estimate,
            issue.started_at,
            issue.completed_at,
            issue.canceled_at,
            issue.lane_id,
            issue.team_id,
            issue.project_id,
            issue.parent_issue_id,
            issue.assignee_id,
            issue.creator_id,
            issue.archived_at,
            issue.remote_id,
            issue.created_at,
            issue.updated_at,
        ],
    )?;
    // Local-only activity history: a new row gets a "created" event, an
    // existing one gets an event per tracked field the pull changed.
    match old {
        Some(old) => {
            events::record_changes(conn, &old, issue, Source::Sync, &issue.updated_at)?;
        }
        None => events::record_created(conn, issue, Source::Sync)?,
    }
    Ok(())
}

// ── Archive missing (soft-delete on next pull when remote omits the row) ──

/// Mark every issue whose `remote_id` is NOT in `active_remote_ids` (and isn't
/// already archived) as archived. Scoped to a single team so we don't archive
/// rows for teams that weren't part of this pull.
pub fn archive_missing_issues(
    conn: &Connection,
    team_id: &str,
    active_remote_ids: &[i64],
) -> Result<usize> {
    let now = crate::db::now_iso();
    if active_remote_ids.is_empty() {
        return conn.execute(
            "UPDATE issues SET archived_at = ?1
             WHERE team_id = ?2 AND remote_id IS NOT NULL AND archived_at IS NULL",
            params![now, team_id],
        );
    }
    let placeholders: Vec<String> = (0..active_remote_ids.len())
        .map(|i| format!("?{}", i + 3))
        .collect();
    let sql = format!(
        "UPDATE issues SET archived_at = ?1
         WHERE team_id = ?2 AND remote_id IS NOT NULL
           AND remote_id NOT IN ({}) AND archived_at IS NULL",
        placeholders.join(",")
    );
    let mut bound: Vec<Value> = vec![now.into(), team_id.to_string().into()];
    bound.extend(active_remote_ids.iter().map(|id| (*id).into()));
    conn.execute(&sql, params_from_iter(bound))
}

/// Archive teams whose `remote_id` is NOT in `active_remote_ids`. Rails returns
/// only teams the current token sees, so this is a per-token view of "active".
/// Intentionally conservative: only flips `archived_at` when the column is NULL,
/// never resurrects.
pub fn archive_missing_teams(conn: &Connection, active_remote_ids: &[i64]) -> Result<usize> {
    let now = crate::db::now_iso();
    if active_remote_ids.is_empty() {
        return conn.execute(
            "UPDATE teams SET archived_at = ?1
             WHERE remote_id IS NOT NULL AND archived_at IS NULL",
            params![now],
        );
    }
    let placeholders: Vec<String> = (0..active_remote_ids.len())
        .map(|i| format!("?{}", i + 2))
        .collect();
    let sql = format!(
        "UPDATE teams SET archived_at = ?1
         WHERE remote_id IS NOT NULL AND remote_id NOT IN ({}) AND archived_at IS NULL",
        placeholders.join(",")
    );
    let mut bound: Vec<Value> = vec![now.into()];
    bound.extend(active_remote_ids.iter().map(|id| (*id).into()));
    conn.execute(&sql, params_from_iter(bound))
}

/// Flag (or clear) a team as inaccessible after Rails answered a push with a
/// team-access 404. Pending edits stay queued; pushes for the team are skipped.
pub fn set_team_access_lost(conn: &Connection, remote_id: i64, lost: bool) -> Result<usize> {
    if lost {
        let now = crate::db::now_iso();
        conn.execute(
            "UPDATE teams SET access_lost_at = ?1 WHERE remote_id = ?2 AND access_lost_at IS NULL",
            params![now, remote_id],
        )
    } else {
        conn.execute(
            "UPDATE teams SET access_lost_at = NULL WHERE remote_id = ?1 AND access_lost_at IS NOT NULL",
            params![remote_id],
        )
    }
}

pub fn team_access_lost(conn: &Connection, remote_id: i64) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM teams WHERE remote_id = ?1 AND access_lost_at IS NOT NULL",
        params![remote_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Identifiers (e.g. "PROJ") of teams currently flagged inaccessible.
pub fn inaccessible_team_identifiers(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT identifier FROM teams WHERE access_lost_at IS NOT NULL ORDER BY identifier",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Detect and remove the locally-seeded "My Project" team if it still looks
/// untouched: matches the seed signature (name="My Project", identifier="PROJ"),
/// has no `remote_id`, and has zero issues attached. Returns the local UUID of
/// the deleted team if cleanup ran, otherwise `None`.
///
/// Called after a successful pull that just added Rails-backed teams — the
/// orphan seed is then redundant and would otherwise block sync if it remained
/// the user's selected team.
pub fn cleanup_orphan_seed_team(conn: &Connection) -> Result<Option<String>> {
    let team_id: Option<String> = conn
        .query_row(
            "SELECT id FROM teams
             WHERE remote_id IS NULL
               AND name = 'My Project'
               AND identifier = 'PROJ'",
            [],
            |row| row.get(0),
        )
        .optional()?;

    let Some(team_id) = team_id else {
        return Ok(None);
    };

    let issue_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issues WHERE team_id = ?1 AND deleted_at IS NULL",
        params![team_id],
        |row| row.get(0),
    )?;
    if issue_count > 0 {
        // User has actual data on the orphan team — leave it alone.
        return Ok(None);
    }

    conn.execute("DELETE FROM lanes WHERE team_id = ?1", params![team_id])?;
    conn.execute("DELETE FROM teams WHERE id = ?1", params![team_id])?;
    Ok(Some(team_id))
}

/// Resolve a local UUID to its Rails `remote_id`. Returns `Ok(None)` if the row
/// exists but hasn't been synced yet, or if the row is missing entirely.
pub fn remote_id_for(conn: &Connection, table: &str, id: &str) -> Result<Option<i64>> {
    let allowed = [
        "teams", "lanes", "issues", "projects", "labels", "members", "comments",
    ];
    if !allowed.contains(&table) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let sql = format!("SELECT remote_id FROM {table} WHERE id = ?1");
    let raw: Option<Option<i64>> = conn
        .query_row(&sql, params![id], |row| row.get::<_, Option<i64>>(0))
        .optional()?;
    Ok(raw.flatten())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;

    const NOW: &str = "2026-01-01T00:00:00.000Z";

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations::run(&conn).unwrap();
        conn.execute(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at)
             VALUES ('t1', 'T', 'T', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lanes (id, name, position, team_id, created_at, updated_at)
             VALUES ('l1', 'Backlog', 0, 't1', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at)
             VALUES ('i1', 'Issue', 4, 'l1', 't1', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        conn
    }

    fn make_label(id: &str, name: &str) -> Label {
        Label {
            id: id.to_string(),
            remote_id: None,
            team_id: "t1".to_string(),
            name: name.to_string(),
            color: Some("#ef4444".to_string()),
            created_at: NOW.to_string(),
            updated_at: NOW.to_string(),
        }
    }

    #[test]
    fn insert_and_list_labels_for_team() {
        let conn = setup();
        insert_label(&conn, &make_label("lab1", "bug")).unwrap();
        insert_label(&conn, &make_label("lab2", "ui")).unwrap();
        let labels = labels_for_team(&conn, "t1").unwrap();
        // Ordered by name: bug, ui
        assert_eq!(
            labels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
            vec!["bug", "ui"]
        );
    }

    #[test]
    fn set_issue_labels_replaces_the_full_set() {
        let conn = setup();
        insert_label(&conn, &make_label("lab1", "bug")).unwrap();
        insert_label(&conn, &make_label("lab2", "ui")).unwrap();

        // Attach both.
        set_issue_labels(&conn, "i1", &["lab1".into(), "lab2".into()]).unwrap();
        let mut ids = label_ids_for_issue(&conn, "i1").unwrap();
        ids.sort();
        assert_eq!(ids, vec!["lab1".to_string(), "lab2".to_string()]);

        // Replacing with a subset drops the removed one (not additive).
        set_issue_labels(&conn, "i1", &["lab1".into()]).unwrap();
        assert_eq!(
            label_ids_for_issue(&conn, "i1").unwrap(),
            vec!["lab1".to_string()]
        );

        // Empty clears all.
        set_issue_labels(&conn, "i1", &[]).unwrap();
        assert!(label_ids_for_issue(&conn, "i1").unwrap().is_empty());
    }

    #[test]
    fn issue_labels_for_team_groups_by_issue() {
        let conn = setup();
        insert_label(&conn, &make_label("lab1", "bug")).unwrap();
        insert_label(&conn, &make_label("lab2", "ui")).unwrap();
        set_issue_labels(&conn, "i1", &["lab1".into(), "lab2".into()]).unwrap();

        let pairs = issue_labels_for_team(&conn, "t1").unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(pairs.iter().all(|(issue_id, _)| issue_id == "i1"));
        let mut names: Vec<&str> = pairs.iter().map(|(_, l)| l.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["bug", "ui"]);
    }

    fn make_comment(id: &str, remote_id: Option<i64>, body: &str, created_at: &str) -> Comment {
        Comment {
            id: id.to_string(),
            remote_id,
            issue_id: Some("i1".to_string()),
            project_id: None,
            author_id: None,
            body: body.to_string(),
            parent_id: None,
            created_at: created_at.to_string(),
            updated_at: created_at.to_string(),
        }
    }

    #[test]
    fn insert_and_list_comments_ordered_oldest_first() {
        let conn = setup();
        insert_comment(
            &conn,
            &make_comment("c2", None, "second", "2026-01-02T00:00:00.000Z"),
        )
        .unwrap();
        insert_comment(
            &conn,
            &make_comment("c1", None, "first", "2026-01-01T00:00:00.000Z"),
        )
        .unwrap();
        let bodies: Vec<String> = comments_for_issue(&conn, "i1")
            .unwrap()
            .into_iter()
            .map(|c| c.body)
            .collect();
        assert_eq!(bodies, vec!["first".to_string(), "second".to_string()]);
    }

    #[test]
    fn delete_issue_tombstones_and_pull_leaves_it_alone() {
        let conn = setup();
        let mut issue = make_issue("i2", None, None);
        issue.remote_id = Some(77);
        issue.title = "keep me gone".into();
        upsert_issue(&conn, &issue).unwrap();
        delete_issue(&conn, "i2").unwrap();
        // Row survives (tombstone) but is hidden from every read.
        assert!(get_issue(&conn, "i2").unwrap().is_none());
        assert!(issues_for_team(&conn, "t1")
            .unwrap()
            .iter()
            .all(|i| i.id != "i2"));
        assert!(is_issue_tombstoned_by_remote_id(&conn, 77).unwrap());
        // A pull of the same remote issue must not overwrite or revive it.
        issue.title = "remote edit".into();
        upsert_issue(&conn, &issue).unwrap();
        let title: String = conn
            .query_row("SELECT title FROM issues WHERE id = 'i2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "keep me gone");
        assert!(get_issue(&conn, "i2").unwrap().is_none());
    }

    #[test]
    fn upsert_comment_sets_remote_id_and_is_idempotent() {
        let conn = setup();
        insert_comment(&conn, &make_comment("c1", None, "hi", NOW)).unwrap();
        // Simulate a successful push stamping the remote id.
        set_comment_remote_id(&conn, "c1", 42).unwrap();
        assert_eq!(
            find_comment_by_remote_id(&conn, 42).unwrap().unwrap().id,
            "c1"
        );
        // Pull-side upsert of the same remote row (same local id) updates in place.
        upsert_comment(&conn, &make_comment("c1", Some(42), "edited", NOW)).unwrap();
        let all = comments_for_issue(&conn, "i1").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].body, "edited");
        assert_eq!(all[0].remote_id, Some(42));
    }

    #[test]
    fn project_comment_round_trips_with_its_owner() {
        let conn = setup();
        conn.execute(
            "INSERT INTO projects (id, team_id, name, created_at, updated_at)
             VALUES ('p1', 't1', 'P', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        let mut c = make_comment("pc1", None, "hello", NOW);
        c.issue_id = None;
        c.project_id = Some("p1".into());
        insert_comment(&conn, &c).unwrap();

        let got = get_comment(&conn, "pc1").unwrap().unwrap();
        assert_eq!(got.owner(), crate::db::models::CommentOwner::Project("p1".into()));
        assert_eq!(got.issue_id, None);
        // Issue comment queries don't pick it up.
        assert!(comments_for_issue(&conn, "i1").unwrap().is_empty());

        let issue_comment = make_comment("ic1", None, "x", NOW);
        assert_eq!(issue_comment.owner(), crate::db::models::CommentOwner::Issue("i1".into()));
    }

    #[test]
    fn deleting_an_issue_cascades_to_comments() {
        let conn = setup();
        insert_comment(&conn, &make_comment("c1", None, "hi", NOW)).unwrap();
        conn.execute("DELETE FROM issues WHERE id = 'i1'", [])
            .unwrap();
        assert!(comments_for_issue(&conn, "i1").unwrap().is_empty());
    }

    fn make_member(id: &str, remote_id: i64, name: &str) -> Member {
        Member {
            id: id.to_string(),
            remote_id: Some(remote_id),
            team_id: "t1".to_string(),
            name: name.to_string(),
            email: format!("{name}@example.com"),
            created_at: NOW.to_string(),
            updated_at: NOW.to_string(),
        }
    }

    fn make_issue(id: &str, assignee_id: Option<&str>, creator_id: Option<&str>) -> Issue {
        Issue {
            id: id.to_string(),
            title: "Issue".to_string(),
            description: None,
            priority: 4,
            team_number: Some(1),
            due_date: None,
            estimate: None,
            started_at: None,
            completed_at: None,
            canceled_at: None,
            lane_id: "l1".to_string(),
            team_id: "t1".to_string(),
            project_id: None,
            parent_issue_id: None,
            assignee_id: assignee_id.map(str::to_string),
            creator_id: creator_id.map(str::to_string),
            archived_at: None,
            remote_id: None,
            created_at: NOW.to_string(),
            updated_at: NOW.to_string(),
        }
    }

    #[test]
    fn insert_issue_persists_assignee_and_creator() {
        let conn = setup();
        upsert_member(&conn, &make_member("m1", 3, "Ryan")).unwrap();
        insert_issue(&conn, &make_issue("new1", Some("m1"), Some("m1"))).unwrap();

        let stored = get_issue(&conn, "new1").unwrap().unwrap();
        assert_eq!(stored.assignee_id.as_deref(), Some("m1"));
        assert_eq!(stored.creator_id.as_deref(), Some("m1"));
    }

    #[test]
    fn update_issue_persists_and_clears_assignee() {
        let conn = setup();
        upsert_member(&conn, &make_member("m1", 3, "Ryan")).unwrap();
        upsert_member(&conn, &make_member("m2", 6, "Andrew")).unwrap();
        insert_issue(&conn, &make_issue("new1", None, None)).unwrap();

        // Assign to m1.
        let mut issue = get_issue(&conn, "new1").unwrap().unwrap();
        issue.assignee_id = Some("m1".to_string());
        update_issue(&conn, &issue).unwrap();
        assert_eq!(
            get_issue(&conn, "new1")
                .unwrap()
                .unwrap()
                .assignee_id
                .as_deref(),
            Some("m1")
        );

        // Reassign to m2.
        issue.assignee_id = Some("m2".to_string());
        update_issue(&conn, &issue).unwrap();
        assert_eq!(
            get_issue(&conn, "new1")
                .unwrap()
                .unwrap()
                .assignee_id
                .as_deref(),
            Some("m2")
        );

        // "Unassigned" clears it.
        issue.assignee_id = None;
        update_issue(&conn, &issue).unwrap();
        assert_eq!(get_issue(&conn, "new1").unwrap().unwrap().assignee_id, None);
    }

    #[test]
    fn deleting_a_label_cascades_to_issue_labels() {
        let conn = setup();
        insert_label(&conn, &make_label("lab1", "bug")).unwrap();
        set_issue_labels(&conn, "i1", &["lab1".into()]).unwrap();
        // ON DELETE CASCADE on issue_labels.label_id should clean the join row.
        conn.execute("DELETE FROM labels WHERE id = 'lab1'", [])
            .unwrap();
        assert!(label_ids_for_issue(&conn, "i1").unwrap().is_empty());
    }

    fn make_dependency(id: &str, blocking: &str, blocked: &str) -> IssueDependency {
        IssueDependency {
            id: id.to_string(),
            remote_id: None,
            blocking_issue_id: blocking.to_string(),
            blocked_issue_id: blocked.to_string(),
            kind: "blocks".into(),
            direction: "blocking".into(),
            created_at: NOW.to_string(),
            updated_at: NOW.to_string(),
        }
    }

    fn make_link(dep_id: &str, kind: &str, is_source: bool) -> DependencyLink {
        DependencyLink {
            dep_id: dep_id.into(),
            issue_id: "far".into(),
            identifier: "T-9".into(),
            title: "far".into(),
            kind: kind.into(),
            is_source,
        }
    }

    fn ids(links: &[DependencyLink]) -> Vec<&str> {
        links.iter().map(|l| l.dep_id.as_str()).collect()
    }

    #[test]
    fn grouping_splits_the_five_sections() {
        let g = group_dependency_links(vec![
            make_link("blocking", "blocks", true),
            make_link("blocked_by", "blocks", false),
            make_link("rel_src", "relates", true),
            make_link("rel_tgt", "relates", false),
            make_link("dup", "duplicates", true),
            make_link("dup_by", "duplicates", false),
        ]);
        assert_eq!(ids(&g.blocking), ["blocking"]);
        assert_eq!(ids(&g.blocked_by), ["blocked_by"]);
        assert_eq!(ids(&g.related), ["rel_src", "rel_tgt"]);
        assert_eq!(ids(&g.duplicates), ["dup"]);
        assert_eq!(ids(&g.duplicated_by), ["dup_by"]);
    }

    #[test]
    fn grouping_drops_unknown_kinds() {
        let g = group_dependency_links(vec![make_link("x", "mystery", true)]);
        assert!(g.blocking.is_empty() && g.related.is_empty() && g.duplicates.is_empty());
    }

    #[test]
    fn dependency_groups_render_both_directions() {
        let conn = setup();
        // i1 blocks i2.
        insert_issue(&conn, &make_issue("i2", None, None)).unwrap();
        insert_issue_dependency(&conn, &make_dependency("d1", "i1", "i2")).unwrap();

        let g1 = dependency_groups_for_issue(&conn, "i1").unwrap();
        assert_eq!(g1.blocking.len(), 1);
        assert_eq!(g1.blocking[0].issue_id, "i2");
        assert_eq!(g1.blocking[0].identifier, "T-1");
        assert!(g1.blocked_by.is_empty());

        let g2 = dependency_groups_for_issue(&conn, "i2").unwrap();
        assert_eq!(g2.blocked_by.len(), 1);
        assert_eq!(g2.blocked_by[0].issue_id, "i1");
        assert_eq!(g2.blocked_by[0].dep_id, "d1");
        assert!(g2.blocking.is_empty());
    }

    #[test]
    fn relates_and_duplicates_never_land_under_blocks() {
        let conn = setup();
        insert_issue(&conn, &make_issue("i2", None, None)).unwrap();
        insert_issue(&conn, &make_issue("i3", None, None)).unwrap();
        let mut rel = make_dependency("r", "i1", "i2");
        rel.kind = "relates".into();
        insert_issue_dependency(&conn, &rel).unwrap();
        let mut dup = make_dependency("u", "i1", "i3");
        dup.kind = "duplicates".into();
        insert_issue_dependency(&conn, &dup).unwrap();

        let g1 = dependency_groups_for_issue(&conn, "i1").unwrap();
        assert!(g1.blocking.is_empty() && g1.blocked_by.is_empty());
        assert_eq!(ids(&g1.related), ["r"]);
        assert_eq!(ids(&g1.duplicates), ["u"]);
        // Relates reads the same from the other issue; duplicates flips.
        let g2 = dependency_groups_for_issue(&conn, "i2").unwrap();
        assert_eq!(ids(&g2.related), ["r"]);
        let g3 = dependency_groups_for_issue(&conn, "i3").unwrap();
        assert_eq!(ids(&g3.duplicated_by), ["u"]);
    }

    #[test]
    fn duplicate_dependency_edge_is_rejected() {
        let conn = setup();
        insert_issue(&conn, &make_issue("i2", None, None)).unwrap();
        insert_issue_dependency(&conn, &make_dependency("d1", "i1", "i2")).unwrap();
        // Same ordered (blocking, blocked) pair violates the UNIQUE constraint.
        assert!(insert_issue_dependency(&conn, &make_dependency("d2", "i1", "i2")).is_err());
    }

    #[test]
    fn delete_issue_dependency_removes_the_edge() {
        let conn = setup();
        insert_issue(&conn, &make_issue("i2", None, None)).unwrap();
        insert_issue_dependency(&conn, &make_dependency("d1", "i1", "i2")).unwrap();
        delete_issue_dependency(&conn, "d1").unwrap();
        assert!(dependency_groups_for_issue(&conn, "i1").unwrap().blocking.is_empty());
        assert!(get_issue_dependency(&conn, "d1").unwrap().is_none());
    }

    fn event_lines(conn: &Connection, issue_id: &str) -> Vec<String> {
        events::events_for_issue(conn, issue_id)
            .unwrap()
            .iter()
            .map(|e| e.description())
            .collect()
    }

    #[test]
    fn local_writes_record_events_and_not_sync_queue() {
        let conn = setup();
        conn.execute(
            "INSERT INTO lanes (id, name, position, team_id, created_at, updated_at)
             VALUES ('l2', 'In Progress', 1, 't1', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        insert_issue(&conn, &make_issue("n1", None, None)).unwrap();
        move_issue_to_lane(&conn, "n1", "l2").unwrap();
        set_issue_priority(&conn, "n1", 1).unwrap();
        // No-op rewrite adds nothing.
        let mut issue = get_issue(&conn, "n1").unwrap().unwrap();
        update_issue(&conn, &issue).unwrap();
        issue.title = "Renamed".into();
        issue.estimate = Some(2.0);
        update_issue(&conn, &issue).unwrap();
        assert_eq!(
            event_lines(&conn, "n1"),
            vec![
                "created the issue",
                "changed status from Backlog to In Progress",
                "changed priority from None to High",
                "changed the title",
                "changed estimate from none to 2",
            ]
        );
        let queued: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_queue", [], |r| r.get(0))
            .unwrap();
        assert_eq!(queued, 0);
    }

    #[test]
    fn pull_upsert_records_created_then_diffs() {
        let conn = setup();
        let mut issue = make_issue("p1", None, None);
        issue.created_at = "2026-03-01T00:00:00.000Z".into();
        issue.updated_at = "2026-03-01T00:00:00.000Z".into();
        upsert_issue(&conn, &issue).unwrap();
        // Re-pulling an unchanged row adds nothing.
        upsert_issue(&conn, &issue).unwrap();
        issue.priority = 0;
        issue.updated_at = "2026-03-02T00:00:00.000Z".into();
        upsert_issue(&conn, &issue).unwrap();

        let evs = events::events_for_issue(&conn, "p1").unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].created_at, "2026-03-01T00:00:00.000Z");
        assert_eq!(evs[0].source, "sync");
        assert_eq!(evs[1].description(), "changed priority from None to Urgent");
        assert_eq!(evs[1].created_at, "2026-03-02T00:00:00.000Z");
        assert_eq!(evs[1].default_actor(), "Someone");
    }

    #[test]
    fn dependency_and_parent_events_resolve_current_names() {
        let conn = setup();
        insert_issue(&conn, &make_issue("i2", None, None)).unwrap();
        insert_issue_dependency(&conn, &make_dependency("d1", "i1", "i2")).unwrap();
        delete_issue_dependency(&conn, "d1").unwrap();
        let i1 = event_lines(&conn, "i1");
        let i2 = event_lines(&conn, "i2");
        assert!(i1[i1.len() - 2].starts_with("now blocks "));
        assert!(i1[i1.len() - 1].starts_with("no longer blocks "));
        assert!(i2[1].starts_with("now blocked by "));
        assert!(i2[2].starts_with("no longer blocked by "));

        // Born with a parent -> parent event, and the column is persisted.
        let mut child = make_issue("c1", None, None);
        child.parent_issue_id = Some("i1".into());
        insert_issue(&conn, &child).unwrap();
        let stored = get_issue(&conn, "c1").unwrap().unwrap();
        assert_eq!(stored.parent_issue_id.as_deref(), Some("i1"));
        assert!(event_lines(&conn, "c1")[1].starts_with("set parent to "));

        // Renamed lanes show their current name when resolved.
        conn.execute(
            "INSERT INTO lanes (id, name, position, team_id, created_at, updated_at)
             VALUES ('l2', 'Doing', 1, 't1', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        move_issue_to_lane(&conn, "c1", "l2").unwrap();
        conn.execute("UPDATE lanes SET name = 'Active' WHERE id = 'l2'", [])
            .unwrap();
        let evs = events::events_for_issue_resolved(&conn, "c1", None).unwrap();
        assert!(evs[2].description().ends_with("to Active"));
        assert_eq!(evs[2].actor_name, None);
    }
}
