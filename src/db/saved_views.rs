//! Local mirror of the user's Saved views. Pure over a `rusqlite::Connection`:
//! no Iced, no HTTP. The Data sync feeds it decoded index rows.
//!
//! The pull cursor lives in `pull_cursors` under [`SCOPE`] / [`RESOURCE`], so it
//! is independent of the per-team resource cursors.
use rusqlite::{params, Connection, OptionalExtension, Result};
use serde::Deserialize;
use uuid::Uuid;

use super::queries::{find_team_by_remote_id, get_pull_cursor, set_pull_cursor};
use super::sync_queue;

/// `team_id` value used in `pull_cursors` (Saved views are fetched cross-team).
pub const SCOPE: &str = "user";
/// Cursor resource for the Saved-views index.
pub const RESOURCE: &str = "saved_views";
/// `sync_queue.entity_type` for Saved-view writes.
pub const ENTITY_TYPE: &str = "saved_view";

/// A mirrored Saved view.
#[allow(dead_code)] // some fields are read by the open-view ticket
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedView {
    /// Local uuid.
    pub id: String,
    pub remote_id: Option<i64>,
    /// Local team uuid.
    pub team_id: String,
    pub name: String,
    /// Raw Rails query map as JSON text, remote ids, unknown keys intact.
    pub query: String,
    pub position: i64,
    pub updated_at: String,
}

/// One row of `GET /api/v1/saved_views`.
#[derive(Debug, Clone, Deserialize)]
pub struct SavedViewIn {
    pub id: i64,
    pub team_id: i64,
    pub name: String,
    #[serde(default)]
    pub query: serde_json::Value,
    #[serde(default)]
    pub position: i64,
    pub updated_at: String,
}

/// What an apply did, for the sync log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub upserted: usize,
    /// Rows on teams that aren't mirrored locally.
    pub skipped_unmirrored: usize,
    /// Rows left alone because a local write is pending.
    pub skipped_pending: usize,
}

/// Upsert pulled rows by `remote_id` (idempotent, so cursor overlap is safe),
/// then advance the cursor to `server_time` if given.
pub fn apply_pulled(
    conn: &Connection,
    rows: &[SavedViewIn],
    server_time: Option<&str>,
) -> Result<Applied> {
    let mut applied = Applied::default();
    for row in rows {
        let Some(team) = find_team_by_remote_id(conn, row.team_id)? else {
            applied.skipped_unmirrored += 1;
            continue;
        };
        let existing = find_by_remote_id(conn, row.id)?;
        if let Some(local) = &existing {
            if sync_queue::has_pending_entry(conn, ENTITY_TYPE, &local.id)? {
                applied.skipped_pending += 1;
                continue;
            }
        }
        let id = existing
            .map(|v| v.id)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let query = if row.query.is_object() {
            row.query.to_string()
        } else {
            "{}".to_string()
        };
        conn.execute(
            "INSERT INTO saved_views (id, remote_id, team_id, name, query, position, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(remote_id) DO UPDATE SET
               team_id = excluded.team_id, name = excluded.name, query = excluded.query,
               position = excluded.position, updated_at = excluded.updated_at",
            params![id, row.id, team.id, row.name, query, row.position, row.updated_at],
        )?;
        applied.upserted += 1;
    }
    if let Some(t) = server_time {
        set_pull_cursor(conn, SCOPE, RESOURCE, t)?;
    }
    Ok(applied)
}

pub fn find_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<SavedView>> {
    conn.query_row(
        "SELECT id, remote_id, team_id, name, query, position, updated_at
         FROM saved_views WHERE remote_id = ?1",
        params![remote_id],
        from_row,
    )
    .optional()
}

/// A team's views in sidebar order (position, then name).
#[allow(dead_code)] // used by the sidebar ticket
pub fn list_for_team(conn: &Connection, team_id: &str) -> Result<Vec<SavedView>> {
    let mut stmt = conn.prepare(
        "SELECT id, remote_id, team_id, name, query, position, updated_at
         FROM saved_views WHERE team_id = ?1 ORDER BY position, name",
    )?;
    let rows = stmt.query_map(params![team_id], from_row)?;
    rows.collect()
}

/// Every team's views, grouped by local team id, each list in sidebar order.
/// Teams without views have no entry.
pub fn grouped_by_team(
    conn: &Connection,
) -> Result<std::collections::HashMap<String, Vec<SavedView>>> {
    let mut stmt = conn.prepare(
        "SELECT id, remote_id, team_id, name, query, position, updated_at
         FROM saved_views ORDER BY position, name",
    )?;
    let rows = stmt.query_map([], from_row)?;
    let mut map: std::collections::HashMap<String, Vec<SavedView>> = Default::default();
    for r in rows {
        let v = r?;
        map.entry(v.team_id.clone()).or_default().push(v);
    }
    Ok(map)
}

/// Stored Saved-views cursor (last applied `server_time`), if any.
pub fn cursor(conn: &Connection) -> Result<Option<String>> {
    get_pull_cursor(conn, SCOPE, RESOURCE)
}

fn from_row(r: &rusqlite::Row) -> Result<SavedView> {
    Ok(SavedView {
        id: r.get(0)?,
        remote_id: r.get(1)?,
        team_id: r.get(2)?,
        name: r.get(3)?,
        query: r.get(4)?,
        position: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES ('t1', 'T', 'T', 700, ?1, ?1)",
            params![now],
        )
        .unwrap();
        conn
    }

    #[test]
    fn grouped_by_team_orders_by_position_then_name_and_omits_empty_teams() {
        let conn = setup();
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES ('t2', 'U', 'U', 701, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        let mut rows = vec![
            view(1, 700, "Zeta", json!({})),
            view(2, 700, "Alpha", json!({})),
            view(3, 700, "First", json!({})),
        ];
        rows[2].position = 0;
        rows[0].position = 5;
        rows[1].position = 5;
        apply_pulled(&conn, &rows, None).unwrap();
        let g = grouped_by_team(&conn).unwrap();
        let names: Vec<&str> = g["t1"].iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["First", "Alpha", "Zeta"]);
        assert!(!g.contains_key("t2"));
    }

    fn view(id: i64, team: i64, name: &str, q: serde_json::Value) -> SavedViewIn {
        SavedViewIn {
            id,
            team_id: team,
            name: name.into(),
            query: q,
            position: id,
            updated_at: "2026-10-31T10:00:00Z".into(),
        }
    }

    fn stored_query(v: &SavedView) -> serde_json::Value {
        serde_json::from_str(&v.query).unwrap()
    }

    #[test]
    fn decodes_the_rails_row() {
        let v: SavedViewIn = serde_json::from_str(
            r#"{"id":5,"team_id":700,"name":"Mine","query":{"view_mode":"list"},
                "position":2,"created_at":"x","updated_at":"2026-10-31T10:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!((v.id, v.team_id, v.position), (5, 700, 2));
    }

    #[test]
    fn upserts_by_remote_id_and_is_idempotent() {
        let conn = setup();
        let q = json!({"view_mode":"list","lane_ids":"1,2"});
        let a = apply_pulled(&conn, &[view(5, 700, "Mine", q.clone())], Some("T1")).unwrap();
        assert_eq!(a.upserted, 1);
        let first = find_by_remote_id(&conn, 5).unwrap().unwrap();
        assert_eq!(
            (first.name.as_str(), first.team_id.as_str(), first.position),
            ("Mine", "t1", 5)
        );
        assert_eq!(stored_query(&first), q);

        // Re-delivery with a rename updates in place, keeping the local id.
        let mut renamed = view(5, 700, "Renamed", q);
        renamed.position = 9;
        apply_pulled(&conn, &[renamed], None).unwrap();
        let second = find_by_remote_id(&conn, 5).unwrap().unwrap();
        assert_eq!(second.id, first.id);
        assert_eq!((second.name.as_str(), second.position), ("Renamed", 9));
        assert_eq!(list_for_team(&conn, "t1").unwrap().len(), 1);
    }

    #[test]
    fn unknown_query_keys_are_stored_as_sent() {
        let conn = setup();
        let q = json!({"creator_ids":"3","future_key":"x","show_dependencies":"true"});
        apply_pulled(&conn, &[view(1, 700, "V", q.clone())], None).unwrap();
        let v = find_by_remote_id(&conn, 1).unwrap().unwrap();
        assert_eq!(stored_query(&v), q);
    }

    #[test]
    fn skips_views_on_unmirrored_teams() {
        let conn = setup();
        let rows = [view(1, 999, "Other", json!({})), view(2, 700, "Ok", json!({}))];
        let a = apply_pulled(&conn, &rows, None).unwrap();
        assert_eq!((a.upserted, a.skipped_unmirrored), (1, 1));
        assert!(find_by_remote_id(&conn, 1).unwrap().is_none());
    }

    #[test]
    fn cursor_is_its_own_and_advances_only_when_given() {
        let conn = setup();
        assert_eq!(cursor(&conn).unwrap(), None);
        apply_pulled(&conn, &[], Some("A")).unwrap();
        apply_pulled(&conn, &[], None).unwrap();
        assert_eq!(cursor(&conn).unwrap().as_deref(), Some("A"));
        assert_eq!(get_pull_cursor(&conn, "t1", "issues").unwrap(), None);
    }

    #[test]
    fn pending_local_write_is_not_overwritten() {
        let conn = setup();
        apply_pulled(&conn, &[view(1, 700, "Server", json!({}))], None).unwrap();
        let local = find_by_remote_id(&conn, 1).unwrap().unwrap();
        sync_queue::enqueue(&conn, ENTITY_TYPE, &local.id, "update", None).unwrap();
        let a = apply_pulled(&conn, &[view(1, 700, "Remote edit", json!({}))], None).unwrap();
        assert_eq!(a.skipped_pending, 1);
        assert_eq!(find_by_remote_id(&conn, 1).unwrap().unwrap().name, "Server");
    }

    #[test]
    fn lists_by_position_then_name() {
        let conn = setup();
        let mut b = view(2, 700, "B", json!({}));
        let mut a = view(1, 700, "A", json!({}));
        let mut c = view(3, 700, "A2", json!({}));
        b.position = 1;
        a.position = 2;
        c.position = 2;
        apply_pulled(&conn, &[a, b, c], None).unwrap();
        let names: Vec<_> = list_for_team(&conn, "t1")
            .unwrap()
            .into_iter()
            .map(|v| v.name)
            .collect();
        assert_eq!(names, ["B", "A", "A2"]);
    }
}
