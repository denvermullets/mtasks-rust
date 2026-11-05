//! The optional working directory each team's new Terminals start in. A local
//! table only: nothing here is ever enqueued for Data sync. A directory that
//! no longer exists is handled where a shell starts (it falls back to home),
//! not here, so the stored path survives a drive being unplugged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, Result};

/// Remember `dir` for `team_id`, replacing any earlier one.
pub fn set(conn: &Connection, team_id: &str, dir: &Path) -> Result<()> {
    conn.execute(
        "INSERT INTO team_terminal_dirs (team_id, path) VALUES (?1, ?2)
         ON CONFLICT(team_id) DO UPDATE SET path = excluded.path",
        params![team_id, dir.to_string_lossy()],
    )?;
    Ok(())
}

/// Forget the team's directory; its new shells start in home again.
pub fn clear(conn: &Connection, team_id: &str) -> Result<()> {
    conn.execute("DELETE FROM team_terminal_dirs WHERE team_id = ?1", params![team_id])?;
    Ok(())
}

/// Every team's directory, by team id.
pub fn all(conn: &Connection) -> Result<HashMap<String, PathBuf>> {
    let mut stmt = conn.prepare("SELECT team_id, path FROM team_terminal_dirs")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, PathBuf::from(r.get::<_, String>(1)?))))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{migrations, sync_queue};

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    fn team(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at)
             VALUES (?1, ?1, ?1, 'x', 'x')",
            params![id],
        )
        .unwrap();
    }

    #[test]
    fn a_directory_round_trips_and_setting_again_replaces_it() {
        let c = mem();
        team(&c, "t1");
        assert!(all(&c).unwrap().is_empty());
        set(&c, "t1", Path::new("/work/api")).unwrap();
        set(&c, "t1", Path::new("/work/web")).unwrap();
        assert_eq!(all(&c).unwrap().get("t1"), Some(&PathBuf::from("/work/web")));
        clear(&c, "t1").unwrap();
        assert!(all(&c).unwrap().is_empty());
    }

    #[test]
    fn storing_a_directory_enqueues_nothing_for_data_sync() {
        let c = mem();
        team(&c, "t1");
        set(&c, "t1", Path::new("/work/api")).unwrap();
        assert_eq!(sync_queue::pending_count(&c).unwrap(), 0);
    }
}
