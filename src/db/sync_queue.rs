use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{params, Connection, Result};

use super::models::SyncQueueEntry;

static LOCAL_WRITE: AtomicBool = AtomicBool::new(false);

/// True once if any entry was enqueued since the last call. The app polls this
/// after each update to send the scheduler a local-write event.
pub fn take_local_write() -> bool {
    LOCAL_WRITE.swap(false, Ordering::SeqCst)
}

pub fn enqueue(
    conn: &Connection,
    entity_type: &str,
    entity_id: &str,
    action: &str,
    payload: Option<&str>,
) -> Result<()> {
    enqueue_returning_id(conn, entity_type, entity_id, action, payload).map(|_| ())
}

/// Like [`enqueue`], but returns the new queue entry's id.
pub fn enqueue_returning_id(
    conn: &Connection,
    entity_type: &str,
    entity_id: &str,
    action: &str,
    payload: Option<&str>,
) -> Result<i64> {
    let now = crate::db::now_iso();
    conn.execute(
        "INSERT INTO sync_queue (entity_type, entity_id, action, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![entity_type, entity_id, action, payload, now],
    )?;
    LOCAL_WRITE.store(true, Ordering::SeqCst);
    Ok(conn.last_insert_rowid())
}

pub fn pending_entries(conn: &Connection) -> Result<Vec<SyncQueueEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, entity_type, entity_id, action, payload, created_at, synced_at FROM sync_queue WHERE synced_at IS NULL ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(SyncQueueEntry {
            id: row.get(0)?,
            entity_type: row.get(1)?,
            entity_id: row.get(2)?,
            action: row.get(3)?,
            payload: row.get(4)?,
            created_at: row.get(5)?,
            synced_at: row.get(6)?,
        })
    })?;
    rows.collect()
}

pub fn pending_count(conn: &Connection) -> Result<i32> {
    conn.query_row(
        "SELECT COUNT(*) FROM sync_queue WHERE synced_at IS NULL",
        [],
        |row| row.get(0),
    )
}

pub fn has_pending_entry(conn: &Connection, entity_type: &str, entity_id: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sync_queue
         WHERE entity_type = ?1 AND entity_id = ?2 AND synced_at IS NULL",
        params![entity_type, entity_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Record that a create's POST is about to be sent. Returns the timestamp of
/// the *earliest* earlier attempt (None on the first attempt). The stored value
/// is never moved forward, so the reconciliation window on a later retry still
/// covers every earlier attempt.
pub fn begin_attempt(conn: &Connection, entry_id: i64) -> Result<Option<String>> {
    let previous: Option<String> = conn.query_row(
        "SELECT attempted_at FROM sync_queue WHERE id = ?1",
        params![entry_id],
        |row| row.get(0),
    )?;
    if previous.is_none() {
        let now = crate::db::now_iso();
        conn.execute(
            "UPDATE sync_queue SET attempted_at = ?1 WHERE id = ?2",
            params![now, entry_id],
        )?;
    }
    Ok(previous)
}

/// Retire an entry that will never be pushed (dropped or rejected): marks it
/// done and removes its Pending changes, atomically.
pub fn discard(conn: &Connection, entry_id: i64) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    crate::db::activity::discard_pending_for_entry(&tx, entry_id)?;
    mark_synced(&tx, entry_id)?;
    tx.commit()
}

pub fn mark_synced(conn: &Connection, entry_id: i64) -> Result<()> {
    let now = crate::db::now_iso();
    conn.execute(
        "UPDATE sync_queue SET synced_at = ?1 WHERE id = ?2",
        params![now, entry_id],
    )?;
    Ok(())
}

/// The Rails id of the team a queue entry belongs to, or `None` for user-level
/// entries and for ones whose row or payload no longer says.
pub fn entry_team_remote_id(conn: &Connection, entry: &SyncQueueEntry) -> Option<i64> {
    let team_remote = |team_id: &str| {
        crate::db::queries::remote_id_for(conn, "teams", team_id)
            .ok()
            .flatten()
    };
    let id = entry.entity_id.as_str();
    match (entry.entity_type.as_str(), entry.action.as_str()) {
        (_, "delete") => {
            let v: serde_json::Value = serde_json::from_str(entry.payload.as_deref()?).ok()?;
            v.get("team_remote_id")?.as_i64()
        }
        ("issue", _) => team_remote(&crate::db::queries::get_issue(conn, id).ok()??.team_id),
        ("project", _) => team_remote(&crate::db::queries::get_project(conn, id).ok()??.team_id),
        ("label", _) => team_remote(&crate::db::queries::get_label(conn, id).ok()??.team_id),
        ("recurring_issue", _) => {
            team_remote(&crate::db::recurring_issues::get(conn, id).ok()??.team_id)
        }
        ("saved_view", _) => {
            let team_id: String = conn
                .query_row("SELECT team_id FROM saved_views WHERE id = ?1", [id], |r| r.get(0))
                .ok()?;
            team_remote(&team_id)
        }
        (crate::db::attachments::UPLOAD_ENTITY, _) | (crate::db::attachments::ATTACHMENT_ENTITY, _) => {
            let a = crate::db::attachments::get(conn, id).ok()??;
            team_remote(&crate::db::attachments::owner_ref(conn, &a).ok()??.team_id)
        }
        (crate::db::attachments::COMMENT_ENTITY, _) => {
            let c = crate::db::queries::get_comment(conn, id).ok()??;
            let team_id = match (&c.issue_id, &c.project_id) {
                (Some(i), _) => crate::db::queries::get_issue(conn, i).ok()??.team_id,
                (None, Some(p)) => crate::db::queries::get_project(conn, p).ok()??.team_id,
                (None, None) => return None,
            };
            team_remote(&team_id)
        }
        ("dependency", _) => {
            let d = crate::db::queries::get_issue_dependency(conn, id).ok()??;
            team_remote(
                &crate::db::queries::get_issue(conn, &d.blocking_issue_id)
                    .ok()??
                    .team_id,
            )
        }
        _ => None,
    }
}

