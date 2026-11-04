//! Server tombstones: the deleted-records feeds and applying them locally.

use std::collections::HashSet;

use rusqlite::Connection;

use crate::api::{ApiClient, deleted};
use crate::api::deleted::TombstoneDto;
use crate::api::dto::CommentDto;
use crate::db::queries;
use crate::sync::cursor;
use crate::sync::engine::{open, phase_scope, sync_log};
use crate::sync::pull::pull_mode;

/// Pull the user-level tombstone feed and remove Saved views deleted on the
/// server. Own cursor; advances to `X-Server-Time` once the page is applied.
pub(super) async fn pull_saved_view_tombstones_step(client: &ApiClient) -> Result<(), String> {
    let since = {
        let conn = open()?;
        crate::db::saved_views::deleted_cursor(&conn)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };
    let page = deleted::list_user(client, since.as_deref())
        .await
        .map_err(|e| format!("deleted (user): {e}"))?;
    let conn = open()?;
    let removed = crate::db::saved_views::apply_tombstones(&conn, &page.rows, page.server_time.as_deref())
        .map_err(|e| format!("db (saved view tombstones): {e}"))?;
    sync_log(
        "phase=pull_deleted_user",
        "ok",
        &format!("mode={} rows={} removed={removed}", pull_mode(&since), page.rows.len()),
    );
    Ok(())
}

/// Cursor resource name for the tombstone feed in `pull_cursors`.
const DELETED_CURSOR: &str = "deleted";

/// Pull the team's deleted-records feed and apply it. Runs after the issue
/// pull. The cursor advances to the response's `X-Server-Time` only when every
/// entry was applied or deliberately ignored.
pub(super) async fn pull_tombstones_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<HashSet<i64>, String> {
    let since = {
        let conn = open()?;
        queries::get_pull_cursor(&conn, local_team_id, DELETED_CURSOR)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };
    let page = deleted::list(client, team_remote_id, since.as_deref())
        .await
        .map_err(|e| format!("deleted: {e}"))?;
    sync_log(
        &phase_scope("pull_deleted", Some(team_remote_id)),
        "ok",
        &format!("mode={} rows={}", pull_mode(&since), page.rows.len()),
    );
    let conn = open()?;
    let failed = apply_tombstones(&conn, local_team_id, &page.rows);
    let deleted_comments = tombstoned_comment_ids(&page.rows);
    if failed > 0 {
        return Err(format!("deleted: {failed} failed (see [sync] log)"));
    }
    if let Some(t) = &page.server_time {
        queries::set_pull_cursor(&conn, local_team_id, DELETED_CURSOR, t)
            .map_err(|e| format!("db (pull cursor): {e}"))?;
    }
    Ok(deleted_comments)
}

/// Remote ids of the comments this feed page deleted.
fn tombstoned_comment_ids(rows: &[TombstoneDto]) -> HashSet<i64> {
    rows.iter()
        .filter(|r| r.record_type == "Comment")
        .map(|r| r.record_id)
        .collect()
}

/// Drop feed comments the tombstone pass just deleted, along with replies to
/// them (their parent is gone, so they would come back as top-level).
pub(super) fn drop_deleted_comments(dtos: Vec<CommentDto>, deleted: &HashSet<i64>) -> Vec<CommentDto> {
    if deleted.is_empty() {
        return dtos;
    }
    let mut gone = deleted.clone();
    loop {
        let before = gone.len();
        for d in &dtos {
            if d.parent_id.is_some_and(|p| gone.contains(&p)) {
                gone.insert(d.id);
            }
        }
        if gone.len() == before {
            break;
        }
    }
    dtos.into_iter().filter(|d| !gone.contains(&d.id)).collect()
}

/// Apply feed entries to the local DB (idempotent, so the cursor overlap is
/// safe). Handles Issue, Comment, Label, Project and
/// RecurringIssue; other record types are ignored. Returns how many entries
/// failed to apply.
fn apply_tombstones(conn: &Connection, local_team_id: &str, rows: &[TombstoneDto]) -> usize {
    let mut failed = 0;
    for row in rows {
        let id = row.record_id;
        let result = match row.record_type.as_str() {
            "Issue" => queries::apply_server_issue_tombstone(conn, local_team_id, id),
            "Comment" => queries::apply_server_comment_tombstone(conn, local_team_id, id),
            "Label" => queries::apply_server_label_tombstone(conn, local_team_id, id),
            "Project" => queries::apply_server_project_tombstone(conn, local_team_id, id),
            "IssueDependency" => {
                queries::apply_server_dependency_tombstone(conn, local_team_id, id)
            }
            "RecurringIssue" => {
                crate::db::recurring_issues::apply_tombstone(conn, local_team_id, id)
            }
            _ => continue,
        };
        if let Err(e) = result {
            sync_log(
                &format!("phase=pull_deleted type={} remote_id={id}", row.record_type),
                "apply_failed",
                &e.to_string(),
            );
            failed += 1;
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sync_queue;
    use crate::sync::pull::apply_pulled_comments;
    use crate::sync::test_support::{
        assert_purged, count, dep_count, exec, feed_project_comment, insert_dep,
        insert_test_label, insert_test_project, setup_dependency_db, setup_labels_db,
        setup_tombstoned_issue,
    };

    fn tomb(kind: &str, id: i64) -> TombstoneDto {
        TombstoneDto {
            record_type: kind.into(),
            record_id: id,
            deleted_at: None,
        }
    }

    fn give_i1_remote_id(conn: &Connection) {
        conn.execute("UPDATE issues SET remote_id = 500 WHERE id = 'i1'", [])
            .unwrap();
    }

    #[test]
    fn issue_tombstone_purges_live_issue_and_attachments() {
        let conn = setup_tombstoned_issue();
        conn.execute("UPDATE issues SET deleted_at = NULL WHERE id = 'i1'", [])
            .unwrap();
        give_i1_remote_id(&conn);
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Issue", 500)]), 0);
        assert_purged(&conn);
    }

    #[test]
    fn issue_tombstone_clears_local_marker_and_pending_delete() {
        let conn = setup_tombstoned_issue();
        give_i1_remote_id(&conn);
        sync_queue::enqueue(&conn, "issue", "i1", "delete", Some("{}")).unwrap();
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Issue", 500)]), 0);
        assert_purged(&conn);
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn tombstone_for_unknown_issue_or_other_team_is_a_noop() {
        let conn = setup_tombstoned_issue();
        give_i1_remote_id(&conn);
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Issue", 999)]), 0);
        assert_eq!(apply_tombstones(&conn, "other", &[tomb("Issue", 500)]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issues WHERE id='i1'"), 1);
    }

    #[test]
    fn unhandled_tombstone_types_are_ignored_and_repeats_are_safe() {
        let conn = setup_tombstoned_issue();
        give_i1_remote_id(&conn);
        let rows = [
            tomb("IssueDependency", 1),
            tomb("SomethingNew", 2),
            tomb("Issue", 500),
        ];
        assert_eq!(apply_tombstones(&conn, "t1", &rows), 0);
        assert_eq!(apply_tombstones(&conn, "t1", &rows), 0);
        assert_purged(&conn);
    }

    #[test]
    fn recurring_issue_tombstone_removes_the_local_row() {
        let conn = setup_labels_db();
        exec(
            &conn,
            "INSERT INTO recurring_issues (id, remote_id, team_id, title, priority, frequency,
                interval, active, created_at, updated_at)
               VALUES ('r1', 40, 't1', 'R', 0, 'daily', 1, 1, '2026-01-01', '2026-01-01');
             INSERT INTO recurring_issue_labels (recurring_issue_id, label_id)
               SELECT 'r1', id FROM labels LIMIT 1;",
        );
        let rows = [tomb("RecurringIssue", 999), tomb("RecurringIssue", 40)];
        assert_eq!(apply_tombstones(&conn, "t1", &rows), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM recurring_issues"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM recurring_issue_labels"), 0);
        assert_eq!(apply_tombstones(&conn, "t1", &rows), 0);
    }

    #[test]
    fn comment_tombstone_deletes_comment_and_replies_only() {
        let conn = setup_labels_db();
        exec(
            &conn,
            "INSERT INTO comments (id, remote_id, issue_id, body, parent_id, created_at, updated_at) VALUES
               ('c1', 901, 'i1', 'a', NULL, '2026-01-01', '2026-01-01'),
               ('c2', 902, 'i1', 'reply', 'c1', '2026-01-01', '2026-01-01'),
               ('c3', 903, 'i1', 'reply2', 'c2', '2026-01-01', '2026-01-01'),
               ('c4', 904, 'i1', 'other', NULL, '2026-01-01', '2026-01-01');",
        );
        sync_queue::enqueue(&conn, "comment", "c2", "create", Some("{}")).unwrap();
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Comment", 901)]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments WHERE id='c4'"), 1);
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 0);
        // Repeat, unknown id and another team are no-ops.
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Comment", 901)]), 0);
        assert_eq!(apply_tombstones(&conn, "other", &[tomb("Comment", 904)]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments"), 1);
    }

    #[test]
    fn label_tombstone_detaches_from_issues_and_projects() {
        let conn = setup_labels_db();
        insert_test_label(&conn, "lab1", Some(11));
        insert_test_label(&conn, "lab2", Some(22));
        exec(
            &conn,
            "INSERT INTO projects (id, remote_id, team_id, name, created_at, updated_at)
               VALUES ('p1', 800, 't1', 'P', '2026-01-01', '2026-01-01');
             INSERT INTO project_labels (project_id, label_id, created_at, updated_at)
               VALUES ('p1', 'lab1', '2026-01-01', '2026-01-01'), ('p1', 'lab2', '2026-01-01', '2026-01-01');",
        );
        queries::set_issue_labels(&conn, "i1", &["lab1".into(), "lab2".into()]).unwrap();
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Label", 11)]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM labels WHERE id='lab1'"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issue_labels"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM project_labels"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM labels WHERE id='lab2'"), 1);
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Label", 11)]), 0);
    }

    #[test]
    fn project_tombstone_unlinks_issues_and_deletes_project() {
        let conn = setup_labels_db();
        insert_test_label(&conn, "lab1", Some(11));
        exec(
            &conn,
            "INSERT INTO projects (id, remote_id, team_id, name, created_at, updated_at)
               VALUES ('p1', 800, 't1', 'P', '2026-01-01', '2026-01-01');
             INSERT INTO project_labels (project_id, label_id, created_at, updated_at)
               VALUES ('p1', 'lab1', '2026-01-01', '2026-01-01');
             UPDATE issues SET project_id = 'p1' WHERE id = 'i1';",
        );
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Project", 800)]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM projects"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM project_labels"), 0);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM issues WHERE id='i1' AND project_id IS NULL"),
            1
        );
        // The label itself survives.
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM labels WHERE id='lab1'"), 1);
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Project", 800)]), 0);
    }

    fn comment_dto(id: i64, parent: Option<i64>) -> CommentDto {
        CommentDto {
            id,
            body: "x".into(),
            parent_id: parent,
            user: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            replies: None,
            issue_id: Some(500),
            project_id: None,
            files: None,
        }
    }

    #[test]
    fn tombstoned_comment_in_feed_is_not_resurrected() {
        let conn = setup_labels_db();
        give_i1_remote_id(&conn);
        exec(
            &conn,
            "INSERT INTO comments (id, remote_id, issue_id, body, created_at, updated_at)
               VALUES ('c1', 901, 'i1', 'a', '2026-01-01', '2026-01-01');",
        );
        let rows = [tomb("Comment", 901)];
        assert_eq!(apply_tombstones(&conn, "t1", &rows), 0);
        let deleted = tombstoned_comment_ids(&rows);
        // Feed still carries the comment, a reply to it, and an unrelated one.
        let feed = vec![
            comment_dto(901, None),
            comment_dto(902, Some(901)),
            comment_dto(903, None),
        ];
        let kept = drop_deleted_comments(feed, &deleted);
        assert_eq!(kept.iter().map(|d| d.id).collect::<Vec<_>>(), vec![903]);
        assert_eq!(apply_pulled_comments(&conn, kept), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments WHERE remote_id IN (901,902)"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments WHERE remote_id = 903"), 1);
    }

    #[test]
    fn comment_tombstone_deletes_project_comment_and_replies() {
        let conn = setup_labels_db();
        insert_test_project(&conn, "p1", 80);
        let dtos = vec![
            feed_project_comment(1, None, 80, "top"),
            feed_project_comment(2, Some(1), 80, "reply"),
        ];
        assert_eq!(apply_pulled_comments(&conn, dtos), 0);
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Comment", 1)]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments"), 0);
    }

    #[test]
    fn dependency_tombstone_removes_edge_and_retires_pending_sync() {
        let conn = setup_dependency_db();
        insert_dep(&conn, "d1", Some(6), "i1", "i2");
        insert_dep(&conn, "d2", Some(7), "i1", "i3");
        conn.execute(
            "INSERT INTO sync_queue (entity_type, entity_id, action, created_at)
             VALUES ('dependency', 'd1', 'delete', 'x')",
            [],
        )
        .unwrap();
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("IssueDependency", 6)]), 0);
        assert_eq!(dep_count(&conn), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issue_dependencies WHERE id = 'd2'"), 1);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM sync_queue WHERE entity_id = 'd1' AND synced_at IS NULL"
            ),
            0
        );
        // Repeat and unknown ids are safe.
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("IssueDependency", 6)]), 0);
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("IssueDependency", 999)]), 0);
        assert_eq!(dep_count(&conn), 1);
    }
}
