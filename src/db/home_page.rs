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
    /// Set instead of `remote_id` (then 0) for a Saved view chosen before its
    /// create has pushed; `adopt_view` swaps it for the remote id.
    pub local_id: Option<String>,
}

/// The stored Home page, if any.
pub fn get(conn: &Connection) -> Result<Option<HomePage>> {
    Ok(get_pull_cursor(conn, SCOPE, RESOURCE)?.and_then(|raw| {
        let (kind, id) = raw.split_once(':')?;
        if let Some(local) = id.strip_prefix('~') {
            return Some(HomePage { kind: kind.to_string(), remote_id: 0, local_id: Some(local.to_string()) });
        }
        Some(HomePage { kind: kind.to_string(), remote_id: id.parse().ok()?, local_id: None })
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

/// Set a Saved view (local uuid) as the Home page. A view with a remote id is
/// an ordinary `set_local`. One not pushed yet is stored by its local id and
/// queued with `local_id`; the engine resolves it once the create (earlier in
/// the FIFO queue) has pushed.
pub fn set_view_local(conn: &Connection, view_id: &str, remote_id: Option<i64>) -> Result<()> {
    if let Some(rid) = remote_id {
        return set_local(conn, "view", rid);
    }
    set_pull_cursor(conn, SCOPE, RESOURCE, &format!("view:~{view_id}"))?;
    let payload = serde_json::json!({ "kind": "view", "local_id": view_id }).to_string();
    super::sync_queue::enqueue(conn, QUEUE_ENTITY, "home", "update", Some(&payload))
}

/// A view just got its remote id: if it is the stored home by local id, store
/// the remote id instead.
pub fn adopt_view(conn: &Connection, local_id: &str, remote_id: i64) -> Result<()> {
    if get(conn)?.is_some_and(|h| h.kind == "view" && h.local_id.as_deref() == Some(local_id)) {
        set_pull_cursor(conn, SCOPE, RESOURCE, &format!("view:{remote_id}"))?;
    }
    Ok(())
}

/// Clear the stored home if it is the unpushed view `local_id`.
pub fn clear_if_local_view(conn: &Connection, local_id: &str) -> Result<()> {
    if get(conn)?.is_some_and(|h| h.kind == "view" && h.local_id.as_deref() == Some(local_id)) {
        clear_pull_cursor(conn, SCOPE, RESOURCE)?;
    }
    Ok(())
}

/// A queued `home_page` payload resolved against the local mirror.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    Ready(String, i64),
    /// The view's create has not pushed yet.
    Waiting,
    /// Unparseable, or the view is gone locally.
    Bad,
}

pub fn resolve_payload(conn: &Connection, payload: Option<&str>) -> Resolved {
    if let Some((kind, id)) = parse_payload(payload) {
        return Resolved::Ready(kind, id);
    }
    let local = payload
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        .and_then(|v| v.get("local_id")?.as_str().map(str::to_string));
    let Some(local) = local else { return Resolved::Bad };
    match conn.query_row("SELECT remote_id FROM saved_views WHERE id = ?1", [&local], |r| r.get::<_, Option<i64>>(0)) {
        Ok(Some(id)) => Resolved::Ready("view".into(), id),
        Ok(None) => Resolved::Waiting,
        Err(_) => Resolved::Bad,
    }
}

/// Clear the stored home if it points at the Saved view `remote_id`. Nothing
/// is queued: the server treats a missing view as the default home.
pub fn clear_if_view(conn: &Connection, remote_id: i64) -> Result<()> {
    if get(conn)?.is_some_and(|h| h.kind == "view" && h.local_id.is_none() && h.remote_id == remote_id) {
        clear_pull_cursor(conn, SCOPE, RESOURCE)?;
    }
    Ok(())
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

/// Where launch should land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchTarget {
    /// A team's Issues board (local team id).
    Team(String),
    /// A Saved view, opened on its team (local ids).
    View { view_id: String, team_id: String },
    /// No usable Home page: the first non-archived team's board.
    Default,
}

/// Resolve the stored Home page against the local mirror. Dashboards, unknown
/// kinds and targets that are not mirrored (deleted, not synced yet, or a
/// view whose team is missing) resolve to `Default`.
pub fn launch_target(
    home: Option<&HomePage>,
    teams: &[crate::db::models::Team],
    views: &std::collections::HashMap<String, Vec<crate::db::saved_views::SavedView>>,
) -> LaunchTarget {
    let Some(h) = home else { return LaunchTarget::Default };
    match h.kind.as_str() {
        "team" => teams
            .iter()
            .find(|t| t.remote_id == Some(h.remote_id))
            .map_or(LaunchTarget::Default, |t| LaunchTarget::Team(t.id.clone())),
        "view" => views
            .values()
            .flatten()
            .find(|v| match &h.local_id {
                Some(l) => &v.id == l,
                None => v.remote_id == Some(h.remote_id),
            })
            .filter(|v| teams.iter().any(|t| t.id == v.team_id))
            .map_or(LaunchTarget::Default, |v| LaunchTarget::View {
                view_id: v.id.clone(),
                team_id: v.team_id.clone(),
            }),
        _ => LaunchTarget::Default,
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
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "team".into(), remote_id: 7, local_id: None }));
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
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "team".into(), remote_id: 2, local_id: None }));
    }

    #[test]
    fn once_pushed_me_restores_the_server_value() {
        // A 422 drops the entry (marked synced); the next /me restores.
        let conn = mem();
        set_local(&conn, "team", 2).unwrap();
        let id = crate::db::sync_queue::pending_entries(&conn).unwrap()[0].id.unwrap();
        crate::db::sync_queue::mark_synced(&conn, id).unwrap();
        apply_me(&conn, Some(&dto("view", 5))).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "view".into(), remote_id: 5, local_id: None }));
    }

    #[test]
    fn a_synced_view_is_set_like_a_team() {
        let conn = mem();
        set_view_local(&conn, "v1", Some(50)).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "view".into(), remote_id: 50, local_id: None }));
        let p = crate::db::sync_queue::pending_entries(&conn).unwrap();
        assert_eq!(parse_payload(p[0].payload.as_deref()), Some(("view".into(), 50)));
    }

    #[test]
    fn an_unpushed_view_is_stored_by_local_id_and_waits_for_its_create() {
        let conn = mem();
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES ('t1', 'T', 'T', 700, '', '')",
            [],
        )
        .unwrap();
        let v = crate::db::saved_views::create_local(&conn, "t1", "Mine", "{}").unwrap();
        set_view_local(&conn, &v.id, None).unwrap();
        let h = get(&conn).unwrap().unwrap();
        assert_eq!((h.kind.as_str(), h.local_id.as_deref()), ("view", Some(v.id.as_str())));
        // FIFO: the view's create is queued before the home write.
        let p = crate::db::sync_queue::pending_entries(&conn).unwrap();
        assert_eq!((p[0].entity_type.as_str(), p[1].entity_type.as_str()), ("saved_view", "home_page"));
        assert_eq!(resolve_payload(&conn, p[1].payload.as_deref()), Resolved::Waiting);
        // The create pushes: the payload now resolves to the remote id and the stored home adopts it.
        let dto = crate::db::saved_views::SavedViewIn {
            id: 88,
            team_id: 1,
            name: "Mine".into(),
            query: serde_json::json!({}),
            position: 1,
            updated_at: "2026-10-31T00:00:00.000Z".into(),
        };
        crate::db::saved_views::adopt_created(&conn, &v.id, &dto).unwrap();
        assert_eq!(resolve_payload(&conn, p[1].payload.as_deref()), Resolved::Ready("view".into(), 88));
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "view".into(), remote_id: 88, local_id: None }));
    }

    #[test]
    fn a_deleted_unpushed_view_payload_is_bad() {
        let conn = mem();
        assert_eq!(resolve_payload(&conn, Some(r#"{"kind":"view","local_id":"gone"}"#)), Resolved::Bad);
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
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "view".into(), remote_id: 42, local_id: None }));
    }

    #[test]
    fn a_later_sync_replaces_the_stored_home() {
        let conn = mem();
        apply_me(&conn, Some(&dto("team", 1))).unwrap();
        apply_me(&conn, Some(&dto("dashboard", 9))).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "dashboard".into(), remote_id: 9, local_id: None }));
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
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "team".into(), remote_id: 1, local_id: None }));
    }

    #[test]
    fn a_pushed_write_no_longer_blocks_the_server_value() {
        let conn = mem();
        queue_home_write(&conn, true);
        apply_me(&conn, Some(&dto("view", 2))).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { kind: "view".into(), remote_id: 2, local_id: None }));
    }

    fn team(id: &str, remote: Option<i64>, archived: bool) -> crate::db::models::Team {
        crate::db::models::Team {
            id: id.into(),
            name: id.into(),
            identifier: id.into(),
            remote_id: remote,
            archived_at: archived.then(|| "2026-01-01T00:00:00.000Z".to_string()),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn sv(id: &str, team: &str, remote: Option<i64>) -> crate::db::saved_views::SavedView {
        crate::db::saved_views::SavedView {
            id: id.into(),
            remote_id: remote,
            team_id: team.into(),
            name: id.into(),
            query: "{}".into(),
            position: 0,
            updated_at: String::new(),
        }
    }

    fn home(kind: &str, id: i64) -> Option<HomePage> {
        Some(HomePage { kind: kind.into(), remote_id: id, local_id: None })
    }

    fn fixture() -> (Vec<crate::db::models::Team>, std::collections::HashMap<String, Vec<crate::db::saved_views::SavedView>>) {
        let teams = vec![team("a", Some(1), false), team("b", Some(2), false)];
        let views = std::collections::HashMap::from([("b".to_string(), vec![sv("v1", "b", Some(50))])]);
        (teams, views)
    }

    #[test]
    fn team_home_launches_that_team() {
        let (t, v) = fixture();
        assert_eq!(launch_target(home("team", 2).as_ref(), &t, &v), LaunchTarget::Team("b".into()));
    }

    #[test]
    fn view_home_launches_that_view() {
        let (t, v) = fixture();
        assert_eq!(
            launch_target(home("view", 50).as_ref(), &t, &v),
            LaunchTarget::View { view_id: "v1".into(), team_id: "b".into() }
        );
    }

    #[test]
    fn no_home_dashboard_and_missing_targets_fall_back() {
        let (t, v) = fixture();
        for h in [None, home("dashboard", 9), home("team", 99), home("view", 99), home("bogus", 1)] {
            assert_eq!(launch_target(h.as_ref(), &t, &v), LaunchTarget::Default);
        }
    }

    #[test]
    fn a_view_whose_team_is_not_mirrored_falls_back() {
        let (t, _) = fixture();
        let views = std::collections::HashMap::from([("gone".to_string(), vec![sv("v2", "gone", Some(60))])]);
        assert_eq!(launch_target(home("view", 60).as_ref(), &t, &views), LaunchTarget::Default);
    }
}
