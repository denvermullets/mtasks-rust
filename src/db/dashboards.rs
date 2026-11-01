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
        super::home_page::clear_if_dashboard(&tx, remote_id)?;
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

/// Rename a dashboard and/or change its description: validate, write the row,
/// and queue an "update" entry (no payload; the push reads the row). A pull
/// leaves the row alone while that entry is pending.
pub fn update_local(
    conn: &Connection,
    id: &str,
    name: &str,
    description: &str,
) -> std::result::Result<Dashboard, CreateError> {
    let name = validate_name(name).map_err(CreateError::Name)?;
    let tx = conn.unchecked_transaction()?;
    let changed = tx.execute(
        "UPDATE dashboards SET name = ?2, description = ?3 WHERE id = ?1",
        params![id, name, description.trim()],
    )?;
    if changed == 0 {
        return Err(CreateError::Db(rusqlite::Error::QueryReturnedNoRows));
    }
    sync_queue::enqueue(&tx, ENTITY_TYPE, id, "update", None)?;
    tx.commit()?;
    Ok(get(conn, id)?.expect("dashboard just updated"))
}

/// Which way to move a group in its dashboard's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveDirection {
    Up,
    Down,
}

/// Move a group one place up or down, renumber the dashboard's groups 0..n
/// locally, and queue the new order. All moves on one dashboard coalesce into
/// a single pending `GROUP_ORDER_ENTITY_TYPE` entry (entity_id = dashboard
/// local id, action "update") whose payload `{"group_ids": [..]}` holds the
/// final order as local group ids. Returns false at an edge (nothing written).
pub fn move_group(conn: &Connection, group_id: &str, direction: MoveDirection) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    let dashboard_id: String = tx.query_row(
        "SELECT dashboard_id FROM dashboard_groups WHERE id = ?1",
        params![group_id],
        |r| r.get(0),
    )?;
    let mut ids: Vec<String> = groups_for(&tx, &dashboard_id)?.into_iter().map(|g| g.id).collect();
    let Some(at) = ids.iter().position(|i| i == group_id) else { return Ok(false) };
    let to = match direction {
        MoveDirection::Up if at > 0 => at - 1,
        MoveDirection::Down if at + 1 < ids.len() => at + 1,
        _ => return Ok(false),
    };
    ids.swap(at, to);
    for (pos, id) in ids.iter().enumerate() {
        tx.execute("UPDATE dashboard_groups SET position = ?2 WHERE id = ?1", params![id, pos as i64])?;
    }
    let payload = serde_json::json!({ "group_ids": ids }).to_string();
    let updated = tx.execute(
        "UPDATE sync_queue SET payload = ?3
         WHERE entity_type = ?1 AND entity_id = ?2 AND synced_at IS NULL AND attempted_at IS NULL",
        params![GROUP_ORDER_ENTITY_TYPE, dashboard_id, payload],
    )?;
    if updated == 0 {
        sync_queue::enqueue(&tx, GROUP_ORDER_ENTITY_TYPE, &dashboard_id, "update", Some(&payload))?;
    }
    tx.commit()?;
    Ok(true)
}

/// What a group-order push needs, read fresh from the database.
#[derive(Debug, PartialEq, Eq)]
pub enum OrderPlan {
    /// The dashboard's remote id, every group's remote id in display order, and
    /// the queue payload they were resolved from (for `finish_order_push`).
    Ready { dashboard_remote_id: i64, group_remote_ids: Vec<i64>, payload: String },
    /// The dashboard or one of its groups has not been created on the server yet.
    Waiting(&'static str),
    /// Nothing left to push (dashboard gone, or no group of the order remains).
    Gone,
}

/// Resolve queue entry `entry_id`'s local group ids to remote ids. Re-reads the
/// payload, since later moves rewrite it in place while the entry is pending.
pub fn plan_order_push(conn: &Connection, entry_id: i64, dashboard_id: &str) -> Result<OrderPlan> {
    let Some(dashboard) = get(conn, dashboard_id)? else { return Ok(OrderPlan::Gone) };
    let payload: Option<String> = conn
        .query_row("SELECT payload FROM sync_queue WHERE id = ?1", params![entry_id], |r| r.get(0))
        .optional()?
        .flatten();
    let Some(payload) = payload else { return Ok(OrderPlan::Gone) };
    let local_ids: Vec<String> = serde_json::from_str::<serde_json::Value>(&payload)
        .ok()
        .and_then(|v| v.get("group_ids").cloned())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let Some(dashboard_remote_id) = dashboard.remote_id else {
        return Ok(OrderPlan::Waiting("dashboard create not yet drained"));
    };
    let groups = groups_for(conn, dashboard_id)?;
    let mut group_remote_ids = Vec::new();
    for id in &local_ids {
        // A group deleted since the move is just left out.
        let Some(group) = groups.iter().find(|g| &g.id == id) else { continue };
        match group.remote_id {
            Some(r) => group_remote_ids.push(r),
            None => return Ok(OrderPlan::Waiting("group create not yet drained")),
        }
    }
    if group_remote_ids.is_empty() {
        return Ok(OrderPlan::Gone);
    }
    Ok(OrderPlan::Ready { dashboard_remote_id, group_remote_ids, payload })
}

/// Adopt the server's returned order as local positions and retire the entry,
/// in one transaction. If another move rewrote the payload while the request
/// was in flight, the response is stale: change nothing and keep the entry
/// pending so the newer order is pushed next. Returns true when retired.
pub fn finish_order_push(
    conn: &Connection,
    entry_id: i64,
    dashboard_id: &str,
    sent_payload: &str,
    returned: &[GroupIn],
) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    let current: Option<String> = tx
        .query_row("SELECT payload FROM sync_queue WHERE id = ?1", params![entry_id], |r| r.get(0))
        .optional()?
        .flatten();
    if current.as_deref() != Some(sent_payload) {
        return Ok(false);
    }
    for (pos, g) in returned.iter().enumerate() {
        tx.execute(
            "UPDATE dashboard_groups SET position = ?3 WHERE remote_id = ?1 AND dashboard_id = ?2",
            params![g.id, dashboard_id, pos as i64],
        )?;
    }
    sync_queue::mark_synced(&tx, entry_id)?;
    tx.commit()?;
    Ok(true)
}

/// Remove a dashboard and its groups and sources locally, and retire every
/// unpushed queue entry for the dashboard, its groups and its group order (a
/// never-pushed create is simply dropped). Returns the `dashboard` delete
/// payload (`{"remote_id": n}`) when the dashboard exists on the server, i.e.
/// when a delete must be queued; `None` otherwise (or if it is already gone).
pub fn delete_local(conn: &Connection, dashboard_id: &str) -> Result<Option<String>> {
    let Some(dashboard) = get(conn, dashboard_id)? else {
        return Ok(None);
    };
    let group_ids: Vec<String> = groups_for(conn, dashboard_id)?.into_iter().map(|g| g.id).collect();
    let now = crate::db::now_iso();
    let tx = conn.unchecked_transaction()?;
    let retire = "UPDATE sync_queue SET synced_at = ?3
                  WHERE entity_type = ?1 AND entity_id = ?2 AND synced_at IS NULL";
    for (entity_type, id) in group_ids
        .iter()
        .map(|g| (GROUP_ENTITY_TYPE, g.as_str()))
        .chain([(ENTITY_TYPE, dashboard_id), (GROUP_ORDER_ENTITY_TYPE, dashboard_id)])
    {
        tx.execute(retire, params![entity_type, id, now])?;
    }
    // Groups and their sources go with the row (ON DELETE CASCADE).
    tx.execute("DELETE FROM dashboards WHERE id = ?1", params![dashboard_id])?;
    if let Some(remote_id) = dashboard.remote_id {
        super::home_page::clear_if_dashboard(&tx, remote_id)?;
    }
    tx.commit()?;
    super::home_page::clear_if_local_dashboard(conn, dashboard_id)?;
    Ok(dashboard
        .remote_id
        .map(|id| serde_json::json!({ "remote_id": id }).to_string()))
}

/// After a successful `POST`: the local row takes the server's id, position
/// and `updated_at`. The name and description stay as typed (a later local
/// edit may be queued). A pull that ran first may have mirrored the pushed
/// row as a second local row; that duplicate goes, so the id stays unique.
pub fn adopt_created(conn: &Connection, id: &str, dto: &DashboardIn) -> Result<()> {
    conn.execute(
        "DELETE FROM dashboards WHERE remote_id = ?2 AND id <> ?1",
        params![id, dto.id],
    )?;
    conn.execute(
        "UPDATE dashboards SET remote_id = ?2, position = ?3, updated_at = ?4 WHERE id = ?1",
        params![id, dto.id, dto.position, dto.updated_at],
    )?;
    super::home_page::adopt_dashboard(conn, id, dto.id)?;
    Ok(())
}

/// After a successful group `POST`: the local group takes the server's id and
/// position, and the sources the server returned replace the local ones (it may
/// have dropped ids it won't accept). Name, description and color stay as typed.
/// A pull that ran first may have mirrored the pushed group as a second row;
/// that duplicate goes, so the remote id stays unique. Takes no transaction of
/// its own: the caller wraps it together with `mark_synced`.
pub fn adopt_created_group(conn: &Connection, id: &str, dto: &GroupIn) -> Result<()> {
    conn.execute(
        "DELETE FROM dashboard_groups WHERE remote_id = ?2 AND id <> ?1",
        params![id, dto.id],
    )?;
    conn.execute(
        "UPDATE dashboard_groups SET remote_id = ?2, position = ?3 WHERE id = ?1",
        params![id, dto.id, dto.position],
    )?;
    replace_sources(conn, id, dto)
}

/// A group by local id.
pub fn get_group(conn: &Connection, id: &str) -> Result<Option<DashboardGroup>> {
    conn.query_row(
        "SELECT id, remote_id, dashboard_id, name, description, color, position
         FROM dashboard_groups WHERE id = ?1",
        params![id],
        group_row,
    )
    .optional()
}

/// The nine group color swatches Rails offers; the first is the default.
pub const GROUP_SWATCHES: [&str; 9] = [
    "#6366f1", "#ec4899", "#3b82f6", "#10b981", "#f59e0b", "#ef4444", "#8b5cf6", "#14b8a6",
    "#737373",
];

/// Working copy of a group being created (the group modal's state, minus UI).
/// Every source kind joins through `toggle_source` / `set_include_all`, so the
/// project and label tickets only add pickers that call these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDraft {
    pub name: String,
    pub description: String,
    pub color: String,
    pub sources: Vec<GroupSource>,
}

impl Default for GroupDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            color: GROUP_SWATCHES[0].to_string(),
            sources: Vec::new(),
        }
    }
}

impl GroupDraft {
    pub fn is_selected(&self, source_type: SourceType, remote_id: i64) -> bool {
        self.sources.iter().any(|s| s.source_type == source_type && s.remote_id == remote_id)
    }

    pub fn include_all(&self, source_type: SourceType, remote_id: i64) -> bool {
        self.sources
            .iter()
            .any(|s| s.source_type == source_type && s.remote_id == remote_id && s.include_all)
    }

    /// Select the source, or deselect it (which drops its include-all flag).
    pub fn toggle_source(&mut self, source_type: SourceType, remote_id: i64) {
        if self.is_selected(source_type, remote_id) {
            self.sources.retain(|s| !(s.source_type == source_type && s.remote_id == remote_id));
        } else {
            self.sources.push(GroupSource { source_type, remote_id, include_all: false });
        }
    }

    /// Set "all issues" on a selected Team or Project source. Ignored for an
    /// unselected source and for labels, which have no such flag.
    pub fn set_include_all(&mut self, source_type: SourceType, remote_id: i64, on: bool) {
        if source_type == SourceType::Label {
            return;
        }
        if let Some(s) = self
            .sources
            .iter_mut()
            .find(|s| s.source_type == source_type && s.remote_id == remote_id)
        {
            s.include_all = on;
        }
    }
}

#[derive(Debug)]
pub enum GroupError {
    Name(NameError),
    /// The parent dashboard doesn't exist locally.
    NoDashboard,
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for GroupError {
    fn from(e: rusqlite::Error) -> Self {
        GroupError::Db(e)
    }
}

/// Create a group offline under a dashboard (pushed or not): validate the name,
/// place it last, store its sources and queue a "create" entry without payload.
/// An unknown color falls back to the default; `include_all` is stored only on
/// selected Team and Project sources.
pub fn create_group_local(
    conn: &Connection,
    dashboard_id: &str,
    draft: &GroupDraft,
) -> std::result::Result<DashboardGroup, GroupError> {
    let name = validate_name(&draft.name).map_err(GroupError::Name)?;
    if get(conn, dashboard_id)?.is_none() {
        return Err(GroupError::NoDashboard);
    }
    let color = if GROUP_SWATCHES.contains(&draft.color.as_str()) {
        draft.color.as_str()
    } else {
        GROUP_SWATCHES[0]
    };
    let tx = conn.unchecked_transaction()?;
    let position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM dashboard_groups WHERE dashboard_id = ?1",
        params![dashboard_id],
        |r| r.get(0),
    )?;
    let id = Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO dashboard_groups (id, remote_id, dashboard_id, name, description, color, position)
         VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6)",
        params![id, dashboard_id, name, draft.description.trim(), color, position],
    )?;
    for s in &draft.sources {
        let all = s.include_all && s.source_type != SourceType::Label;
        tx.execute(
            "INSERT OR IGNORE INTO dashboard_group_sources (group_id, source_type, source_remote_id, include_all)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, s.source_type.as_str(), s.remote_id, all as i64],
        )?;
    }
    sync_queue::enqueue(&tx, GROUP_ENTITY_TYPE, &id, "create", None)?;
    tx.commit()?;
    Ok(groups_for(conn, dashboard_id)?.into_iter().find(|g| g.id == id).expect("group just inserted"))
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

    fn draft_named(name: &str) -> GroupDraft {
        let mut d = GroupDraft::default();
        d.name = name.into();
        d
    }

    #[test]
    fn adopt_created_group_takes_remote_id_and_server_sources() {
        let conn = setup();
        let d = create_local(&conn, "D", "").unwrap();
        let mut draft = draft_named("G");
        draft.toggle_source(SourceType::Team, 1);
        draft.toggle_source(SourceType::Team, 2);
        draft.set_include_all(SourceType::Team, 1, true);
        let g = create_group_local(&conn, &d.id, &draft).unwrap();
        // The server dropped team 2 and kept team 1 as include-all.
        let mut dto = group(40, "G", 3);
        dto.team_ids = vec![1];
        dto.all_team_ids = vec![1];
        adopt_created_group(&conn, &g.id, &dto).unwrap();
        let got = groups_for(&conn, &d.id).unwrap();
        assert_eq!((got.len(), got[0].remote_id, got[0].position), (1, Some(40), 3));
        assert_eq!(got[0].name, "G");
        assert_eq!(
            sources_for(&conn, &g.id).unwrap(),
            [GroupSource { source_type: SourceType::Team, remote_id: 1, include_all: true }]
        );
    }

    #[test]
    fn adopt_created_group_removes_a_pull_mirrored_duplicate() {
        let conn = setup();
        let d = create_local(&conn, "D", "").unwrap();
        let g = create_group_local(&conn, &d.id, &draft_named("G")).unwrap();
        // A pull saw the pushed group first and mirrored it as its own row.
        conn.execute(
            "INSERT INTO dashboard_groups (id, remote_id, dashboard_id, name, description, color, position)
             VALUES ('dup', 40, ?1, 'G', '', '#6366f1', 1)",
            params![d.id],
        )
        .unwrap();
        adopt_created_group(&conn, &g.id, &group(40, "G", 1)).unwrap();
        let ids: Vec<_> = groups_for(&conn, &d.id).unwrap().into_iter().map(|g| g.id).collect();
        assert_eq!(ids, [g.id]);
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

    fn seeded_three() -> (Connection, String, Vec<String>) {
        let conn = setup();
        apply_snapshot(
            &conn,
            &[dash(1, "A", 0, vec![group(10, "G", 0), group(11, "H", 1), group(12, "I", 2)])],
        )
        .unwrap();
        let d = find_by_remote_id(&conn, 1).unwrap().unwrap();
        let ids = groups_for(&conn, &d.id).unwrap().into_iter().map(|g| g.id).collect();
        (conn, d.id, ids)
    }

    fn names(conn: &Connection, d: &str) -> Vec<String> {
        groups_for(conn, d).unwrap().into_iter().map(|g| g.name).collect()
    }

    #[test]
    fn move_group_renumbers_the_groups_contiguously() {
        let (conn, d, ids) = seeded_three();
        // Server positions with gaps are normalised to 0..n.
        conn.execute("UPDATE dashboard_groups SET position = position * 10 + 5", []).unwrap();
        assert!(move_group(&conn, &ids[2], MoveDirection::Up).unwrap());
        assert_eq!(names(&conn, &d), ["G", "I", "H"]);
        let pos: Vec<_> = groups_for(&conn, &d).unwrap().into_iter().map(|g| g.position).collect();
        assert_eq!(pos, [0, 1, 2]);
        assert!(move_group(&conn, &ids[0], MoveDirection::Down).unwrap());
        assert_eq!(names(&conn, &d), ["I", "G", "H"]);
    }

    #[test]
    fn move_group_at_an_edge_changes_and_queues_nothing() {
        let (conn, d, ids) = seeded_three();
        assert!(!move_group(&conn, &ids[0], MoveDirection::Up).unwrap());
        assert!(!move_group(&conn, &ids[2], MoveDirection::Down).unwrap());
        assert_eq!(names(&conn, &d), ["G", "H", "I"]);
        assert!(sync_queue::pending_entries(&conn).unwrap().is_empty());
    }

    #[test]
    fn several_moves_coalesce_into_one_entry_with_the_final_order() {
        let (conn, d, ids) = seeded_three();
        move_group(&conn, &ids[2], MoveDirection::Up).unwrap();
        move_group(&conn, &ids[2], MoveDirection::Up).unwrap();
        move_group(&conn, &ids[0], MoveDirection::Down).unwrap();
        let q = sync_queue::pending_entries(&conn).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!((q[0].entity_type.as_str(), q[0].action.as_str(), q[0].entity_id.as_str()),
                   (GROUP_ORDER_ENTITY_TYPE, "update", d.as_str()));
        let payload: serde_json::Value = serde_json::from_str(q[0].payload.as_deref().unwrap()).unwrap();
        let order: Vec<_> = groups_for(&conn, &d).unwrap().into_iter().map(|g| g.id).collect();
        assert_eq!(payload["group_ids"], serde_json::json!(order));
        assert_eq!(names(&conn, &d), ["I", "H", "G"]);
    }

    #[test]
    fn a_move_after_the_order_was_pushed_queues_a_fresh_entry() {
        let (conn, d, ids) = seeded_three();
        move_group(&conn, &ids[0], MoveDirection::Down).unwrap();
        conn.execute("UPDATE sync_queue SET synced_at = 'x'", []).unwrap();
        move_group(&conn, &ids[0], MoveDirection::Down).unwrap();
        let q = sync_queue::pending_entries(&conn).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].entity_id, d);
        assert_eq!(names(&conn, &d), ["H", "I", "G"]);
    }

    #[test]
    fn moves_on_different_dashboards_queue_separate_entries() {
        let (conn, _d, ids) = seeded_three();
        apply_snapshot(
            &conn,
            &[
                dash(1, "A", 0, vec![group(10, "G", 0), group(11, "H", 1), group(12, "I", 2)]),
                dash(2, "B", 1, vec![group(20, "X", 0), group(21, "Y", 1)]),
            ],
        )
        .unwrap();
        let x = find_group_by_remote_id(&conn, 20).unwrap().unwrap();
        move_group(&conn, &ids[0], MoveDirection::Down).unwrap();
        move_group(&conn, &x.id, MoveDirection::Down).unwrap();
        assert_eq!(sync_queue::pending_entries(&conn).unwrap().len(), 2);
    }

    #[test]
    fn adopt_created_takes_id_position_and_updated_at() {
        let conn = setup();
        let d = create_local(&conn, "Mine", "desc").unwrap();
        adopt_created(&conn, &d.id, &dash(77, "Mine", 5, vec![])).unwrap();
        let got = find_by_remote_id(&conn, 77).unwrap().unwrap();
        assert_eq!((got.id.as_str(), got.position), (d.id.as_str(), 5));
        assert_eq!(got.updated_at, "2026-01-01T00:00:00.000Z");
        assert_eq!((got.name.as_str(), got.description.as_str()), ("Mine", "desc"));
    }

    #[test]
    fn a_pull_after_adoption_updates_the_row_without_duplicating() {
        let conn = setup();
        let d = create_local(&conn, "Mine", "").unwrap();
        adopt_created(&conn, &d.id, &dash(77, "Mine", 0, vec![])).unwrap();
        let entry = sync_queue::pending_entries(&conn).unwrap()[0].id;
        sync_queue::mark_synced(&conn, entry.unwrap()).unwrap();
        apply_snapshot(&conn, &[dash(77, "Renamed", 0, vec![])]).unwrap();
        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].id.as_str(), all[0].name.as_str()), (d.id.as_str(), "Renamed"));
    }

    #[test]
    fn adopting_after_a_pull_already_mirrored_the_row_drops_the_duplicate() {
        let conn = setup();
        let d = create_local(&conn, "Mine", "").unwrap();
        apply_snapshot(&conn, &[dash(77, "Mine", 0, vec![group(5, "G", 0)])]).unwrap();
        assert_eq!(list(&conn).unwrap().len(), 2);
        adopt_created(&conn, &d.id, &dash(77, "Mine", 0, vec![])).unwrap();
        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, d.id);
        assert_eq!(all[0].remote_id, Some(77));
    }

    #[test]
    fn create_group_local_places_last_stores_sources_and_queues_a_create() {
        let conn = setup();
        let d = create_local(&conn, "D", "").unwrap();
        let mut draft =
            GroupDraft { name: " Mine ".into(), description: " d ".into(), ..Default::default() };
        draft.color = "#ec4899".into();
        draft.toggle_source(SourceType::Team, 7);
        draft.set_include_all(SourceType::Team, 7, true);
        draft.toggle_source(SourceType::Team, 8);
        let a = create_group_local(&conn, &d.id, &draft).unwrap();
        let b = create_group_local(&conn, &d.id, &GroupDraft { name: "B".into(), ..Default::default() })
            .unwrap();
        assert_eq!(
            (a.name.as_str(), a.description.as_str(), a.color.as_str(), a.position),
            ("Mine", "d", "#ec4899", 0)
        );
        assert_eq!((b.position, b.remote_id, b.color.as_str()), (1, None, "#6366f1"));
        assert_eq!(
            sources_for(&conn, &a.id).unwrap(),
            [
                GroupSource { source_type: SourceType::Team, remote_id: 7, include_all: true },
                GroupSource { source_type: SourceType::Team, remote_id: 8, include_all: false },
            ]
        );
        let q: Vec<_> = sync_queue::pending_entries(&conn)
            .unwrap()
            .into_iter()
            .filter(|e| e.entity_type == GROUP_ENTITY_TYPE)
            .collect();
        assert_eq!(q.len(), 2);
        assert_eq!((q[0].action.as_str(), q[0].entity_id.as_str()), ("create", a.id.as_str()));
    }

    #[test]
    fn create_group_local_rejects_bad_name_and_missing_dashboard_without_writing() {
        let conn = setup();
        let d = create_local(&conn, "D", "").unwrap();
        let blank = GroupDraft { name: "  ".into(), ..Default::default() };
        assert!(matches!(
            create_group_local(&conn, &d.id, &blank),
            Err(GroupError::Name(NameError::Blank))
        ));
        let ok = GroupDraft { name: "G".into(), ..Default::default() };
        assert!(matches!(create_group_local(&conn, "nope", &ok), Err(GroupError::NoDashboard)));
        assert!(groups_for(&conn, &d.id).unwrap().is_empty());
        assert_eq!(sync_queue::pending_entries(&conn).unwrap().len(), 1);
    }

    #[test]
    fn include_all_counts_only_on_a_selected_source() {
        let mut draft = GroupDraft::default();
        draft.set_include_all(SourceType::Team, 1, true);
        assert!(!draft.include_all(SourceType::Team, 1));
        draft.toggle_source(SourceType::Team, 1);
        draft.set_include_all(SourceType::Team, 1, true);
        assert!(draft.include_all(SourceType::Team, 1));
        draft.toggle_source(SourceType::Team, 1);
        draft.toggle_source(SourceType::Team, 1);
        assert!(!draft.include_all(SourceType::Team, 1));
        draft.toggle_source(SourceType::Label, 5);
        draft.set_include_all(SourceType::Label, 5, true);
        assert!(!draft.include_all(SourceType::Label, 5));
    }

    #[test]
    fn a_local_group_under_an_unpushed_dashboard_survives_a_pull() {
        let conn = setup();
        let d = create_local(&conn, "D", "").unwrap();
        let g = create_group_local(&conn, &d.id, &GroupDraft { name: "G".into(), ..Default::default() })
            .unwrap();
        apply_snapshot(&conn, &[]).unwrap();
        assert_eq!(groups_for(&conn, &d.id).unwrap()[0].id, g.id);
    }

    #[test]
    fn update_local_writes_the_row_and_queues_an_update_without_payload() {
        let conn = setup();
        let d = create_local(&conn, "Old", "x").unwrap();
        let u = update_local(&conn, &d.id, "  New  ", " desc ").unwrap();
        assert_eq!((u.name.as_str(), u.description.as_str()), ("New", "desc"));
        let q = sync_queue::pending_entries(&conn).unwrap();
        let upd: Vec<_> = q.iter().filter(|e| e.action == "update").collect();
        assert_eq!(upd.len(), 1);
        assert_eq!((upd[0].entity_type.as_str(), upd[0].entity_id.as_str()), (ENTITY_TYPE, d.id.as_str()));
        assert!(upd[0].payload.is_none());
    }

    #[test]
    fn update_local_validates_like_create_and_queues_nothing_on_error() {
        let conn = setup();
        let d = create_local(&conn, "Old", "").unwrap();
        assert!(matches!(update_local(&conn, &d.id, "  ", ""), Err(CreateError::Name(NameError::Blank))));
        let long = "x".repeat(81);
        assert!(matches!(update_local(&conn, &d.id, &long, ""), Err(CreateError::Name(NameError::TooLong))));
        assert!(update_local(&conn, &d.id, &"x".repeat(80), "").is_ok());
        assert_eq!(get(&conn, &d.id).unwrap().unwrap().name.len(), 80);
        let updates = sync_queue::pending_entries(&conn).unwrap().iter().filter(|e| e.action == "update").count();
        assert_eq!(updates, 1);
    }

    #[test]
    fn pull_does_not_overwrite_a_dashboard_with_a_pending_edit() {
        let conn = setup();
        apply_snapshot(&conn, &[dash(1, "Server", 0, vec![])]).unwrap();
        let local = find_by_remote_id(&conn, 1).unwrap().unwrap();
        update_local(&conn, &local.id, "Mine", "").unwrap();
        let a = apply_snapshot(&conn, &[dash(1, "Server2", 0, vec![])]).unwrap();
        assert_eq!(a.skipped_pending, 1);
        assert_eq!(get(&conn, &local.id).unwrap().unwrap().name, "Mine");
    }

    fn queue_for(conn: &Connection, entity_type: &str) -> Vec<(String, String)> {
        sync_queue::pending_entries(conn)
            .unwrap()
            .into_iter()
            .filter(|e| e.entity_type == entity_type)
            .map(|e| (e.entity_id, e.action))
            .collect()
    }

    #[test]
    fn delete_local_of_a_never_pushed_dashboard_discards_everything_and_queues_nothing() {
        let conn = setup();
        let d = create_local(&conn, "D", "").unwrap();
        let mut draft = GroupDraft { name: "G".into(), ..Default::default() };
        draft.toggle_source(SourceType::Team, 7);
        let g = create_group_local(&conn, &d.id, &draft).unwrap();
        sync_queue::enqueue(&conn, GROUP_ORDER_ENTITY_TYPE, &d.id, "update", Some("{}")).unwrap();
        assert_eq!(delete_local(&conn, &d.id).unwrap(), None);
        assert!(get(&conn, &d.id).unwrap().is_none());
        assert!(groups_for(&conn, &d.id).unwrap().is_empty());
        assert!(sources_for(&conn, &g.id).unwrap().is_empty());
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn delete_local_of_a_pushed_dashboard_cascades_retires_queue_and_returns_payload() {
        let conn = setup();
        let mut gi = group(10, "G", 0);
        gi.team_ids = vec![1];
        apply_snapshot(&conn, &[dash(5, "A", 0, vec![gi]), dash(6, "B", 1, vec![group(11, "H", 0)])])
            .unwrap();
        let a = find_by_remote_id(&conn, 5).unwrap().unwrap();
        let b = find_by_remote_id(&conn, 6).unwrap().unwrap();
        let ga = groups_for(&conn, &a.id).unwrap().remove(0);
        let gb = groups_for(&conn, &b.id).unwrap().remove(0);
        sync_queue::enqueue(&conn, ENTITY_TYPE, &a.id, "update", None).unwrap();
        sync_queue::enqueue(&conn, GROUP_ENTITY_TYPE, &ga.id, "update", None).unwrap();
        sync_queue::enqueue(&conn, GROUP_ORDER_ENTITY_TYPE, &a.id, "update", Some("{}")).unwrap();
        sync_queue::enqueue(&conn, GROUP_ENTITY_TYPE, &gb.id, "update", None).unwrap();

        let payload = delete_local(&conn, &a.id).unwrap().unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&payload).unwrap()["remote_id"], 5);
        assert!(get(&conn, &a.id).unwrap().is_none());
        assert!(sources_for(&conn, &ga.id).unwrap().is_empty());
        assert!(queue_for(&conn, ENTITY_TYPE).is_empty());
        assert!(queue_for(&conn, GROUP_ORDER_ENTITY_TYPE).is_empty());
        // The other dashboard is untouched.
        assert_eq!(queue_for(&conn, GROUP_ENTITY_TYPE), [(gb.id.clone(), "update".to_string())]);
        assert!(get(&conn, &b.id).unwrap().is_some());
    }

    #[test]
    fn delete_local_of_a_missing_dashboard_is_a_no_op() {
        let conn = setup();
        assert_eq!(delete_local(&conn, "nope").unwrap(), None);
    }
}

#[cfg(test)]
mod order_push_tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        conn
    }

    fn gin(id: i64, pos: i64) -> GroupIn {
        GroupIn {
            id,
            name: format!("g{id}"),
            description: None,
            color: None,
            position: pos,
            team_ids: vec![],
            project_ids: vec![],
            label_ids: vec![],
            all_team_ids: vec![],
            all_project_ids: vec![],
        }
    }

    fn snapshot() -> DashboardIn {
        DashboardIn {
            id: 1,
            name: "A".into(),
            description: None,
            position: 0,
            updated_at: String::new(),
            groups: vec![gin(10, 0), gin(11, 1), gin(12, 2)],
        }
    }

    /// A mirrored dashboard (remote 1) with groups remote 10, 11, 12.
    fn seeded() -> (Connection, String, Vec<String>) {
        let conn = conn();
        apply_snapshot(&conn, &[snapshot()]).unwrap();
        let d = find_by_remote_id(&conn, 1).unwrap().unwrap();
        let ids = groups_for(&conn, &d.id).unwrap().into_iter().map(|g| g.id).collect();
        (conn, d.id, ids)
    }

    fn order_entry(conn: &Connection, d: &str) -> i64 {
        sync_queue::pending_entries(conn)
            .unwrap()
            .into_iter()
            .find(|e| e.entity_type == GROUP_ORDER_ENTITY_TYPE && e.entity_id == d)
            .unwrap()
            .id
            .unwrap()
    }

    fn remote_order(conn: &Connection, d: &str) -> Vec<i64> {
        groups_for(conn, d).unwrap().into_iter().map(|g| g.remote_id.unwrap()).collect()
    }

    #[test]
    fn a_burst_of_moves_resolves_to_one_final_remote_order() {
        let (conn, d, ids) = seeded();
        move_group(&conn, &ids[2], MoveDirection::Up).unwrap();
        move_group(&conn, &ids[2], MoveDirection::Up).unwrap();
        move_group(&conn, &ids[1], MoveDirection::Down).unwrap();
        let entry = order_entry(&conn, &d);
        let OrderPlan::Ready { dashboard_remote_id, group_remote_ids, .. } =
            plan_order_push(&conn, entry, &d).unwrap()
        else {
            panic!("ready")
        };
        assert_eq!(dashboard_remote_id, 1);
        assert_eq!(group_remote_ids, remote_order(&conn, &d));
        assert_eq!(group_remote_ids, [12, 10, 11]);
    }

    #[test]
    fn waits_while_the_dashboard_or_a_group_is_unpushed() {
        let (conn, d, ids) = seeded();
        move_group(&conn, &ids[0], MoveDirection::Down).unwrap();
        let entry = order_entry(&conn, &d);
        conn.execute("UPDATE dashboard_groups SET remote_id = NULL WHERE id = ?1", params![ids[1]]).unwrap();
        assert!(matches!(plan_order_push(&conn, entry, &d).unwrap(), OrderPlan::Waiting(_)));
        conn.execute("UPDATE dashboard_groups SET remote_id = 11 WHERE id = ?1", params![ids[1]]).unwrap();
        conn.execute("UPDATE dashboards SET remote_id = NULL WHERE id = ?1", params![d]).unwrap();
        assert!(matches!(plan_order_push(&conn, entry, &d).unwrap(), OrderPlan::Waiting(_)));
    }

    #[test]
    fn a_deleted_dashboard_has_nothing_to_push() {
        let (conn, d, ids) = seeded();
        move_group(&conn, &ids[0], MoveDirection::Down).unwrap();
        let entry = order_entry(&conn, &d);
        conn.execute("DELETE FROM dashboards WHERE id = ?1", params![d]).unwrap();
        assert_eq!(plan_order_push(&conn, entry, &d).unwrap(), OrderPlan::Gone);
    }

    #[test]
    fn the_returned_order_sets_positions_and_retires_the_entry() {
        let (conn, d, ids) = seeded();
        move_group(&conn, &ids[2], MoveDirection::Up).unwrap();
        let entry = order_entry(&conn, &d);
        let OrderPlan::Ready { payload, .. } = plan_order_push(&conn, entry, &d).unwrap() else {
            panic!("ready")
        };
        // The server answers with its own order, which wins.
        let returned = [gin(11, 0), gin(12, 1), gin(10, 2)];
        assert!(finish_order_push(&conn, entry, &d, &payload, &returned).unwrap());
        assert_eq!(remote_order(&conn, &d), [11, 12, 10]);
        assert!(!sync_queue::has_pending_entry(&conn, GROUP_ORDER_ENTITY_TYPE, &d).unwrap());
    }

    #[test]
    fn a_move_made_while_the_push_was_in_flight_stays_queued() {
        let (conn, d, ids) = seeded();
        move_group(&conn, &ids[2], MoveDirection::Up).unwrap();
        let entry = order_entry(&conn, &d);
        let OrderPlan::Ready { payload, .. } = plan_order_push(&conn, entry, &d).unwrap() else {
            panic!("ready")
        };
        move_group(&conn, &ids[0], MoveDirection::Down).unwrap();
        let before = groups_for(&conn, &d).unwrap();
        let stale = [gin(10, 0), gin(12, 1), gin(11, 2)];
        assert!(!finish_order_push(&conn, entry, &d, &payload, &stale).unwrap());
        assert_eq!(groups_for(&conn, &d).unwrap(), before);
        assert!(sync_queue::has_pending_entry(&conn, GROUP_ORDER_ENTITY_TYPE, &d).unwrap());
    }

    #[test]
    fn a_pull_leaves_positions_alone_while_the_order_is_pending() {
        let (conn, d, ids) = seeded();
        move_group(&conn, &ids[2], MoveDirection::Up).unwrap();
        apply_snapshot(&conn, &[snapshot()]).unwrap();
        assert_eq!(remote_order(&conn, &d), [10, 12, 11]);
    }
}
