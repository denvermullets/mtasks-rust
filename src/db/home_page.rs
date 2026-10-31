//! The user's Home page as last reported by the server, kept locally as
//! `{kind, remote id}` so a launch can resolve it even offline.
//!
//! Stored in `pull_cursors` (scope `user`, resource `home_page`) as
//! `"<kind>:<id>"`; no row means no Home page.

use rusqlite::{params, Connection, Result};

use super::queries::{clear_pull_cursor, get_pull_cursor, set_pull_cursor};
use crate::api::HomePageDto;

const SCOPE: &str = "user";
const RESOURCE: &str = "home_page";

/// Queue `entity_type` for a locally-set Home page awaiting push.
pub const QUEUE_ENTITY: &str = "home_page";

/// A stored Home page target. `kind` is `team`, `view` or `dashboard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomePage {
    pub kind: String,
    pub remote_id: i64,
}

/// The stored Home page, if any.
pub fn get(conn: &Connection) -> Result<Option<HomePage>> {
    Ok(get_pull_cursor(conn, SCOPE, RESOURCE)?.and_then(|raw| {
        let (kind, id) = raw.split_once(':')?;
        Some(HomePage { kind: kind.to_string(), remote_id: id.parse().ok()? })
    }))
}

/// True while a `home_page` write is queued and not yet pushed. The local
/// choice then wins over what the server reports.
fn write_pending(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_queue WHERE entity_type = ?1 AND synced_at IS NULL)",
        params![QUEUE_ENTITY],
        |r| r.get(0),
    )
}

/// Set the Home page locally and queue the write. The stored home changes at
/// once (offline too); while the write is pending `/me` cannot overwrite it.
pub fn set_local(conn: &Connection, kind: &str, remote_id: i64) -> Result<()> {
    set_pull_cursor(conn, SCOPE, RESOURCE, &format!("{kind}:{remote_id}"))?;
    let payload = serde_json::json!({ "kind": kind, "id": remote_id }).to_string();
    super::sync_queue::enqueue(conn, QUEUE_ENTITY, "home", "update", Some(&payload))
}

/// `(kind, id)` from a queued `home_page` payload.
pub fn parse_payload(payload: Option<&str>) -> Option<(String, i64)> {
    let v: serde_json::Value = serde_json::from_str(payload?).ok()?;
    Some((v.get("kind")?.as_str()?.to_string(), v.get("id")?.as_i64()?))
}

/// Store what `/me` reported: a value replaces the stored home, `None` clears
/// it. Skipped while a local `home_page` write is pending.
pub fn apply_me(conn: &Connection, reported: Option<&HomePageDto>) -> Result<()> {
    if write_pending(conn)? {
        return Ok(());
    }
    match reported {
        Some(h) => set_pull_cursor(conn, SCOPE, RESOURCE, &format!("{}:{}", h.kind, h.id)),
        None => clear_pull_cursor(conn, SCOPE, RESOURCE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    fn dto(kind: &str, id: i64) -> HomePageDto {
        HomePageDto { kind: kind.into(), id }
    }

    fn queue_home_write(conn: &Connection, synced: bool) {
        conn.execute(
            "INSERT INTO sync_queue (entity_type, entity_id, action, payload, created_at, synced_at)
             VALUES ('home_page', 'home', 'update', NULL, '2026-10-31T00:00:00.000Z', ?1)",
            params![synced.then_some("2026-10-31T00:00:01.000Z")],
        )
        .unwrap();
    }

    #[test]
    fn set_local_stores_at_once_and_queues_the_put() {
        let conn = mem();
        set_local(&conn, "team", 7).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "team".into(), remote_id: 7 }));
        let pending = crate::db::sync_queue::pending_entries(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!((pending[0].entity_type.as_str(), pending[0].action.as_str()), ("home_page", "update"));
        assert_eq!(parse_payload(pending[0].payload.as_deref()), Some(("team".into(), 7)));
    }

    #[test]
    fn a_pending_set_is_not_overwritten_by_me() {
        let conn = mem();
        apply_me(&conn, Some(&dto("team", 1))).unwrap();
        set_local(&conn, "team", 2).unwrap();
        apply_me(&conn, Some(&dto("team", 1))).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "team".into(), remote_id: 2 }));
    }

    #[test]
    fn once_pushed_me_restores_the_server_value() {
        // A 422 drops the entry (marked synced); the next /me restores.
        let conn = mem();
        set_local(&conn, "team", 2).unwrap();
        let id = crate::db::sync_queue::pending_entries(&conn).unwrap()[0].id.unwrap();
        crate::db::sync_queue::mark_synced(&conn, id).unwrap();
        apply_me(&conn, Some(&dto("view", 5))).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "view".into(), remote_id: 5 }));
    }

    #[test]
    fn bad_payloads_do_not_parse() {
        for bad in [None, Some("nope"), Some(r#"{"kind":"team"}"#)] {
            assert_eq!(parse_payload(bad), None);
        }
    }

    #[test]
    fn nothing_stored_until_a_sync_reports_one() {
        assert_eq!(get(&mem()).unwrap(), None);
    }

    #[test]
    fn stores_kind_and_remote_id() {
        let conn = mem();
        apply_me(&conn, Some(&dto("view", 42))).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "view".into(), remote_id: 42 }));
    }

    #[test]
    fn a_later_sync_replaces_the_stored_home() {
        let conn = mem();
        apply_me(&conn, Some(&dto("team", 1))).unwrap();
        apply_me(&conn, Some(&dto("dashboard", 9))).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "dashboard".into(), remote_id: 9 }));
    }

    #[test]
    fn null_clears_the_stored_home() {
        let conn = mem();
        apply_me(&conn, Some(&dto("team", 1))).unwrap();
        apply_me(&conn, None).unwrap();
        assert_eq!(get(&conn).unwrap(), None);
    }

    #[test]
    fn pending_local_write_is_not_overwritten_or_cleared() {
        let conn = mem();
        apply_me(&conn, Some(&dto("team", 1))).unwrap();
        queue_home_write(&conn, false);
        apply_me(&conn, Some(&dto("view", 2))).unwrap();
        apply_me(&conn, None).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "team".into(), remote_id: 1 }));
    }

    #[test]
    fn a_pushed_write_no_longer_blocks_the_server_value() {
        let conn = mem();
        queue_home_write(&conn, true);
        apply_me(&conn, Some(&dto("view", 2))).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "view".into(), remote_id: 2 }));
    }
}
