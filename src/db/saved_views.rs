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
/// Cursor resource for the user-level tombstone feed (`GET /deleted`).
pub const DELETED_RESOURCE: &str = "deleted";
/// `record_type` of a Saved view in the tombstone feed.
const TOMBSTONE_TYPE: &str = "SavedView";
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

/// Delete mirrored views the user-level tombstone feed reports (idempotent, so
/// cursor overlap is safe). Other record types and unmirrored ids are ignored.
/// Advances the tombstone cursor to `server_time` if given; returns how many
/// views were removed.
pub fn apply_tombstones(
    conn: &Connection,
    rows: &[crate::api::deleted::TombstoneDto],
    server_time: Option<&str>,
) -> Result<usize> {
    let mut removed = 0;
    for row in rows.iter().filter(|r| r.record_type == TOMBSTONE_TYPE) {
        removed += conn.execute(
            "DELETE FROM saved_views WHERE remote_id = ?1",
            params![row.record_id],
        )?;
        super::home_page::clear_if_view(conn, row.record_id)?;
    }
    if let Some(t) = server_time {
        set_pull_cursor(conn, SCOPE, DELETED_RESOURCE, t)?;
    }
    Ok(removed)
}

/// Stored user-level tombstone cursor, if any.
pub fn deleted_cursor(conn: &Connection) -> Result<Option<String>> {
    get_pull_cursor(conn, SCOPE, DELETED_RESOURCE)
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

/// Remove a view locally and retire its pending queue entries (a never-pushed
/// create is simply dropped). Returns the `saved_view` delete payload
/// (`{"remote_id": n}`) when the view exists on the server, i.e. when a delete
/// must be queued; `None` otherwise.
pub fn delete_local(conn: &Connection, view_id: &str) -> Result<Option<String>> {
    let remote_id: Option<Option<i64>> = conn
        .query_row("SELECT remote_id FROM saved_views WHERE id = ?1", params![view_id], |r| r.get(0))
        .optional()?;
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM saved_views WHERE id = ?1", params![view_id])?;
    tx.execute(
        "UPDATE sync_queue SET synced_at = ?3
         WHERE entity_type = ?1 AND entity_id = ?2 AND synced_at IS NULL",
        params![ENTITY_TYPE, view_id, crate::db::now_iso()],
    )?;
    tx.commit()?;
    super::home_page::clear_if_local_view(conn, view_id)?;
    if let Some(Some(id)) = remote_id {
        super::home_page::clear_if_view(conn, id)?;
    }
    Ok(remote_id
        .flatten()
        .map(|id| serde_json::json!({ "remote_id": id }).to_string()))
}

/// Add a new view to `team_id` (local uuid), last in the sidebar, and queue
/// its `saved_view` create. `query` is the encoded Rails query as JSON text.
/// Position mirrors Rails (highest + 1) until the push adopts the server's.
pub fn create_local(conn: &Connection, team_id: &str, name: &str, query: &str) -> Result<SavedView> {
    let tx = conn.unchecked_transaction()?;
    let position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position), 0) + 1 FROM saved_views",
        [],
        |r| r.get(0),
    )?;
    let view = SavedView {
        id: Uuid::new_v4().to_string(),
        remote_id: None,
        team_id: team_id.to_string(),
        name: name.to_string(),
        query: query.to_string(),
        position,
        updated_at: crate::db::now_iso(),
    };
    tx.execute(
        "INSERT INTO saved_views (id, remote_id, team_id, name, query, position, updated_at)
         VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6)",
        params![view.id, view.team_id, view.name, view.query, view.position, view.updated_at],
    )?;
    sync_queue::enqueue(&tx, ENTITY_TYPE, &view.id, "create", None)?;
    tx.commit()?;
    Ok(view)
}

/// Link a pushed view to the server's row: remote id, position, query and
/// timestamp come from the create response.
pub fn adopt_created(conn: &Connection, view_id: &str, dto: &SavedViewIn) -> Result<()> {
    let query = if dto.query.is_object() {
        dto.query.to_string()
    } else {
        "{}".to_string()
    };
    // A pull may already have mirrored the pushed row as a separate view.
    conn.execute(
        "DELETE FROM saved_views WHERE remote_id = ?2 AND id <> ?1",
        params![view_id, dto.id],
    )?;
    conn.execute(
        "UPDATE saved_views SET remote_id = ?2, position = ?3, query = ?4, updated_at = ?5
         WHERE id = ?1",
        params![view_id, dto.id, dto.position, query, dto.updated_at],
    )?;
    super::home_page::adopt_view(conn, view_id, dto.id)?;
    Ok(())
}

/// Store a new definition for a view locally, as JSON object text (remote
/// ids, unknown keys intact). The queue entry carries no payload: the push
/// re-reads the row so the newest query goes out however many updates were
/// queued. Returns whether the view exists.
pub fn update_query_local(
    conn: &Connection,
    view_id: &str,
    query: &std::collections::BTreeMap<String, String>,
) -> Result<bool> {
    let json = serde_json::to_string(query).unwrap_or_else(|_| "{}".into());
    let n = conn.execute(
        "UPDATE saved_views SET query = ?2, updated_at = ?3 WHERE id = ?1",
        params![view_id, json, crate::db::now_iso()],
    )?;
    Ok(n > 0)
}

/// A view by local id.
pub fn get(conn: &Connection, view_id: &str) -> Result<Option<SavedView>> {
    conn.query_row(
        "SELECT id, remote_id, team_id, name, query, position, updated_at
         FROM saved_views WHERE id = ?1",
        params![view_id],
        from_row,
    )
    .optional()
}

/// Adopt the server's returned query after a successful update push, but only
/// if the row still holds `pushed` (a newer local edit wins and is pushed by
/// its own entry). Returns whether the row changed.
pub fn adopt_server_query(
    conn: &Connection,
    view_id: &str,
    pushed: &str,
    server: &serde_json::Value,
) -> Result<bool> {
    if !server.is_object() {
        return Ok(false);
    }
    let n = conn.execute(
        "UPDATE saved_views SET query = ?3 WHERE id = ?1 AND query = ?2",
        params![view_id, pushed, server.to_string()],
    )?;
    Ok(n > 0)
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
    fn update_query_local_stores_the_encoded_query_and_keeps_unknown_keys() {
        let conn = setup();
        apply_pulled(&conn, &[view(5, 700, "A", json!({"view_mode":"list"}))], None).unwrap();
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;
        let mut q = std::collections::BTreeMap::new();
        q.insert("view_mode".to_string(), "board".to_string());
        q.insert("future_key".to_string(), "x".to_string());
        assert!(update_query_local(&conn, &id, &q).unwrap());
        let v = get(&conn, &id).unwrap().unwrap();
        assert_eq!(stored_query(&v), json!({"view_mode":"board","future_key":"x"}));
        assert!(!update_query_local(&conn, "nope", &q).unwrap());
    }

    #[test]
    fn adopt_server_query_applies_only_when_the_row_is_unchanged() {
        let conn = setup();
        apply_pulled(&conn, &[view(5, 700, "A", json!({"a":"1"}))], None).unwrap();
        let v = find_by_remote_id(&conn, 5).unwrap().unwrap();
        let server = json!({"a":"1","b":"2"});
        assert!(adopt_server_query(&conn, &v.id, &v.query, &server).unwrap());
        assert_eq!(stored_query(&get(&conn, &v.id).unwrap().unwrap()), server);
        // Row moved on since the push: leave it.
        assert!(!adopt_server_query(&conn, &v.id, &v.query, &json!({"z":"9"})).unwrap());
        assert_eq!(stored_query(&get(&conn, &v.id).unwrap().unwrap()), server);
        // Non-object responses are ignored.
        let cur = get(&conn, &v.id).unwrap().unwrap().query;
        assert!(!adopt_server_query(&conn, &v.id, &cur, &json!(null)).unwrap());
    }

    #[test]
    fn pending_update_blocks_a_pull_until_it_drains() {
        let conn = setup();
        apply_pulled(&conn, &[view(5, 700, "A", json!({"a":"1"}))], None).unwrap();
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;
        let mut q = std::collections::BTreeMap::new();
        q.insert("a".to_string(), "2".to_string());
        update_query_local(&conn, &id, &q).unwrap();
        sync_queue::enqueue(&conn, ENTITY_TYPE, &id, "update", None).unwrap();
        let e = sync_queue::pending_entries(&conn).unwrap()[0].id.unwrap();
        let a = apply_pulled(&conn, &[view(5, 700, "A", json!({"a":"server"}))], None).unwrap();
        assert_eq!(a.skipped_pending, 1);
        assert_eq!(stored_query(&get(&conn, &id).unwrap().unwrap()), json!({"a":"2"}));
        sync_queue::mark_synced(&conn, e).unwrap();
        apply_pulled(&conn, &[view(5, 700, "A", json!({"a":"server"}))], None).unwrap();
        assert_eq!(stored_query(&get(&conn, &id).unwrap().unwrap()), json!({"a":"server"}));
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

    #[test]
    fn delete_local_removes_the_view_and_returns_a_delete_payload() {
        let conn = setup();
        apply_pulled(&conn, &[view(5, 700, "A", json!({})), view(6, 700, "B", json!({}))], None).unwrap();
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;
        let payload = delete_local(&conn, &id).unwrap();
        assert_eq!(payload.as_deref(), Some(r#"{"remote_id":5}"#));
        assert!(find_by_remote_id(&conn, 5).unwrap().is_none());
        assert_eq!(list_for_team(&conn, "t1").unwrap().len(), 1);
    }

    #[test]
    fn delete_local_of_a_never_pushed_view_queues_nothing_and_drops_its_create() {
        let conn = setup();
        conn.execute(
            "INSERT INTO saved_views (id, remote_id, team_id, name, query, position, updated_at)
             VALUES ('loc', NULL, 't1', 'New', '{}', 0, '2026-10-31T10:00:00Z')",
            [],
        )
        .unwrap();
        sync_queue::enqueue(&conn, ENTITY_TYPE, "loc", "create", None).unwrap();
        assert_eq!(delete_local(&conn, "loc").unwrap(), None);
        assert!(sync_queue::pending_entries(&conn).unwrap().is_empty());
        assert!(list_for_team(&conn, "t1").unwrap().is_empty());
    }

    fn tomb(kind: &str, id: i64) -> crate::api::deleted::TombstoneDto {
        crate::api::deleted::TombstoneDto {
            record_type: kind.into(),
            record_id: id,
            deleted_at: None,
        }
    }

    #[test]
    fn tombstone_removes_the_matching_view_only() {
        let conn = setup();
        apply_pulled(&conn, &[view(1, 700, "A", json!({})), view(2, 700, "B", json!({}))], None).unwrap();
        let n = apply_tombstones(&conn, &[tomb("SavedView", 1)], Some("T1")).unwrap();
        assert_eq!(n, 1);
        assert!(find_by_remote_id(&conn, 1).unwrap().is_none());
        assert!(find_by_remote_id(&conn, 2).unwrap().is_some());
    }

    #[test]
    fn tombstone_for_an_unmirrored_view_is_a_noop() {
        let conn = setup();
        apply_pulled(&conn, &[view(2, 700, "B", json!({}))], None).unwrap();
        assert_eq!(apply_tombstones(&conn, &[tomb("SavedView", 99)], None).unwrap(), 0);
        assert_eq!(list_for_team(&conn, "t1").unwrap().len(), 1);
    }

    #[test]
    fn other_record_types_are_ignored() {
        let conn = setup();
        apply_pulled(&conn, &[view(1, 700, "A", json!({}))], None).unwrap();
        let rows = [tomb("Dashboard", 1), tomb("DashboardGroup", 1)];
        assert_eq!(apply_tombstones(&conn, &rows, None).unwrap(), 0);
        assert!(find_by_remote_id(&conn, 1).unwrap().is_some());
    }

    #[test]
    fn tombstone_cursor_is_its_own_and_advances_only_when_given() {
        let conn = setup();
        assert_eq!(deleted_cursor(&conn).unwrap(), None);
        apply_tombstones(&conn, &[], Some("D1")).unwrap();
        apply_tombstones(&conn, &[], None).unwrap();
        assert_eq!(deleted_cursor(&conn).unwrap().as_deref(), Some("D1"));
        assert_eq!(cursor(&conn).unwrap(), None);
    }

    #[test]
    fn create_local_adds_a_view_after_the_others_and_queues_a_create() {
        let conn = setup();
        apply_pulled(&conn, &[view(4, 700, "Old", json!({}))], None).unwrap();
        let q = serde_json::json!({"view_mode": "board"}).to_string();
        let sv = create_local(&conn, "t1", "Mine", &q).unwrap();
        assert_eq!((sv.remote_id, sv.name.as_str(), sv.team_id.as_str()), (None, "Mine", "t1"));
        assert_eq!(sv.position, 5);
        assert_eq!(sv.query, q);
        let names: Vec<_> = list_for_team(&conn, "t1").unwrap().into_iter().map(|v| v.name).collect();
        assert_eq!(names, ["Old", "Mine"]);
        let pending = sync_queue::pending_entries(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            (pending[0].entity_type.as_str(), pending[0].action.as_str(), pending[0].entity_id.as_str()),
            ("saved_view", "create", sv.id.as_str())
        );
    }

    #[test]
    fn adopt_created_takes_id_position_and_query_from_the_server() {
        let conn = setup();
        let sv = create_local(&conn, "t1", "Mine", "{\"a\":\"1\"}").unwrap();
        let mut dto = view(77, 700, "Mine", json!({"a": "1", "view_mode": "list"}));
        dto.position = 9;
        adopt_created(&conn, &sv.id, &dto).unwrap();
        let got = find_by_remote_id(&conn, 77).unwrap().unwrap();
        assert_eq!((got.id.as_str(), got.position), (sv.id.as_str(), 9));
        assert_eq!(stored_query(&got), json!({"a": "1", "view_mode": "list"}));
        assert_eq!(got.updated_at, "2026-10-31T10:00:00Z");
    }

    fn home_is(conn: &Connection, kind: &str, id: i64) {
        crate::db::home_page::apply_me(conn, Some(&crate::api::HomePageDto { kind: kind.into(), id })).unwrap();
    }

    #[test]
    fn deleting_the_home_view_locally_clears_home_without_queuing_a_home_write() {
        let conn = setup();
        apply_pulled(&conn, &[view(5, 700, "A", json!({}))], None).unwrap();
        home_is(&conn, "view", 5);
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;
        delete_local(&conn, &id).unwrap();
        assert_eq!(crate::db::home_page::get(&conn).unwrap(), None);
        assert!(sync_queue::pending_entries(&conn).unwrap().iter().all(|e| e.entity_type != "home_page"));
    }

    #[test]
    fn deleting_another_view_keeps_home() {
        let conn = setup();
        apply_pulled(&conn, &[view(5, 700, "A", json!({})), view(6, 700, "B", json!({}))], None).unwrap();
        home_is(&conn, "view", 5);
        let id = find_by_remote_id(&conn, 6).unwrap().unwrap().id;
        delete_local(&conn, &id).unwrap();
        assert!(crate::db::home_page::get(&conn).unwrap().is_some());
    }

    #[test]
    fn a_team_home_with_the_same_id_survives_a_view_delete() {
        let conn = setup();
        apply_pulled(&conn, &[view(5, 700, "A", json!({}))], None).unwrap();
        home_is(&conn, "team", 5);
        let id = find_by_remote_id(&conn, 5).unwrap().unwrap().id;
        delete_local(&conn, &id).unwrap();
        apply_tombstones(&conn, &[tomb("SavedView", 5)], None).unwrap();
        assert!(crate::db::home_page::get(&conn).unwrap().is_some());
    }

    #[test]
    fn a_tombstone_for_the_home_view_clears_home() {
        let conn = setup();
        apply_pulled(&conn, &[view(5, 700, "A", json!({}))], None).unwrap();
        home_is(&conn, "view", 5);
        apply_tombstones(&conn, &[tomb("SavedView", 5)], None).unwrap();
        assert_eq!(crate::db::home_page::get(&conn).unwrap(), None);
        assert!(sync_queue::pending_entries(&conn).unwrap().is_empty());
    }
}
