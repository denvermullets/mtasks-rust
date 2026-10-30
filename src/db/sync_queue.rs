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
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    conn.execute(
        "INSERT INTO sync_queue (entity_type, entity_id, action, payload, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![entity_type, entity_id, action, payload, now],
    )?;
    LOCAL_WRITE.store(true, Ordering::SeqCst);
    Ok(())
}

pub fn pending_entries(conn: &Connection) -> Result<Vec<SyncQueueEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, entity_type, entity_id, action, payload, created_at, synced_at FROM sync_queue WHERE synced_at IS NULL ORDER BY created_at",
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
        let now = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        conn.execute(
            "UPDATE sync_queue SET attempted_at = ?1 WHERE id = ?2",
            params![now, entry_id],
        )?;
    }
    Ok(previous)
}

pub fn mark_synced(conn: &Connection, entry_id: i64) -> Result<()> {
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    conn.execute(
        "UPDATE sync_queue SET synced_at = ?1 WHERE id = ?2",
        params![now, entry_id],
    )?;
    Ok(())
}
