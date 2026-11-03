use rusqlite::{Connection, Result};
use std::path::PathBuf;

use super::migrations;

/// The directory holding the database. Test builds never reach the real one:
/// they use `MTASKS_TEST_DATA_DIR` when set (live checks), else a temp directory
/// for this test process.
#[cfg(test)]
fn data_dir() -> PathBuf {
    std::env::var_os("MTASKS_TEST_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("mtasks-test-{}", std::process::id())))
}

#[cfg(not(test))]
fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(crate::config::APP_DIR)
}

pub fn db_path() -> PathBuf {
    let data_dir = data_dir();
    std::fs::create_dir_all(&data_dir).expect("Failed to create data directory");
    data_dir.join("jait.db")
}

pub fn open() -> Result<Connection> {
    // Parallel tests share one fresh database, so migrate one at a time.
    #[cfg(test)]
    static MIGRATING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    #[cfg(test)]
    let _migrating = MIGRATING.lock().unwrap_or_else(|e| e.into_inner());

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

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn test_builds_never_use_the_real_data_directory() {
        let real = dirs::data_local_dir().unwrap().join(crate::config::APP_DIR);
        assert!(!db_path().starts_with(real));
    }
}
