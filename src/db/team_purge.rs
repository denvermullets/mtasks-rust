//! Removing the local copy of teams the token can no longer see (JRST-323).
//!
//! A successful `GET /teams` is the only signal: a per-team pull failure says
//! nothing about access, since `/teams` listed that team moments earlier.
//! A revoked user must not keep the team's data, so the purge also drops the
//! team's unsynced edits: they are the one thing here that can't be pulled again.

use std::path::PathBuf;

use rusqlite::{params, Connection, Result, Transaction, TransactionBehavior};

use super::models::SyncQueueEntry;
use super::{queries, sync_queue};

/// A team removed from the local database.
#[derive(Debug)]
pub struct PurgedTeam {
    pub team_id: String,
    pub team_name: String,
    /// Unsynced edits dropped with the team, for the caller to log.
    pub dropped_edits: Vec<SyncQueueEntry>,
    /// Staged upload copies the caller should delete now the rows are gone.
    pub outbox_paths: Vec<PathBuf>,
}

impl PurgedTeam {
    /// What to tell the user, since their data and edits just went away.
    pub fn notice(&self) -> String {
        let removed = match self.dropped_edits.len() {
            0 => "Its local data was removed".to_string(),
            1 => "Its local data and 1 unsynced edit were removed".to_string(),
            n => format!("Its local data and {n} unsynced edits were removed"),
        };
        format!("You no longer have access to {}. {removed}.", self.team_name)
    }
}

/// What [`reconcile_listed_teams`] did with the teams `/teams` left out.
#[derive(Debug, Default)]
pub struct Reconciled {
    pub purged: Vec<PurgedTeam>,
    /// Teams whose purge failed, with the reason. They are retried next pull.
    pub failed: Vec<(String, String)>,
    /// An empty list just archived teams that were still active. Only true on
    /// that first response, so repeats of it don't tell the user again.
    pub archived_all: bool,
}

impl Reconciled {
    /// One message for each thing the user needs to hear about.
    pub fn notices(&self) -> Vec<String> {
        let mut notices: Vec<String> = self.purged.iter().map(PurgedTeam::notice).collect();
        if self.archived_all {
            notices.push(
                "Your token no longer lists any teams. Local data is kept in case access returns."
                    .to_string(),
            );
        }
        notices
    }
}

/// Bring local teams in line with a successful `/teams` response.
///
/// Listed teams are restored if they were archived or flagged. Unlisted teams
/// are purged. An empty list only archives: it hides the teams without deleting
/// anything, so one odd response can't wipe the database.
pub fn reconcile_listed_teams(conn: &Connection, listed: &[i64]) -> Result<Reconciled> {
    let mut reconciled = Reconciled::default();
    let now = crate::db::now_iso();
    if listed.is_empty() {
        let archived = conn.execute(
            "UPDATE teams SET archived_at = ?1
             WHERE remote_id IS NOT NULL AND archived_at IS NULL",
            params![now],
        )?;
        reconciled.archived_all = archived > 0;
        return Ok(reconciled);
    }
    for remote_id in listed {
        queries::set_team_access_lost(conn, *remote_id, false)?;
        conn.execute(
            "UPDATE teams SET archived_at = NULL
             WHERE remote_id = ?1 AND archived_at IS NOT NULL",
            params![remote_id],
        )?;
    }
    for team in queries::all_teams(conn)? {
        let Some(remote_id) = team.remote_id else { continue };
        if listed.contains(&remote_id) {
            continue;
        }
        match purge_team(conn, &team.id, &team.name, remote_id) {
            Ok(purged) => reconciled.purged.push(purged),
            Err(e) => reconciled.failed.push((team.id, e.to_string())),
        }
    }
    Ok(reconciled)
}

/// Immediate, so an edit made during the purge waits and can't be half-applied.
fn purge_team(conn: &Connection, team_id: &str, team_name: &str, remote_id: i64) -> Result<PurgedTeam> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    // Before the rows go: resolving an entry's team reads them.
    let dropped_edits = pending_entries_for_team(&tx, remote_id)?;
    for entry_id in dropped_edits.iter().filter_map(|e| e.id) {
        crate::db::activity::discard_pending_for_entry(&tx, entry_id)?;
        tx.execute("DELETE FROM sync_queue WHERE id = ?1", params![entry_id])?;
    }
    let outbox_paths = purge(&tx, team_id, remote_id)?;
    tx.commit()?;
    Ok(PurgedTeam {
        team_id: team_id.to_string(),
        team_name: team_name.to_string(),
        dropped_edits,
        outbox_paths,
    })
}

fn pending_entries_for_team(conn: &Connection, remote_id: i64) -> Result<Vec<SyncQueueEntry>> {
    Ok(sync_queue::pending_entries(conn)?
        .into_iter()
        .filter(|e| sync_queue::entry_team_remote_id(conn, e) == Some(remote_id))
        .collect())
}

/// Ids of the comments on team `?1`'s issues and projects.
const OWNED_COMMENTS: &str = "SELECT id FROM comments
    WHERE issue_id IN (SELECT id FROM issues WHERE team_id = ?1)
       OR project_id IN (SELECT id FROM projects WHERE team_id = ?1)";

/// Delete the team's rows. Returns the staged upload copies they leave behind.
fn purge(conn: &Connection, team_id: &str, remote_id: i64) -> Result<Vec<PathBuf>> {
    let outbox_paths = delete_attachments(conn, team_id)?;
    delete_synced_queue_rows(conn, team_id, remote_id)?;

    // Other teams' rows may point here. A failure on any other reference rolls
    // the whole purge back, and it is retried next pull.
    conn.execute(
        "UPDATE issues SET parent_issue_id = NULL
         WHERE team_id != ?1
           AND parent_issue_id IN (SELECT id FROM issues WHERE team_id = ?1)",
        params![team_id],
    )?;
    conn.execute(
        "UPDATE issues SET project_id = NULL
         WHERE team_id != ?1
           AND project_id IN (SELECT id FROM projects WHERE team_id = ?1)",
        params![team_id],
    )?;

    // Comments, events, labels links and activity cascade from these.
    for table in ["issues", "recurring_issues", "projects", "labels", "lanes", "saved_views"] {
        conn.execute(&format!("DELETE FROM {table} WHERE team_id = ?1"), params![team_id])?;
    }
    settle_members(conn, team_id)?;
    conn.execute(
        "DELETE FROM notifications WHERE team_remote_id = ?1",
        params![remote_id],
    )?;
    conn.execute("DELETE FROM pull_cursors WHERE team_id = ?1", params![team_id])?;
    conn.execute("DELETE FROM list_etags WHERE team_id = ?1", params![team_id])?;
    conn.execute("DELETE FROM team_terminal_dirs WHERE team_id = ?1", params![team_id])?;
    conn.execute("DELETE FROM terminal_records WHERE team_id = ?1", params![team_id])?;
    conn.execute("DELETE FROM teams WHERE id = ?1", params![team_id])?;

    Ok(outbox_paths)
}

fn delete_attachments(conn: &Connection, team_id: &str) -> Result<Vec<PathBuf>> {
    let owned = format!(
        "(owner_kind = 'issue' AND owner_id IN (SELECT id FROM issues WHERE team_id = ?1))
         OR (owner_kind = 'project' AND owner_id IN (SELECT id FROM projects WHERE team_id = ?1))
         OR (owner_kind = 'comment' AND owner_id IN ({OWNED_COMMENTS}))"
    );
    let paths = {
        let mut stmt = conn.prepare(&format!(
            "SELECT outbox_path FROM attachments WHERE outbox_path IS NOT NULL AND ({owned})"
        ))?;
        let rows = stmt.query_map(params![team_id], |r| r.get::<_, String>(0))?;
        rows.map(|p| p.map(PathBuf::from)).collect::<Result<Vec<_>>>()?
    };
    conn.execute(&format!("DELETE FROM attachments WHERE {owned}"), params![team_id])?;
    Ok(paths)
}

/// Pushed queue rows keep their payloads, which hold team content. Runs before
/// the owning rows go, since it finds them by entity id. Attachments are gone
/// by now, so their rows are matched through the payload's team instead.
fn delete_synced_queue_rows(conn: &Connection, team_id: &str, remote_id: i64) -> Result<()> {
    conn.execute(
        &format!(
            "DELETE FROM sync_queue
             WHERE synced_at IS NOT NULL AND (
                entity_id IN (
                    SELECT id FROM issues WHERE team_id = ?1
                    UNION SELECT id FROM projects WHERE team_id = ?1
                    UNION SELECT id FROM labels WHERE team_id = ?1
                    UNION SELECT id FROM recurring_issues WHERE team_id = ?1
                    UNION SELECT id FROM saved_views WHERE team_id = ?1
                    UNION {OWNED_COMMENTS}
                )
                OR CASE WHEN json_valid(payload)
                        THEN json_extract(payload, '$.team_remote_id') END = ?2
             )"
        ),
        params![team_id, remote_id],
    )?;
    Ok(())
}

/// A member row is shared across teams: `members.remote_id` is unique, so the
/// row's `team_id` is whichever team pulled it last. Anyone another team still
/// references moves to that team instead of being deleted. Dropping the member
/// list ETags makes the same pull refetch every remaining team's list and put
/// the rows back where they belong.
fn settle_members(conn: &Connection, team_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE members SET team_id = (
            SELECT owner FROM (
                SELECT assignee_id AS member, team_id AS owner FROM issues
                UNION ALL SELECT creator_id, team_id FROM issues
                UNION ALL SELECT lead_id, team_id FROM projects
                UNION ALL SELECT c.author_id, i.team_id FROM comments c JOIN issues i ON i.id = c.issue_id
                UNION ALL SELECT c.author_id, p.team_id FROM comments c JOIN projects p ON p.id = c.project_id
            ) WHERE member = members.id LIMIT 1
         )
         WHERE team_id = ?1 AND id IN (
            SELECT assignee_id FROM issues
            UNION SELECT creator_id FROM issues
            UNION SELECT lead_id FROM projects
            UNION SELECT author_id FROM comments
         )",
        params![team_id],
    )?;
    conn.execute("DELETE FROM members WHERE team_id = ?1", params![team_id])?;
    conn.execute("DELETE FROM list_etags WHERE list = 'members'", [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;

    const NOW: &str = "2026-01-01T00:00:00.000Z";

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    /// Team `t` (remote `rid`) with lane `t-l`, member `t-m` and issue `t-i`.
    fn team(conn: &Connection, t: &str, rid: i64) {
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES (?1, ?1, ?1, ?2, ?3, ?3)",
            params![t, rid, NOW],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lanes (id, name, position, team_id, created_at, updated_at)
             VALUES (?1 || '-l', 'L', 0, ?1, ?2, ?2)",
            params![t, NOW],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO members (id, remote_id, team_id, name, email, created_at, updated_at)
             VALUES (?1 || '-m', ?2 * 100, ?1, 'M', ?1 || '@x', ?3, ?3)",
            params![t, rid, NOW],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, assignee_id, created_at, updated_at)
             VALUES (?1 || '-i', 'I', 4, ?1 || '-l', ?1, ?1 || '-m', ?2, ?2)",
            params![t, NOW],
        )
        .unwrap();
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {sql}"), [], |r| r.get(0)).unwrap()
    }

    fn enqueue(conn: &Connection, entity: &str, id: &str, action: &str) {
        sync_queue::enqueue(conn, entity, id, action, None).unwrap();
    }

    #[test]
    fn an_unlisted_team_is_removed_and_a_listed_one_is_untouched() {
        let c = mem();
        team(&c, "gone", 1);
        team(&c, "kept", 2);
        c.execute(
            "INSERT INTO notifications (id, remote_id, action, message, team_remote_id, created_at, updated_at)
             VALUES ('n', 1, 'a', 'm', 1, ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        c.execute(
            "INSERT INTO pull_cursors (team_id, resource, cursor) VALUES ('gone', 'issues', ?1)",
            params![NOW],
        )
        .unwrap();
        c.execute("INSERT INTO list_etags (team_id, list, etag) VALUES ('gone', 'lanes', 'e')", [])
            .unwrap();

        let r = reconcile_listed_teams(&c, &[2]).unwrap();

        assert_eq!(r.purged.len(), 1);
        assert_eq!(r.purged[0].team_id, "gone");
        assert!(r.failed.is_empty());
        assert_eq!(count(&c, "teams WHERE id = 'gone'"), 0);
        assert_eq!(count(&c, "issues WHERE team_id = 'gone'"), 0);
        assert_eq!(count(&c, "lanes WHERE team_id = 'gone'"), 0);
        assert_eq!(count(&c, "members WHERE team_id = 'gone'"), 0);
        assert_eq!(count(&c, "notifications"), 0);
        assert_eq!(count(&c, "pull_cursors WHERE team_id = 'gone'"), 0);
        assert_eq!(count(&c, "list_etags WHERE team_id = 'gone'"), 0);
        assert_eq!(count(&c, "issues WHERE team_id = 'kept'"), 1);
    }

    #[test]
    fn purging_a_team_removes_its_terminal_directory_and_only_its_own() {
        let c = mem();
        team(&c, "gone", 1);
        team(&c, "kept", 2);
        crate::db::terminal_dirs::set(&c, "gone", std::path::Path::new("/a")).unwrap();
        crate::db::terminal_dirs::set(&c, "kept", std::path::Path::new("/b")).unwrap();

        reconcile_listed_teams(&c, &[2]).unwrap();

        let dirs = crate::db::terminal_dirs::all(&c).unwrap();
        assert_eq!(dirs.keys().collect::<Vec<_>>(), vec!["kept"]);
    }

    #[test]
    fn purging_a_team_removes_its_terminal_records_and_enqueues_nothing() {
        use crate::db::terminal_records::{all, apply, Change, Stored};
        let c = mem();
        team(&c, "gone", 1);
        team(&c, "kept", 2);
        let rec = |k: &str, t: &str| Stored { key: k.into(), team_id: t.into(), name: k.into(), cwd: None };
        apply(&c, &[Change::Added(rec("a", "gone")), Change::Added(rec("b", "kept"))]).unwrap();
        let before = count(&c, "sync_queue");

        reconcile_listed_teams(&c, &[2]).unwrap();

        assert_eq!(all(&c).unwrap(), vec![rec("b", "kept")]);
        assert_eq!(count(&c, "sync_queue"), before);
    }

    #[test]
    fn a_team_with_pending_edits_is_still_purged_and_the_edits_go_with_it() {
        let c = mem();
        team(&c, "gone", 1);
        team(&c, "kept", 2);
        enqueue(&c, "issue", "gone-i", "update");
        // Blocking issue is on the purged team, so this entry resolves to it
        // even though its entity id isn't one of the team's own rows.
        c.execute(
            "INSERT INTO issue_dependencies (id, blocking_issue_id, blocked_issue_id, kind, created_at, updated_at)
             VALUES ('d', 'gone-i', 'kept-i', 'blocks', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        enqueue(&c, "dependency", "d", "create");

        let r = reconcile_listed_teams(&c, &[2]).unwrap();

        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(r.purged.len(), 1);
        assert_eq!(count(&c, "teams WHERE id = 'gone'"), 0);
        assert_eq!(count(&c, "issues WHERE team_id = 'gone'"), 0);
        assert_eq!(count(&c, "sync_queue"), 0);
    }

    #[test]
    fn another_teams_pending_edit_survives_the_purge() {
        let c = mem();
        team(&c, "gone", 1);
        team(&c, "kept", 2);
        enqueue(&c, "issue", "kept-i", "update");

        let r = reconcile_listed_teams(&c, &[2]).unwrap();

        assert_eq!(r.purged.len(), 1);
        assert!(r.purged[0].dropped_edits.is_empty());
        assert_eq!(count(&c, "sync_queue WHERE synced_at IS NULL"), 1);
    }

    #[test]
    fn the_purge_reports_each_edit_it_dropped() {
        let c = mem();
        team(&c, "gone", 1);
        team(&c, "kept", 2);
        enqueue(&c, "issue", "gone-i", "update");
        enqueue(&c, "issue", "gone-i", "move");
        enqueue(&c, "issue", "kept-i", "update");

        let r = reconcile_listed_teams(&c, &[2]).unwrap();

        let dropped: Vec<_> = r.purged[0]
            .dropped_edits
            .iter()
            .map(|e| (e.entity_id.as_str(), e.action.as_str()))
            .collect();
        assert_eq!(dropped, vec![("gone-i", "update"), ("gone-i", "move")]);
    }

    #[test]
    fn the_notice_names_the_team_and_counts_the_lost_edits() {
        let c = mem();
        team(&c, "none", 1);
        team(&c, "one", 2);
        team(&c, "two", 3);
        team(&c, "kept", 4);
        enqueue(&c, "issue", "one-i", "update");
        enqueue(&c, "issue", "two-i", "update");
        enqueue(&c, "issue", "two-i", "move");

        let r = reconcile_listed_teams(&c, &[4]).unwrap();

        let notice = |name: &str| {
            r.purged.iter().find(|t| t.team_id == name).unwrap().notice()
        };
        assert_eq!(notice("none"), "You no longer have access to none. Its local data was removed.");
        assert_eq!(
            notice("one"),
            "You no longer have access to one. Its local data and 1 unsynced edit were removed."
        );
        assert_eq!(
            notice("two"),
            "You no longer have access to two. Its local data and 2 unsynced edits were removed."
        );
    }

    #[test]
    fn an_empty_list_archives_without_deleting() {
        let c = mem();
        team(&c, "a", 1);

        let r = reconcile_listed_teams(&c, &[]).unwrap();

        assert!(r.purged.is_empty());
        assert_eq!(count(&c, "issues WHERE team_id = 'a'"), 1);
        assert_eq!(count(&c, "teams WHERE archived_at IS NOT NULL"), 1);
    }

    #[test]
    fn an_empty_list_gives_one_notice_until_teams_return() {
        let c = mem();
        team(&c, "a", 1);

        let first = reconcile_listed_teams(&c, &[]).unwrap();
        assert_eq!(
            first.notices(),
            vec!["Your token no longer lists any teams. Local data is kept in case access returns."]
        );

        let repeat = reconcile_listed_teams(&c, &[]).unwrap();
        assert!(repeat.notices().is_empty());

        assert!(reconcile_listed_teams(&c, &[1]).unwrap().notices().is_empty());
        assert_eq!(reconcile_listed_teams(&c, &[]).unwrap().notices().len(), 1);
    }

    #[test]
    fn an_archived_team_is_not_reported_as_inaccessible() {
        let c = mem();
        team(&c, "a", 1);
        queries::set_team_access_lost(&c, 1, true).unwrap();
        assert_eq!(queries::inaccessible_team_identifiers(&c).unwrap(), vec!["a"]);

        reconcile_listed_teams(&c, &[]).unwrap();

        assert!(queries::inaccessible_team_identifiers(&c).unwrap().is_empty());
    }

    #[test]
    fn an_empty_list_with_only_local_teams_gives_no_notice() {
        let c = mem();
        c.execute(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at)
             VALUES ('local', 'Local', 'LOC', ?1, ?1)",
            params![NOW],
        )
        .unwrap();

        assert!(reconcile_listed_teams(&c, &[]).unwrap().notices().is_empty());
    }

    #[test]
    fn a_listed_team_comes_back_from_archived_and_flagged() {
        let c = mem();
        team(&c, "a", 1);
        reconcile_listed_teams(&c, &[]).unwrap();
        queries::set_team_access_lost(&c, 1, true).unwrap();

        reconcile_listed_teams(&c, &[1]).unwrap();

        assert_eq!(count(&c, "teams WHERE archived_at IS NULL AND access_lost_at IS NULL"), 1);
    }

    #[test]
    fn a_local_only_team_is_never_touched() {
        let c = mem();
        c.execute(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at)
             VALUES ('local', 'L', 'L', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        team(&c, "kept", 2);

        reconcile_listed_teams(&c, &[2]).unwrap();

        assert_eq!(count(&c, "teams WHERE id = 'local'"), 1);
    }

    #[test]
    fn a_member_another_team_references_moves_there() {
        let c = mem();
        team(&c, "gone", 1);
        team(&c, "kept", 2);
        c.execute("UPDATE issues SET creator_id = 'gone-m' WHERE id = 'kept-i'", []).unwrap();
        c.execute("INSERT INTO list_etags (team_id, list, etag) VALUES ('kept', 'members', 'e')", [])
            .unwrap();

        let r = reconcile_listed_teams(&c, &[2]).unwrap();

        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(count(&c, "members WHERE id = 'gone-m' AND team_id = 'kept'"), 1);
        assert_eq!(count(&c, "issues WHERE id = 'kept-i' AND creator_id = 'gone-m'"), 1);
        // The next pull refetches the list, even if it would have been a 304.
        assert_eq!(count(&c, "list_etags WHERE list = 'members'"), 0);
    }

    #[test]
    fn links_from_other_teams_are_cleared() {
        let c = mem();
        team(&c, "gone", 1);
        team(&c, "kept", 2);
        c.execute(
            "INSERT INTO projects (id, team_id, name, created_at, updated_at)
             VALUES ('gone-p', 'gone', 'P', ?1, ?1)",
            params![NOW],
        )
        .unwrap();
        c.execute(
            "UPDATE issues SET parent_issue_id = 'gone-i', project_id = 'gone-p' WHERE id = 'kept-i'",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO issue_dependencies (id, blocking_issue_id, blocked_issue_id, kind, created_at, updated_at)
             VALUES ('d', 'kept-i', 'gone-i', 'blocks', ?1, ?1)",
            params![NOW],
        )
        .unwrap();

        let r = reconcile_listed_teams(&c, &[2]).unwrap();

        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(
            count(&c, "issues WHERE id = 'kept-i' AND parent_issue_id IS NULL AND project_id IS NULL"),
            1
        );
        assert_eq!(count(&c, "issue_dependencies"), 0);
    }

    #[test]
    fn comments_attachments_and_synced_queue_rows_go_too() {
        let c = mem();
        team(&c, "gone", 1);
        team(&c, "kept", 2);
        c.execute_batch(&format!(
            "INSERT INTO comments (id, issue_id, body, created_at, updated_at)
               VALUES ('c1', 'gone-i', 'b', '{NOW}', '{NOW}');
             INSERT INTO comments (id, issue_id, parent_id, body, created_at, updated_at)
               VALUES ('c2', 'gone-i', 'c1', 'b', '{NOW}', '{NOW}');
             INSERT INTO attachments (id, owner_kind, owner_id, filename, created_at, outbox_path)
               VALUES ('a1', 'comment', 'c1', 'f', '{NOW}', '/tmp/x'),
                      ('a2', 'issue', 'kept-i', 'f', '{NOW}', NULL);"
        ))
        .unwrap();
        enqueue(&c, "issue", "gone-i", "update");
        enqueue(&c, "issue", "kept-i", "update");
        c.execute("UPDATE sync_queue SET synced_at = ?1", params![NOW]).unwrap();

        let r = reconcile_listed_teams(&c, &[2]).unwrap();

        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(r.purged[0].outbox_paths, vec![PathBuf::from("/tmp/x")]);
        assert_eq!(count(&c, "comments"), 0);
        assert_eq!(count(&c, "attachments"), 1);
        assert_eq!(count(&c, "sync_queue WHERE entity_id = 'gone-i'"), 0);
        assert_eq!(count(&c, "sync_queue WHERE entity_id = 'kept-i'"), 1);
    }
}
