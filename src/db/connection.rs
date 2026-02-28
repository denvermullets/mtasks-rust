use rusqlite::{Connection, Result};
use std::path::PathBuf;

pub fn db_path() -> PathBuf {
    let data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mtasks-rusty");
    std::fs::create_dir_all(&data_dir).expect("Failed to create data directory");
    data_dir.join("jait.db")
}

pub fn open() -> Result<Connection> {
    let path = db_path();
    let conn = Connection::open(&path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    run_migrations(&conn)?;
    Ok(conn)
}

fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS teams (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            identifier TEXT NOT NULL,
            remote_id INTEGER,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS lanes (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            color TEXT,
            position INTEGER NOT NULL,
            team_id TEXT NOT NULL REFERENCES teams(id),
            remote_id INTEGER,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS issues (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            description TEXT,
            priority INTEGER NOT NULL DEFAULT 4,
            team_number INTEGER,
            due_date TEXT,
            estimate REAL,
            started_at TEXT,
            completed_at TEXT,
            canceled_at TEXT,
            lane_id TEXT NOT NULL REFERENCES lanes(id),
            team_id TEXT NOT NULL REFERENCES teams(id),
            remote_id INTEGER,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sync_queue (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            entity_type TEXT NOT NULL,
            entity_id TEXT NOT NULL,
            action TEXT NOT NULL,
            payload TEXT,
            created_at TEXT NOT NULL,
            synced_at TEXT
        );
        ",
    )?;
    Ok(())
}

pub fn seed_default_data(conn: &Connection) -> Result<()> {
    let count: i32 = conn.query_row("SELECT COUNT(*) FROM teams", [], |row| row.get(0))?;
    if count > 0 {
        return Ok(());
    }

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
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
