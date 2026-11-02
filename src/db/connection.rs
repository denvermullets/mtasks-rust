use rusqlite::{Connection, Result};
use std::path::PathBuf;

use super::migrations;

pub fn db_path() -> PathBuf {
    // Test builds only: live checks point the engine's `open()` at a scratch dir.
    #[cfg(test)]
    if let Ok(dir) = std::env::var("MTASKS_TEST_DATA_DIR") {
        std::fs::create_dir_all(&dir).expect("Failed to create test data directory");
        return PathBuf::from(dir).join("jait.db");
    }
    let data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(crate::config::APP_DIR);
    std::fs::create_dir_all(&data_dir).expect("Failed to create data directory");
    data_dir.join("jait.db")
}

pub fn open() -> Result<Connection> {
    let path = db_path();
    let conn = Connection::open(&path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    migrations::run(&conn)?;
    Ok(conn)
}

pub fn seed_default_data(conn: &Connection) -> Result<()> {
    let count: i32 = conn.query_row("SELECT COUNT(*) FROM teams", [], |row| row.get(0))?;
    if count > 0 {
        return Ok(());
    }

    let now = crate::db::now_iso();
    let team_id = uuid::Uuid::new_v4().to_string();

    conn.execute(
        "INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![team_id, "My Project", "PROJ", &now, &now],
    )?;

    let lanes = [
        ("Backlog", "#6B7280", 0),
        ("In Progress", "#3B82F6", 1),
        ("Done", "#10B981", 2),
        ("Canceled", "#EF4444", 3),
    ];

    for (name, color, position) in &lanes {
        let lane_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO lanes (id, name, color, position, team_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![lane_id, name, color, position, team_id, &now, &now],
        )?;
    }

    Ok(())
}
