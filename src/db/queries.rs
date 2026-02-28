use rusqlite::{params, Connection, Result};

use super::models::{Issue, Lane, Team};

// ── Teams ──

pub fn all_teams(conn: &Connection) -> Result<Vec<Team>> {
    let mut stmt = conn.prepare("SELECT id, name, identifier, remote_id, created_at, updated_at FROM teams ORDER BY name")?;
    let rows = stmt.query_map([], |row| {
        Ok(Team {
            id: row.get(0)?,
            name: row.get(1)?,
            identifier: row.get(2)?,
            remote_id: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    rows.collect()
}

// ── Lanes ──

pub fn lanes_for_team(conn: &Connection, team_id: &str) -> Result<Vec<Lane>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, color, position, team_id, remote_id, created_at, updated_at FROM lanes WHERE team_id = ?1 ORDER BY position",
    )?;
    let rows = stmt.query_map(params![team_id], |row| {
        Ok(Lane {
            id: row.get(0)?,
            name: row.get(1)?,
            color: row.get(2)?,
            position: row.get(3)?,
            team_id: row.get(4)?,
            remote_id: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    rows.collect()
}

// ── Issues ──

pub fn issues_for_team(conn: &Connection, team_id: &str) -> Result<Vec<Issue>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, description, priority, team_number, due_date, estimate, started_at, completed_at, canceled_at, lane_id, team_id, remote_id, created_at, updated_at
         FROM issues WHERE team_id = ?1 ORDER BY priority, created_at",
    )?;
    let rows = stmt.query_map(params![team_id], |row| {
        Ok(Issue {
            id: row.get(0)?,
            title: row.get(1)?,
            description: row.get(2)?,
            priority: row.get(3)?,
            team_number: row.get(4)?,
            due_date: row.get(5)?,
            estimate: row.get(6)?,
            started_at: row.get(7)?,
            completed_at: row.get(8)?,
            canceled_at: row.get(9)?,
            lane_id: row.get(10)?,
            team_id: row.get(11)?,
            remote_id: row.get(12)?,
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
        })
    })?;
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
