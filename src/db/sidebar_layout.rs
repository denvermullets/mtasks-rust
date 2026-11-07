//! The user's sidebar groups and per-scope team order as last reported by
//! `GET /me`, kept locally so launch can lay out the sidebar with no network
//! (the same idea as `db::home_page`).
//!
//! Stored relationally (three small tables) rather than as one JSON blob: the
//! one write the desktop makes on its own is flipping a single group's
//! `collapsed`, which is a one-row UPDATE here, and group/team ids stay
//! queryable. Team ids are Rails (remote) ids: a group or the order may name a
//! team this device has not mirrored, and the layout function resolves them.
//!
//! `apply_me` replaces everything wholesale on each pull, except the
//! `collapsed` of groups whose collapse is still waiting to push (see
//! `pending_collapse_group_ids`), so a pull never undoes a local toggle.

use rusqlite::{params, Connection, Result};

use crate::api::SidebarDto;

/// A sidebar group, in display order within `StoredSidebar::groups`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarGroup {
    /// Opaque string id minted by Rails.
    pub id: String,
    pub name: String,
    /// Remote team ids in the group's order. A team may be listed by more
    /// than one group; the first group (in display order) claims it.
    pub team_ids: Vec<i64>,
    pub collapsed: bool,
}

/// What `/me` reported for the sidebar. `load` returns this only once a pull
/// has reported a sidebar; an empty value is a real report of "no groups, no
/// stored order".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredSidebar {
    pub groups: Vec<SidebarGroup>,
    /// Remote team ids, owned teams not in a group.
    pub owned_order: Vec<i64>,
    /// Remote team ids, joined teams not in a group.
    pub joined_order: Vec<i64>,
}

/// Queue `entity_type` for a group's collapsed state awaiting push. The
/// entry's `entity_id` is the Rails group id and its payload is
/// `{"collapsed": bool}`.
pub const QUEUE_ENTITY: &str = "sidebar_group";

/// Group ids whose collapsed state was changed locally and has not pushed yet.
/// `apply_me` keeps those groups' local `collapsed`.
pub fn pending_collapse_group_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT entity_id FROM sync_queue WHERE entity_type = ?1 AND synced_at IS NULL",
    )?;
    let rows = stmt.query_map(params![QUEUE_ENTITY], |r| r.get(0))?;
    rows.collect()
}

/// Set one group's collapsed state locally and, when `queue_push` (a token that
/// can write), queue the push. A newer toggle retires the group's still-pending
/// entry first, so at most one entry per group is ever waiting. Returns false
/// (changing nothing) for an unknown group.
pub fn toggle_local(
    conn: &Connection,
    group_id: &str,
    collapsed: bool,
    queue_push: bool,
) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    if !set_collapsed(&tx, group_id, collapsed)? {
        return Ok(false);
    }
    if queue_push {
        tx.execute(
            "UPDATE sync_queue SET synced_at = ?3
             WHERE entity_type = ?1 AND entity_id = ?2 AND synced_at IS NULL",
            params![QUEUE_ENTITY, group_id, crate::db::now_iso()],
        )?;
        let payload = serde_json::json!({ "collapsed": collapsed }).to_string();
        crate::db::sync_queue::enqueue(&tx, QUEUE_ENTITY, group_id, "update", Some(&payload))?;
    }
    tx.commit()?;
    Ok(true)
}

/// `collapsed` from a queued `sidebar_group` payload.
pub fn parse_payload(payload: Option<&str>) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(payload?).ok()?.get("collapsed")?.as_bool()
}

/// Read the stored layout, or `None` when no pull has reported one (an old
/// server, or never pulled). Never touches the network.
pub fn load(conn: &Connection) -> Result<Option<StoredSidebar>> {
    let reported: bool =
        conn.query_row("SELECT EXISTS(SELECT 1 FROM sidebar_layout_state)", [], |r| r.get(0))?;
    if !reported {
        return Ok(None);
    }
    let mut groups: Vec<SidebarGroup> = {
        let mut stmt = conn
            .prepare("SELECT id, name, collapsed FROM sidebar_groups ORDER BY position")?;
        let rows = stmt.query_map([], |r| {
            Ok(SidebarGroup {
                id: r.get(0)?,
                name: r.get(1)?,
                team_ids: Vec::new(),
                collapsed: r.get::<_, i64>(2)? != 0,
            })
        })?;
        rows.collect::<Result<_>>()?
    };
    {
        let mut stmt = conn.prepare(
            "SELECT group_id, team_remote_id FROM sidebar_group_teams ORDER BY group_id, position",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (group_id, team) = row?;
            if let Some(g) = groups.iter_mut().find(|g| g.id == group_id) {
                g.team_ids.push(team);
            }
        }
    }
    let order = |scope: &str| -> Result<Vec<i64>> {
        let mut stmt = conn.prepare(
            "SELECT team_remote_id FROM sidebar_team_order WHERE scope = ?1 ORDER BY position",
        )?;
        let rows = stmt.query_map(params![scope], |r| r.get(0))?;
        rows.collect()
    };
    Ok(Some(StoredSidebar { groups, owned_order: order("owned")?, joined_order: order("joined")? }))
}

/// Store what `/me` reported, replacing the stored layout. `None` (the server
/// does not report a sidebar) clears it, so `load` returns `None` and the
/// sidebar falls back to its ungrouped layout. Groups with a pending local collapse keep their stored
/// `collapsed` value.
pub fn apply_me(conn: &Connection, reported: Option<&SidebarDto>) -> Result<()> {
    let keep = pending_collapse_group_ids(conn)?;
    apply_keeping_collapsed(conn, reported, &keep)
}

/// `apply_me` with an explicit set of group ids whose local `collapsed` value
/// survives the replacement (ignored for ids the server no longer reports).
pub fn apply_keeping_collapsed(
    conn: &Connection,
    reported: Option<&SidebarDto>,
    keep_collapsed: &[String],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let kept: Vec<(String, bool)> = keep_collapsed
        .iter()
        .filter_map(|id| {
            tx.query_row("SELECT collapsed FROM sidebar_groups WHERE id = ?1", params![id], |r| {
                r.get::<_, i64>(0)
            })
            .ok()
            .map(|c| (id.clone(), c != 0))
        })
        .collect();

    tx.execute("DELETE FROM sidebar_group_teams", [])?;
    tx.execute("DELETE FROM sidebar_groups", [])?;
    tx.execute("DELETE FROM sidebar_team_order", [])?;
    tx.execute("DELETE FROM sidebar_layout_state", [])?;

    if let Some(s) = reported {
        tx.execute("INSERT INTO sidebar_layout_state (id) VALUES (1)", [])?;
        // A duplicated group id keeps its first occurrence.
        let mut seen = std::collections::HashSet::new();
        let mut pos = 0i64;
        for g in &s.groups {
            if !seen.insert(g.id.as_str()) {
                continue;
            }
            let collapsed =
                kept.iter().find(|(id, _)| *id == g.id).map_or(g.collapsed, |(_, c)| *c);
            tx.execute(
                "INSERT INTO sidebar_groups (id, name, position, collapsed) VALUES (?1, ?2, ?3, ?4)",
                params![g.id, g.name, pos, collapsed as i64],
            )?;
            for (i, team) in g.team_ids.iter().enumerate() {
                tx.execute(
                    "INSERT INTO sidebar_group_teams (group_id, position, team_remote_id)
                     VALUES (?1, ?2, ?3)",
                    params![g.id, i as i64, team],
                )?;
            }
            pos += 1;
        }
        for (scope, ids) in [("owned", &s.team_order.owned), ("joined", &s.team_order.joined)] {
            for (i, team) in ids.iter().enumerate() {
                tx.execute(
                    "INSERT INTO sidebar_team_order (scope, position, team_remote_id)
                     VALUES (?1, ?2, ?3)",
                    params![scope, i as i64, team],
                )?;
            }
        }
    }
    tx.commit()
}

/// Set one group's collapsed state locally. Returns false when the group is
/// unknown. Does not queue a push; the collapse-sync work does that.
pub fn set_collapsed(conn: &Connection, group_id: &str, collapsed: bool) -> Result<bool> {
    let n = conn.execute(
        "UPDATE sidebar_groups SET collapsed = ?2 WHERE id = ?1",
        params![group_id, collapsed as i64],
    )?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::users::{SidebarGroupDto, SidebarTeamOrderDto};
    use crate::db::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    fn group(id: &str, name: &str, teams: &[i64], collapsed: bool) -> SidebarGroupDto {
        SidebarGroupDto {
            id: id.into(),
            name: name.into(),
            team_ids: teams.to_vec(),
            collapsed,
        }
    }

    fn dto() -> SidebarDto {
        SidebarDto {
            groups: vec![group("g1", "Bozo", &[12, 3], false), group("g2", "Later", &[], true)],
            team_order: SidebarTeamOrderDto { owned: vec![3, 12], joined: vec![8, 9] },
        }
    }

    #[test]
    fn a_store_nothing_was_pulled_into_loads_as_none() {
        assert_eq!(load(&mem()).unwrap(), None);
    }

    #[test]
    fn round_trips_groups_team_order_and_collapsed() {
        let conn = mem();
        apply_me(&conn, Some(&dto())).unwrap();
        let s = load(&conn).unwrap().unwrap();
        assert_eq!(
            s.groups,
            vec![
                SidebarGroup {
                    id: "g1".into(),
                    name: "Bozo".into(),
                    team_ids: vec![12, 3],
                    collapsed: false
                },
                SidebarGroup {
                    id: "g2".into(),
                    name: "Later".into(),
                    team_ids: vec![],
                    collapsed: true
                },
            ]
        );
        assert_eq!(s.owned_order, vec![3, 12]);
        assert_eq!(s.joined_order, vec![8, 9]);
    }

    #[test]
    fn each_pull_replaces_the_stored_layout_wholesale() {
        let conn = mem();
        apply_me(&conn, Some(&dto())).unwrap();
        let next = SidebarDto {
            groups: vec![group("g3", "New", &[8], false)],
            team_order: SidebarTeamOrderDto { owned: vec![1], joined: vec![] },
        };
        apply_me(&conn, Some(&next)).unwrap();
        let s = load(&conn).unwrap().unwrap();
        assert_eq!(s.groups.len(), 1);
        assert_eq!(s.groups[0].id, "g3");
        assert_eq!(s.groups[0].team_ids, vec![8]);
        assert_eq!(s.owned_order, vec![1]);
        assert!(s.joined_order.is_empty());
    }

    #[test]
    fn an_empty_or_absent_sidebar_clears_the_store() {
        let conn = mem();
        apply_me(&conn, Some(&dto())).unwrap();
        apply_me(&conn, Some(&SidebarDto::default())).unwrap();
        // Reported but empty is a real layout, not "never pulled".
        assert_eq!(load(&conn).unwrap(), Some(StoredSidebar::default()));
        apply_me(&conn, Some(&dto())).unwrap();
        apply_me(&conn, None).unwrap();
        assert_eq!(load(&conn).unwrap(), None);
    }

    #[test]
    fn a_duplicated_group_id_keeps_the_first() {
        let conn = mem();
        let d = SidebarDto {
            groups: vec![group("g1", "A", &[1], false), group("g1", "B", &[2], true)],
            team_order: Default::default(),
        };
        apply_me(&conn, Some(&d)).unwrap();
        let s = load(&conn).unwrap().unwrap();
        assert_eq!(s.groups.len(), 1);
        assert_eq!(s.groups[0].name, "A");
    }

    #[test]
    fn set_collapsed_changes_one_group_locally() {
        let conn = mem();
        apply_me(&conn, Some(&dto())).unwrap();
        assert!(set_collapsed(&conn, "g1", true).unwrap());
        assert!(set_collapsed(&conn, "g2", false).unwrap());
        assert!(!set_collapsed(&conn, "nope", true).unwrap());
        let s = load(&conn).unwrap().unwrap();
        assert!(s.groups[0].collapsed);
        assert!(!s.groups[1].collapsed);
    }

    fn queued(conn: &Connection) -> Vec<crate::db::models::SyncQueueEntry> {
        crate::db::sync_queue::pending_entries(conn).unwrap()
    }

    #[test]
    fn a_toggle_queues_one_push_and_a_newer_toggle_replaces_it() {
        let conn = mem();
        apply_me(&conn, Some(&dto())).unwrap();
        assert!(toggle_local(&conn, "g1", true, true).unwrap());
        let q = queued(&conn);
        assert_eq!(q.len(), 1);
        assert_eq!((q[0].entity_type.as_str(), q[0].entity_id.as_str()), (QUEUE_ENTITY, "g1"));
        assert_eq!(parse_payload(q[0].payload.as_deref()), Some(true));

        // Toggling back coalesces: still one entry, now carrying the newest value.
        assert!(toggle_local(&conn, "g1", false, true).unwrap());
        let q = queued(&conn);
        assert_eq!(q.len(), 1);
        assert_eq!(parse_payload(q[0].payload.as_deref()), Some(false));

        // Another group queues its own entry.
        assert!(toggle_local(&conn, "g2", false, true).unwrap());
        assert_eq!(queued(&conn).len(), 2);
        let mut pending = pending_collapse_group_ids(&conn).unwrap();
        pending.sort();
        assert_eq!(pending, vec!["g1".to_string(), "g2".to_string()]);
    }

    #[test]
    fn a_read_only_toggle_stays_local_and_queues_nothing() {
        let conn = mem();
        apply_me(&conn, Some(&dto())).unwrap();
        assert!(toggle_local(&conn, "g1", true, false).unwrap());
        assert!(queued(&conn).is_empty());
        assert!(load(&conn).unwrap().unwrap().groups[0].collapsed);
        // The next pull resets it to the server value.
        apply_me(&conn, Some(&dto())).unwrap();
        assert!(!load(&conn).unwrap().unwrap().groups[0].collapsed);
    }

    #[test]
    fn toggling_an_unknown_group_changes_and_queues_nothing() {
        let conn = mem();
        apply_me(&conn, Some(&dto())).unwrap();
        assert!(!toggle_local(&conn, "nope", true, true).unwrap());
        assert!(queued(&conn).is_empty());
    }

    #[test]
    fn a_pull_between_toggle_and_push_does_not_revert_the_toggle() {
        let conn = mem();
        apply_me(&conn, Some(&dto())).unwrap();
        toggle_local(&conn, "g1", true, true).unwrap();
        // The server still reports g1 expanded.
        apply_me(&conn, Some(&dto())).unwrap();
        assert!(load(&conn).unwrap().unwrap().groups[0].collapsed);
        // Once the push has landed, the server value rules again.
        let id = queued(&conn)[0].id.unwrap();
        crate::db::sync_queue::mark_synced(&conn, id).unwrap();
        apply_me(&conn, Some(&dto())).unwrap();
        assert!(!load(&conn).unwrap().unwrap().groups[0].collapsed);
    }

    #[test]
    fn a_pending_local_collapse_survives_the_pull() {
        let conn = mem();
        apply_me(&conn, Some(&dto())).unwrap();
        set_collapsed(&conn, "g1", true).unwrap();
        // The server still says g1 is expanded and g2 collapsed.
        apply_keeping_collapsed(&conn, Some(&dto()), &["g1".to_string(), "gone".to_string()])
            .unwrap();
        let s = load(&conn).unwrap().unwrap();
        assert!(s.groups[0].collapsed, "kept the local value");
        assert!(s.groups[1].collapsed, "unlisted groups take the server value");
        // Without the hook the server value wins.
        apply_me(&conn, Some(&dto())).unwrap();
        assert!(!load(&conn).unwrap().unwrap().groups[0].collapsed);
    }
}
