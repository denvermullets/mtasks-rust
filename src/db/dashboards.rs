//! Local mirror of the user's Dashboards, their groups and group sources.
//! Pure over a `rusqlite::Connection`: no Iced, no HTTP. The Data sync feeds it
//! the decoded `GET /api/v1/dashboards` snapshot.
//!
//! Sources are stored by their remote id (they point at server records) and
//! translated through the mirror at evaluation time.
use std::collections::HashSet;

use rusqlite::{params, Connection, OptionalExtension, Result};
use serde::Deserialize;
use uuid::Uuid;

use super::sync_queue;

/// `sync_queue.entity_type` for dashboard writes.
pub const ENTITY_TYPE: &str = "dashboard";
/// `sync_queue.entity_type` for dashboard group writes.
pub const GROUP_ENTITY_TYPE: &str = "dashboard_group";
/// `sync_queue.entity_type` for a dashboard's group order (entity_id = dashboard local id).
pub const GROUP_ORDER_ENTITY_TYPE: &str = "dashboard_group_order";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dashboard {
    /// Local uuid.
    pub id: String,
    pub remote_id: Option<i64>,
    pub name: String,
    pub description: String,
    pub position: i64,
    pub updated_at: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardGroup {
    /// Local uuid.
    pub id: String,
    pub remote_id: Option<i64>,
    /// Local dashboard uuid.
    pub dashboard_id: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub position: i64,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceType {
    Team,
    Project,
    Label,
}

impl SourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceType::Team => "team",
            SourceType::Project => "project",
            SourceType::Label => "label",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "team" => Some(SourceType::Team),
            "project" => Some(SourceType::Project),
            "label" => Some(SourceType::Label),
            _ => None,
        }
    }
}

/// One source of a group. `include_all` is meaningful for Team and Project only.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupSource {
    pub source_type: SourceType,
    /// The source's remote id.
    pub remote_id: i64,
    pub include_all: bool,
}

/// One group of a `GET /api/v1/dashboards` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct GroupIn {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub team_ids: Vec<i64>,
    #[serde(default)]
    pub project_ids: Vec<i64>,
    #[serde(default)]
    pub label_ids: Vec<i64>,
    #[serde(default)]
    pub all_team_ids: Vec<i64>,
    #[serde(default)]
    pub all_project_ids: Vec<i64>,
}

/// One entry of `GET /api/v1/dashboards`.
#[derive(Debug, Clone, Deserialize)]
pub struct DashboardIn {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub groups: Vec<GroupIn>,
}

/// What an apply did, for the sync log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub dashboards_upserted: usize,
    pub dashboards_removed: usize,
    pub groups_upserted: usize,
    pub groups_removed: usize,
    /// Dashboards/groups left alone because a local write is pending.
    pub skipped_pending: usize,
}

fn pending(conn: &Connection, entity_type: &str, id: &str) -> Result<bool> {
    sync_queue::has_pending_entry(conn, entity_type, id)
}

/// True while a delete for `remote_id` is queued and unpushed (payload
/// `{"remote_id": n}`); the local row is gone then, so a pull must not bring it back.
fn has_pending_delete(conn: &Connection, entity_type: &str, remote_id: i64) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_queue
         WHERE entity_type = ?1 AND action = 'delete' AND synced_at IS NULL
           AND json_extract(payload, '$.remote_id') = ?2)",
        params![entity_type, remote_id],
        |r| r.get(0),
    )
}

/// Apply a full snapshot: upsert by remote id, remove mirrored dashboards and
/// groups missing from it. Never removes rows without a remote id, and leaves
/// alone any dashboard or group with a pending queued write (a pending group
/// order protects the dashboard's group positions).
pub fn apply_snapshot(conn: &Connection, rows: &[DashboardIn]) -> Result<Applied> {
    let tx = conn.unchecked_transaction()?;
    let mut applied = Applied::default();
    let mut seen_dashboards: HashSet<i64> = HashSet::new();

    for row in rows {
        seen_dashboards.insert(row.id);
        if has_pending_delete(&tx, ENTITY_TYPE, row.id)? {
            applied.skipped_pending += 1;
            continue;
        }
        let existing = find_by_remote_id(&tx, row.id)?;
        let local_id = match &existing {
            Some(d) => d.id.clone(),
            None => Uuid::new_v4().to_string(),
        };
        if existing.is_some() && pending(&tx, ENTITY_TYPE, &local_id)? {
            applied.skipped_pending += 1;
        } else {
            tx.execute(
                "INSERT INTO dashboards (id, remote_id, name, description, position, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(remote_id) DO UPDATE SET
                   name = excluded.name, description = excluded.description,
                   position = excluded.position, updated_at = excluded.updated_at",
                params![
                    local_id,
                    row.id,
                    row.name,
                    row.description.clone().unwrap_or_default(),
                    row.position,
                    row.updated_at
                ],
            )?;
            applied.dashboards_upserted += 1;
        }
        apply_groups(&tx, &local_id, row, &mut applied)?;
    }

    // Mirrored dashboards the server no longer lists.
    let stale: Vec<(String, i64)> = {
        let mut stmt = tx.prepare("SELECT id, remote_id FROM dashboards WHERE remote_id IS NOT NULL")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect::<Result<Vec<_>>>()?
    };
    for (id, remote_id) in stale {
        if seen_dashboards.contains(&remote_id) {
            continue;
        }
        if pending(&tx, ENTITY_TYPE, &id)?
            || pending(&tx, GROUP_ORDER_ENTITY_TYPE, &id)?
            || has_unsynced_groups(&tx, &id)?
        {
            applied.skipped_pending += 1;
            continue;
        }
        tx.execute("DELETE FROM dashboards WHERE id = ?1", params![id])?;
        applied.dashboards_removed += 1;
    }
    tx.commit()?;
    Ok(applied)
}

/// A dashboard's groups include a local-only create or one with a pending write.
fn has_unsynced_groups(conn: &Connection, dashboard_id: &str) -> Result<bool> {
    for g in groups_for(conn, dashboard_id)? {
        if g.remote_id.is_none() || pending(conn, GROUP_ENTITY_TYPE, &g.id)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn apply_groups(conn: &Connection, dashboard_id: &str, row: &DashboardIn, applied: &mut Applied) -> Result<()> {
    let order_pending = pending(conn, GROUP_ORDER_ENTITY_TYPE, dashboard_id)?;
    let mut seen: HashSet<i64> = HashSet::new();
    for g in &row.groups {
        seen.insert(g.id);
        if has_pending_delete(conn, GROUP_ENTITY_TYPE, g.id)? {
            applied.skipped_pending += 1;
            continue;
        }
        let existing = find_group_by_remote_id(conn, g.id)?;
        let local_id = match &existing {
            Some(e) => e.id.clone(),
            None => Uuid::new_v4().to_string(),
        };
        if let Some(e) = &existing {
            if pending(conn, GROUP_ENTITY_TYPE, &e.id)? {
                applied.skipped_pending += 1;
                continue;
            }
            // A pending order owns the position; keep the local one.
            if order_pending {
                conn.execute(
                    "UPDATE dashboard_groups SET dashboard_id = ?2, name = ?3, description = ?4, color = ?5
                     WHERE id = ?1",
                    params![e.id, dashboard_id, g.name, g.description.clone().unwrap_or_default(), color_of(g)],
                )?;
                replace_sources(conn, &e.id, g)?;
                applied.groups_upserted += 1;
                continue;
            }
        }
        conn.execute(
            "INSERT INTO dashboard_groups (id, remote_id, dashboard_id, name, description, color, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(remote_id) DO UPDATE SET
               dashboard_id = excluded.dashboard_id, name = excluded.name,
               description = excluded.description, color = excluded.color,
               position = excluded.position",
            params![
                local_id,
                g.id,
                dashboard_id,
                g.name,
                g.description.clone().unwrap_or_default(),
                color_of(g),
                g.position
            ],
        )?;
        replace_sources(conn, &local_id, g)?;
        applied.groups_upserted += 1;
    }
    // Mirrored groups of this dashboard that the server no longer lists.
    for g in groups_for(conn, dashboard_id)? {
        let Some(remote) = g.remote_id else { continue };
        if seen.contains(&remote) {
            continue;
        }
        if pending(conn, GROUP_ENTITY_TYPE, &g.id)? {
            applied.skipped_pending += 1;
            continue;
        }
        conn.execute("DELETE FROM dashboard_groups WHERE id = ?1", params![g.id])?;
        applied.groups_removed += 1;
    }
    Ok(())
}

fn color_of(g: &GroupIn) -> String {
    g.color.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| "#6366f1".to_string())
}

fn replace_sources(conn: &Connection, group_id: &str, g: &GroupIn) -> Result<()> {
    conn.execute("DELETE FROM dashboard_group_sources WHERE group_id = ?1", params![group_id])?;
    let insert = |t: SourceType, ids: &[i64], all: &[i64]| -> Result<()> {
        for id in ids {
            conn.execute(
                "INSERT OR IGNORE INTO dashboard_group_sources (group_id, source_type, source_remote_id, include_all)
                 VALUES (?1, ?2, ?3, ?4)",
                params![group_id, t.as_str(), id, all.contains(id) as i64],
            )?;
        }
        Ok(())
    };
    insert(SourceType::Team, &g.team_ids, &g.all_team_ids)?;
    insert(SourceType::Project, &g.project_ids, &g.all_project_ids)?;
    insert(SourceType::Label, &g.label_ids, &[])?;
    Ok(())
}

/// Longest dashboard name Rails accepts.
pub const MAX_NAME_LEN: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    Blank,
    TooLong,
}

impl NameError {
    pub fn message(self) -> &'static str {
        match self {
            NameError::Blank => "Name can't be blank",
            NameError::TooLong => "Name must be 80 characters or fewer",
        }
    }
}

/// Local check of a dashboard name (matches Rails): trimmed, 1 to 80 characters.
pub fn validate_name(name: &str) -> std::result::Result<String, NameError> {
    let name = name.trim();
    if name.is_empty() {
        Err(NameError::Blank)
    } else if name.chars().count() > MAX_NAME_LEN {
        Err(NameError::TooLong)
    } else {
        Ok(name.to_string())
    }
}

#[derive(Debug)]
pub enum CreateError {
    Name(NameError),
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for CreateError {
    fn from(e: rusqlite::Error) -> Self {
        CreateError::Db(e)
    }
}

/// Create a dashboard offline: validate, append after the last position, and
/// queue a "create" entry. The push ticket reads the row, so no payload.
pub fn create_local(
    conn: &Connection,
    name: &str,
    description: &str,
) -> std::result::Result<Dashboard, CreateError> {
    let name = validate_name(name).map_err(CreateError::Name)?;
    let tx = conn.unchecked_transaction()?;
    let position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM dashboards",
        [],
        |r| r.get(0),
    )?;
    let id = Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO dashboards (id, remote_id, name, description, position, updated_at)
         VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
        params![id, name, description.trim(), position, crate::db::now_iso()],
    )?;
    sync_queue::enqueue(&tx, ENTITY_TYPE, &id, "create", None)?;
    tx.commit()?;
    Ok(get(conn, &id)?.expect("dashboard just inserted"))
}

pub fn find_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<Dashboard>> {
    conn.query_row(
        "SELECT id, remote_id, name, description, position, updated_at FROM dashboards WHERE remote_id = ?1",
        params![remote_id],
        dashboard_row,
    )
    .optional()
}

/// A dashboard by local id.
pub fn get(conn: &Connection, id: &str) -> Result<Option<Dashboard>> {
    conn.query_row(
        "SELECT id, remote_id, name, description, position, updated_at FROM dashboards WHERE id = ?1",
        params![id],
        dashboard_row,
    )
    .optional()
}

/// Every dashboard in sidebar order (position, then name, then id).
pub fn list(conn: &Connection) -> Result<Vec<Dashboard>> {
    let mut stmt = conn.prepare(
        "SELECT id, remote_id, name, description, position, updated_at
         FROM dashboards ORDER BY position, name, id",
    )?;
    let rows = stmt.query_map([], dashboard_row)?;
    rows.collect()
}

pub fn find_group_by_remote_id(conn: &Connection, remote_id: i64) -> Result<Option<DashboardGroup>> {
    conn.query_row(
        "SELECT id, remote_id, dashboard_id, name, description, color, position
         FROM dashboard_groups WHERE remote_id = ?1",
        params![remote_id],
        group_row,
    )
    .optional()
}

/// A dashboard's groups in display order (position, then id).
pub fn groups_for(conn: &Connection, dashboard_id: &str) -> Result<Vec<DashboardGroup>> {
    let mut stmt = conn.prepare(
        "SELECT id, remote_id, dashboard_id, name, description, color, position
         FROM dashboard_groups WHERE dashboard_id = ?1 ORDER BY position, id",
    )?;
    let rows = stmt.query_map(params![dashboard_id], group_row)?;
    rows.collect()
}

/// A group's sources, ordered by type then remote id.
#[allow(dead_code)]
pub fn sources_for(conn: &Connection, group_id: &str) -> Result<Vec<GroupSource>> {
    let mut stmt = conn.prepare(
        "SELECT source_type, source_remote_id, include_all FROM dashboard_group_sources
         WHERE group_id = ?1 ORDER BY source_type, source_remote_id",
    )?;
    let rows = stmt.query_map(params![group_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (t, id, all) = r?;
        if let Some(source_type) = SourceType::parse(&t) {
            out.push(GroupSource { source_type, remote_id: id, include_all: all != 0 });
        }
    }
    Ok(out)
}

fn dashboard_row(r: &rusqlite::Row) -> Result<Dashboard> {
    Ok(Dashboard {
        id: r.get(0)?,
        remote_id: r.get(1)?,
        name: r.get(2)?,
        description: r.get(3)?,
        position: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

fn group_row(r: &rusqlite::Row) -> Result<DashboardGroup> {
    Ok(DashboardGroup {
        id: r.get(0)?,
        remote_id: r.get(1)?,
        dashboard_id: r.get(2)?,
        name: r.get(3)?,
        description: r.get(4)?,
        color: r.get(5)?,
        position: r.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        conn
    }

    fn group(id: i64, name: &str, pos: i64) -> GroupIn {
        GroupIn {
            id,
            name: name.into(),
            description: None,
            color: Some("#ec4899".into()),
            position: pos,
            team_ids: vec![],
            project_ids: vec![],
            label_ids: vec![],
            all_team_ids: vec![],
            all_project_ids: vec![],
        }
    }

    fn dash(id: i64, name: &str, pos: i64, groups: Vec<GroupIn>) -> DashboardIn {
        DashboardIn {
            id,
            name: name.into(),
            description: Some("d".into()),
            position: pos,
            updated_at: "2026-01-01T00:00:00.000Z".into(),
            groups,
        }
    }

    #[test]
    fn inserts_dashboards_groups_and_sources_by_remote_id() {
        let conn = setup();
        let mut g = group(10, "G", 0);
        g.team_ids = vec![1, 2];
        g.all_team_ids = vec![2];
        g.project_ids = vec![5];
        g.label_ids = vec![7];
        let a = apply_snapshot(&conn, &[dash(1, "B", 1, vec![g]), dash(2, "A", 0, vec![])]).unwrap();
        assert_eq!((a.dashboards_upserted, a.groups_upserted), (2, 1));
        let names: Vec<_> = list(&conn).unwrap().into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["A", "B"]);
        let d = find_by_remote_id(&conn, 1).unwrap().unwrap();
        let gs = groups_for(&conn, &d.id).unwrap();
        assert_eq!(gs[0].color, "#ec4899");
        let src = sources_for(&conn, &gs[0].id).unwrap();
        assert_eq!(src.len(), 4);
        let t2 = src.iter().find(|s| s.source_type == SourceType::Team && s.remote_id == 2).unwrap();
        assert!(t2.include_all);
        let t1 = src.iter().find(|s| s.source_type == SourceType::Team && s.remote_id == 1).unwrap();
        assert!(!t1.include_all);
        assert!(src.iter().any(|s| s.source_type == SourceType::Label && s.remote_id == 7));
    }

    #[test]
    fn updates_in_place_keeping_local_ids_and_replaces_sources() {
        let conn = setup();
        let mut g = group(10, "G", 0);
        g.team_ids = vec![1];
        apply_snapshot(&conn, &[dash(1, "A", 0, vec![g])]).unwrap();
        let before = find_by_remote_id(&conn, 1).unwrap().unwrap();
        let gid = groups_for(&conn, &before.id).unwrap()[0].id.clone();
        let mut g2 = group(10, "G2", 3);
        g2.label_ids = vec![8];
        apply_snapshot(&conn, &[dash(1, "A2", 0, vec![g2])]).unwrap();
        let after = find_by_remote_id(&conn, 1).unwrap().unwrap();
        assert_eq!((after.id.as_str(), after.name.as_str()), (before.id.as_str(), "A2"));
        let gs = groups_for(&conn, &after.id).unwrap();
        assert_eq!((gs[0].id.as_str(), gs[0].name.as_str(), gs[0].position), (gid.as_str(), "G2", 3));
        let src = sources_for(&conn, &gid).unwrap();
        assert_eq!(src, vec![GroupSource { source_type: SourceType::Label, remote_id: 8, include_all: false }]);
    }

    #[test]
    fn removes_dashboards_and_groups_missing_from_the_snapshot() {
        let conn = setup();
        apply_snapshot(
            &conn,
            &[dash(1, "A", 0, vec![group(10, "G", 0), group(11, "H", 1)]), dash(2, "B", 1, vec![])],
        )
        .unwrap();
        let a = apply_snapshot(&conn, &[dash(1, "A", 0, vec![group(11, "H", 0)])]).unwrap();
        assert_eq!((a.dashboards_removed, a.groups_removed), (1, 1));
        assert!(find_by_remote_id(&conn, 2).unwrap().is_none());
        assert!(find_group_by_remote_id(&conn, 10).unwrap().is_none());
        assert!(find_group_by_remote_id(&conn, 11).unwrap().is_some());
        // Cascade leaves no orphan groups.
        apply_snapshot(&conn, &[]).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM dashboard_groups", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn never_removes_local_only_creates() {
        let conn = setup();
        conn.execute(
            "INSERT INTO dashboards (id, remote_id, name, position, updated_at) VALUES ('loc', NULL, 'L', 9, '')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO dashboard_groups (id, remote_id, dashboard_id, name) VALUES ('lg', NULL, 'loc', 'G')",
            [],
        )
        .unwrap();
        apply_snapshot(&conn, &[dash(1, "A", 0, vec![])]).unwrap();
        assert!(get(&conn, "loc").unwrap().is_some());
        assert_eq!(groups_for(&conn, "loc").unwrap().len(), 1);
        // A local-only group under a mirrored dashboard survives its siblings' removal.
        apply_snapshot(&conn, &[dash(1, "A", 0, vec![group(10, "G", 0)])]).unwrap();
        let d = find_by_remote_id(&conn, 1).unwrap().unwrap();
        conn.execute(
            "INSERT INTO dashboard_groups (id, remote_id, dashboard_id, name) VALUES ('lg2', NULL, ?1, 'N')",
            params![d.id],
        )
        .unwrap();
        apply_snapshot(&conn, &[dash(1, "A", 0, vec![])]).unwrap();
        let ids: Vec<_> = groups_for(&conn, &d.id).unwrap().into_iter().map(|g| g.id).collect();
        assert_eq!(ids, ["lg2"]);
        // And the dashboard itself stays while it has a local-only group.
        apply_snapshot(&conn, &[]).unwrap();
        assert!(get(&conn, &d.id).unwrap().is_some());
    }

    #[test]
    fn leaves_rows_with_pending_writes_alone() {
        let conn = setup();
        apply_snapshot(&conn, &[dash(1, "A", 0, vec![group(10, "G", 0)])]).unwrap();
        let d = find_by_remote_id(&conn, 1).unwrap().unwrap();
        let g = find_group_by_remote_id(&conn, 10).unwrap().unwrap();
        sync_queue::enqueue(&conn, ENTITY_TYPE, &d.id, "update", None).unwrap();
        sync_queue::enqueue(&conn, GROUP_ENTITY_TYPE, &g.id, "update", None).unwrap();
        let a = apply_snapshot(&conn, &[dash(1, "Server", 5, vec![group(10, "Server", 5)])]).unwrap();
        assert_eq!(a.skipped_pending, 2);
        assert_eq!(get(&conn, &d.id).unwrap().unwrap().name, "A");
        assert_eq!(groups_for(&conn, &d.id).unwrap()[0].name, "G");
        // Missing from the snapshot but pending: kept.
        apply_snapshot(&conn, &[]).unwrap();
        assert!(get(&conn, &d.id).unwrap().is_some());
        assert!(find_group_by_remote_id(&conn, 10).unwrap().is_some());
        // Once pushed, the server wins again.
        conn.execute("UPDATE sync_queue SET synced_at = 'x'", []).unwrap();
        apply_snapshot(&conn, &[dash(1, "Server", 5, vec![group(10, "Server", 5)])]).unwrap();
        assert_eq!(get(&conn, &d.id).unwrap().unwrap().name, "Server");
    }

    #[test]
    fn a_pending_group_order_keeps_local_positions() {
        let conn = setup();
        apply_snapshot(&conn, &[dash(1, "A", 0, vec![group(10, "G", 0), group(11, "H", 1)])]).unwrap();
        let d = find_by_remote_id(&conn, 1).unwrap().unwrap();
        sync_queue::enqueue(&conn, GROUP_ORDER_ENTITY_TYPE, &d.id, "update", None).unwrap();
        apply_snapshot(&conn, &[dash(1, "A", 0, vec![group(10, "G", 5), group(11, "H2", 6)])]).unwrap();
        let gs = groups_for(&conn, &d.id).unwrap();
        assert_eq!((gs[0].position, gs[1].position, gs[1].name.as_str()), (0, 1, "H2"));
    }

    #[test]
    fn a_pull_does_not_resurrect_a_dashboard_or_group_with_a_pending_delete() {
        let conn = setup();
        apply_snapshot(&conn, &[dash(1, "A", 0, vec![group(10, "G", 0)])]).unwrap();
        let d = find_by_remote_id(&conn, 1).unwrap().unwrap();
        let g = find_group_by_remote_id(&conn, 10).unwrap().unwrap();
        conn.execute("DELETE FROM dashboard_groups WHERE id = ?1", params![g.id]).unwrap();
        sync_queue::enqueue(&conn, GROUP_ENTITY_TYPE, &g.id, "delete", Some(r#"{"remote_id":10}"#)).unwrap();
        conn.execute("DELETE FROM dashboards WHERE id = ?1", params![d.id]).unwrap();
        sync_queue::enqueue(&conn, ENTITY_TYPE, &d.id, "delete", Some(r#"{"remote_id":1}"#)).unwrap();
        apply_snapshot(&conn, &[dash(1, "A", 0, vec![group(10, "G", 0)]), dash(2, "B", 1, vec![])]).unwrap();
        assert!(find_by_remote_id(&conn, 1).unwrap().is_none());
        assert!(find_group_by_remote_id(&conn, 10).unwrap().is_none());
        assert!(find_by_remote_id(&conn, 2).unwrap().is_some());
    }

    #[test]
    fn create_local_appends_after_the_last_position_and_queues_a_create() {
        let conn = setup();
        apply_snapshot(&conn, &[dash(1, "A", 4, vec![])]).unwrap();
        let a = create_local(&conn, "  Mine  ", " about ").unwrap();
        let b = create_local(&conn, "Next", "").unwrap();
        assert_eq!((a.name.as_str(), a.description.as_str(), a.position), ("Mine", "about", 5));
        assert_eq!((b.position, b.remote_id), (6, None));
        let names: Vec<_> = list(&conn).unwrap().into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["A", "Mine", "Next"]);
        let q = sync_queue::pending_entries(&conn).unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(
            (q[0].entity_type.as_str(), q[0].action.as_str(), q[0].entity_id.as_str()),
            (ENTITY_TYPE, "create", a.id.as_str())
        );
    }

    #[test]
    fn create_local_first_dashboard_gets_position_zero() {
        let conn = setup();
        assert_eq!(create_local(&conn, "First", "").unwrap().position, 0);
    }

    #[test]
    fn create_local_rejects_blank_and_overlong_names_without_writing() {
        let conn = setup();
        assert!(matches!(create_local(&conn, "   ", ""), Err(CreateError::Name(NameError::Blank))));
        assert!(matches!(
            create_local(&conn, &"x".repeat(81), ""),
            Err(CreateError::Name(NameError::TooLong))
        ));
        assert_eq!(list(&conn).unwrap().len(), 0);
        assert_eq!(sync_queue::pending_entries(&conn).unwrap().len(), 0);
        assert!(create_local(&conn, &"x".repeat(80), "").is_ok());
        assert!(create_local(&conn, &"é".repeat(80), "").is_ok());
    }

    #[test]
    fn a_created_dashboard_survives_a_pull_that_does_not_list_it() {
        let conn = setup();
        let d = create_local(&conn, "Local", "").unwrap();
        apply_snapshot(&conn, &[]).unwrap();
        assert!(get(&conn, &d.id).unwrap().is_some());
    }
}
