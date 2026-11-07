//! The user's Home page as last reported by the server, kept locally in the
//! single-row `home_page` table so a launch can resolve it even offline.
//! No row means no Home page.

use rusqlite::{params, Connection, OptionalExtension, Result};

use crate::api::HomePageDto;
use crate::db::dashboards::Dashboard;
use crate::db::models::Team;
use crate::db::saved_views::SavedView;

/// Queue `entity_type` for a locally-set Home page awaiting push.
pub const QUEUE_ENTITY: &str = "home_page";

/// What the Home page points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeTarget {
    /// A team, by remote id.
    Team(i64),
    /// A Saved view, by remote id.
    View(i64),
    /// A Saved view chosen before its create has pushed (local uuid);
    /// `adopt_view` swaps it for `View` once it has a remote id.
    LocalView(String),
    /// A dashboard, by remote id (not shown on the desktop yet).
    Dashboard(i64),
    /// A dashboard chosen before its create has pushed (local uuid);
    /// `adopt_dashboard` swaps it for `Dashboard` once it has a remote id.
    LocalDashboard(String),
}

impl HomeTarget {
    /// Build from the wire `{kind, id}`; unknown kinds are `None`.
    pub fn from_wire(kind: &str, id: i64) -> Option<Self> {
        match kind {
            "team" => Some(Self::Team(id)),
            "view" => Some(Self::View(id)),
            "dashboard" => Some(Self::Dashboard(id)),
            _ => None,
        }
    }
}

/// A stored Home page target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomePage {
    pub target: HomeTarget,
}

impl HomePage {
    /// True if this Home page is the given Saved view (by remote or local id).
    pub fn matches_view(&self, view: &SavedView) -> bool {
        match &self.target {
            HomeTarget::View(id) => view.remote_id == Some(*id),
            HomeTarget::LocalView(local) => &view.id == local,
            _ => false,
        }
    }

    /// True if this Home page is the given dashboard (by remote or local id).
    pub fn matches_dashboard(&self, dashboard: &Dashboard) -> bool {
        match &self.target {
            HomeTarget::Dashboard(id) => dashboard.remote_id == Some(*id),
            HomeTarget::LocalDashboard(local) => &dashboard.id == local,
            _ => false,
        }
    }

    /// True if this Home page is the given team.
    pub fn matches_team(&self, team: &Team) -> bool {
        matches!(&self.target, HomeTarget::Team(id) if team.remote_id == Some(*id))
    }
}

fn store(conn: &Connection, target: &HomeTarget) -> Result<()> {
    let (kind, remote_id, local): (&str, i64, Option<&str>) = match target {
        HomeTarget::Team(id) => ("team", *id, None),
        HomeTarget::View(id) => ("view", *id, None),
        HomeTarget::LocalView(l) => ("view", 0, Some(l)),
        HomeTarget::Dashboard(id) => ("dashboard", *id, None),
        HomeTarget::LocalDashboard(l) => ("dashboard", 0, Some(l)),
    };
    conn.execute(
        "INSERT INTO home_page (id, kind, remote_id, local_view_id) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET kind = excluded.kind, remote_id = excluded.remote_id,
            local_view_id = excluded.local_view_id",
        params![kind, remote_id, local],
    )?;
    Ok(())
}

fn clear(conn: &Connection) -> Result<()> {
    conn.execute("DELETE FROM home_page", [])?;
    Ok(())
}

/// The stored Home page, if any.
pub fn get(conn: &Connection) -> Result<Option<HomePage>> {
    let row = conn
        .query_row("SELECT kind, remote_id, local_view_id FROM home_page WHERE id = 1", [], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?))
        })
        .optional()?;
    Ok(row.and_then(|(kind, id, local)| {
        let target = match local {
            Some(l) if kind == "view" => HomeTarget::LocalView(l),
            Some(l) if kind == "dashboard" => HomeTarget::LocalDashboard(l),
            _ => HomeTarget::from_wire(&kind, id)?,
        };
        Some(HomePage { target })
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
    if let Some(target) = HomeTarget::from_wire(kind, remote_id) {
        store(conn, &target)?;
    }
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
    store(conn, &HomeTarget::LocalView(view_id.to_string()))?;
    let payload = serde_json::json!({ "kind": "view", "local_id": view_id }).to_string();
    super::sync_queue::enqueue(conn, QUEUE_ENTITY, "home", "update", Some(&payload))
}

/// Set a dashboard (local uuid) as the Home page. One with a remote id is an
/// ordinary `set_local`. One not pushed yet is stored by its local id and
/// queued with `local_id`; the engine resolves it once the create (earlier in
/// the FIFO queue) has pushed.
pub fn set_dashboard_local(conn: &Connection, dashboard_id: &str, remote_id: Option<i64>) -> Result<()> {
    if let Some(rid) = remote_id {
        return set_local(conn, "dashboard", rid);
    }
    store(conn, &HomeTarget::LocalDashboard(dashboard_id.to_string()))?;
    let payload = serde_json::json!({ "kind": "dashboard", "local_id": dashboard_id }).to_string();
    super::sync_queue::enqueue(conn, QUEUE_ENTITY, "home", "update", Some(&payload))
}

fn is_local_dashboard(conn: &Connection, local_id: &str) -> Result<bool> {
    Ok(get(conn)?.is_some_and(|h| matches!(&h.target, HomeTarget::LocalDashboard(l) if l == local_id)))
}

/// A dashboard just got its remote id: if it is the stored home by local id,
/// store the remote id instead.
pub fn adopt_dashboard(conn: &Connection, local_id: &str, remote_id: i64) -> Result<()> {
    if is_local_dashboard(conn, local_id)? {
        store(conn, &HomeTarget::Dashboard(remote_id))?;
    }
    Ok(())
}

/// Clear the stored home if it is the unpushed dashboard `local_id`.
pub fn clear_if_local_dashboard(conn: &Connection, local_id: &str) -> Result<()> {
    if is_local_dashboard(conn, local_id)? {
        clear(conn)?;
    }
    Ok(())
}

fn is_local_view(conn: &Connection, local_id: &str) -> Result<bool> {
    Ok(get(conn)?.is_some_and(|h| matches!(&h.target, HomeTarget::LocalView(l) if l == local_id)))
}

/// A view just got its remote id: if it is the stored home by local id, store
/// the remote id instead.
pub fn adopt_view(conn: &Connection, local_id: &str, remote_id: i64) -> Result<()> {
    if is_local_view(conn, local_id)? {
        store(conn, &HomeTarget::View(remote_id))?;
    }
    Ok(())
}

/// Clear the stored home if it is the unpushed view `local_id`.
pub fn clear_if_local_view(conn: &Connection, local_id: &str) -> Result<()> {
    if is_local_view(conn, local_id)? {
        clear(conn)?;
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
    let is_dashboard = payload
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        .is_some_and(|v| v.get("kind").and_then(|k| k.as_str()) == Some("dashboard"));
    let (kind, remote) = if is_dashboard {
        ("dashboard", super::dashboards::get(conn, &local).map(|d| d.map(|d| d.remote_id)))
    } else {
        ("view", super::saved_views::remote_id_of(conn, &local))
    };
    match remote {
        Ok(Some(Some(id))) => Resolved::Ready(kind.into(), id),
        Ok(Some(None)) => Resolved::Waiting,
        Ok(None) | Err(_) => Resolved::Bad,
    }
}

/// Clear the stored home if it points at the Saved view `remote_id`. Nothing
/// is queued: the server treats a missing view as the default home.
pub fn clear_if_view(conn: &Connection, remote_id: i64) -> Result<()> {
    if get(conn)?.is_some_and(|h| h.target == HomeTarget::View(remote_id)) {
        clear(conn)?;
    }
    Ok(())
}

/// Clear the stored home if it points at the dashboard `remote_id`. Nothing
/// is queued: the server treats a missing dashboard as the default home.
pub fn clear_if_dashboard(conn: &Connection, remote_id: i64) -> Result<()> {
    if get(conn)?.is_some_and(|h| h.target == HomeTarget::Dashboard(remote_id)) {
        clear(conn)?;
    }
    Ok(())
}

/// `(kind, id)` from a queued `home_page` payload.
pub fn parse_payload(payload: Option<&str>) -> Option<(String, i64)> {
    let v: serde_json::Value = serde_json::from_str(payload?).ok()?;
    Some((v.get("kind")?.as_str()?.to_string(), v.get("id")?.as_i64()?))
}

/// Store what `/me` reported: a value replaces the stored home, `None` (or a
/// kind this app does not know) clears it. Skipped while a local `home_page`
/// write is pending.
pub fn apply_me(conn: &Connection, reported: Option<&HomePageDto>) -> Result<()> {
    if write_pending(conn)? {
        return Ok(());
    }
    match reported.and_then(|h| HomeTarget::from_wire(&h.kind, h.id)) {
        Some(t) => store(conn, &t),
        None => clear(conn),
    }
}

/// Where launch should land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchTarget {
    /// A team's Issues board (local team id).
    Team(String),
    /// A Saved view, opened on its team (local ids).
    View { view_id: String, team_id: String },
    /// A dashboard (local id).
    Dashboard(String),
    /// No usable Home page: the first non-archived team's board.
    Default,
}

/// Resolve the stored Home page against the local mirror. Unknown
/// kinds and targets that are not mirrored (deleted, not synced yet, or a
/// view whose team is missing) resolve to `Default`.
pub fn launch_target(
    home: Option<&HomePage>,
    teams: &[Team],
    views: &std::collections::HashMap<String, Vec<SavedView>>,
    dashboards: &[Dashboard],
) -> LaunchTarget {
    let Some(h) = home else { return LaunchTarget::Default };
    match &h.target {
        HomeTarget::Team(_) => teams
            .iter()
            .find(|t| h.matches_team(t))
            .map_or(LaunchTarget::Default, |t| LaunchTarget::Team(t.id.clone())),
        HomeTarget::View(_) | HomeTarget::LocalView(_) => views
            .values()
            .flatten()
            .find(|v| h.matches_view(v))
            .filter(|v| teams.iter().any(|t| t.id == v.team_id))
            .map_or(LaunchTarget::Default, |v| LaunchTarget::View {
                view_id: v.id.clone(),
                team_id: v.team_id.clone(),
            }),
        HomeTarget::Dashboard(_) | HomeTarget::LocalDashboard(_) => dashboards
            .iter()
            .find(|d| h.matches_dashboard(d))
            .map_or(LaunchTarget::Default, |d| LaunchTarget::Dashboard(d.id.clone())),
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

    fn mirror(conn: &Connection, remote_id: i64, name: &str) -> Dashboard {
        let row = crate::db::dashboards::DashboardIn {
            id: remote_id,
            name: name.into(),
            description: None,
            position: remote_id,
            updated_at: "2026-10-31T00:00:00.000Z".into(),
            groups: vec![],
        };
        crate::db::dashboards::apply_snapshot(conn, &[row]).unwrap();
        crate::db::dashboards::find_by_remote_id(conn, remote_id).unwrap().unwrap()
    }

    #[test]
    fn local_delete_of_the_home_dashboard_clears_home_and_queues_no_home_write() {
        let conn = mem();
        let d = mirror(&conn, 3, "Home one");
        apply_me(&conn, Some(&dto("dashboard", 3))).unwrap();
        crate::db::dashboards::delete_local(&conn, &d.id).unwrap();
        assert_eq!(get(&conn).unwrap(), None);
        assert_eq!(launch_target(get(&conn).unwrap().as_ref(), &[], &Default::default(), &[]), LaunchTarget::Default);
        assert!(crate::db::sync_queue::pending_entries(&conn).unwrap().is_empty());
    }

    #[test]
    fn pull_that_drops_the_home_dashboard_clears_home() {
        let conn = mem();
        mirror(&conn, 3, "Home one");
        apply_me(&conn, Some(&dto("dashboard", 3))).unwrap();
        crate::db::dashboards::apply_snapshot(&conn, &[]).unwrap();
        assert_eq!(get(&conn).unwrap(), None);
        assert_eq!(launch_target(get(&conn).unwrap().as_ref(), &[], &Default::default(), &[]), LaunchTarget::Default);
    }

    #[test]
    fn deleting_another_dashboard_leaves_home_alone() {
        let conn = mem();
        mirror(&conn, 3, "Home one");
        let other = mirror(&conn, 4, "Other");
        apply_me(&conn, Some(&dto("dashboard", 3))).unwrap();
        crate::db::dashboards::delete_local(&conn, &other.id).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { target: HomeTarget::Dashboard(3) }));
    }

    #[test]
    fn set_local_stores_at_once_and_queues_the_put() {
        let conn = mem();
        set_local(&conn, "team", 7).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { target: HomeTarget::Team(7) }));
        let pending = crate::db::sync_queue::pending_entries(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!((pending[0].entity_type.as_str(), pending[0].action.as_str()), ("home_page", "update"));
        assert_eq!(parse_payload(pending[0].payload.as_deref()), Some(("team".into(), 7)));
    }

    #[test]
    fn setting_a_dashboard_stores_it_and_queues_the_dashboard_kind() {
        let conn = mem();
        set_local(&conn, "team", 7).unwrap();
        set_local(&conn, "dashboard", 3).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { target: HomeTarget::Dashboard(3) }));
        let pending = crate::db::sync_queue::pending_entries(&conn).unwrap();
        assert_eq!(parse_payload(pending[1].payload.as_deref()), Some(("dashboard".into(), 3)));
    }

    #[test]
    fn a_pending_set_is_not_overwritten_by_me() {
        let conn = mem();
        apply_me(&conn, Some(&dto("team", 1))).unwrap();
        set_local(&conn, "team", 2).unwrap();
        apply_me(&conn, Some(&dto("team", 1))).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { target: HomeTarget::Team(2) }));
    }

    #[test]
    fn once_pushed_me_restores_the_server_value() {
        // A 422 drops the entry (marked synced); the next /me restores.
        let conn = mem();
        set_local(&conn, "team", 2).unwrap();
        let id = crate::db::sync_queue::pending_entries(&conn).unwrap()[0].id.unwrap();
        crate::db::sync_queue::mark_synced(&conn, id).unwrap();
        apply_me(&conn, Some(&dto("view", 5))).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { target: HomeTarget::View(5) }));
    }

    #[test]
    fn a_synced_view_is_set_like_a_team() {
        let conn = mem();
        set_view_local(&conn, "v1", Some(50)).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { target: HomeTarget::View(50) }));
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
        assert_eq!(h.target, HomeTarget::LocalView(v.id.clone()));
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
        assert_eq!(get(&conn).unwrap(), Some(HomePage { target: HomeTarget::View(88) }));
    }

    #[test]
    fn an_unpushed_dashboard_is_stored_by_local_id_swapped_on_adopt_and_launches() {
        let conn = mem();
        let d = crate::db::dashboards::create_local(&conn, "Mine", "").unwrap();
        set_dashboard_local(&conn, &d.id, None).unwrap();
        let h = get(&conn).unwrap().unwrap();
        assert_eq!(h.target, HomeTarget::LocalDashboard(d.id.clone()));
        assert!(h.matches_dashboard(&d));
        assert_eq!(
            launch_target(Some(&h), &[], &Default::default(), std::slice::from_ref(&d)),
            LaunchTarget::Dashboard(d.id.clone())
        );
        // FIFO: the create first, then the home write, which waits.
        let p = crate::db::sync_queue::pending_entries(&conn).unwrap();
        assert_eq!((p[0].entity_type.as_str(), p[1].entity_type.as_str()), ("dashboard", "home_page"));
        assert_eq!(resolve_payload(&conn, p[1].payload.as_deref()), Resolved::Waiting);
        // The create is adopted: the payload resolves and the stored home swaps.
        let dto = crate::db::dashboards::DashboardIn {
            id: 41,
            name: "Mine".into(),
            description: None,
            position: 1,
            updated_at: "2026-11-01T00:00:00.000Z".into(),
            groups: vec![],
        };
        crate::db::dashboards::adopt_created(&conn, &d.id, &dto).unwrap();
        assert_eq!(resolve_payload(&conn, p[1].payload.as_deref()), Resolved::Ready("dashboard".into(), 41));
        let h = get(&conn).unwrap().unwrap();
        assert_eq!(h.target, HomeTarget::Dashboard(41));
        let d = crate::db::dashboards::get(&conn, &d.id).unwrap().unwrap();
        assert!(h.matches_dashboard(&d));
    }

    #[test]
    fn a_synced_dashboard_is_set_like_a_team_and_deleting_an_unpushed_one_clears_home() {
        let conn = mem();
        set_dashboard_local(&conn, "d1", Some(9)).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { target: HomeTarget::Dashboard(9) }));
        let d = crate::db::dashboards::create_local(&conn, "N", "").unwrap();
        set_dashboard_local(&conn, &d.id, None).unwrap();
        crate::db::dashboards::delete_local(&conn, &d.id).unwrap();
        assert_eq!(get(&conn).unwrap(), None);
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
        assert_eq!(get(&conn).unwrap(), Some(HomePage { target: HomeTarget::View(42) }));
    }

    #[test]
    fn a_later_sync_replaces_the_stored_home() {
        let conn = mem();
        apply_me(&conn, Some(&dto("team", 1))).unwrap();
        apply_me(&conn, Some(&dto("dashboard", 9))).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { target: HomeTarget::Dashboard(9) }));
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
        assert_eq!(get(&conn).unwrap(), Some(HomePage { target: HomeTarget::Team(1) }));
    }

    #[test]
    fn a_pushed_write_no_longer_blocks_the_server_value() {
        let conn = mem();
        queue_home_write(&conn, true);
        apply_me(&conn, Some(&dto("view", 2))).unwrap();
        assert_eq!(get(&conn).unwrap(), Some(HomePage { target: HomeTarget::View(2) }));
    }

    fn team(id: &str, remote: Option<i64>, archived: bool) -> crate::db::models::Team {
        crate::db::models::Team {
            id: id.into(),
            name: id.into(),
            identifier: id.into(),
            remote_id: remote,
            archived_at: archived.then(|| "2026-01-01T00:00:00.000Z".to_string()),
            role: None,
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
        Some(HomePage { target: HomeTarget::from_wire(kind, id).unwrap_or(HomeTarget::Dashboard(-1)) })
    }

    fn fixture() -> (Vec<crate::db::models::Team>, std::collections::HashMap<String, Vec<crate::db::saved_views::SavedView>>) {
        let teams = vec![team("a", Some(1), false), team("b", Some(2), false)];
        let views = std::collections::HashMap::from([("b".to_string(), vec![sv("v1", "b", Some(50))])]);
        (teams, views)
    }

    #[test]
    fn team_home_launches_that_team() {
        let (t, v) = fixture();
        assert_eq!(launch_target(home("team", 2).as_ref(), &t, &v, &[]), LaunchTarget::Team("b".into()));
    }

    #[test]
    fn view_home_launches_that_view() {
        let (t, v) = fixture();
        assert_eq!(
            launch_target(home("view", 50).as_ref(), &t, &v, &[]),
            LaunchTarget::View { view_id: "v1".into(), team_id: "b".into() }
        );
    }

    #[test]
    fn no_home_dashboard_and_missing_targets_fall_back() {
        let (t, v) = fixture();
        for h in [None, home("dashboard", 9), home("team", 99), home("view", 99), home("bogus", 1)] {
            assert_eq!(launch_target(h.as_ref(), &t, &v, &[]), LaunchTarget::Default);
        }
    }

    #[test]
    fn a_view_whose_team_is_not_mirrored_falls_back() {
        let (t, _) = fixture();
        let views = std::collections::HashMap::from([("gone".to_string(), vec![sv("v2", "gone", Some(60))])]);
        assert_eq!(launch_target(home("view", 60).as_ref(), &t, &views, &[]), LaunchTarget::Default);
    }

    #[test]
    fn a_mirrored_dashboard_home_launches_it() {
        let (t, v) = fixture();
        let d = crate::db::dashboards::Dashboard {
            id: "d1".into(),
            remote_id: Some(9),
            name: "D".into(),
            description: String::new(),
            position: 0,
            updated_at: String::new(),
        };
        assert_eq!(
            launch_target(home("dashboard", 9).as_ref(), &t, &v, &[d.clone()]),
            LaunchTarget::Dashboard("d1".into())
        );
        assert_eq!(launch_target(home("dashboard", 10).as_ref(), &t, &v, &[d]), LaunchTarget::Default);
    }
}
