use rusqlite::{params, Connection, OptionalExtension, Result};

use super::models::{Issue, IssueDependency, Label, Lane, Project, Team};

// ── Teams ──

pub fn all_teams(conn: &Connection) -> Result<Vec<Team>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, identifier, remote_id, created_at, updated_at
         FROM teams ORDER BY name",
    )?;
    let rows = stmt.query_map([], Team::from_row)?;
    rows.collect()
}

// ── Lanes ──

pub fn lanes_for_team(conn: &Connection, team_id: &str) -> Result<Vec<Lane>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, color, position, team_id, remote_id, created_at, updated_at
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
         FROM issues WHERE team_id = ?1 ORDER BY priority, created_at",
    )?;
    let rows = stmt.query_map(params![team_id], Issue::from_row)?;
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
        "INSERT INTO issues (id, title, description, priority, team_number, due_date, estimate, started_at, completed_at, canceled_at, lane_id, team_id, remote_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
            issue.remote_id,
            issue.created_at,
            issue.updated_at,
        ],
    )?;
    Ok(())
}

pub fn update_issue(conn: &Connection, issue: &Issue) -> Result<()> {
    conn.execute(
        "UPDATE issues SET title=?1, description=?2, priority=?3, due_date=?4, estimate=?5, started_at=?6, completed_at=?7, canceled_at=?8, lane_id=?9, updated_at=?10 WHERE id=?11",
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
            issue.updated_at,
            issue.id,
        ],
    )?;
    Ok(())
}

pub fn delete_issue(conn: &Connection, issue_id: &str) -> Result<()> {
    conn.execute("DELETE FROM issues WHERE id = ?1", params![issue_id])?;
    Ok(())
}

pub fn move_issue_to_lane(conn: &Connection, issue_id: &str, lane_id: &str) -> Result<()> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    conn.execute(
        "UPDATE issues SET lane_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![lane_id, now, issue_id],
    )?;
    Ok(())
}

pub fn set_issue_priority(conn: &Connection, issue_id: &str, priority: i32) -> Result<()> {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    conn.execute(
        "UPDATE issues SET priority = ?1, updated_at = ?2 WHERE id = ?3",
        params![priority, now, issue_id],
    )?;
    Ok(())
}

pub fn get_issue(conn: &Connection, issue_id: &str) -> Result<Option<Issue>> {
    conn.query_row(
        "SELECT id, title, description, priority, team_number, due_date, estimate,
                started_at, completed_at, canceled_at, lane_id, team_id,
                project_id, parent_issue_id, assignee_id, creator_id, archived_at,
                remote_id, created_at, updated_at
         FROM issues WHERE id = ?1",
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
                created_at, updated_at
         FROM projects WHERE id = ?1",
        params![project_id],
        Project::from_row,
    )
    .optional()
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

pub fn get_issue_dependency(conn: &Connection, dep_id: &str) -> Result<Option<IssueDependency>> {
    conn.query_row(
        "SELECT id, remote_id, blocking_issue_id, blocked_issue_id, created_at, updated_at
         FROM issue_dependencies WHERE id = ?1",
        params![dep_id],
        IssueDependency::from_row,
    )
    .optional()
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

/// Resolve a local UUID to its Rails `remote_id`. Returns `Ok(None)` if the row
/// exists but hasn't been synced yet, or if the row is missing entirely.
pub fn remote_id_for(conn: &Connection, table: &str, id: &str) -> Result<Option<i64>> {
    let allowed = ["teams", "lanes", "issues", "projects", "labels", "members"];
    if !allowed.contains(&table) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let sql = format!("SELECT remote_id FROM {table} WHERE id = ?1");
    let raw: Option<Option<i64>> = conn
        .query_row(&sql, params![id], |row| row.get::<_, Option<i64>>(0))
        .optional()?;
    Ok(raw.flatten())
}
