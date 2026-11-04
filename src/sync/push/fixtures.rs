//! Test fixtures shared by the push submodules.

use std::collections::HashSet;

use rusqlite::Connection;

use crate::db::{attachments, queries, sync_queue};
use crate::db::attachments::Attachment;
use crate::sync::test_support::{exec, setup_labels_db, setup_move_db};
use super::outcome::Outcome;

pub(super) const PH: &str = "11111111-1111-4111-8111-111111111111";

pub(super) fn is_drop(o: &Outcome) -> bool {
    matches!(o, Outcome::DropAndLog(_))
}

pub(super) fn is_retry(o: &Outcome) -> bool {
    matches!(o, Outcome::LeavePending(_))
}

/// A project (remote id `project_remote`) in team t1 (remote 700) with one
/// top-level project comment "pc".
pub(super) fn project_comment_fixture(project_remote: Option<i64>) -> Connection {
    let conn = setup_labels_db();
    let remote = project_remote.map_or("NULL".to_string(), |r| r.to_string());
    exec(
        &conn,
        &format!(
            "INSERT INTO projects (id, remote_id, team_id, name, created_at, updated_at)
             VALUES ('p1', {remote}, 't1', 'P', 'x', 'x')"
        ),
    );
    let c = crate::db::models::Comment {
        id: "pc".into(),
        remote_id: None,
        issue_id: None,
        project_id: Some("p1".into()),
        author_id: None,
        body: "hello".into(),
        parent_id: None,
        created_at: "x".into(),
        updated_at: "x".into(),
    };
    queries::insert_comment(&conn, &c).unwrap();
    conn
}

pub(super) fn saved_view_conn() -> (Connection, String) {
    let conn = setup_labels_db();
    let sv = crate::db::saved_views::create_local(&conn, "t1", "Mine", r#"{"view_mode":"list"}"#)
        .unwrap();
    (conn, sv.id)
}

pub(super) fn saved_view_dto(id: i64) -> crate::api::saved_views::SavedViewDto {
    serde_json::from_value(serde_json::json!({
        "id": id, "team_id": 700, "name": "Mine",
        "query": {"view_mode": "list"}, "position": 3,
        "updated_at": "2026-10-31T10:00:00.000Z"
    }))
    .unwrap()
}

pub(super) fn none() -> HashSet<i64> {
    HashSet::new()
}

/// i2 (remote 2) moved locally t1 -> t2 and queued; returns the queued payload.
pub(super) fn moved_locally() -> (Connection, String) {
    let conn = setup_move_db();
    queries::move_issue_to_team(&conn, "i2", "t2").unwrap();
    assert!(queries::queue_issue_move(&conn, "i2", "t1").unwrap());
    let payload = sync_queue::pending_entries(&conn)
        .unwrap()
        .into_iter()
        .find(|e| e.action == "move")
        .and_then(|e| e.payload)
        .unwrap();
    (conn, payload)
}

// ── Comment files (JRST-206) ──

pub(super) fn staged_comment_file(conn: &Connection, comment: &str) -> Attachment {
    attachments::add_pending(
        conn,
        &attachments::NewPending {
            owner_kind: crate::db::attachments::OwnerKind::Comment,
            owner_id: comment.into(),
            filename: "log.txt".into(),
            content_type: "text/plain".into(),
            byte_size: 3,
            checksum: "abc==".into(),
            outbox_path: "/tmp/outbox/c".into(),
            kind: crate::files::validate::UploadKind::Attachment,
            placeholder_uuid: None,
        },
    )
    .unwrap()
}
