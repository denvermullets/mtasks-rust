use rusqlite::{params, Connection, Result};

use super::models::SyncQueueEntry;

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
