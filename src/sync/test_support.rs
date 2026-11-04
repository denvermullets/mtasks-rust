//! Fixtures shared by the sync module test suites.

use rusqlite::Connection;

use crate::api::MeDto;
use crate::api::dto::{CommentDto, IssueDto, ProjectDto};
use crate::db::queries;
use crate::sync::engine::CycleCtx;

pub(super) fn ctx_with_scopes(scopes: &[&str]) -> CycleCtx {
    let me: MeDto = serde_json::from_value(serde_json::json!({
        "id": 7, "name": "Me", "email": "me@example.com",
        "token": { "name": "t", "scopes": scopes }
    }))
    .unwrap();
    CycleCtx { me: Ok(me) }
}

pub(super) fn setup_labels_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    crate::db::migrations::run(&conn).unwrap();
    let now = "2026-01-01T00:00:00.000Z";
    conn.execute(
        "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
         VALUES ('t1', 'T', 'T', 700, ?1, ?1)",
        rusqlite::params![now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO lanes (id, name, position, team_id, created_at, updated_at)
         VALUES ('l1', 'B', 0, 't1', ?1, ?1)",
        rusqlite::params![now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at)
         VALUES ('i1', 'Issue', 4, 'l1', 't1', ?1, ?1)",
        rusqlite::params![now],
    )
    .unwrap();
    conn
}

pub(super) fn insert_test_label(conn: &Connection, id: &str, remote_id: Option<i64>) {
    let now = "2026-01-01T00:00:00.000Z";
    conn.execute(
        "INSERT INTO labels (id, remote_id, team_id, name, color, created_at, updated_at)
         VALUES (?1, ?2, 't1', ?1, '#ef4444', ?3, ?3)",
        rusqlite::params![id, remote_id, now],
    )
    .unwrap();
}

pub(super) fn setup_tombstoned_issue() -> Connection {
    let conn = setup_labels_db();
    insert_test_label(&conn, "lab1", Some(11));
    queries::set_issue_labels(&conn, "i1", &["lab1".into()]).unwrap();
    conn.execute_batch(
        "INSERT INTO issues (id, title, priority, lane_id, team_id, parent_issue_id, created_at, updated_at)
           VALUES ('i2', 'Other', 4, 'l1', 't1', 'i1', '2026-01-01', '2026-01-01');
         INSERT INTO comments (id, issue_id, body, created_at, updated_at)
           VALUES ('c1', 'i1', 'hi', '2026-01-01', '2026-01-01');
         INSERT INTO issue_dependencies (id, blocking_issue_id, blocked_issue_id, created_at, updated_at)
           VALUES ('d1', 'i1', 'i2', '2026-01-01', '2026-01-01');",
    )
    .unwrap();
    queries::delete_issue(&conn, "i1").unwrap();
    conn
}

pub(super) fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

pub(super) fn assert_purged(conn: &Connection) {
    assert_eq!(count(conn, "SELECT COUNT(*) FROM issues WHERE id='i1'"), 0);
    assert_eq!(count(conn, "SELECT COUNT(*) FROM comments"), 0);
    assert_eq!(count(conn, "SELECT COUNT(*) FROM issue_labels"), 0);
    assert_eq!(count(conn, "SELECT COUNT(*) FROM issue_dependencies"), 0);
    // Sub-issue survives, detached.
    assert_eq!(
        count(conn, "SELECT COUNT(*) FROM issues WHERE id='i2' AND parent_issue_id IS NULL"),
        1
    );
}

pub(super) fn exec(conn: &Connection, sql: &str) {
    conn.execute_batch(sql).unwrap();
}

pub(super) fn setup_dependency_db() -> Connection {
    let conn = setup_labels_db();
    let now = "2026-01-01T00:00:00.000Z";
    conn.execute("UPDATE issues SET remote_id = 1 WHERE id = 'i1'", [])
        .unwrap();
    for (id, remote) in [("i2", 2), ("i3", 3)] {
        conn.execute(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, remote_id, created_at, updated_at)
             VALUES (?1, 'x', 4, 'l1', 't1', ?2, ?3, ?3)",
            rusqlite::params![id, remote, now],
        )
        .unwrap();
    }
    conn
}

pub(super) fn project_dto(id: i64, name: &str, created_at: &str) -> ProjectDto {
    serde_json::from_value(serde_json::json!({
        "id": id, "name": name, "description": null, "status": "backlog",
        "priority": "no_priority", "lead": null, "start_date": null,
        "due_date": null, "velocity_score": 0, "completed_issues_count": 0,
        "total_issues_count": 0, "progress_percentage": 0,
        "behind_schedule": false, "roadmap_commitment": null,
        "created_at": created_at, "updated_at": created_at,
    }))
    .unwrap()
}

pub(super) fn detail_dto(id: i64, title: &str, updated_at: &str) -> IssueDto {
    serde_json::from_value(serde_json::json!({
        "id": id, "identifier": format!("T-{id}"), "title": title,
        "priority": "medium", "estimate": null, "due_date": null,
        "lane": {"id": 10, "name": "B"}, "assignee": null, "creator": null,
        "project": null, "labels": [],
        "created_at": "2026-01-01T00:00:00.000Z", "updated_at": updated_at,
        "description": format!("desc {id}"),
        "parent_issue": {"id": 1, "identifier": "T-1"},
        "blocking_issues": [{"id": 3, "identifier": "T-3", "title": "x", "dependency_id": 55}],
        "blocked_issues": [],
        "started_at": "2026-02-01T00:00:00.000Z",
        "completed_at": "2026-03-01T00:00:00.000Z",
        "canceled_at": null
    }))
    .unwrap()
}

pub(super) fn feed_comment(id: i64, parent: Option<i64>, issue: Option<i64>, body: &str) -> CommentDto {
    CommentDto {
        id,
        body: body.into(),
        parent_id: parent,
        user: None,
        created_at: "2026-02-01T00:00:00.000Z".into(),
        updated_at: "2026-02-01T00:00:00.000Z".into(),
        replies: None,
        issue_id: issue,
        project_id: None,
        files: None,
    }
}

pub(super) fn feed_project_comment(id: i64, parent: Option<i64>, project: i64, body: &str) -> CommentDto {
    CommentDto {
        issue_id: None,
        project_id: Some(project),
        ..feed_comment(id, parent, None, body)
    }
}

pub(super) fn insert_test_project(conn: &Connection, id: &str, remote: i64) {
    exec(
        conn,
        &format!(
            "INSERT INTO projects (id, remote_id, team_id, name, created_at, updated_at)
             VALUES ('{id}', {remote}, 't1', 'P', '2026-01-01', '2026-01-01')"
        ),
    );
}

pub(super) fn dep_count(conn: &Connection) -> i64 {
    count(conn, "SELECT COUNT(*) FROM issue_dependencies")
}

pub(super) fn insert_dep(conn: &Connection, id: &str, remote: Option<i64>, blocking: &str, blocked: &str) {
    conn.execute(
        "INSERT INTO issue_dependencies
            (id, remote_id, blocking_issue_id, blocked_issue_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'x', 'x')",
        rusqlite::params![id, remote, blocking, blocked],
    )
    .unwrap();
}

// ── Issues moved to another team on the web (JRST-69) ──

/// i1..i3 (remote 1..3) live in t1; t2 is a second team with lane l2.
pub(super) fn setup_move_db() -> Connection {
    let conn = setup_dependency_db();
    let now = "2026-01-01T00:00:00.000Z";
    conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
    conn.execute(
        "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
         VALUES ('t2', 'U', 'U', 701, ?1, ?1)",
        rusqlite::params![now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO lanes (id, name, position, team_id, remote_id, created_at, updated_at)
         VALUES ('l2', 'B', 0, 't2', 20, ?1, ?1)",
        rusqlite::params![now],
    )
    .unwrap();
    conn
}

/// Remote issue 2 as the target team lists it after the move.
pub(super) fn moved_dto() -> IssueDto {
    let mut dto = detail_dto(2, "two", "2026-05-01T00:00:00.000Z");
    dto.identifier = "U-9".into();
    dto.lane = Some(serde_json::from_value(serde_json::json!({"id": 20, "name": "B"})).unwrap());
    dto
}

pub(super) fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}
