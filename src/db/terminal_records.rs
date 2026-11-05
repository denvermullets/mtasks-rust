//! The Terminals in each team's strip, saved locally so a later launch can list
//! them again (JRST-414). A record is a name and a working directory: never a
//! process or scrollback. A local table only: nothing here is ever enqueued
//! for Data sync, and a team's records go with the team.
//!
//! `key` is the record's own id, stable across launches. It is not the
//! engine's `TerminalId`, which is new on every spawn and every restart.

use std::path::PathBuf;

use rusqlite::{params, Connection, Result};

/// One stored Terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    pub key: String,
    pub team_id: String,
    pub name: String,
    pub cwd: Option<PathBuf>,
}

/// A change to the strip, as `Roster` reports it and `apply` stores it.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Added(Stored),
    Renamed { key: String, name: String },
    Removed { key: String },
}

/// Store `changes` in order, in one transaction. Changes to a record that is
/// gone (its team was purged while the Terminal kept running) do nothing; an
/// add for a team that is gone is dropped.
pub fn apply(conn: &Connection, changes: &[Change]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for change in changes {
        match change {
            Change::Added(s) => {
                tx.execute(
                    "INSERT OR IGNORE INTO terminal_records (id, team_id, name, cwd, position)
                     SELECT ?1, id, ?3, ?4,
                            (SELECT COALESCE(MAX(position) + 1, 0) FROM terminal_records)
                     FROM teams WHERE id = ?2",
                    params![s.key, s.team_id, s.name, s.cwd.as_ref().map(|p| p.to_string_lossy())],
                )?;
            }
            Change::Renamed { key, name } => {
                tx.execute("UPDATE terminal_records SET name = ?2 WHERE id = ?1", params![key, name])?;
            }
            Change::Removed { key } => {
                tx.execute("DELETE FROM terminal_records WHERE id = ?1", params![key])?;
            }
        }
    }
    tx.commit()
}

/// Every stored Terminal, in the order they were added. Read at launch.
pub fn all(conn: &Connection) -> Result<Vec<Stored>> {
    let mut stmt = conn
        .prepare("SELECT id, team_id, name, cwd FROM terminal_records ORDER BY position, id")?;
    let rows = stmt.query_map([], |r| {
        Ok(Stored {
            key: r.get(0)?,
            team_id: r.get(1)?,
            name: r.get(2)?,
            cwd: r.get::<_, Option<String>>(3)?.map(PathBuf::from),
        })
    })?;
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

    fn stored(key: &str, team: &str, name: &str, cwd: Option<&str>) -> Stored {
        Stored { key: key.into(), team_id: team.into(), name: name.into(), cwd: cwd.map(PathBuf::from) }
    }

    #[test]
    fn records_round_trip_in_the_order_they_were_added() {
        let c = mem();
        team(&c, "t1");
        team(&c, "t2");
        let a = stored("k-b", "t1", "Terminal 1", Some("/work/api"));
        let b = stored("k-a", "t2", "Terminal 1", None);
        let d = stored("k-c", "t1", "Terminal 2", None);
        apply(&c, &[Change::Added(a.clone()), Change::Added(b.clone())]).unwrap();
        apply(&c, &[Change::Added(d.clone())]).unwrap();
        assert_eq!(all(&c).unwrap(), vec![a, b, d]);
    }

    #[test]
    fn renaming_and_closing_update_the_record() {
        let c = mem();
        team(&c, "t1");
        apply(
            &c,
            &[
                Change::Added(stored("k1", "t1", "Terminal 1", None)),
                Change::Added(stored("k2", "t1", "Terminal 2", None)),
                Change::Renamed { key: "k1".into(), name: "claude".into() },
                Change::Removed { key: "k2".into() },
            ],
        )
        .unwrap();
        assert_eq!(all(&c).unwrap(), vec![stored("k1", "t1", "claude", None)]);
    }

    #[test]
    fn a_change_to_a_record_that_is_gone_does_nothing() {
        let c = mem();
        apply(
            &c,
            &[
                Change::Added(stored("k1", "no-such-team", "Terminal 1", None)),
                Change::Renamed { key: "k1".into(), name: "x".into() },
                Change::Removed { key: "k1".into() },
            ],
        )
        .unwrap();
        assert!(all(&c).unwrap().is_empty());
    }

    #[test]
    fn deleting_a_team_removes_its_records_and_only_its_own() {
        let c = mem();
        team(&c, "gone");
        team(&c, "kept");
        apply(
            &c,
            &[
                Change::Added(stored("k1", "gone", "Terminal 1", None)),
                Change::Added(stored("k2", "kept", "Terminal 1", None)),
            ],
        )
        .unwrap();
        c.execute("DELETE FROM teams WHERE id = 'gone'", []).unwrap();
        assert_eq!(all(&c).unwrap(), vec![stored("k2", "kept", "Terminal 1", None)]);
    }

    #[test]
    fn storing_records_enqueues_nothing_for_data_sync() {
        let c = mem();
        team(&c, "t1");
        apply(
            &c,
            &[
                Change::Added(stored("k1", "t1", "Terminal 1", None)),
                Change::Renamed { key: "k1".into(), name: "x".into() },
                Change::Removed { key: "k1".into() },
            ],
        )
        .unwrap();
        assert_eq!(sync_queue::pending_count(&c).unwrap(), 0);
    }
}
