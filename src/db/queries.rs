use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension, Result};

use super::activity::{self, Source};
use super::models::{Comment, CommentOwner, Issue, IssueDependency, Label, Lane, Member, Project, Team};

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

/// An issue plus its team's identifier and name, for cross-team search.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchableIssue {
    pub issue: Issue,
    pub team_identifier: String,
    pub team_name: String,
}

/// Every non-archived issue in every non-archived team, with its team's
/// identifier. Soft-deleted issues are left out too.
pub fn searchable_issues(conn: &Connection) -> Result<Vec<SearchableIssue>> {
    let mut stmt = conn.prepare(
        "SELECT i.id, i.title, i.description, i.priority, i.team_number, i.due_date, i.estimate,
                i.started_at, i.completed_at, i.canceled_at, i.lane_id, i.team_id,
                i.project_id, i.parent_issue_id, i.assignee_id, i.creator_id, i.archived_at,
                i.remote_id, i.created_at, i.updated_at, t.identifier, t.name
         FROM issues i JOIN teams t ON t.id = i.team_id
         WHERE i.deleted_at IS NULL AND i.archived_at IS NULL AND t.archived_at IS NULL
         ORDER BY i.created_at",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(SearchableIssue {
            issue: Issue::from_row(row)?,
            team_identifier: row.get(20)?,
            team_name: row.get(21)?,
        })
    })?;
    rows.collect()
}

/// A project plus its team's id and name, for cross-team search.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchableProject {
    pub id: String,
    pub name: String,
    pub team_id: String,
    pub team_name: String,
}

/// Every project of every non-archived team, in one query.
pub fn searchable_projects(conn: &Connection) -> Result<Vec<SearchableProject>> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.name, p.team_id, t.name
         FROM projects p JOIN teams t ON t.id = p.team_id
         WHERE t.archived_at IS NULL
         ORDER BY p.name",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(SearchableProject {
            id: row.get(0)?,
            name: row.get(1)?,
            team_id: row.get(2)?,
            team_name: row.get(3)?,
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
    // No stored events: Activity derives the "created the issue" line at read
    // time, and a parent set at creation travels in the create payload.
    Ok(())
}

fn now_ts() -> String {
    crate::db::now_iso()
}

/// Record Pending changes for a write that turned `old` into the
/// current row. No-op when the issue didn't exist beforehand.
fn record_local_diff(conn: &Connection, old: Option<Issue>, issue_id: &str) -> Result<()> {
    if let (Some(old), Some(new)) = (old, get_issue(conn, issue_id)?) {
        activity::record_changes(conn, &old, &new, Source::Local, &now_ts())?;
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
    // The server cascades the delete to the issue's Notifications and never
    // re-sends them, so drop the mirrored ones with the issue.
    tx.execute(
        "DELETE FROM notifications WHERE issue_remote_id IS NOT NULL
           AND issue_remote_id = (SELECT remote_id FROM issues WHERE id = ?1)",
        params![issue_id],
    )?;
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
    tx.execute(
        "DELETE FROM issue_activity_entries WHERE issue_id = ?1",
        params![issue_id],
    )?;
    tx.execute(
        "DELETE FROM issue_activity_state WHERE issue_id = ?1",
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
    crate::db::activity::discard_pending_for_entity(conn, "issue", &local_id)?;
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

pub(crate) fn retire_pending_sync(conn: &Connection, entity_type: &str, local_id: &str) -> Result<()> {
    crate::db::activity::discard_pending_for_entity(conn, entity_type, local_id)?;
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
            "SELECT c.id FROM comments c
             LEFT JOIN issues i ON i.id = c.issue_id
             LEFT JOIN projects p ON p.id = c.project_id
             WHERE c.remote_id = ?1 AND COALESCE(i.team_id, p.team_id) = ?2",
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

/// Ids of `root_id` and all its descendant replies, parents before children.
fn comment_subtree_ids(conn: &Connection, root_id: &str) -> Result<Vec<String>> {
    let mut ids = vec![root_id.to_string()];
    let mut i = 0;
    while i < ids.len() {
        let mut stmt = conn.prepare("SELECT id FROM comments WHERE parent_id = ?1")?;
        let kids: Vec<String> = stmt
            .query_map(params![ids[i]], |r| r.get(0))?
            .collect::<Result<_>>()?;
        ids.extend(kids);
        i += 1;
    }
    Ok(ids)
}

/// Number of replies (any depth) under a comment.
pub fn comment_reply_count(conn: &Connection, comment_id: &str) -> Result<usize> {
    Ok(comment_subtree_ids(conn, comment_id)?.len() - 1)
}

/// Locally delete a comment that never reached the server, with its whole reply
/// subtree, and retire their pending `comment/create` entries so nothing is
/// pushed. Nothing is enqueued. Refuses (returns 0) when the comment has a
/// `remote_id`. Returns the number of comments removed.
pub fn delete_unsynced_comment(conn: &Connection, comment_id: &str) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let remote_id: Option<Option<i64>> = tx
        .query_row(
            "SELECT remote_id FROM comments WHERE id = ?1",
            params![comment_id],
            |r| r.get(0),
        )
        .optional()?;
    if !matches!(remote_id, Some(None)) {
        return Ok(0);
    }
    let ids = comment_subtree_ids(&tx, comment_id)?;
    for id in ids.iter().rev() {
        tx.execute("DELETE FROM comments WHERE id = ?1", params![id])?;
        retire_pending_sync(&tx, "comment", id)?;
    }
    tx.commit()?;
    Ok(ids.len())
}

/// Delete the user's own comment and its reply subtree. A comment the server
/// has seen also yields the `comment/delete` queue payload, captured before the
/// rows go: `{team_remote_id, issue_remote_id, remote_id}`. Unsynced comments
/// are dropped as by [`delete_unsynced_comment`] and yield no payload. Returns
/// `(removed count, payload)`.
pub fn delete_comment(conn: &Connection, comment_id: &str) -> Result<(usize, Option<String>)> {
    let Some(comment) = get_comment(conn, comment_id)? else {
        return Ok((0, None));
    };
    let Some(remote_id) = comment.remote_id else {
        return Ok((delete_unsynced_comment(conn, comment_id)?, None));
    };
    let owner_remote = match comment.owner() {
        CommentOwner::Issue(issue_id) => get_issue(conn, &issue_id)?.and_then(|i| {
            let team = remote_id_for(conn, "teams", &i.team_id).ok().flatten()?;
            Some((team, i.remote_id?))
        }),
        CommentOwner::Project(_) => None,
    };
    let tx = conn.unchecked_transaction()?;
    let ids = comment_subtree_ids(&tx, comment_id)?;
    for id in ids.iter().rev() {
        tx.execute("DELETE FROM comments WHERE id = ?1", params![id])?;
        retire_pending_sync(&tx, "comment", id)?;
    }
    tx.commit()?;
    let payload = owner_remote.map(|(team_remote_id, issue_remote_id)| {
        serde_json::json!({
            "team_remote_id": team_remote_id,
            "issue_remote_id": issue_remote_id,
            "remote_id": remote_id,
        })
        .to_string()
    });
    Ok((ids.len(), payload))
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

/// What a local team move changed, for the toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueMoveSummary {
    pub team_number: i32,
    pub lane_id: String,
    /// True when the target had no lane with the old lane's name.
    pub lane_fell_back: bool,
    pub assignee_cleared: bool,
    pub project_cleared: bool,
    pub parent_cleared: bool,
    pub labels_removed: usize,
    pub sub_issues_detached: usize,
}

/// Move an issue to another local team in one transaction, mirroring the Rails
/// `IssueTeamMover`. Not queued here; the normal `update_issue` path still
/// never writes `team_id` / `team_number`.
pub fn move_issue_to_team(
    conn: &Connection,
    issue_id: &str,
    target_team_id: &str,
) -> Result<IssueMoveSummary> {
    let tx = conn.unchecked_transaction()?;
    let old = get_issue(&tx, issue_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    if old.team_id == target_team_id {
        return Err(rusqlite::Error::InvalidParameterName(
            "issue is already in that team".into(),
        ));
    }

    // Lane: case-insensitive name match, else the target's first lane.
    let old_lane_name: Option<String> = tx
        .query_row(
            "SELECT name FROM lanes WHERE id = ?1",
            params![old.lane_id],
            |r| r.get(0),
        )
        .optional()?;
    let target_lanes = lanes_for_team(&tx, target_team_id)?;
    let matched = old_lane_name.as_ref().and_then(|n| {
        target_lanes
            .iter()
            .find(|l| l.name.to_lowercase() == n.to_lowercase())
    });
    let lane = matched
        .or_else(|| target_lanes.first())
        .ok_or(rusqlite::Error::QueryReturnedNoRows)?;

    // Assignee: members are per-team rows, so keep the person by finding their
    // row in the target team. `members.remote_id` is the per-team membership id
    // (unique per row), so it never matches across teams; email is the identity
    // that does.
    let new_assignee: Option<String> = match &old.assignee_id {
        Some(a) => tx
            .query_row(
                "SELECT t.id FROM members m JOIN members t ON t.team_id = ?2
                   AND (t.id = m.id OR (m.remote_id IS NOT NULL AND t.remote_id = m.remote_id)
                        OR LOWER(t.email) = LOWER(m.email))
                 WHERE m.id = ?1 LIMIT 1",
                params![a, target_team_id],
                |r| r.get(0),
            )
            .optional()?,
        None => None,
    };

    // Parent survives only if it's already in the target team.
    let parent_kept = match &old.parent_issue_id {
        Some(p) => tx
            .query_row(
                "SELECT 1 FROM issues WHERE id = ?1 AND team_id = ?2",
                params![p, target_team_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some(),
        None => false,
    };

    let team_number = next_team_number(&tx, target_team_id)?;
    let now = now_ts();
    tx.execute(
        "UPDATE issues SET team_id=?1, team_number=?2, lane_id=?3, assignee_id=?4,
                project_id=NULL, parent_issue_id=?5, updated_at=?6 WHERE id=?7",
        params![
            target_team_id,
            team_number,
            lane.id,
            new_assignee,
            if parent_kept { old.parent_issue_id.clone() } else { None },
            now,
            issue_id
        ],
    )?;
    let labels_removed = tx.execute(
        "DELETE FROM issue_labels WHERE issue_id = ?1",
        params![issue_id],
    )?;
    let sub_issues_detached = tx.execute(
        "UPDATE issues SET parent_issue_id = NULL, updated_at = ?2 WHERE parent_issue_id = ?1",
        params![issue_id, now],
    )?;
    record_local_diff(&tx, Some(old.clone()), issue_id)?;
    tx.commit()?;

    Ok(IssueMoveSummary {
        team_number,
        lane_id: lane.id.clone(),
        lane_fell_back: matched.is_none(),
        assignee_cleared: old.assignee_id.is_some() && new_assignee.is_none(),
        project_cleared: old.project_id.is_some(),
        parent_cleared: old.parent_issue_id.is_some() && !parent_kept,
        labels_removed,
        sub_issues_detached,
    })
}

impl IssueMoveSummary {
    /// One-line toast text: where the issue went and what the move cleared.
    pub fn toast(&self, target_team_name: &str) -> String {
        let mut cleared: Vec<String> = Vec::new();
        if self.lane_fell_back {
            cleared.push("lane reset to first".into());
        }
        if self.assignee_cleared {
            cleared.push("assignee".into());
        }
        if self.project_cleared {
            cleared.push("project".into());
        }
        if self.parent_cleared {
            cleared.push("parent".into());
        }
        if self.labels_removed > 0 {
            cleared.push(format!("{} label(s)", self.labels_removed));
        }
        if self.sub_issues_detached > 0 {
            cleared.push(format!("{} sub-issue link(s)", self.sub_issues_detached));
        }
        if cleared.is_empty() {
            format!("Moved to {target_team_name}")
        } else {
            format!("Moved to {target_team_name}; cleared: {}", cleared.join(", "))
        }
    }
}

/// Queue the `issue/move` push for an issue that was just moved locally out of
/// `source_team_id` (see [`move_issue_to_team`]). The payload captures the
/// remote ids now, since the source team is gone from the row afterwards; a
/// target team without a remote id is left null and resolved at push time.
/// Returns false (nothing queued) when the server has never seen the issue:
/// its pending create already posts to the new team.
pub fn queue_issue_move(conn: &Connection, issue_id: &str, source_team_id: &str) -> Result<bool> {
    let issue = get_issue(conn, issue_id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    let Some(issue_remote_id) = issue.remote_id else {
        return Ok(false);
    };
    let payload = serde_json::json!({
        "source_team_id": source_team_id,
        "target_team_id": issue.team_id,
        "source_team_remote_id": remote_id_for(conn, "teams", source_team_id)?,
        "issue_remote_id": issue_remote_id,
        "target_team_remote_id": remote_id_for(conn, "teams", &issue.team_id)?,
    })
    .to_string();
    super::sync_queue::enqueue(conn, "issue", issue_id, "move", Some(&payload))?;
    Ok(true)
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

/// Everything an issue's hover card shows, read straight from the local
/// database: the issue and the team, lane, assignee and project it points at,
/// whichever team they belong to. Anything missing is left out, and an unknown
/// issue gives empty records. Local only, never the network.
pub fn card_records(conn: &Connection, issue_id: &str) -> Result<crate::body_hover::OwnedRecords> {
    let mut out = crate::body_hover::OwnedRecords::default();
    let Some(issue) = get_issue(conn, issue_id)? else { return Ok(out) };
    out.teams = all_teams(conn)?.into_iter().filter(|t| t.id == issue.team_id).collect();
    out.lanes = lanes_for_team(conn, &issue.team_id)?.into_iter().filter(|l| l.id == issue.lane_id).collect();
    if let Some(id) = &issue.assignee_id {
        out.members = members_for_team(conn, &issue.team_id)?.into_iter().filter(|m| m.id == *id).collect();
    }
    if let Some(id) = &issue.project_id {
        out.projects = get_project(conn, id)?.into_iter().collect();
    }
    out.issues = vec![issue];
    Ok(out)
}

/// Records for a mention card in a body that belongs to `team_id`: that team's
/// members and issues (the issues give the open count). The body's team, not
/// the selected one, so a body opened from My Issues resolves its own people.
pub fn mention_records(conn: &Connection, team_id: &str) -> Result<crate::body_hover::OwnedRecords> {
    Ok(crate::body_hover::OwnedRecords {
        members: members_for_team(conn, team_id)?,
        issues: issues_for_team(conn, team_id)?,
        ..Default::default()
    })
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

/// Flat list of comments on an issue, oldest first; nesting is in `parent_id`
/// (see `Comment::thread_order`).
pub fn comments_for_issue(conn: &Connection, issue_id: &str) -> Result<Vec<Comment>> {
    let mut stmt = conn.prepare(
        "SELECT id, remote_id, issue_id, project_id, author_id, body, parent_id, created_at, updated_at
         FROM comments WHERE issue_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt.query_map(params![issue_id], Comment::from_row)?;
    rows.collect()
}

/// Every comment on any issue of a team (for local "Referenced In" scanning).
/// Flat list of comments on a project, oldest first.
pub fn comments_for_project(conn: &Connection, project_id: &str) -> Result<Vec<Comment>> {
    let mut stmt = conn.prepare(
        "SELECT id, remote_id, issue_id, project_id, author_id, body, parent_id, created_at, updated_at
         FROM comments WHERE project_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt.query_map(params![project_id], Comment::from_row)?;
    rows.collect()
}

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

/// Why a label edit was refused locally, shown inline in the labels panel.
#[derive(Debug, PartialEq, Eq)]
pub enum LabelEditError {
    Blank,
    Duplicate,
    Missing,
}

impl LabelEditError {
    pub fn message(&self) -> &'static str {
        match self {
            LabelEditError::Blank => "Name can't be blank",
            LabelEditError::Duplicate => "A label with that name already exists",
            LabelEditError::Missing => "Label no longer exists",
        }
    }
}

/// Rename/recolor a label after the same checks Rails applies (non-blank, unique
/// per team case-insensitively, excluding the label itself). Returns whether a
/// `label/update` must be queued: only when the label already has a remote id,
/// since an unsynced label's pending create re-reads the row and carries the
/// new values.
pub fn edit_label(
    conn: &Connection,
    label_id: &str,
    name: &str,
    color: &str,
) -> std::result::Result<bool, LabelEditError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LabelEditError::Blank);
    }
    let label = get_label(conn, label_id)
        .ok()
        .flatten()
        .ok_or(LabelEditError::Missing)?;
    let dup = labels_for_team(conn, &label.team_id)
        .map_err(|_| LabelEditError::Missing)?
        .iter()
        .any(|l| l.id != label.id && l.name.to_lowercase() == name.to_lowercase());
    if dup {
        return Err(LabelEditError::Duplicate);
    }
    conn.execute(
        "UPDATE labels SET name = ?1, color = ?2, updated_at = ?3 WHERE id = ?4",
        params![name, color, crate::db::now_iso(), label_id],
    )
    .map_err(|_| LabelEditError::Missing)?;
    Ok(label.remote_id.is_some())
}

/// How many issues carry the label; the delete confirmation shows it.
pub fn label_issue_count(conn: &Connection, label_id: &str) -> Result<usize> {
    conn.query_row(
        "SELECT COUNT(*) FROM issue_labels WHERE label_id = ?1",
        params![label_id],
        |r| r.get::<_, i64>(0).map(|n| n as usize),
    )
}

/// Delete a label locally with the same effect as the server tombstone: detach
/// it from issues and projects and remove it in one transaction, retiring any
/// pending queue entries for it. Returns the `label/delete` payload
/// (`{team_remote_id, remote_id}`) when the label and its team are synced,
/// i.e. when a delete must be queued; `None` otherwise (an unsynced label's
/// create is simply retired).
pub fn delete_label(conn: &Connection, label_id: &str) -> Result<Option<String>> {
    let Some(label) = get_label(conn, label_id)? else {
        return Ok(None);
    };
    let payload = match (label.remote_id, remote_id_for(conn, "teams", &label.team_id)?) {
        (Some(remote_id), Some(team_remote_id)) => Some(
            serde_json::json!({ "team_remote_id": team_remote_id, "remote_id": remote_id })
                .to_string(),
        ),
        _ => None,
    };
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM issue_labels WHERE label_id = ?1", params![label_id])?;
    tx.execute("DELETE FROM project_labels WHERE label_id = ?1", params![label_id])?;
    tx.execute("DELETE FROM labels WHERE id = ?1", params![label_id])?;
    retire_pending_sync(&tx, "label", label_id)?;
    tx.commit()?;
    Ok(payload)
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

/// Whether the two issues are already linked, in either order and of any kind
/// (Rails allows one link per pair).
pub fn issue_dependency_pair_exists(conn: &Connection, a: &str, b: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM issue_dependencies
         WHERE (blocking_issue_id = ?1 AND blocked_issue_id = ?2)
            OR (blocking_issue_id = ?2 AND blocked_issue_id = ?1))",
        params![a, b],
        |r| r.get(0),
    )
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
    activity::record_dependency(
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
        activity::record_dependency(
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

/// Run a local issue edit and enqueue its push in ONE transaction. Returns the
/// new sync-queue entry id; if either the edit or the enqueue fails, neither
/// is committed.
pub fn edit_issue_queued(
    conn: &Connection,
    issue_id: &str,
    action: &str,
    payload: Option<&str>,
    edit: impl FnOnce(&Connection) -> Result<()>,
) -> Result<i64> {
    let tx = conn.unchecked_transaction()?;
    let events_before = max_event_id(&tx)?;
    edit(&tx)?;
    let id = super::sync_queue::enqueue_returning_id(&tx, "issue", issue_id, action, payload)?;
    // The edit's new Activity rows are its Pending changes: link them to the
    // queue entry that will push them.
    link_new_pending_changes(&tx, events_before, id, Some(issue_id))?;
    tx.commit()?;
    Ok(id)
}

fn max_event_id(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COALESCE(MAX(id), 0) FROM issue_events", [], |r| r.get(0))?)
}

/// Link every unlinked locally-written Activity row after `events_before` to
/// the queue entry that will push it, making them Pending changes. `issue_id`
/// limits it to one issue; `None` takes all (a dependency change writes one
/// row on each of its two issues).
fn link_new_pending_changes(
    conn: &Connection,
    events_before: i64,
    queue_id: i64,
    issue_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE issue_events SET sync_queue_id = ?1
         WHERE id > ?2 AND (?3 IS NULL OR issue_id = ?3)
           AND source = 'local' AND sync_queue_id IS NULL",
        params![queue_id, events_before, issue_id],
    )?;
    Ok(())
}

/// [`update_issue`] plus its `issue/update` queue entry (payload = the issue
/// JSON), atomically.
pub fn update_issue_queued(conn: &Connection, issue: &Issue) -> Result<i64> {
    let payload = serde_json::to_string(issue).ok();
    edit_issue_queued(conn, &issue.id, "update", payload.as_deref(), |c| {
        update_issue(c, issue)
    })
}

/// [`insert_issue_dependency`] plus its `dependency/create` queue entry,
/// atomically. A UNIQUE-constraint failure leaves no queue entry behind.
pub fn insert_issue_dependency_queued(conn: &Connection, dep: &IssueDependency) -> Result<i64> {
    let tx = conn.unchecked_transaction()?;
    let events_before = max_event_id(&tx)?;
    insert_issue_dependency(&tx, dep)?;
    let id = super::sync_queue::enqueue_returning_id(&tx, "dependency", &dep.id, "create", None)?;
    link_new_pending_changes(&tx, events_before, id, None)?;
    tx.commit()?;
    Ok(id)
}

/// Delete a dependency edge and, when the server knows it, enqueue the
/// `dependency/delete` push (carrying the remote ids captured before the row
/// goes), atomically. Returns the queue entry id, or None when nothing needed
/// pushing (unknown edge, never synced, or ids unresolvable).
pub fn delete_issue_dependency_queued(conn: &Connection, dep_id: &str) -> Result<Option<i64>> {
    let Some(dep) = get_issue_dependency(conn, dep_id)? else {
        return Ok(None);
    };
    let tx = conn.unchecked_transaction()?;
    // The Rails edge lives under the BLOCKING issue's endpoint.
    let mut payload = None;
    if let (Some(remote_id), Some(blocking)) =
        (dep.remote_id, get_issue(&tx, &dep.blocking_issue_id)?)
    {
        if let (Some(issue_remote_id), Some(team_remote_id)) = (
            blocking.remote_id,
            remote_id_for(&tx, "teams", &blocking.team_id)?,
        ) {
            payload = Some(
                serde_json::json!({
                    "team_remote_id": team_remote_id,
                    "issue_remote_id": issue_remote_id,
                    "remote_id": remote_id,
                })
                .to_string(),
            );
        }
    }
    let queued = match &payload {
        Some(p) => Some(super::sync_queue::enqueue_returning_id(
            &tx,
            "dependency",
            dep_id,
            "delete",
            Some(p),
        )?),
        None => None,
    };
    let events_before = max_event_id(&tx)?;
    delete_issue_dependency(&tx, dep_id)?;
    if let Some(qid) = queued {
        link_new_pending_changes(&tx, events_before, qid, None)?;
    }
    tx.commit()?;
    Ok(queued)
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

    #[test]
    fn mention_card_resolves_on_the_bodys_team_not_a_same_named_member_elsewhere() {
        let conn = setup(); // team t1 is the "selected" one in the app
        for sql in [
            "INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t2', 'Other', 'OTH', '2026-01-01', '2026-01-01')",
            "INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l2', 'Doing', 0, 't2', '2026-01-01', '2026-01-01')",
            "INSERT INTO members (id, team_id, name, email, created_at, updated_at) VALUES ('m1', 't1', 'Ann', 'ann@one.io', '2026-01-01', '2026-01-01')",
            "INSERT INTO members (id, team_id, name, email, created_at, updated_at) VALUES ('m2', 't2', 'Ann', 'ann@two.io', '2026-01-01', '2026-01-01')",
            "INSERT INTO issues (id, title, priority, lane_id, team_id, assignee_id, team_number, created_at, updated_at)
             VALUES ('a1', 'one', 1, 'l1', 't1', 'm1', 1, '2026-01-01', '2026-01-01')",
            "INSERT INTO issues (id, title, priority, lane_id, team_id, assignee_id, team_number, created_at, updated_at)
             VALUES ('b1', 'two', 1, 'l2', 't2', 'm2', 1, '2026-01-01', '2026-01-01')",
            "INSERT INTO issues (id, title, priority, lane_id, team_id, assignee_id, team_number, created_at, updated_at)
             VALUES ('b2', 'two more', 1, 'l2', 't2', 'm2', 2, '2026-01-01', '2026-01-01')",
        ] {
            conn.execute(sql, []).unwrap();
        }
        let card = |team: &str| {
            let rec = mention_records(&conn, team).unwrap();
            match crate::body_hover::content_for(
                &crate::body_hover::Anchor::Mention("@Ann".into()),
                &rec.records(),
            ) {
                Some(crate::body_hover::CardContent::Member(m)) => (m.email, m.open_issues),
                other => panic!("{other:?}"),
            }
        };
        // A body on t2 (t1 selected) gets t2's Ann, with t2's open issues.
        assert_eq!(card("t2"), ("ann@two.io".to_string(), 2));
        assert_eq!(card("t1"), ("ann@one.io".to_string(), 1));
        // A team with nobody by that name draws no card.
        conn.execute(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t3', 'Empty', 'EMP', '2026-01-01', '2026-01-01')",
            [],
        )
        .unwrap();
        let rec = mention_records(&conn, "t3").unwrap();
        assert!(crate::body_hover::content_for(
            &crate::body_hover::Anchor::Mention("@Ann".into()),
            &rec.records()
        )
        .is_none());
    }

    #[test]
    fn card_records_follow_an_issue_into_another_team() {
        let conn = setup();
        for sql in [
            "INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t2', 'Other', 'OTH', '2026-01-01', '2026-01-01')",
            "INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l2', 'Doing', 0, 't2', '2026-01-01', '2026-01-01')",
            "INSERT INTO members (id, team_id, name, email, created_at, updated_at) VALUES ('m2', 't2', 'Ann', 'a@x.y', '2026-01-01', '2026-01-01')",
            "INSERT INTO projects (id, team_id, name, created_at, updated_at) VALUES ('p2', 't2', 'Proj', '2026-01-01', '2026-01-01')",
            "INSERT INTO issues (id, title, priority, lane_id, team_id, assignee_id, project_id, team_number, created_at, updated_at)
             VALUES ('x1', 'Far', 1, 'l2', 't2', 'm2', 'p2', 5, '2026-01-01', '2026-01-01')",
        ] {
            conn.execute(sql, []).unwrap();
        }
        let rec = card_records(&conn, "x1").unwrap();
        assert_eq!(rec.issues.len(), 1);
        assert_eq!(rec.teams[0].identifier, "OTH");
        assert_eq!(rec.lanes[0].name, "Doing");
        assert_eq!(rec.members[0].name, "Ann");
        assert_eq!(rec.projects[0].name, "Proj");
        let card = crate::body_hover::content_for(
            &crate::body_hover::Anchor::Issue("x1".into()),
            &rec.records(),
        );
        let Some(crate::body_hover::CardContent::Issue(card)) = card else { panic!("card expected") };
        assert_eq!(card.identifier, "OTH-5");
        assert_eq!(card.assignee.as_deref(), Some("Ann"));
        assert_eq!(card.project.as_deref(), Some("Proj"));
        assert_eq!(card.lane.unwrap().name, "Doing");

        // Unresolved references: no records, no card. Bare issue: no extras.
        let none = card_records(&conn, "nope").unwrap();
        assert!(none.issues.is_empty());
        assert!(crate::body_hover::content_for(
            &crate::body_hover::Anchor::Issue("nope".into()),
            &none.records()
        )
        .is_none());
        let bare = card_records(&conn, "i1").unwrap();
        assert!(bare.members.is_empty() && bare.projects.is_empty());
    }

    #[test]
    fn edit_and_queue_entry_commit_together_and_return_the_entry_id() {
        let conn = setup();
        let mut issue = get_issue(&conn, "i1").unwrap().unwrap();
        issue.title = "Renamed".into();
        let id = update_issue_queued(&conn, &issue).unwrap();
        assert_eq!(get_issue(&conn, "i1").unwrap().unwrap().title, "Renamed");
        let entries = crate::db::sync_queue::pending_entries(&conn).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, Some(id));
        assert_eq!(entries[0].entity_type, "issue");
        assert_eq!(entries[0].action, "update");
    }

    #[test]
    fn failed_edit_leaves_no_queue_entry() {
        let conn = setup();
        let r = edit_issue_queued(&conn, "i1", "update", None, |c| {
            c.execute("UPDATE issues SET title = 'half' WHERE id = 'i1'", [])?;
            c.execute("UPDATE no_such_table SET x = 1", [])?;
            Ok(())
        });
        assert!(r.is_err());
        assert_eq!(get_issue(&conn, "i1").unwrap().unwrap().title, "Issue");
        assert!(crate::db::sync_queue::pending_entries(&conn).unwrap().is_empty());
    }

    #[test]
    fn failed_enqueue_rolls_back_the_edit() {
        let conn = setup();
        conn.execute_batch(
            "CREATE TRIGGER no_queue BEFORE INSERT ON sync_queue
             BEGIN SELECT RAISE(ABORT, 'nope'); END;",
        )
        .unwrap();
        let mut issue = get_issue(&conn, "i1").unwrap().unwrap();
        issue.title = "Renamed".into();
        assert!(update_issue_queued(&conn, &issue).is_err());
        assert_eq!(get_issue(&conn, "i1").unwrap().unwrap().title, "Issue");
    }

    fn two_issue_dep(conn: &Connection) -> IssueDependency {
        conn.execute(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at)
             VALUES ('i2', 'Two', 0, 'l1', 't1', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        IssueDependency {
            id: "d1".into(),
            remote_id: None,
            blocking_issue_id: "i1".into(),
            blocked_issue_id: "i2".into(),
            kind: "blocks".into(),
            direction: "blocks".into(),
            created_at: NOW.into(),
            updated_at: NOW.into(),
        }
    }

    #[test]
    fn dependency_add_queues_atomically_and_duplicate_queues_nothing() {
        let conn = setup();
        let dep = two_issue_dep(&conn);
        let id = insert_issue_dependency_queued(&conn, &dep).unwrap();
        let entries = crate::db::sync_queue::pending_entries(&conn).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, Some(id));
        assert_eq!(entries[0].entity_type, "dependency");

        let dup = IssueDependency { id: "d2".into(), ..dep };
        assert!(insert_issue_dependency_queued(&conn, &dup).is_err());
        assert_eq!(crate::db::sync_queue::pending_entries(&conn).unwrap().len(), 1);
    }

    #[test]
    fn dependency_remove_queues_only_when_the_server_knows_the_edge() {
        let conn = setup();
        let dep = two_issue_dep(&conn);
        insert_issue_dependency_queued(&conn, &dep).unwrap();
        // Never synced: deleted locally, nothing queued beyond the create.
        assert_eq!(delete_issue_dependency_queued(&conn, "d1").unwrap(), None);
        assert!(get_issue_dependency(&conn, "d1").unwrap().is_none());
        assert_eq!(crate::db::sync_queue::pending_entries(&conn).unwrap().len(), 1);

        // Synced edge: delete is queued with the captured remote ids.
        conn.execute("UPDATE teams SET remote_id = 9 WHERE id = 't1'", []).unwrap();
        conn.execute("UPDATE issues SET remote_id = 50 WHERE id = 'i1'", []).unwrap();
        insert_issue_dependency(&conn, &IssueDependency { remote_id: Some(77), ..dep }).unwrap();
        let id = delete_issue_dependency_queued(&conn, "d1").unwrap().unwrap();
        let entry = crate::db::sync_queue::pending_entries(&conn)
            .unwrap()
            .into_iter()
            .find(|e| e.id == Some(id))
            .unwrap();
        assert_eq!(entry.action, "delete");
        assert!(entry.payload.unwrap().contains("\"remote_id\":77"));
        assert!(get_issue_dependency(&conn, "d1").unwrap().is_none());
    }

    #[test]
    fn server_issue_tombstone_removes_the_issues_notifications() {
        let conn = setup();
        conn.execute("UPDATE issues SET remote_id = 50 WHERE id = 'i1'", []).unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO notifications (id, remote_id, action, message, issue_remote_id, created_at, updated_at)
               VALUES ('a', 1, 'mentioned', 'm', 50, '{NOW}', '{NOW}'),
                      ('b', 2, 'mentioned', 'm', 51, '{NOW}', '{NOW}');"
        ))
        .unwrap();
        assert!(apply_server_issue_tombstone(&conn, "t1", 50).unwrap());
        let left: Vec<String> = conn
            .prepare("SELECT id FROM notifications").unwrap()
            .query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(left, vec!["b".to_string()]);
    }

    #[test]
    fn searchable_issues_spans_teams_and_skips_archived() {
        let conn = setup();
        conn.execute_batch(&format!(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at)
               VALUES ('t2', 'Two', 'TWO', '{NOW}', '{NOW}');
             INSERT INTO teams (id, name, identifier, archived_at, created_at, updated_at)
               VALUES ('t3', 'Dead', 'DEAD', '{NOW}', '{NOW}', '{NOW}');
             INSERT INTO lanes (id, name, position, team_id, created_at, updated_at)
               VALUES ('l2', 'B', 0, 't2', '{NOW}', '{NOW}'), ('l3', 'B', 0, 't3', '{NOW}', '{NOW}');
             INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at)
               VALUES ('i2', 'Other team', 0, 'l2', 't2', '{NOW}', '{NOW}'),
                      ('i3', 'In dead team', 0, 'l3', 't3', '{NOW}', '{NOW}');
             INSERT INTO issues (id, title, priority, lane_id, team_id, archived_at, created_at, updated_at)
               VALUES ('i4', 'Archived', 0, 'l1', 't1', '{NOW}', '{NOW}', '{NOW}');"
        ))
        .unwrap();
        let mut found: Vec<_> = searchable_issues(&conn)
            .unwrap()
            .into_iter()
            .map(|s| (s.issue.id, s.team_identifier, s.team_name))
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                ("i1".to_string(), "T".to_string(), "T".to_string()),
                ("i2".to_string(), "TWO".to_string(), "Two".to_string())
            ]
        );
    }

    #[test]
    fn searchable_projects_spans_teams_and_skips_archived_teams() {
        let conn = setup();
        conn.execute_batch(&format!(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at)
               VALUES ('t2', 'Two', 'TWO', '{NOW}', '{NOW}');
             INSERT INTO teams (id, name, identifier, archived_at, created_at, updated_at)
               VALUES ('t3', 'Dead', 'DEAD', '{NOW}', '{NOW}', '{NOW}');
             INSERT INTO projects (id, team_id, name, created_at, updated_at)
               VALUES ('p1', 't1', 'Alpha', '{NOW}', '{NOW}'),
                      ('p2', 't2', 'Beta', '{NOW}', '{NOW}'),
                      ('p3', 't3', 'Gone', '{NOW}', '{NOW}');"
        ))
        .unwrap();
        let found: Vec<_> = searchable_projects(&conn)
            .unwrap()
            .into_iter()
            .map(|p| (p.id, p.team_id, p.team_name))
            .collect();
        assert_eq!(
            found,
            [
                ("p1".to_string(), "t1".to_string(), "T".to_string()),
                ("p2".to_string(), "t2".to_string(), "Two".to_string())
            ]
        );
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
    fn edit_label_rejects_blank_and_duplicate_names() {
        let conn = setup();
        insert_label(&conn, &make_label("lab1", "bug")).unwrap();
        insert_label(&conn, &make_label("lab2", "ui")).unwrap();
        assert_eq!(edit_label(&conn, "lab1", "   ", "#000000"), Err(LabelEditError::Blank));
        assert_eq!(edit_label(&conn, "lab1", "UI", "#000000"), Err(LabelEditError::Duplicate));
        // Nothing changed.
        assert_eq!(get_label(&conn, "lab1").unwrap().unwrap().name, "bug");
        // Keeping its own name (different case) is fine.
        assert!(edit_label(&conn, "lab1", "BUG", "#3b82f6").is_ok());
        let l = get_label(&conn, "lab1").unwrap().unwrap();
        assert_eq!((l.name.as_str(), l.color.as_deref()), ("BUG", Some("#3b82f6")));
    }

    #[test]
    fn edit_label_only_asks_for_a_queue_entry_when_synced() {
        let conn = setup();
        insert_label(&conn, &make_label("lab1", "bug")).unwrap();
        assert_eq!(edit_label(&conn, "lab1", "defect", "#ef4444"), Ok(false));
        set_label_remote_id(&conn, "lab1", 5).unwrap();
        assert_eq!(edit_label(&conn, "lab1", "defect2", "#ef4444"), Ok(true));
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

    fn queue_comment_create(conn: &Connection, id: &str) {
        crate::db::sync_queue::enqueue(conn, "comment", id, "create", Some("{}")).unwrap();
    }

    fn pending_count(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM sync_queue WHERE synced_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// c1 -> c2 -> c3, c1 -> c4, plus an unrelated top-level c5.
    fn seed_reply_tree(conn: &Connection) {
        let tree = [
            ("c1", None),
            ("c2", Some("c1")),
            ("c3", Some("c2")),
            ("c4", Some("c1")),
            ("c5", None),
        ];
        for (id, parent) in tree {
            let mut c = make_comment(id, None, id, NOW);
            c.parent_id = parent.map(str::to_string);
            insert_comment(conn, &c).unwrap();
            queue_comment_create(conn, id);
        }
    }

    #[test]
    fn delete_unsynced_comment_removes_subtree_and_retires_creates() {
        let conn = setup();
        seed_reply_tree(&conn);
        assert_eq!(comment_reply_count(&conn, "c1").unwrap(), 3);
        assert_eq!(delete_unsynced_comment(&conn, "c1").unwrap(), 4);
        let left: Vec<String> = comments_for_issue(&conn, "i1")
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(left, ["c5"]);
        // Only the unrelated comment's create is still pending, and no delete queued.
        assert_eq!(pending_count(&conn), 1);
        let deletes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_queue WHERE action = 'delete'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(deletes, 0);
    }

    fn link_remote_owner(conn: &Connection) {
        conn.execute_batch(
            "UPDATE issues SET remote_id = 50 WHERE id = 'i1';
             UPDATE teams SET remote_id = 7 WHERE id = (SELECT team_id FROM issues WHERE id = 'i1');",
        )
        .unwrap();
    }

    #[test]
    fn delete_comment_synced_root_captures_payload_and_retires_subtree() {
        let conn = setup();
        seed_reply_tree(&conn);
        link_remote_owner(&conn);
        set_comment_remote_id(&conn, "c1", 9).unwrap();
        set_comment_remote_id(&conn, "c2", 10).unwrap();
        let (removed, payload) = delete_comment(&conn, "c1").unwrap();
        assert_eq!(removed, 4);
        let payload: serde_json::Value = serde_json::from_str(&payload.unwrap()).unwrap();
        assert_eq!(
            payload,
            serde_json::json!({"team_remote_id": 7, "issue_remote_id": 50, "remote_id": 9})
        );
        assert_eq!(comments_for_issue(&conn, "i1").unwrap().len(), 1);
        // Pending creates for the subtree are retired; only c5's remains.
        assert_eq!(pending_count(&conn), 1);
    }

    #[test]
    fn delete_comment_unsynced_root_has_no_payload() {
        let conn = setup();
        seed_reply_tree(&conn);
        link_remote_owner(&conn);
        let (removed, payload) = delete_comment(&conn, "c1").unwrap();
        assert_eq!(removed, 4);
        assert!(payload.is_none());
    }

    #[test]
    fn delete_comment_unknown_id_removes_nothing() {
        let conn = setup();
        assert_eq!(delete_comment(&conn, "nope").unwrap(), (0, None));
    }

    #[test]
    fn delete_unsynced_comment_refuses_a_synced_comment() {
        let conn = setup();
        seed_reply_tree(&conn);
        set_comment_remote_id(&conn, "c1", 9).unwrap();
        assert_eq!(delete_unsynced_comment(&conn, "c1").unwrap(), 0);
        assert_eq!(comments_for_issue(&conn, "i1").unwrap().len(), 5);
        assert_eq!(pending_count(&conn), 5);
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
    fn delete_label_detaches_and_removes_in_one_go() {
        let conn = setup();
        insert_label(&conn, &make_label("lab1", "bug")).unwrap();
        set_issue_labels(&conn, "i1", &["lab1".into()]).unwrap();
        assert_eq!(label_issue_count(&conn, "lab1").unwrap(), 1);
        assert!(delete_label(&conn, "lab1").unwrap().is_none());
        assert!(get_label(&conn, "lab1").unwrap().is_none());
        assert!(label_ids_for_issue(&conn, "i1").unwrap().is_empty());
        assert_eq!(label_issue_count(&conn, "lab1").unwrap(), 0);
    }

    #[test]
    fn delete_label_unsynced_retires_pending_create_and_queues_nothing() {
        let conn = setup();
        insert_label(&conn, &make_label("lab1", "bug")).unwrap();
        crate::db::sync_queue::enqueue(&conn, "label", "lab1", "create", None).unwrap();
        assert!(delete_label(&conn, "lab1").unwrap().is_none());
        assert_eq!(crate::db::sync_queue::pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn delete_label_synced_returns_payload() {
        let conn = setup();
        insert_label(&conn, &make_label("lab1", "bug")).unwrap();
        set_label_remote_id(&conn, "lab1", 11).unwrap();
        let team_id = get_label(&conn, "lab1").unwrap().unwrap().team_id;
        conn.execute("UPDATE teams SET remote_id = 700 WHERE id = ?1", params![team_id])
            .unwrap();
        let payload = delete_label(&conn, "lab1").unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["team_remote_id"], 700);
        assert_eq!(v["remote_id"], 11);
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
    fn issue_dependency_pair_exists_matches_either_order() {
        let conn = setup();
        insert_issue(&conn, &make_issue("i2", None, None)).unwrap();
        assert!(!issue_dependency_pair_exists(&conn, "i1", "i2").unwrap());
        insert_issue_dependency(&conn, &make_dependency("d1", "i1", "i2")).unwrap();
        assert!(issue_dependency_pair_exists(&conn, "i1", "i2").unwrap());
        assert!(issue_dependency_pair_exists(&conn, "i2", "i1").unwrap());
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

    /// Wording of the issue's Pending changes, as Activity shows them. Activity
    /// only shows events linked to a queue entry, so this links every local
    /// one first (linking itself is covered by the `*_queued` tests).
    fn event_lines(conn: &Connection, issue_id: &str) -> Vec<String> {
        conn.execute(
            "UPDATE issue_events SET sync_queue_id = 1
             WHERE source = 'local' AND sync_queue_id IS NULL",
            [],
        )
        .unwrap();
        activity::activity_for_issue(conn, issue_id)
            .unwrap()
            .into_iter()
            .filter(|i| i.origin == activity::ItemOrigin::Pending)
            .map(|i| i.text)
            .collect()
    }

    #[test]
    fn local_insert_shows_only_the_derived_created_line() {
        let conn = setup();
        insert_issue(&conn, &make_issue("n0", None, None)).unwrap();
        assert!(event_lines(&conn, "n0").is_empty());
        let stored: i64 = conn
            .query_row("SELECT COUNT(*) FROM issue_events WHERE issue_id = 'n0'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, 0);
    }

    #[test]
    fn local_writes_record_pending_changes() {
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
                "changed status from Backlog to In Progress",
                "changed priority from None to High",
                "changed the title",
                "changed estimate from none to 2",
            ]
        );
    }

    #[test]
    fn pull_upsert_records_no_events() {
        let conn = setup();
        let mut issue = make_issue("p1", None, None);
        issue.created_at = "2026-03-01T00:00:00.000Z".into();
        issue.updated_at = "2026-03-01T00:00:00.000Z".into();
        upsert_issue(&conn, &issue).unwrap();
        issue.priority = 0;
        issue.updated_at = "2026-03-02T00:00:00.000Z".into();
        upsert_issue(&conn, &issue).unwrap();

        assert!(event_lines(&conn, "p1").is_empty());
        assert_eq!(get_issue(&conn, "p1").unwrap().unwrap().priority, 0);
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
        assert!(i2[0].starts_with("now blocked by "));
        assert!(i2[1].starts_with("no longer blocked by "));

        // Born with a parent: the column is persisted, no event is stored.
        let mut child = make_issue("c1", None, None);
        child.parent_issue_id = Some("i1".into());
        insert_issue(&conn, &child).unwrap();
        let stored = get_issue(&conn, "c1").unwrap().unwrap();
        assert_eq!(stored.parent_issue_id.as_deref(), Some("i1"));
        assert!(event_lines(&conn, "c1").is_empty());

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
        assert_eq!(event_lines(&conn, "c1"), vec!["changed status from Backlog to Doing"]);
    }

    // ── move_issue_to_team ──

    /// `setup()` plus a target team t2 with lanes "Todo"(0), "in progress"(1),
    /// members shared by email, and a source-team lane/member set.
    fn move_setup() -> Connection {
        let conn = setup();
        let ex = |sql: &str| conn.execute_batch(sql).unwrap();
        ex("INSERT INTO teams (id, name, identifier, created_at, updated_at)
            VALUES ('t2', 'T2', 'TWO', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
            INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES
              ('l1b', 'In Progress', 1, 't1', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z'),
              ('l2a', 'Todo', 0, 't2', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z'),
              ('l2b', 'in progress', 1, 't2', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
            INSERT INTO members (id, remote_id, team_id, name, email, created_at, updated_at) VALUES
              ('m1', 1, 't1', 'Ann', 'ann@x.io', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z'),
              ('m2', 2, 't1', 'Bob', 'bob@x.io', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z'),
              ('m1t2', NULL, 't2', 'Ann', 'ANN@x.io', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
            INSERT INTO projects (id, team_id, name, created_at, updated_at)
              VALUES ('p1', 't1', 'P', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');
            INSERT INTO issues (id, title, priority, lane_id, team_id, team_number, created_at, updated_at)
              VALUES ('x2', 'Existing', 4, 'l2a', 't2', 7, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z');");
        conn
    }

    fn issue_row(conn: &Connection, id: &str) -> Issue {
        get_issue(conn, id).unwrap().unwrap()
    }

    #[test]
    fn move_sets_team_and_unique_provisional_number() {
        let conn = move_setup();
        let s = move_issue_to_team(&conn, "i1", "t2").unwrap();
        let i = issue_row(&conn, "i1");
        assert_eq!(i.team_id, "t2");
        assert_eq!(i.team_number, Some(8));
        assert_eq!(s.team_number, 8);
    }

    #[test]
    fn move_remaps_lane_by_name_case_insensitively() {
        let conn = move_setup();
        conn.execute("UPDATE issues SET lane_id='l1b' WHERE id='i1'", []).unwrap();
        let s = move_issue_to_team(&conn, "i1", "t2").unwrap();
        assert_eq!(issue_row(&conn, "i1").lane_id, "l2b");
        assert!(!s.lane_fell_back);
    }

    #[test]
    fn move_falls_back_to_first_lane_by_position() {
        let conn = move_setup();
        // "Backlog" has no match in t2.
        let s = move_issue_to_team(&conn, "i1", "t2").unwrap();
        assert_eq!(issue_row(&conn, "i1").lane_id, "l2a");
        assert!(s.lane_fell_back);
    }

    #[test]
    fn move_keeps_assignee_who_is_a_target_member() {
        let conn = move_setup();
        conn.execute("UPDATE issues SET assignee_id='m1' WHERE id='i1'", []).unwrap();
        let s = move_issue_to_team(&conn, "i1", "t2").unwrap();
        assert_eq!(issue_row(&conn, "i1").assignee_id.as_deref(), Some("m1t2"));
        assert!(!s.assignee_cleared);
    }

    #[test]
    fn move_clears_assignee_who_is_not_a_target_member() {
        let conn = move_setup();
        conn.execute("UPDATE issues SET assignee_id='m2' WHERE id='i1'", []).unwrap();
        let s = move_issue_to_team(&conn, "i1", "t2").unwrap();
        assert_eq!(issue_row(&conn, "i1").assignee_id, None);
        assert!(s.assignee_cleared);
    }

    #[test]
    fn move_clears_project_and_removes_all_labels() {
        let conn = move_setup();
        insert_label(&conn, &make_label("lab1", "bug")).unwrap();
        insert_label(&conn, &make_label("lab2", "ui")).unwrap();
        set_issue_labels(&conn, "i1", &["lab1".into(), "lab2".into()]).unwrap();
        conn.execute("UPDATE issues SET project_id='p1' WHERE id='i1'", []).unwrap();
        let s = move_issue_to_team(&conn, "i1", "t2").unwrap();
        assert_eq!(issue_row(&conn, "i1").project_id, None);
        assert!(label_ids_for_issue(&conn, "i1").unwrap().is_empty());
        assert!(s.project_cleared);
        assert_eq!(s.labels_removed, 2);
        // The labels themselves survive.
        assert_eq!(labels_for_team(&conn, "t1").unwrap().len(), 2);
    }

    #[test]
    fn move_clears_cross_team_parent() {
        let conn = move_setup();
        conn.execute(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at)
             VALUES ('par', 'Parent', 4, 'l1', 't1', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        conn.execute("UPDATE issues SET parent_issue_id='par' WHERE id='i1'", []).unwrap();
        let s = move_issue_to_team(&conn, "i1", "t2").unwrap();
        assert_eq!(issue_row(&conn, "i1").parent_issue_id, None);
        assert!(s.parent_cleared);
    }

    fn move_entries(conn: &Connection) -> Vec<crate::db::models::SyncQueueEntry> {
        crate::db::sync_queue::pending_entries(conn)
            .unwrap()
            .into_iter()
            .filter(|e| e.entity_type == "issue" && e.action == "move")
            .collect()
    }

    #[test]
    fn queue_move_captures_remote_ids_before_the_team_changes() {
        let conn = move_setup();
        conn.execute_batch(
            "UPDATE teams SET remote_id = 100 WHERE id = 't1';
             UPDATE teams SET remote_id = 200 WHERE id = 't2';
             UPDATE issues SET remote_id = 55 WHERE id = 'i1';",
        )
        .unwrap();
        move_issue_to_team(&conn, "i1", "t2").unwrap();
        assert!(queue_issue_move(&conn, "i1", "t1").unwrap());
        let entries = move_entries(&conn);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entity_id, "i1");
        let p: serde_json::Value =
            serde_json::from_str(entries[0].payload.as_deref().unwrap()).unwrap();
        assert_eq!(p["source_team_remote_id"], 100);
        assert_eq!(p["issue_remote_id"], 55);
        assert_eq!(p["target_team_remote_id"], 200);
        assert_eq!(p["target_team_id"], "t2");
    }

    #[test]
    fn queue_move_keeps_unknown_target_remote_id_null() {
        let conn = move_setup();
        conn.execute_batch(
            "UPDATE teams SET remote_id = 100 WHERE id = 't1';
             UPDATE issues SET remote_id = 55 WHERE id = 'i1';",
        )
        .unwrap();
        move_issue_to_team(&conn, "i1", "t2").unwrap();
        assert!(queue_issue_move(&conn, "i1", "t1").unwrap());
        let p: serde_json::Value =
            serde_json::from_str(move_entries(&conn)[0].payload.as_deref().unwrap()).unwrap();
        assert!(p["target_team_remote_id"].is_null());
    }

    #[test]
    fn queue_move_skips_an_issue_the_server_has_never_seen() {
        // Its pending create already posts to the new team.
        let conn = move_setup();
        move_issue_to_team(&conn, "i1", "t2").unwrap();
        assert!(!queue_issue_move(&conn, "i1", "t1").unwrap());
        assert!(move_entries(&conn).is_empty());
    }

    #[test]
    fn move_toast_names_the_team_and_what_was_cleared() {
        let conn = move_setup();
        conn.execute("UPDATE issues SET assignee_id='m2', project_id='p1' WHERE id='i1'", [])
            .unwrap();
        let s = move_issue_to_team(&conn, "i1", "t2").unwrap();
        assert_eq!(s.toast("T2"), "Moved to T2; cleared: lane reset to first, assignee, project");
        let plain = IssueMoveSummary {
            team_number: 1,
            lane_id: "l".into(),
            lane_fell_back: false,
            assignee_cleared: false,
            project_cleared: false,
            parent_cleared: false,
            labels_removed: 0,
            sub_issues_detached: 0,
        };
        assert_eq!(plain.toast("T2"), "Moved to T2");
    }

    #[test]
    fn move_keeps_a_parent_already_in_the_target_team() {
        let conn = move_setup();
        conn.execute("UPDATE issues SET parent_issue_id='x2' WHERE id='i1'", []).unwrap();
        let s = move_issue_to_team(&conn, "i1", "t2").unwrap();
        assert_eq!(issue_row(&conn, "i1").parent_issue_id.as_deref(), Some("x2"));
        assert!(!s.parent_cleared);
    }

    #[test]
    fn move_detaches_sub_issues() {
        let conn = move_setup();
        conn.execute(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, parent_issue_id, created_at, updated_at)
             VALUES ('kid', 'Kid', 4, 'l1', 't1', 'i1', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        let s = move_issue_to_team(&conn, "i1", "t2").unwrap();
        let kid = issue_row(&conn, "kid");
        assert_eq!(kid.parent_issue_id, None);
        assert_eq!(kid.team_id, "t1");
        assert_eq!(s.sub_issues_detached, 1);
    }

    #[test]
    fn move_leaves_comments_and_dependencies_alone() {
        let conn = move_setup();
        conn.execute(
            "INSERT INTO comments (id, issue_id, body, created_at, updated_at)
             VALUES ('c1', 'i1', 'hi', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO issue_dependencies (id, blocking_issue_id, blocked_issue_id, created_at, updated_at)
             VALUES ('d1', 'i1', 'x2', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        move_issue_to_team(&conn, "i1", "t2").unwrap();
        let n = |t: &str| -> i64 {
            conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap()
        };
        assert_eq!((n("comments"), n("issue_dependencies")), (1, 1));
    }

    #[test]
    fn move_to_same_team_or_laneless_team_is_refused_without_changes() {
        let conn = move_setup();
        assert!(move_issue_to_team(&conn, "i1", "t1").is_err());
        conn.execute(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at)
             VALUES ('t3', 'T3', 'TRE', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        assert!(move_issue_to_team(&conn, "i1", "t3").is_err());
        assert_eq!(issue_row(&conn, "i1").team_id, "t1");
    }
}
