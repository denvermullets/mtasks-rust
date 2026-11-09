//! Pushing Dashboard and Dashboard group changes.

use rusqlite::Connection;

use crate::api::{ApiClient, ApiError};
use crate::db::{dashboards, sync_queue};
use crate::sync::engine::open;
use super::outcome::{Outcome, Prep, classify, done_if_gone};
use super::saved_view::saved_view_delete_id;

// ── Dashboards ────────────────────────────────────────────────────────────
//
// Dashboards are user-level (no team), so errors go through plain `classify`.
// A later dashboard entry that needs a parent's server id follows
// `dashboard_create_prep`: read the row and `LeavePending` while `remote_id`
// is still `None` (the earlier create entry drains first, FIFO).

struct DashboardCreatePrep {
    local_id: String,
    name: String,
    description: String,
}

fn dashboard_create_prep(conn: &Connection, entity_id: &str) -> Prep<DashboardCreatePrep> {
    match dashboards::get(conn, entity_id) {
        Ok(Some(d)) if d.remote_id.is_some() => Prep::Done(Outcome::Success),
        Ok(Some(d)) => Prep::Ready(DashboardCreatePrep {
            local_id: d.id,
            name: d.name,
            description: d.description,
        }),
        // Deleted before it was ever pushed.
        Ok(None) => Prep::Done(Outcome::DropAndLog(format!(
            "dashboard {entity_id} missing locally"
        ))),
        Err(e) => Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    }
}

/// Adopt the server's row and retire the queue entry together.
fn commit_dashboard_create(
    conn: &mut Connection,
    entry_id: i64,
    local_id: &str,
    dto: &crate::api::dashboards::DashboardDto,
) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| format!("begin: {e}"))?;
    dashboards::adopt_created(&tx, local_id, dto).map_err(|e| format!("adopt dashboard: {e}"))?;
    sync_queue::mark_synced(&tx, entry_id).map_err(|e| format!("mark_synced: {e}"))?;
    tx.commit().map_err(|e| format!("commit: {e}"))
}

/// `POST /dashboards`. Rails has no lookup for these, so a re-drain after a
/// lost response creates a second server row that the pull mirrors separately.
pub(super) async fn dashboard_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match open() {
        Ok(conn) => match dashboard_create_prep(&conn, entity_id) {
            Prep::Ready(p) => p,
            Prep::Done(o) => return o,
        },
        Err(e) => return Outcome::LeavePending(e),
    };
    match crate::api::dashboards::create(client, &prep.name, &prep.description).await {
        Ok(dto) => {
            let mut conn = match open() {
                Ok(c) => c,
                Err(e) => return Outcome::LeavePending(e),
            };
            match commit_dashboard_create(&mut conn, entry_id, &prep.local_id, &dto) {
                Ok(()) => Outcome::Created,
                Err(e) => Outcome::LeavePending(e),
            }
        }
        Err(e) => classify(&e),
    }
}

struct DashboardUpdatePrep {
    remote_id: i64,
    name: String,
    description: String,
}

/// Waits (`LeavePending`) while the dashboard's create hasn't been adopted.
fn dashboard_update_prep(conn: &Connection, entity_id: &str) -> Prep<DashboardUpdatePrep> {
    match dashboards::get(conn, entity_id) {
        Ok(Some(d)) => match d.remote_id {
            Some(remote_id) => Prep::Ready(DashboardUpdatePrep {
                remote_id,
                name: d.name,
                description: d.description,
            }),
            None => Prep::Done(Outcome::LeavePending("dashboard create not yet drained".into())),
        },
        Ok(None) => Prep::Done(Outcome::DropAndLog(format!("dashboard {entity_id} missing locally"))),
        Err(e) => Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    }
}

/// `PATCH /dashboards/:id`. A 404 means it is gone on the server: done (the
/// next snapshot removes the row).
pub(super) async fn dashboard_update(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match open() {
        Ok(conn) => match dashboard_update_prep(&conn, entity_id) {
            Prep::Ready(p) => p,
            Prep::Done(o) => return o,
        },
        Err(e) => return Outcome::LeavePending(e),
    };
    let result =
        crate::api::dashboards::update(client, prep.remote_id, &prep.name, &prep.description).await;
    done_if_gone(result, |_| {})
}

struct DashboardGroupCreatePrep {
    local_id: String,
    /// The parent dashboard's server id, resolved at push time.
    dashboard_remote_id: i64,
    name: String,
    description: String,
    color: String,
    sources: Vec<dashboards::GroupSource>,
}

/// The group's dashboard's server id, or the outcome that ends the prep: wait
/// while the dashboard's create hasn't drained, drop if it is gone locally.
/// Shared by the group create and update preps, which differ in everything else.
fn group_parent_remote_id(
    conn: &Connection,
    entity_id: &str,
    group: &dashboards::DashboardGroup,
) -> Result<i64, Outcome> {
    match dashboards::get(conn, &group.dashboard_id) {
        Ok(Some(d)) => d
            .remote_id
            .ok_or_else(|| Outcome::LeavePending("dashboard create not yet drained".into())),
        Ok(None) => Err(Outcome::DropAndLog(format!(
            "dashboard group {entity_id}: dashboard missing locally"
        ))),
        Err(e) => Err(Outcome::LeavePending(format!("db: {e}"))),
    }
}

/// Re-read the group and its parent. Waits (`LeavePending`) while the parent
/// dashboard has no server id; its create entry is earlier in the queue and
/// drains first. Sources are read as stored, so edits made while waiting go out.
fn dashboard_group_create_prep(conn: &Connection, entity_id: &str) -> Prep<DashboardGroupCreatePrep> {
    let group = match dashboards::get_group(conn, entity_id) {
        Ok(Some(g)) => g,
        // Deleted (or its dashboard was) before it was ever pushed.
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "dashboard group {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    if group.remote_id.is_some() {
        return Prep::Done(Outcome::Success);
    }
    let dashboard_remote_id = match group_parent_remote_id(conn, entity_id, &group) {
        Ok(id) => id,
        Err(o) => return Prep::Done(o),
    };
    let sources = match dashboards::sources_for(conn, &group.id) {
        Ok(s) => s,
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    Prep::Ready(DashboardGroupCreatePrep {
        local_id: group.id,
        dashboard_remote_id,
        name: group.name,
        description: group.description,
        color: group.color,
        sources,
    })
}

/// Adopt the server's group (id, position, returned sources) and retire the
/// queue entry together.
fn commit_dashboard_group_create(
    conn: &mut Connection,
    entry_id: i64,
    local_id: &str,
    dto: &crate::api::dashboards::GroupDto,
) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| format!("begin: {e}"))?;
    dashboards::adopt_created_group(&tx, local_id, dto).map_err(|e| format!("adopt group: {e}"))?;
    sync_queue::mark_synced(&tx, entry_id).map_err(|e| format!("mark_synced: {e}"))?;
    tx.commit().map_err(|e| format!("commit: {e}"))
}

/// `POST /dashboards/:id/groups`. Like the dashboard create, a re-drain after a
/// lost response makes a second server group that the pull mirrors separately.
pub(super) async fn dashboard_group_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match open() {
        Ok(conn) => match dashboard_group_create_prep(&conn, entity_id) {
            Prep::Ready(p) => p,
            Prep::Done(o) => return o,
        },
        Err(e) => return Outcome::LeavePending(e),
    };
    let result = crate::api::dashboards::create_group(
        client,
        prep.dashboard_remote_id,
        &prep.name,
        &prep.description,
        &prep.color,
        &prep.sources,
    )
    .await;
    match result {
        Ok(dto) => {
            let mut conn = match open() {
                Ok(c) => c,
                Err(e) => return Outcome::LeavePending(e),
            };
            match commit_dashboard_group_create(&mut conn, entry_id, &prep.local_id, &dto) {
                Ok(()) => Outcome::Created,
                Err(e) => Outcome::LeavePending(e),
            }
        }
        Err(ApiError::NotFound(m)) => orphaned_group_create(entity_id, &m),
        Err(e) => classify(&e),
    }
}

/// A 404 on `POST /dashboards/:id/groups` means the dashboard was deleted on
/// the web, so the group can never be pushed. Discard it (and its queue
/// entries); otherwise the unpushed group keeps `apply_snapshot` from ever
/// removing the dashboard. The next pull then drops the dashboard.
fn orphaned_group_create(group_id: &str, message: &str) -> Outcome {
    match open().and_then(|c| {
        dashboards::delete_group_local(&c, group_id).map_err(|e| format!("db: {e}"))
    }) {
        Ok(_) => Outcome::DropAndLog(format!("dashboard gone on the server, group discarded: {message}")),
        Err(e) => Outcome::LeavePending(e),
    }
}

struct DashboardGroupUpdatePrep {
    local_id: String,
    dashboard_remote_id: i64,
    remote_id: i64,
    name: String,
    description: String,
    color: String,
    sources: Vec<dashboards::GroupSource>,
}

/// Re-read the group, its parent and its sources. Waits (`LeavePending`) while
/// the group or its dashboard has no server id: their create entries are
/// earlier in the queue and drain first.
fn dashboard_group_update_prep(conn: &Connection, entity_id: &str) -> Prep<DashboardGroupUpdatePrep> {
    let group = match dashboards::get_group(conn, entity_id) {
        Ok(Some(g)) => g,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "dashboard group {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let Some(remote_id) = group.remote_id else {
        return Prep::Done(Outcome::LeavePending("dashboard group create not yet drained".into()));
    };
    let dashboard_remote_id = match group_parent_remote_id(conn, entity_id, &group) {
        Ok(id) => id,
        Err(o) => return Prep::Done(o),
    };
    let sources = match dashboards::sources_for(conn, &group.id) {
        Ok(s) => s,
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    Prep::Ready(DashboardGroupUpdatePrep {
        local_id: group.id,
        dashboard_remote_id,
        remote_id,
        name: group.name,
        description: group.description,
        color: group.color,
        sources,
    })
}

/// Retire the queue entry, then let the returned sources replace the local
/// ones (unless a newer edit is already queued), in one transaction.
fn commit_dashboard_group_update(
    conn: &mut Connection,
    entry_id: i64,
    local_id: &str,
    dto: &crate::api::dashboards::GroupDto,
) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| format!("begin: {e}"))?;
    sync_queue::mark_synced(&tx, entry_id).map_err(|e| format!("mark_synced: {e}"))?;
    dashboards::adopt_updated_group(&tx, local_id, dto).map_err(|e| format!("adopt group: {e}"))?;
    tx.commit().map_err(|e| format!("commit: {e}"))
}

/// `PATCH /dashboards/:id/groups/:id`. A 404 means it is gone on the server:
/// done (the next snapshot removes the row).
pub(super) async fn dashboard_group_update(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match open() {
        Ok(conn) => match dashboard_group_update_prep(&conn, entity_id) {
            Prep::Ready(p) => p,
            Prep::Done(o) => return o,
        },
        Err(e) => return Outcome::LeavePending(e),
    };
    let result = crate::api::dashboards::update_group(
        client,
        prep.dashboard_remote_id,
        prep.remote_id,
        &prep.name,
        &prep.description,
        &prep.color,
        &prep.sources,
    )
    .await;
    match result {
        Ok(dto) => {
            let mut conn = match open() {
                Ok(c) => c,
                Err(e) => return Outcome::LeavePending(e),
            };
            match commit_dashboard_group_update(&mut conn, entry_id, &prep.local_id, &dto) {
                // The commit already retired the entry.
                Ok(()) => Outcome::Created,
                Err(e) => Outcome::LeavePending(e),
            }
        }
        Err(ApiError::NotFound(_)) => Outcome::Success,
        Err(e) => classify(&e),
    }
}

// Group order: one coalesced entry per dashboard (see `dashboards::move_group`).

fn dashboard_group_order_prep(
    conn: &Connection,
    entry_id: i64,
    dashboard_id: &str,
) -> Prep<dashboards::OrderPlan> {
    match dashboards::plan_order_push(conn, entry_id, dashboard_id) {
        Ok(dashboards::OrderPlan::Waiting(why)) => Prep::Done(Outcome::LeavePending(why.into())),
        Ok(dashboards::OrderPlan::Gone) => Prep::Done(Outcome::DropAndLog(format!(
            "dashboard {dashboard_id} or its groups missing locally"
        ))),
        Ok(ready) => Prep::Ready(ready),
        Err(e) => Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    }
}

/// `PATCH /dashboards/:id/groups/reorder`. The response's order becomes the
/// local positions. A 404 means the dashboard is gone: done.
pub(super) async fn dashboard_group_order(client: &ApiClient, dashboard_id: &str, entry_id: i64) -> Outcome {
    let plan = match open() {
        Ok(conn) => match dashboard_group_order_prep(&conn, entry_id, dashboard_id) {
            Prep::Ready(p) => p,
            Prep::Done(o) => return o,
        },
        Err(e) => return Outcome::LeavePending(e),
    };
    let dashboards::OrderPlan::Ready { dashboard_remote_id, group_remote_ids, payload } = plan else {
        return Outcome::LeavePending("group order not ready".into());
    };
    match crate::api::dashboards::reorder_groups(client, dashboard_remote_id, &group_remote_ids).await {
        Ok(returned) => match open() {
            Ok(conn) => {
                match dashboards::finish_order_push(&conn, entry_id, dashboard_id, &payload, &returned) {
                    Ok(true) => Outcome::Success,
                    Ok(false) => Outcome::LeavePending("group order changed while pushing".into()),
                    Err(e) => Outcome::LeavePending(format!("db: {e}")),
                }
            }
            Err(e) => Outcome::LeavePending(e),
        },
        Err(ApiError::NotFound(_)) => Outcome::Success,
        Err(e) => classify(&e),
    }
}

/// `DELETE /dashboards/:id`. A 404 means it is already gone: done.
pub(super) async fn dashboard_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let Some(remote_id) = saved_view_delete_id(payload) else {
        return Outcome::DropAndLog("dashboard delete: bad payload".into());
    };
    dashboard_delete_outcome(crate::api::dashboards::delete(client, remote_id).await)
}

/// `DELETE /dashboards/:dashboard_id/groups/:id`. A 404 means it is already gone: done.
pub(super) async fn dashboard_group_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let Some(p) = dashboards::GroupDeletePayload::parse(payload) else {
        return Outcome::DropAndLog("dashboard group delete: bad payload".into());
    };
    dashboard_delete_outcome(
        crate::api::dashboards::delete_group(client, p.dashboard_remote_id, p.remote_id).await,
    )
}

fn dashboard_delete_outcome(result: Result<(), ApiError>) -> Outcome {
    done_if_gone(result, |()| {})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_delete_payload_needs_both_remote_ids() {
        let parse = dashboards::GroupDeletePayload::parse;
        assert_eq!(
            parse(Some(r#"{"dashboard_remote_id":5,"remote_id":10}"#)),
            Some(dashboards::GroupDeletePayload { dashboard_remote_id: 5, remote_id: 10 })
        );
        assert_eq!(parse(Some(r#"{"remote_id":10}"#)), None);
        assert_eq!(parse(None), None);
    }

    #[test]
    fn dashboard_delete_treats_success_and_404_as_done_and_retries_the_rest() {
        assert!(matches!(dashboard_delete_outcome(Ok(())), Outcome::Success));
        assert!(matches!(
            dashboard_delete_outcome(Err(ApiError::NotFound("gone".into()))),
            Outcome::Success
        ));
        assert!(matches!(
            dashboard_delete_outcome(Err(ApiError::MissingToken)),
            Outcome::LeavePending(_)
        ));
    }

    #[test]
    fn dashboard_update_waits_for_the_create_then_pushes_the_row() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        let d = dashboards::create_local(&conn, "A", "").unwrap();
        dashboards::update_local(&conn, &d.id, "B", "z").unwrap();
        assert!(matches!(
            dashboard_update_prep(&conn, &d.id),
            Prep::Done(Outcome::LeavePending(_))
        ));
        conn.execute("UPDATE dashboards SET remote_id = 9 WHERE id = ?1", [&d.id]).unwrap();
        let Prep::Ready(p) = dashboard_update_prep(&conn, &d.id) else { panic!("ready") };
        assert_eq!((p.remote_id, p.name.as_str(), p.description.as_str()), (9, "B", "z"));
        assert!(matches!(
            dashboard_update_prep(&conn, "nope"),
            Prep::Done(Outcome::DropAndLog(_))
        ));
    }

    fn group_prep_db() -> (Connection, String, String) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        let d = dashboards::create_local(&conn, "D", "").unwrap();
        let mut draft = dashboards::GroupDraft { name: "G".into(), ..Default::default() };
        draft.toggle_source(dashboards::SourceType::Team, 3);
        let g = dashboards::create_group_local(&conn, &d.id, &draft).unwrap();
        (conn, d.id, g.id)
    }

    #[test]
    fn committing_a_group_create_adopts_and_retires_the_entry() {
        let (mut conn, did, gid) = group_prep_db();
        let entry = sync_queue::pending_entries(&conn)
            .unwrap()
            .into_iter()
            .find(|e| e.entity_type == dashboards::GROUP_ENTITY_TYPE)
            .and_then(|e| e.id)
            .unwrap();
        let dto: crate::api::dashboards::GroupDto = serde_json::from_value(serde_json::json!({
            "id": 41, "name": "G", "position": 2, "team_ids": [3]
        }))
        .unwrap();
        commit_dashboard_group_create(&mut conn, entry, &gid, &dto).unwrap();
        let g = dashboards::groups_for(&conn, &did).unwrap();
        assert_eq!((g[0].remote_id, g[0].position), (Some(41), 2));
        assert!(!sync_queue::has_pending_entry(&conn, dashboards::GROUP_ENTITY_TYPE, &gid).unwrap());
    }

    fn group_entry(conn: &Connection, action: &str) -> i64 {
        sync_queue::pending_entries(conn)
            .unwrap()
            .into_iter()
            .find(|e| e.entity_type == dashboards::GROUP_ENTITY_TYPE && e.action == action)
            .and_then(|e| e.id)
            .unwrap()
    }

    #[test]
    fn committing_a_group_update_replaces_sources_with_the_returned_ones_and_retires_the_entry() {
        let (mut conn, did, gid) = group_prep_db();
        conn.execute("UPDATE dashboards SET remote_id = 9 WHERE id = ?1", [&did]).unwrap();
        conn.execute("UPDATE dashboard_groups SET remote_id = 41 WHERE id = ?1", [&gid]).unwrap();
        let mut draft = dashboards::group_draft(&conn, &gid).unwrap().unwrap();
        draft.toggle_source(dashboards::SourceType::Team, 4);
        draft.toggle_source(dashboards::SourceType::Team, 5);
        dashboards::update_group_local(&conn, &gid, &draft).unwrap();
        // The server dropped team 5.
        let dto: crate::api::dashboards::GroupDto = serde_json::from_value(serde_json::json!({
            "id": 41, "name": "G", "team_ids": [3, 4]
        }))
        .unwrap();
        let entry = group_entry(&conn, "update");
        commit_dashboard_group_update(&mut conn, entry, &gid, &dto).unwrap();
        let ids: Vec<_> =
            dashboards::sources_for(&conn, &gid).unwrap().iter().map(|s| s.remote_id).collect();
        assert_eq!(ids, [3, 4]);
        let pending_updates = sync_queue::pending_entries(&conn)
            .unwrap()
            .iter()
            .filter(|e| e.action == "update")
            .count();
        assert_eq!(pending_updates, 0);
    }

    #[test]
    fn group_update_waits_for_the_group_and_dashboard_creates_then_is_ready() {
        let (conn, did, gid) = group_prep_db();
        dashboards::update_group_local(
            &conn,
            &gid,
            &dashboards::group_draft(&conn, &gid).unwrap().unwrap(),
        )
        .unwrap();
        assert!(matches!(
            dashboard_group_update_prep(&conn, &gid),
            Prep::Done(Outcome::LeavePending(_))
        ));
        conn.execute("UPDATE dashboard_groups SET remote_id = 41 WHERE id = ?1", [&gid]).unwrap();
        assert!(matches!(
            dashboard_group_update_prep(&conn, &gid),
            Prep::Done(Outcome::LeavePending(_))
        ));
        conn.execute("UPDATE dashboards SET remote_id = 9 WHERE id = ?1", [&did]).unwrap();
        match dashboard_group_update_prep(&conn, &gid) {
            Prep::Ready(p) => {
                assert_eq!((p.dashboard_remote_id, p.remote_id, p.sources.len()), (9, 41, 1));
            }
            _ => panic!("expected Ready"),
        }
        conn.execute("DELETE FROM dashboard_groups WHERE id = ?1", [&gid]).unwrap();
        assert!(matches!(
            dashboard_group_update_prep(&conn, &gid),
            Prep::Done(Outcome::DropAndLog(_))
        ));
    }

    #[test]
    fn group_create_waits_for_the_parent_dashboards_server_id() {
        let (conn, _, gid) = group_prep_db();
        assert!(matches!(
            dashboard_group_create_prep(&conn, &gid),
            Prep::Done(Outcome::LeavePending(_))
        ));
    }

    #[test]
    fn group_create_is_ready_once_the_parent_is_adopted() {
        let (conn, did, gid) = group_prep_db();
        conn.execute("UPDATE dashboards SET remote_id = 9 WHERE id = ?1", [&did]).unwrap();
        match dashboard_group_create_prep(&conn, &gid) {
            Prep::Ready(p) => {
                assert_eq!((p.dashboard_remote_id, p.name.as_str()), (9, "G"));
                assert_eq!(p.sources.len(), 1);
            }
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn group_create_drops_when_gone_and_succeeds_when_already_pushed() {
        let (conn, did, gid) = group_prep_db();
        conn.execute("UPDATE dashboard_groups SET remote_id = 5 WHERE id = ?1", [&gid]).unwrap();
        assert!(matches!(dashboard_group_create_prep(&conn, &gid), Prep::Done(Outcome::Success)));
        conn.execute("DELETE FROM dashboards WHERE id = ?1", [&did]).unwrap();
        assert!(matches!(
            dashboard_group_create_prep(&conn, &gid),
            Prep::Done(Outcome::DropAndLog(_))
        ));
    }
}

#[cfg(test)]
mod group_order_live {
    use super::*;

    /// Live check of `dashboard_group_order` against local Rails (:3005), run
    /// with `--ignored` and MTASKS_TEST_DATA_DIR pointed at a scratch dir, so
    /// the engine's `open()` uses a scratch database. Creates a dashboard with
    /// three groups, makes several quick local moves, drains the one queue
    /// entry through the engine, reads the web order back, and always deletes
    /// the dashboard.
    #[tokio::test]
    #[ignore]
    async fn reorder_through_the_engine_matches_the_server() {
        let (cfg, _) = crate::config::load();
        let client = ApiClient::new(cfg.api_base_url.trim(), cfg.api_token.unwrap_or_default()).unwrap();
        let created = crate::api::dashboards::create(&client, "zz order check", "").await.unwrap();
        let outcome = run_live(&client, created.id).await;
        crate::api::dashboards::delete(&client, created.id).await.unwrap();
        outcome.unwrap();
    }

    async fn run_live(client: &ApiClient, dashboard: i64) -> Result<(), String> {
        for name in ["one", "two", "three"] {
            let body = serde_json::json!({"dashboard_group": {"name": name, "description": "",
                "color": "#6366f1", "team_ids": [], "project_ids": [], "label_ids": [],
                "all_team_ids": [], "all_project_ids": []}});
            let _: crate::db::dashboards::GroupIn = client
                .post(&format!("api/v1/dashboards/{dashboard}/groups"), &body)
                .await
                .map_err(|e| format!("create group: {e}"))?;
        }
        let conn = open()?;
        let rows = crate::api::dashboards::list(client).await.map_err(|e| e.to_string())?;
        dashboards::apply_snapshot(&conn, &rows).map_err(|e| e.to_string())?;
        let d = dashboards::find_by_remote_id(&conn, dashboard).unwrap().unwrap();
        let ids: Vec<String> =
            dashboards::groups_for(&conn, &d.id).unwrap().into_iter().map(|g| g.id).collect();
        use dashboards::MoveDirection::{Down, Up};
        for (i, dir) in [(2, Up), (2, Up), (1, Down), (0, Down)] {
            dashboards::move_group(&conn, &ids[i], dir).unwrap();
        }
        let remote_order = |conn: &Connection| -> Vec<i64> {
            dashboards::groups_for(conn, &d.id)
                .unwrap()
                .into_iter()
                .map(|g| g.remote_id.unwrap())
                .collect()
        };
        let want = remote_order(&conn);
        let entries = sync_queue::pending_entries(&conn).unwrap();
        let order: Vec<_> =
            entries.iter().filter(|e| e.entity_type == dashboards::GROUP_ORDER_ENTITY_TYPE).collect();
        if order.len() != 1 {
            return Err(format!("expected one coalesced entry, got {}", order.len()));
        }
        let entry_id = order[0].id.unwrap();
        match dashboard_group_order(client, &d.id, entry_id).await {
            Outcome::Success => {}
            _ => return Err("engine did not report success".into()),
        }
        if sync_queue::has_pending_entry(&conn, dashboards::GROUP_ORDER_ENTITY_TYPE, &d.id).unwrap() {
            return Err("entry still pending".into());
        }
        let web = crate::api::dashboards::list(client).await.map_err(|e| e.to_string())?;
        let web_order: Vec<i64> = web
            .iter()
            .find(|x| x.id == dashboard)
            .map(|x| x.groups.iter().map(|g| g.id).collect())
            .unwrap_or_default();
        let local = remote_order(&conn);
        if web_order != want || local != want {
            return Err(format!("want {want:?}, web {web_order:?}, local {local:?}"));
        }
        Ok(())
    }
}
