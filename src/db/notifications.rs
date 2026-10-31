//! Local mirror of the user's Notifications. Pure over a `rusqlite::Connection`:
//! no Iced, no HTTP. The Notification poll feeds it decoded envelopes; the
//! Inbox view reads it back.
//!
//! Bookkeeping lives in the existing tables: the cursor in `pull_cursors` and
//! the ETag in `list_etags` (both under [`SCOPE`]), and the server-reported
//! unread count in `pull_cursors` under [`UNREAD_RESOURCE`].
// The poll, Inbox and banner tickets build on this API; until they land the
// public surface is unused.
#![allow(dead_code)]

use rusqlite::{params, Connection, OptionalExtension, Result};
use serde::Deserialize;
use uuid::Uuid;

use super::queries::{get_list_etag, get_pull_cursor, set_list_etag, set_pull_cursor};
use super::sync_queue;

/// `team_id` value used in `pull_cursors` / `list_etags` (Notifications are not team-scoped).
pub const SCOPE: &str = "user";
/// Cursor resource / ETag list name for the Notification feed.
pub const RESOURCE: &str = "notifications";
/// `pull_cursors` resource holding the server-reported unread count (decimal text).
pub const UNREAD_RESOURCE: &str = "notifications_unread";
/// `sync_queue.entity_type` for Notification writes.
pub const ENTITY_TYPE: &str = "notification";

/// A mirrored Notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// Local uuid.
    pub id: String,
    pub remote_id: i64,
    pub action: String,
    pub message: String,
    pub actor_name: Option<String>,
    pub issue_remote_id: Option<i64>,
    pub issue_identifier: Option<String>,
    pub issue_title: Option<String>,
    pub team_remote_id: Option<i64>,
    pub comment_remote_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    pub read_at: Option<String>,
}

impl Notification {
    pub fn is_unread(&self) -> bool {
        self.read_at.is_none()
    }
}

/// One Notification as delivered by `GET /api/v1/notifications` (wire shape).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct NotificationIn {
    pub id: i64,
    pub action: String,
    pub message: String,
    #[serde(default)]
    pub read_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub actor: Option<ActorIn>,
    #[serde(default)]
    pub issue: Option<IssueIn>,
    #[serde(default)]
    pub comment_id: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ActorIn {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct IssueIn {
    pub id: i64,
    pub identifier: String,
    pub title: String,
    #[serde(default)]
    pub team_id: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MetaIn {
    pub unread_count: i64,
    pub server_time: String,
}

/// The decoded list envelope `{notifications, meta}`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Envelope {
    pub notifications: Vec<NotificationIn>,
    pub meta: MetaIn,
}

/// Inbox filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Unread,
    All,
}

const COLUMNS: &str = "id, remote_id, action, message, actor_name, issue_remote_id, \
    issue_identifier, issue_title, team_remote_id, comment_remote_id, created_at, updated_at, read_at";

fn from_row(r: &rusqlite::Row) -> Result<Notification> {
    Ok(Notification {
        id: r.get(0)?,
        remote_id: r.get(1)?,
        action: r.get(2)?,
        message: r.get(3)?,
        actor_name: r.get(4)?,
        issue_remote_id: r.get(5)?,
        issue_identifier: r.get(6)?,
        issue_title: r.get(7)?,
        team_remote_id: r.get(8)?,
        comment_remote_id: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        read_at: r.get(12)?,
    })
}

/// Apply one poll. Upserts by remote id (re-applying changes nothing), keeps
/// the `read_at` of rows with a pending local read (single or read-all) even
/// when the server row still says unread, stores the server's unread count and
/// advances the cursor to `meta.server_time`. Returns the Notifications that
/// were new to the mirror and unread, in envelope order.
pub fn apply_poll(conn: &Connection, envelope: &Envelope, now: &str) -> Result<Vec<Notification>> {
    let tx = conn.unchecked_transaction()?;
    let pending = PendingReads::load(&tx)?;
    let mut fresh = Vec::new();

    for n in &envelope.notifications {
        let existing: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT id, read_at FROM notifications WHERE remote_id = ?1",
                params![n.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let mut read_at = n.read_at.clone();
        if read_at.is_none() {
            if let Some((_, Some(local))) = &existing {
                if pending.single.contains(&n.id) {
                    read_at = Some(local.clone());
                }
            }
            if read_at.is_none() && pending.covers_all(&n.created_at) {
                read_at = Some(now.to_string());
            }
        }

        let (actor, issue) = (n.actor.as_ref(), n.issue.as_ref());
        let local_id = existing
            .as_ref()
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        tx.execute(
            "INSERT INTO notifications (id, remote_id, action, message, actor_name,
                issue_remote_id, issue_identifier, issue_title, team_remote_id,
                comment_remote_id, created_at, updated_at, read_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(remote_id) DO UPDATE SET
                action = excluded.action, message = excluded.message,
                actor_name = excluded.actor_name,
                issue_remote_id = excluded.issue_remote_id,
                issue_identifier = excluded.issue_identifier,
                issue_title = excluded.issue_title,
                team_remote_id = excluded.team_remote_id,
                comment_remote_id = excluded.comment_remote_id,
                created_at = excluded.created_at, updated_at = excluded.updated_at,
                read_at = excluded.read_at",
            params![
                local_id,
                n.id,
                n.action,
                n.message,
                actor.map(|a| a.name.as_str()),
                issue.map(|i| i.id),
                issue.map(|i| i.identifier.as_str()),
                issue.map(|i| i.title.as_str()),
                issue.and_then(|i| i.team_id),
                n.comment_id,
                n.created_at,
                n.updated_at,
                read_at,
            ],
        )?;

        if existing.is_none() && read_at.is_none() {
            fresh.push(tx.query_row(
                &format!("SELECT {COLUMNS} FROM notifications WHERE id = ?1"),
                params![local_id],
                from_row,
            )?);
        }
    }

    set_unread_count(&tx, envelope.meta.unread_count)?;
    set_pull_cursor(&tx, SCOPE, RESOURCE, &envelope.meta.server_time)?;
    tx.commit()?;
    Ok(fresh)
}

/// Pending (unsynced) local reads on the sync queue.
struct PendingReads {
    single: std::collections::HashSet<i64>,
    /// Newest `before` among pending read-alls; rows created at or before it are read.
    all_before: Option<String>,
}

impl PendingReads {
    fn load(conn: &Connection) -> Result<Self> {
        let mut single = std::collections::HashSet::new();
        let mut all_before: Option<String> = None;
        for e in sync_queue::pending_entries(conn)? {
            if e.entity_type != ENTITY_TYPE {
                continue;
            }
            match e.action.as_str() {
                "read" => {
                    if let Ok(id) = e.entity_id.parse::<i64>() {
                        single.insert(id);
                    }
                }
                "read_all" => {
                    let before = e
                        .payload
                        .as_deref()
                        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
                        .and_then(|v| v.get("before").and_then(|b| b.as_str().map(String::from)));
                    if let Some(b) = before {
                        if all_before.as_ref().map_or(true, |cur| b > *cur) {
                            all_before = Some(b);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(Self { single, all_before })
    }

    fn covers_all(&self, created_at: &str) -> bool {
        self.all_before.as_deref().is_some_and(|b| created_at <= b)
    }
}

/// Badge number: the stored unread count (server's after each poll, lowered by
/// local marks). Falls back to counting local unread rows before any poll.
pub fn unread_count(conn: &Connection) -> Result<i64> {
    match get_pull_cursor(conn, SCOPE, UNREAD_RESOURCE)? {
        Some(s) => Ok(s.parse().unwrap_or(0)),
        None => conn.query_row(
            "SELECT COUNT(*) FROM notifications WHERE read_at IS NULL",
            [],
            |r| r.get(0),
        ),
    }
}

/// Read rows older than this many days are dropped by [`prune`].
pub const READ_RETENTION_DAYS: i64 = 30;

/// Bound the mirror: drop read rows whose `read_at` is over 30 days before
/// `now`, and rows whose issue carries the local delete tombstone. Unread rows
/// are kept however old. Returns the number of rows removed.
pub fn prune(conn: &Connection, now: &str) -> Result<usize> {
    let cutoff = chrono::NaiveDateTime::parse_from_str(now.trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S%.3f")
        .map(|t| {
            (t - chrono::Duration::days(READ_RETENTION_DAYS))
                .format(crate::db::TS_FORMAT)
                .to_string()
        })
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    conn.execute(
        "DELETE FROM notifications
         WHERE (read_at IS NOT NULL AND read_at < ?1)
            OR issue_remote_id IN (SELECT remote_id FROM issues
                                   WHERE deleted_at IS NOT NULL AND remote_id IS NOT NULL)",
        params![cutoff],
    )
}

/// Drop every Notification for a server issue id. Used when the issue is
/// purged locally: the server cascades the delete and never re-sends them.
pub fn delete_for_issue(conn: &Connection, issue_remote_id: i64) -> Result<usize> {
    conn.execute(
        "DELETE FROM notifications WHERE issue_remote_id = ?1",
        params![issue_remote_id],
    )
}

/// Mark one Notification read locally and queue `notification`/`read` for the
/// server (entity_id = remote id). Lowers the stored badge count by one.
/// Returns false (and does nothing) when the row is unknown or already read.
pub fn mark_read(conn: &Connection, remote_id: i64, now: &str) -> Result<bool> {
    let before = unread_count(conn)?;
    let changed = conn.execute(
        "UPDATE notifications SET read_at = ?1 WHERE remote_id = ?2 AND read_at IS NULL",
        params![now, remote_id],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    set_unread_count(conn, before - 1)?;
    sync_queue::enqueue(conn, ENTITY_TYPE, &remote_id.to_string(), "read", None)?;
    Ok(true)
}

/// Overwrite the badge number (clamped at 0), e.g. from a `read_all` response.
pub fn set_unread_count(conn: &Connection, count: i64) -> Result<()> {
    set_pull_cursor(conn, SCOPE, UNREAD_RESOURCE, &count.max(0).to_string())
}

/// Inbox rows, newest first by `created_at`, then id.
pub fn inbox_list(conn: &Connection, filter: Filter) -> Result<Vec<Notification>> {
    let where_clause = match filter {
        Filter::Unread => "WHERE read_at IS NULL",
        Filter::All => "",
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM notifications {where_clause} ORDER BY created_at DESC, id DESC"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    rows.collect()
}

/// Stored Notification cursor (the last applied `server_time`), if any.
pub fn cursor(conn: &Connection) -> Result<Option<String>> {
    get_pull_cursor(conn, SCOPE, RESOURCE)
}

/// Stored ETag of the Notification list.
pub fn etag(conn: &Connection) -> Result<Option<String>> {
    get_list_etag(conn, SCOPE, RESOURCE)
}

pub fn set_etag(conn: &Connection, etag: &str) -> Result<()> {
    set_list_etag(conn, SCOPE, RESOURCE, etag)
}

/// Most per-item banners one poll may post; more collapses into a single one.
pub const MAX_ITEM_BANNERS: usize = 3;

/// A native banner to post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub title: String,
    pub body: String,
}

/// Which banners a poll should post. `new_unread` is `PollReport::new_unread`.
/// Nothing when the window is focused, the poll was the backfill, banners are
/// disabled, or there is nothing new; up to 3 items get one banner each; more
/// collapse into one "N new notifications" banner.
pub fn banners_to_fire(
    new_unread: &[Notification],
    window_focused: bool,
    backfill: bool,
    enabled: bool,
) -> Vec<Banner> {
    if window_focused || backfill || !enabled || new_unread.is_empty() {
        return Vec::new();
    }
    if new_unread.len() > MAX_ITEM_BANNERS {
        return vec![Banner {
            title: "JAIT".into(),
            body: format!("{} new notifications", new_unread.len()),
        }];
    }
    new_unread
        .iter()
        .map(|n| Banner {
            title: match (&n.actor_name, &n.issue_identifier) {
                (Some(a), Some(i)) => format!("{a} \u{b7} {i}"),
                (Some(a), None) => a.clone(),
                (None, Some(i)) => i.clone(),
                (None, None) => "JAIT".into(),
            },
            body: n.message.clone(),
        })
        .collect()
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

    fn n(id: i64, created: &str, read: Option<&str>) -> NotificationIn {
        NotificationIn {
            id,
            action: "mentioned".into(),
            message: format!("msg {id}"),
            read_at: read.map(String::from),
            created_at: created.into(),
            updated_at: created.into(),
            actor: Some(ActorIn { id: 1, name: "Ann".into() }),
            issue: Some(IssueIn { id: 10 + id, identifier: "JRST-1".into(), title: "T".into(), team_id: Some(47) }),
            comment_id: Some(5),
        }
    }

    fn env(items: Vec<NotificationIn>, unread: i64, server_time: &str) -> Envelope {
        Envelope {
            notifications: items,
            meta: MetaIn { unread_count: unread, server_time: server_time.into() },
        }
    }

    const NOW: &str = "2026-10-31T12:00:00.000Z";

    fn mirrored(count: i64) -> Vec<Notification> {
        let items = (1..=count).map(|i| n(i, "2026-10-31T10:00:00.000Z", None)).collect();
        apply_poll(&mem(), &env(items, count, NOW), NOW).unwrap()
    }

    #[test]
    fn banners_nothing_when_window_focused() {
        assert!(banners_to_fire(&mirrored(2), true, false, true).is_empty());
    }

    #[test]
    fn banners_nothing_on_backfill() {
        assert!(banners_to_fire(&mirrored(2), false, true, true).is_empty());
    }

    #[test]
    fn banners_nothing_when_disabled() {
        assert!(banners_to_fire(&mirrored(2), false, false, false).is_empty());
    }

    #[test]
    fn banners_nothing_for_empty_poll() {
        assert!(banners_to_fire(&[], false, false, true).is_empty());
    }

    #[test]
    fn banners_one_each_up_to_three() {
        for count in 1..=3 {
            let b = banners_to_fire(&mirrored(count), false, false, true);
            assert_eq!(b.len(), count as usize);
        }
        let b = banners_to_fire(&mirrored(1), false, false, true);
        assert_eq!(b[0].title, "Ann \u{b7} JRST-1");
        assert_eq!(b[0].body, "msg 1");
    }

    #[test]
    fn banners_collapse_above_three() {
        let b = banners_to_fire(&mirrored(4), false, false, true);
        assert_eq!(b, vec![Banner { title: "JAIT".into(), body: "4 new notifications".into() }]);
    }

    #[test]
    fn banner_title_falls_back_without_actor_or_issue() {
        let mut one = mirrored(1);
        one[0].actor_name = None;
        one[0].issue_identifier = None;
        assert_eq!(banners_to_fire(&one, false, false, true)[0].title, "JAIT");
    }

    #[test]
    fn reapplying_same_envelope_is_idempotent() {
        let conn = mem();
        let e = env(vec![n(2, "2026-10-31T10:00:00.000Z", None), n(1, "2026-10-31T09:00:00.000Z", None)], 2, NOW);
        let first = apply_poll(&conn, &e, NOW).unwrap();
        let before = inbox_list(&conn, Filter::All).unwrap();
        let second = apply_poll(&conn, &e, NOW).unwrap();
        assert_eq!(first.len(), 2);
        assert!(second.is_empty());
        assert_eq!(inbox_list(&conn, Filter::All).unwrap(), before);
    }

    #[test]
    fn stores_wire_fields() {
        let conn = mem();
        apply_poll(&conn, &env(vec![n(1, "2026-10-31T09:00:00.000Z", None)], 1, NOW), NOW).unwrap();
        let row = &inbox_list(&conn, Filter::All).unwrap()[0];
        assert_eq!(row.remote_id, 1);
        assert_eq!(row.actor_name.as_deref(), Some("Ann"));
        assert_eq!(row.issue_remote_id, Some(11));
        assert_eq!(row.issue_identifier.as_deref(), Some("JRST-1"));
        assert_eq!(row.issue_title.as_deref(), Some("T"));
        assert_eq!(row.team_remote_id, Some(47));
        assert_eq!(row.comment_remote_id, Some(5));
    }

    #[test]
    fn new_rows_reported_only_when_unseen_and_unread() {
        let conn = mem();
        apply_poll(&conn, &env(vec![n(1, "2026-10-31T09:00:00.000Z", None)], 1, NOW), NOW).unwrap();
        let e = env(
            vec![
                n(3, "2026-10-31T11:00:00.000Z", None),
                n(2, "2026-10-31T10:00:00.000Z", Some("2026-10-31T10:30:00.000Z")),
                n(1, "2026-10-31T09:00:00.000Z", None),
            ],
            2,
            NOW,
        );
        let fresh = apply_poll(&conn, &e, NOW).unwrap();
        assert_eq!(fresh.iter().map(|f| f.remote_id).collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn server_read_updates_existing_row() {
        let conn = mem();
        apply_poll(&conn, &env(vec![n(1, "2026-10-31T09:00:00.000Z", None)], 1, NOW), NOW).unwrap();
        let read = "2026-10-31T09:30:00.000Z";
        apply_poll(&conn, &env(vec![n(1, "2026-10-31T09:00:00.000Z", Some(read))], 0, NOW), NOW).unwrap();
        assert_eq!(inbox_list(&conn, Filter::All).unwrap()[0].read_at.as_deref(), Some(read));
    }

    fn mark_locally_read(conn: &Connection, remote_id: i64, at: &str) {
        conn.execute("UPDATE notifications SET read_at = ?1 WHERE remote_id = ?2", params![at, remote_id]).unwrap();
        sync_queue::enqueue(conn, ENTITY_TYPE, &remote_id.to_string(), "read", None).unwrap();
    }

    #[test]
    fn mark_read_sets_read_at_lowers_count_and_enqueues_one_entry() {
        let conn = mem();
        let c = "2026-10-31T09:00:00.000Z";
        apply_poll(&conn, &env(vec![n(1, c, None), n(2, c, None)], 2, NOW), NOW).unwrap();
        assert!(mark_read(&conn, 1, NOW).unwrap());
        let all = inbox_list(&conn, Filter::All).unwrap();
        assert_eq!(all.iter().find(|x| x.remote_id == 1).unwrap().read_at.as_deref(), Some(NOW));
        assert_eq!(unread_count(&conn).unwrap(), 1);
        let q = sync_queue::pending_entries(&conn).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!((q[0].entity_type.as_str(), q[0].entity_id.as_str(), q[0].action.as_str()), ("notification", "1", "read"));
    }

    #[test]
    fn mark_read_twice_or_unknown_is_a_noop() {
        let conn = mem();
        apply_poll(&conn, &env(vec![n(1, "2026-10-31T09:00:00.000Z", None)], 1, NOW), NOW).unwrap();
        assert!(mark_read(&conn, 1, NOW).unwrap());
        assert!(!mark_read(&conn, 1, NOW).unwrap());
        assert!(!mark_read(&conn, 99, NOW).unwrap());
        assert_eq!(unread_count(&conn).unwrap(), 0);
        assert_eq!(sync_queue::pending_entries(&conn).unwrap().len(), 1);
    }

    #[test]
    fn poll_after_mark_read_keeps_row_read() {
        let conn = mem();
        let c = "2026-10-31T09:00:00.000Z";
        apply_poll(&conn, &env(vec![n(1, c, None)], 1, NOW), NOW).unwrap();
        mark_read(&conn, 1, "2026-10-31T09:45:00.000Z").unwrap();
        apply_poll(&conn, &env(vec![n(1, c, None)], 1, NOW), NOW).unwrap();
        assert!(!inbox_list(&conn, Filter::All).unwrap()[0].is_unread());
    }

    #[test]
    fn pending_local_read_survives_older_unread_server_row() {
        let conn = mem();
        let c = "2026-10-31T09:00:00.000Z";
        apply_poll(&conn, &env(vec![n(1, c, None)], 1, NOW), NOW).unwrap();
        let local = "2026-10-31T09:45:00.000Z";
        mark_locally_read(&conn, 1, local);
        let fresh = apply_poll(&conn, &env(vec![n(1, c, None)], 1, NOW), NOW).unwrap();
        assert!(fresh.is_empty());
        assert_eq!(inbox_list(&conn, Filter::All).unwrap()[0].read_at.as_deref(), Some(local));
    }

    #[test]
    fn synced_local_read_yields_to_server() {
        let conn = mem();
        let c = "2026-10-31T09:00:00.000Z";
        apply_poll(&conn, &env(vec![n(1, c, None)], 1, NOW), NOW).unwrap();
        mark_locally_read(&conn, 1, "2026-10-31T09:45:00.000Z");
        let entry = &sync_queue::pending_entries(&conn).unwrap()[0];
        sync_queue::mark_synced(&conn, entry.id.unwrap()).unwrap();
        apply_poll(&conn, &env(vec![n(1, c, None)], 1, NOW), NOW).unwrap();
        assert!(inbox_list(&conn, Filter::All).unwrap()[0].is_unread());
    }

    #[test]
    fn pending_read_all_covers_rows_created_up_to_before() {
        let conn = mem();
        sync_queue::enqueue(
            &conn,
            ENTITY_TYPE,
            "all",
            "read_all",
            Some(r#"{"before":"2026-10-31T10:00:00.000Z"}"#),
        )
        .unwrap();
        let e = env(vec![n(2, "2026-10-31T11:00:00.000Z", None), n(1, "2026-10-31T10:00:00.000Z", None)], 2, NOW);
        let fresh = apply_poll(&conn, &e, NOW).unwrap();
        assert_eq!(fresh.iter().map(|f| f.remote_id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(inbox_list(&conn, Filter::Unread).unwrap().len(), 1);
    }

    #[test]
    fn unread_count_follows_meta() {
        let conn = mem();
        apply_poll(&conn, &env(vec![n(1, "2026-10-31T09:00:00.000Z", None)], 7, NOW), NOW).unwrap();
        // The server knows of more unread rows than the mirror holds.
        assert_eq!(unread_count(&conn).unwrap(), 7);
        apply_poll(&conn, &env(vec![], 3, NOW), NOW).unwrap();
        assert_eq!(unread_count(&conn).unwrap(), 3);
    }

    #[test]
    fn unread_count_before_any_poll_counts_local_rows() {
        let conn = mem();
        assert_eq!(unread_count(&conn).unwrap(), 0);
    }

    #[test]
    fn cursor_advances_to_server_time() {
        let conn = mem();
        assert_eq!(cursor(&conn).unwrap(), None);
        apply_poll(&conn, &env(vec![], 0, "2026-10-31T11:00:00.000Z"), NOW).unwrap();
        assert_eq!(cursor(&conn).unwrap().as_deref(), Some("2026-10-31T11:00:00.000Z"));
        apply_poll(&conn, &env(vec![], 0, "2026-10-31T11:00:15.000Z"), NOW).unwrap();
        assert_eq!(cursor(&conn).unwrap().as_deref(), Some("2026-10-31T11:00:15.000Z"));
    }

    #[test]
    fn etag_round_trips() {
        let conn = mem();
        assert_eq!(etag(&conn).unwrap(), None);
        set_etag(&conn, "W/\"a\"").unwrap();
        set_etag(&conn, "W/\"b\"").unwrap();
        assert_eq!(etag(&conn).unwrap().as_deref(), Some("W/\"b\""));
    }

    #[test]
    fn inbox_orders_newest_first_and_filters() {
        let conn = mem();
        let e = env(
            vec![
                n(1, "2026-10-31T09:00:00.000Z", None),
                n(2, "2026-10-31T11:00:00.000Z", Some("2026-10-31T11:30:00.000Z")),
                n(3, "2026-10-31T10:00:00.000Z", None),
            ],
            2,
            NOW,
        );
        apply_poll(&conn, &e, NOW).unwrap();
        let ids = |f| inbox_list(&conn, f).unwrap().iter().map(|x| x.remote_id).collect::<Vec<_>>();
        assert_eq!(ids(Filter::All), vec![2, 3, 1]);
        assert_eq!(ids(Filter::Unread), vec![3, 1]);
    }

    #[test]
    fn inbox_ties_break_by_id_descending() {
        let conn = mem();
        let t = "2026-10-31T10:00:00.000Z";
        apply_poll(&conn, &env(vec![n(1, t, None), n(2, t, None)], 2, NOW), NOW).unwrap();
        let rows = inbox_list(&conn, Filter::All).unwrap();
        assert!(rows[0].id > rows[1].id);
    }

    fn count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM notifications", [], |r| r.get(0)).unwrap()
    }

    fn remote_ids(conn: &Connection) -> Vec<i64> {
        inbox_list(conn, Filter::All).unwrap().into_iter().map(|x| x.remote_id).collect::<Vec<_>>()
    }

    #[test]
    fn prune_removes_read_rows_older_than_30_days() {
        let conn = mem();
        // NOW is 2026-10-31; 30 days earlier is 2026-10-01T12:00.
        let items = vec![
            n(1, "2026-09-01T00:00:00.000Z", Some("2026-09-02T00:00:00.000Z")),
            n(2, "2026-09-01T00:00:00.000Z", Some("2026-10-30T00:00:00.000Z")),
        ];
        apply_poll(&conn, &env(items, 0, NOW), NOW).unwrap();
        assert_eq!(prune(&conn, NOW).unwrap(), 1);
        assert_eq!(remote_ids(&conn), vec![2]);
    }

    #[test]
    fn prune_keeps_old_unread_rows() {
        let conn = mem();
        apply_poll(&conn, &env(vec![n(1, "2026-09-01T00:00:00.000Z", None)], 1, NOW), NOW).unwrap();
        assert_eq!(prune(&conn, NOW).unwrap(), 0);
        assert_eq!(count(&conn), 1);
    }

    #[test]
    fn prune_removes_rows_of_tombstoned_issues() {
        let conn = mem();
        let items = vec![n(1, "2026-10-30T00:00:00.000Z", None), n(2, "2026-10-30T00:00:00.000Z", None)];
        apply_poll(&conn, &env(items, 2, NOW), NOW).unwrap();
        // n(1) points at issue remote id 11; tombstone the local issue.
        conn.execute_batch(&format!(
            "INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t', 'T', 'T', '{NOW}', '{NOW}');
             INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l', 'B', 0, 't', '{NOW}', '{NOW}');
             INSERT INTO issues (id, title, priority, lane_id, team_id, remote_id, deleted_at, created_at, updated_at)
               VALUES ('i', 'X', 0, 'l', 't', 11, '{NOW}', '{NOW}', '{NOW}');"
        ))
        .unwrap();
        assert_eq!(prune(&conn, NOW).unwrap(), 1);
        assert_eq!(remote_ids(&conn), vec![2]);
    }

    #[test]
    fn delete_for_issue_removes_only_that_issues_rows() {
        let conn = mem();
        let items = vec![n(1, "2026-10-30T00:00:00.000Z", None), n(2, "2026-10-30T00:00:00.000Z", None)];
        apply_poll(&conn, &env(items, 2, NOW), NOW).unwrap();
        assert_eq!(delete_for_issue(&conn, 12).unwrap(), 1);
        assert_eq!(remote_ids(&conn), vec![1]);
    }

    #[test]
    fn prune_keeps_everything_else() {
        let conn = mem();
        let items = vec![
            n(1, "2026-10-30T00:00:00.000Z", None),
            n(2, "2026-10-20T00:00:00.000Z", Some("2026-10-25T00:00:00.000Z")),
        ];
        apply_poll(&conn, &env(items, 1, NOW), NOW).unwrap();
        assert_eq!(prune(&conn, NOW).unwrap(), 0);
        assert_eq!(count(&conn), 2);
    }

    #[test]
    fn envelope_decodes_from_rails_json() {
        let json = r#"{"notifications":[{"id":9,"action":"commented","message":"hi","read_at":null,
            "created_at":"2026-10-31T10:00:00.000Z","updated_at":"2026-10-31T10:00:00.000Z",
            "actor":{"id":2,"name":"Bo"},"issue":{"id":4,"identifier":"A-1","title":"X","team_id":3},
            "comment_id":null,"version_id":12}],
            "meta":{"unread_count":1,"server_time":"2026-10-31T10:00:01.000Z"}}"#;
        let e: Envelope = serde_json::from_str(json).unwrap();
        assert_eq!(e.notifications[0].issue.as_ref().unwrap().identifier, "A-1");
        assert_eq!(e.meta.unread_count, 1);
    }
}
