use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Result};

use super::models::{Issue, IssueDependency, Label, Lane, Member, Project, Team};

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

pub fn projects_for_team(conn: &Connection, team_id: &str) -> Result<Vec<Project>> {
    let mut stmt = conn.prepare(
        "SELECT id, remote_id, team_id, name, description, status, priority,
                lead_id, start_date, due_date, roadmap_commitment,
                velocity_score, completed_issues_count, total_issues_count,
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

pub fn insert_project(conn: &Connection, project: &Project) -> Result<()> {
    conn.execute(
        "INSERT INTO projects
            (id, remote_id, team_id, name, description, status, priority, lead_id,
             start_date, due_date, roadmap_commitment, velocity_score,
             completed_issues_count, total_issues_count, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        params![
            project.id, project.remote_id, project.team_id, project.name, project.description,
            project.status, project.priority, project.lead_id, project.start_date, project.due_date,
            project.roadmap_commitment, project.velocity_score, project.completed_issues_count,
            project.total_issues_count, project.created_at, project.updated_at,
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
            project.name, project.description, project.status, project.priority, project.lead_id,
            project.start_date, project.due_date, project.roadmap_commitment, project.updated_at,
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
    let mut stmt = conn.prepare(
        "SELECT label_id FROM issue_labels WHERE issue_id = ?1",
    )?;
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
         WHERE i.team_id = ?1
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
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM issue_labels WHERE issue_id = ?1", params![issue_id])?;
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

// ── Find by remote_id (used by pull-side merge) ──

pub fn find_team_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<Team>> {
    conn.query_row(
        "SELECT id, name, identifier, remote_id, created_at, updated_at
         FROM teams WHERE remote_id = ?1",
        params![remote_id],
        Team::from_row,
    )
    .optional()
}

pub fn find_lane_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<Lane>> {
    conn.query_row(
        "SELECT id, name, color, position, team_id, remote_id, created_at, updated_at
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
                created_at, updated_at
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
        params![team.id, team.name, team.identifier, team.remote_id, team.created_at, team.updated_at],
    )?;
    Ok(())
}

pub fn upsert_lane(conn: &Connection, lane: &Lane) -> Result<()> {
    conn.execute(
        "INSERT INTO lanes (id, name, color, position, team_id, remote_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            name = excluded.name,
            color = excluded.color,
            position = excluded.position,
            team_id = excluded.team_id,
            remote_id = excluded.remote_id,
            updated_at = excluded.updated_at",
        params![lane.id, lane.name, lane.color, lane.position, lane.team_id, lane.remote_id, lane.created_at, lane.updated_at],
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
        params![label.id, label.remote_id, label.team_id, label.name, label.color, label.created_at, label.updated_at],
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
        params![member.id, member.remote_id, member.team_id, member.name, member.email, member.created_at, member.updated_at],
    )?;
    Ok(())
}

pub fn upsert_project(conn: &Connection, project: &Project) -> Result<()> {
    conn.execute(
        "INSERT INTO projects
            (id, remote_id, team_id, name, description, status, priority, lead_id,
             start_date, due_date, roadmap_commitment, velocity_score,
             completed_issues_count, total_issues_count, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
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
            updated_at = excluded.updated_at",
        params![
            project.id, project.remote_id, project.team_id, project.name, project.description,
            project.status, project.priority, project.lead_id, project.start_date, project.due_date,
            project.roadmap_commitment, project.velocity_score, project.completed_issues_count,
            project.total_issues_count, project.created_at, project.updated_at,
        ],
    )?;
    Ok(())
}

pub fn upsert_issue(conn: &Connection, issue: &Issue) -> Result<()> {
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
            issue.id, issue.title, issue.description, issue.priority, issue.team_number,
            issue.due_date, issue.estimate, issue.started_at, issue.completed_at, issue.canceled_at,
            issue.lane_id, issue.team_id, issue.project_id, issue.parent_issue_id,
            issue.assignee_id, issue.creator_id, issue.archived_at, issue.remote_id,
            issue.created_at, issue.updated_at,
        ],
    )?;
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
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
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
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
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

    let Some(team_id) = team_id else { return Ok(None) };

    let issue_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM issues WHERE team_id = ?1",
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
        assert_eq!(label_ids_for_issue(&conn, "i1").unwrap(), vec!["lab1".to_string()]);

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

    #[test]
    fn deleting_a_label_cascades_to_issue_labels() {
        let conn = setup();
        insert_label(&conn, &make_label("lab1", "bug")).unwrap();
        set_issue_labels(&conn, "i1", &["lab1".into()]).unwrap();
        // ON DELETE CASCADE on issue_labels.label_id should clean the join row.
        conn.execute("DELETE FROM labels WHERE id = 'lab1'", []).unwrap();
        assert!(label_ids_for_issue(&conn, "i1").unwrap().is_empty());
    }
}
