//! Local cache of server Activity entries for an issue (`issue_activity_entries`).
//! Pure over a `rusqlite::Connection`: no Iced, no HTTP.
//!
//! Entries are keyed by `(issue_id, entry_cursor)` where the cursor is the full
//! `<type>:<id>` string: numeric ids collide across entry types.
// Unused until the fetch routine and display layer land; drop then.

use rusqlite::{params, Connection, Result};
use serde_json::{json, Value};

mod pending;

use pending::{describe, priority_label, EventKind};
pub use pending::{record_changes, record_dependency, Source};
use super::models::Priority;
use crate::api::activity::{
    ActivityActor, ActivityEntry, ActivityRef, ChangeValue, DependencyDirection, DependencyEvent,
    DependencyKind, FieldChange,
};

/// A cached entry as stored. `payload` holds the type-specific structured data.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEntry {
    pub cursor: String,
    /// `version`, `dependency` or `reference`.
    pub entry_type: String,
    pub created_at: String,
    pub actor_remote_id: Option<i64>,
    pub actor_name: Option<String>,
    pub payload: Value,
    /// Server text fallback (HTML); empty for references.
    pub description: String,
}

/// Per-issue Activity fetch state (`issue_activity_state`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FetchState {
    /// `X-Server-Time` of the last successful fetch; `None` if never fetched.
    pub cursor: Option<String>,
    /// True once the full history has been fetched.
    pub fully_fetched: bool,
}

/// Store a fetched batch for `issue_id`, upserting by entry cursor so entries
/// re-delivered at a cursor overlap replace themselves rather than duplicate.
/// The fetch state is written in the same transaction: a `Some` cursor replaces
/// the stored one (`None` keeps it), and `fully_fetched` only ever flips to true.
///
/// Also prunes settled Pending changes (see [`prune_settled`]), treating the
/// fetch as having started now. Callers that know when the request actually
/// began should use [`store_batch_at`]. Test helper only.
#[cfg(test)]
pub fn store_batch(
    conn: &Connection,
    issue_id: &str,
    entries: &[ActivityEntry],
    cursor: Option<&str>,
    fully_fetched: bool,
) -> Result<()> {
    store_batch_at(conn, issue_id, entries, cursor, fully_fetched, &super::now_iso())
}

/// [`store_batch`] with the local time (`db::now_iso()` format) at which the
/// fetch began. Pending changes whose queue entry synced at or before that
/// instant are pruned in the same transaction.
pub fn store_batch_at(
    conn: &Connection,
    issue_id: &str,
    entries: &[ActivityEntry],
    cursor: Option<&str>,
    fully_fetched: bool,
    fetch_started_at: &str,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO issue_activity_entries
                (issue_id, entry_cursor, entry_type, created_at, actor_remote_id, actor_name, payload, description)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (issue_id, entry_cursor) DO UPDATE SET
                entry_type = excluded.entry_type,
                created_at = excluded.created_at,
                actor_remote_id = excluded.actor_remote_id,
                actor_name = excluded.actor_name,
                payload = excluded.payload,
                description = excluded.description",
        )?;
        for entry in entries {
            let row = to_row(entry);
            stmt.execute(params![
                issue_id,
                row.cursor,
                row.entry_type,
                row.created_at,
                row.actor_remote_id,
                row.actor_name,
                row.payload.to_string(),
                row.description,
            ])?;
        }
    }
    tx.execute(
        "INSERT INTO issue_activity_state (issue_id, cursor, fully_fetched)
         VALUES (?1, ?2, ?3)
         ON CONFLICT (issue_id) DO UPDATE SET
            cursor = COALESCE(excluded.cursor, cursor),
            fully_fetched = MAX(fully_fetched, excluded.fully_fetched)",
        params![issue_id, cursor, fully_fetched],
    )?;
    prune_settled(&tx, issue_id, fetch_started_at)?;
    tx.commit()
}

/// Remove the issue's settled Pending changes: Pending-change `issue_events` rows linked
/// to a sync-queue entry that has synced AND whose `synced_at` is not later
/// than `fetch_started_at`, i.e. the server had already applied the change
/// when the fetch began, so the fetched Activity carries it.
///
/// Both timestamps are local-clock `db::now_iso()` strings (fixed-width UTC, so
/// string comparison orders them). The server's `X-Server-Time` cursor is never
/// compared with `synced_at`: that would mix clocks. Comparing against the
/// fetch's *start* (not its completion) also keeps a fetch that was already in
/// flight when a push landed from pruning a change its response may predate.
/// A queue entry that is unsynced, or missing, never settles. Returns the
/// number of rows removed.
pub fn prune_settled(conn: &Connection, issue_id: &str, fetch_started_at: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM issue_events
         WHERE issue_id = ?1 AND source = 'local' AND sync_queue_id IN (
             SELECT id FROM sync_queue
             WHERE synced_at IS NOT NULL AND synced_at <= ?2)",
        params![issue_id, fetch_started_at],
    )
}

/// The issue's fetch state; the default (no cursor, not fully fetched) if none stored.
pub fn fetch_state(conn: &Connection, issue_id: &str) -> Result<FetchState> {
    let row = conn.query_row(
        "SELECT cursor, fully_fetched FROM issue_activity_state WHERE issue_id = ?1",
        params![issue_id],
        |r| {
            Ok(FetchState {
                cursor: r.get(0)?,
                fully_fetched: r.get::<_, i64>(1)? != 0,
            })
        },
    );
    match row {
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(FetchState::default()),
        other => other,
    }
}

/// Every cached entry for the issue, oldest first (ties broken by insert order).
pub fn entries_for_issue(conn: &Connection, issue_id: &str) -> Result<Vec<StoredEntry>> {
    let mut stmt = conn.prepare(
        "SELECT entry_cursor, entry_type, created_at, actor_remote_id, actor_name, payload, description
         FROM issue_activity_entries WHERE issue_id = ?1 ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map(params![issue_id], |r| {
        let payload: String = r.get(5)?;
        Ok(StoredEntry {
            cursor: r.get(0)?,
            entry_type: r.get(1)?,
            created_at: r.get(2)?,
            actor_remote_id: r.get(3)?,
            actor_name: r.get(4)?,
            payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
            description: r.get(6)?,
        })
    })?;
    rows.collect()
}

fn to_row(entry: &ActivityEntry) -> StoredEntry {
    match entry {
        ActivityEntry::Version(v) => row(
            "version",
            &v.cursor,
            &v.created_at,
            v.actor.as_ref(),
            json!({ "changes": v.changes.iter().map(change_json).collect::<Vec<_>>() }),
            &v.description,
        ),
        ActivityEntry::Dependency(d) => row(
            "dependency",
            &d.cursor,
            &d.created_at,
            d.actor.as_ref(),
            json!({
                "event": match d.event {
                    DependencyEvent::Create => "create",
                    DependencyEvent::Destroy => "destroy",
                    DependencyEvent::Unknown => "unknown",
                },
                "kind": match d.kind {
                    DependencyKind::Blocks => "blocks",
                    DependencyKind::Relates => "relates",
                    DependencyKind::Duplicates => "duplicates",
                    DependencyKind::Unknown => "unknown",
                },
                "direction": match d.direction {
                    DependencyDirection::Blocking => "blocking",
                    DependencyDirection::BlockedBy => "blocked_by",
                    DependencyDirection::Relates => "relates",
                    DependencyDirection::Duplicates => "duplicates",
                    DependencyDirection::DuplicatedBy => "duplicated_by",
                    DependencyDirection::Unknown => "unknown",
                },
                "other_issue": d.other_issue.as_ref().map(|o| json!({
                    "id": o.id, "identifier": o.identifier, "title": o.title,
                })),
            }),
            &d.description,
        ),
        ActivityEntry::Reference(r) => row(
            "reference",
            &r.cursor,
            &r.created_at,
            r.user.as_ref(),
            json!({
                "source_type": r.source_type,
                "source_issue": r.source_issue.as_ref().map(|s| json!({
                    "id": s.id, "identifier": s.identifier, "title": s.title,
                })),
            }),
            "",
        ),
    }
}

/// Re-format a server time (`...:47Z`, `...:47.5Z`, or with an offset) into the
/// fixed-width local format of `db::now_iso()`, so string order matches time
/// order against Pending changes in the same second. Unparseable input is
/// stored as received.
fn normalise_time(server: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(server)
        .map(|t| t.with_timezone(&chrono::Utc).format(super::TS_FORMAT).to_string())
        .unwrap_or_else(|_| server.to_string())
}

fn row(
    entry_type: &str,
    cursor: &str,
    created_at: &str,
    actor: Option<&ActivityActor>,
    payload: Value,
    description: &str,
) -> StoredEntry {
    StoredEntry {
        cursor: cursor.to_string(),
        entry_type: entry_type.to_string(),
        created_at: normalise_time(created_at),
        actor_remote_id: actor.map(|a| a.id),
        actor_name: actor.map(|a| a.name.clone()),
        payload,
        description: description.to_string(),
    }
}

fn change_json(c: &FieldChange) -> Value {
    json!({ "field": c.field, "from": value_json(&c.from), "to": value_json(&c.to) })
}

fn value_json(v: &Option<ChangeValue>) -> Value {
    match v {
        None => Value::Null,
        Some(ChangeValue::Scalar(s)) => s.clone(),
        Some(ChangeValue::Ref(ActivityRef { id, name, identifier })) => {
            json!({ "id": id, "name": name, "identifier": identifier })
        }
    }
}

// ---------------------------------------------------------------------------
// Display-ready Activity
// ---------------------------------------------------------------------------

/// Where a display item came from. `Pending` is reserved for unsynced local
/// edits (JRST-141); server-cached entries are always `Server`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemOrigin {
    Server,
    Pending,
}

/// Who did it, as the server reported it. The resolver (JRST-136) maps
/// `remote_id` to a local member; until then `name` is the server's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorSlot {
    pub remote_id: Option<i64>,
    pub name: Option<String>,
}

/// One rendered line of Activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityItem {
    /// Stable per line: `<entry cursor>#<line index>`.
    pub key: String,
    /// The server's `created_at` for the entry.
    pub at: String,
    pub actor: ActorSlot,
    /// Plain text, no HTML.
    pub text: String,
    pub origin: ItemOrigin,
}

/// Display-ready Activity for an issue, oldest first. Each version change,
/// dependency event or reference becomes its own line.
pub fn activity_for_issue(conn: &Connection, issue_id: &str) -> Result<Vec<ActivityItem>> {
    let mut items = Vec::new();
    items.extend(created_item(conn, issue_id)?);
    for entry in entries_for_issue(conn, issue_id)? {
        let actor = resolve_actor(conn, entry.actor_remote_id, entry.actor_name.as_deref())?;
        let texts = match entry.entry_type.as_str() {
            "version" => version_lines(conn, &entry),
            "reference" => vec![reference_line(&entry)],
            "dependency" => dependency_line(&entry).into_iter().collect(),
            _ => Vec::new(),
        };
        for (i, text) in texts.into_iter().enumerate() {
            items.push(ActivityItem {
                key: format!("{}#{i}", entry.cursor),
                at: entry.created_at.clone(),
                actor: actor.clone(),
                text,
                origin: ItemOrigin::Server,
            });
        }
    }
    merge_pending(conn, issue_id, &mut items)?;
    Ok(items)
}

/// One slot of the issue-detail Activity list: a display line or a comment thread.
pub enum TimelineItem<'a> {
    Activity(&'a ActivityItem),
    /// A top-level comment with its replies as `(depth, comment)` in display order.
    Thread(Vec<(usize, &'a crate::db::models::Comment)>),
}

/// Interleave Activity lines with comment threads by time. The lines keep their
/// own order (created stays on top); a thread goes after every line not later
/// than its first comment, so a line wins a timestamp tie.
pub fn timeline<'a>(
    items: &'a [ActivityItem],
    comments: &'a [crate::db::models::Comment],
) -> Vec<TimelineItem<'a>> {
    use crate::db::models::Comment;
    let mut threads: Vec<Vec<(usize, &Comment)>> = Vec::new();
    for (depth, c) in Comment::thread_order(comments) {
        if depth == 0 {
            threads.push(Vec::new());
        }
        match threads.last_mut() {
            Some(t) => t.push((depth, c)),
            None => threads.push(vec![(depth, c)]),
        }
    }
    threads.sort_by(|a, b| a[0].1.created_at.cmp(&b[0].1.created_at));
    let mut out = Vec::with_capacity(items.len() + threads.len());
    let mut lines = items.iter().peekable();
    for thread in threads {
        let at = thread[0].1.created_at.as_str();
        while let Some(item) = lines.next_if(|i| i.at.as_str() <= at) {
            out.push(TimelineItem::Activity(item));
        }
        out.push(TimelineItem::Thread(thread));
    }
    out.extend(lines.map(TimelineItem::Activity));
    out
}

/// Add the issue's Pending changes (local edits linked to a sync-queue entry),
/// attributed to "You", each after the last item not later than itself. A
/// local-only issue's created line is the derived `created_item`, never a
/// stored `created` event.
fn merge_pending(conn: &Connection, issue_id: &str, items: &mut Vec<ActivityItem>) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, old_value, new_value, created_at, sync_queue_id IS NOT NULL
         FROM issue_events WHERE issue_id = ?1 AND source = 'local'
         ORDER BY created_at, id",
    )?;
    let rows = stmt.query_map(params![issue_id], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, bool>(5)?,
        ))
    })?;
    for row in rows {
        let (id, kind, old, new, at, linked) = row?;
        let Some(kind) = EventKind::parse(&kind) else {
            continue;
        };
        if !linked {
            continue;
        }
        let item = ActivityItem {
            key: format!("pending:{id}#0"),
            text: describe(kind, old.as_deref(), new.as_deref()),
            at,
            actor: ActorSlot { remote_id: None, name: Some("You".to_string()) },
            origin: ItemOrigin::Pending,
        };
        let pos = items.iter().rposition(|i| i.at <= item.at).map_or(0, |p| p + 1);
        items.insert(pos, item);
    }
    Ok(())
}

/// "mentioned in MTK-2: Title"; the source issue may be gone (null).
fn reference_line(entry: &StoredEntry) -> String {
    let src = &entry.payload["source_issue"];
    match src["identifier"].as_str() {
        Some(ident) => match src["title"].as_str().filter(|t| !t.is_empty()) {
            Some(title) => format!("mentioned in {ident}: {title}"),
            None => format!("mentioned in {ident}"),
        },
        None => "mentioned in another issue".to_string(),
    }
}

/// Resolve an entry's actor: the local member with that remote id wins, then the
/// name the server sent. No actor at all gives no name (never "Someone").
fn resolve_actor(conn: &Connection, remote_id: Option<i64>, server_name: Option<&str>) -> Result<ActorSlot> {
    let local = match remote_id {
        Some(id) => crate::db::queries::find_member_by_remote_id(conn, id)?.map(|m| m.name),
        None => None,
    };
    Ok(ActorSlot {
        remote_id,
        name: local.or_else(|| server_name.map(str::to_string)),
    })
}

/// The desktop-only "created the issue" line, derived from the issue's own
/// `creator_id` and `created_at` (the server timeline has no create entry).
/// `None` only if the issue doesn't exist. A missing creator gives an empty actor.
fn created_item(conn: &Connection, issue_id: &str) -> Result<Option<ActivityItem>> {
    let row = conn.query_row(
        "SELECT i.created_at, m.remote_id, m.name
         FROM issues i LEFT JOIN members m ON m.id = i.creator_id
         WHERE i.id = ?1",
        params![issue_id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<String>>(2)?)),
    );
    match row {
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
        Ok((at, remote_id, name)) => Ok(Some(ActivityItem {
            key: "created#0".to_string(),
            at,
            actor: resolve_actor(conn, remote_id, name.as_deref())?,
            text: "created the issue".to_string(),
            origin: ItemOrigin::Server,
        })),
    }
}

/// One line per change in a version entry. Changes this layer can't word
/// (unknown fields) are covered by a single fallback line: the server's
/// description with tags stripped (JRST-135), once per entry.
fn version_lines(conn: &Connection, entry: &StoredEntry) -> Vec<String> {
    let Some(changes) = entry.payload["changes"].as_array() else {
        return Vec::new();
    };
    let mut unknown = false;
    let mut lines: Vec<String> = changes
        .iter()
        .filter_map(|c| {
            let line = plain_change_line(c).or_else(|| reference_change_line(conn, c));
            unknown |= line.is_none();
            line
        })
        .collect();
    if unknown {
        let text = strip_html(&entry.description);
        if !text.is_empty() {
            lines.push(text);
        }
    }
    lines
}

/// Server HTML as plain text: tags removed, common entities decoded,
/// whitespace collapsed.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Reference fields (JRST-134): each side is resolved to the local entity's
/// current name by remote id, falling back to the name the server sent.
fn reference_change_line(conn: &Connection, change: &Value) -> Option<String> {
    let field = change["field"].as_str()?;
    // `None` kind = team, which `describe` has no wording for.
    let (kind, table) = match field {
        "lane_id" => (Some(EventKind::Status), "lanes"),
        "assignee_id" => (Some(EventKind::Assignee), "members"),
        "project_id" => (Some(EventKind::Project), "projects"),
        "parent_issue_id" => (Some(EventKind::Parent), "issues"),
        "team_id" => (None, "teams"),
        _ => return None,
    };
    let name = |v: &Value| -> Option<String> {
        if v.is_null() {
            return None;
        }
        let remote = v["id"].as_i64();
        let sent = |key: &str| v[key].as_str().filter(|s| !s.is_empty()).map(String::from);
        remote
            .and_then(|rid| local_ref_name(conn, table, rid))
            .or_else(|| sent(if table == "issues" { "identifier" } else { "name" }))
            .or_else(|| sent("name"))
            .or_else(|| remote.map(|r| format!("#{r}")))
    };
    let (old, new) = (name(&change["from"]), name(&change["to"]));
    match kind {
        None => Some(match (old, new) {
            (Some(o), Some(n)) => format!("moved from team {o} to team {n}"),
            (None, Some(n)) => format!("moved to team {n}"),
            (Some(o), None) => format!("removed from team {o}"),
            (None, None) => "changed the team".to_string(),
        }),
        Some(EventKind::Project) if new.is_none() => Some(match old {
            Some(o) => format!("removed from project {o}"),
            None => "removed from project".to_string(),
        }),
        Some(k) => Some(describe(k, old.as_deref(), new.as_deref())),
    }
}

fn local_ref_name(conn: &Connection, table: &str, remote_id: i64) -> Option<String> {
    let sql = match table {
        "lanes" => "SELECT name FROM lanes WHERE remote_id = ?1",
        "members" => "SELECT name FROM members WHERE remote_id = ?1",
        "projects" => "SELECT name FROM projects WHERE remote_id = ?1",
        "teams" => "SELECT name FROM teams WHERE remote_id = ?1",
        _ => {
            "SELECT t.identifier || '-' || i.team_number FROM issues i
             JOIN teams t ON t.id = i.team_id WHERE i.remote_id = ?1"
        }
    };
    conn.query_row(sql, params![remote_id], |r| r.get::<_, Option<String>>(0))
        .ok()
        .flatten()
}

/// One line for a dependency entry, worded from this issue's side using
/// `direction` and `event` (the server description is worded from the blocking
/// side). `relates`, `duplicates` and `duplicated_by` are worded here.
fn dependency_line(entry: &StoredEntry) -> Option<String> {
    let direction = entry.payload["direction"].as_str()?;
    let other = entry.payload["other_issue"]["identifier"]
        .as_str()
        .map(str::to_string);
    let kind = match direction {
        "blocking" => EventKind::Blocking,
        "blocked_by" => EventKind::BlockedBy,
        "relates" | "duplicates" | "duplicated_by" => {
            let (added, removed, generic) = match direction {
                "relates" => ("now related to", "no longer related to", "changed related issues"),
                "duplicates" => ("now a duplicate of", "no longer a duplicate of", "changed duplicate issues"),
                _ => ("now duplicated by", "no longer duplicated by", "changed duplicate issues"),
            };
            let verb = match entry.payload["event"].as_str()? {
                "create" => added,
                "destroy" => removed,
                _ => return None,
            };
            return Some(match other {
                Some(id) => format!("{verb} {id}"),
                None => generic.to_string(),
            });
        }
        _ => return None,
    };
    let (old, new) = match entry.payload["event"].as_str()? {
        "create" => (None, other),
        "destroy" => (other, None),
        _ => return None,
    };
    Some(describe(kind, old.as_deref(), new.as_deref()))
}

/// Wording for a plain (non-reference) field, reusing `describe`.
fn plain_change_line(change: &Value) -> Option<String> {
    let field = change["field"].as_str()?;
    let kind = match field {
        "priority" => EventKind::Priority,
        "title" => EventKind::Title,
        "description" => EventKind::Description,
        "estimate" => EventKind::Estimate,
        "due_date" => EventKind::DueDate,
        _ => return None,
    };
    let value = |v: &Value| -> Option<String> {
        match v {
            Value::Null => None,
            Value::String(s) if kind == EventKind::Priority => Some(priority_name_label(s)),
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    };
    Some(describe(
        kind,
        value(&change["from"]).as_deref(),
        value(&change["to"]).as_deref(),
    ))
}

/// Server priority name (`"no_priority"`, `"high"`, ...) as the desktop label.
fn priority_name_label(name: &str) -> String {
    match Priority::from_rails_str(name) {
        Ok(p) => priority_label(p.to_i32()),
        Err(_) => name.to_string(),
    }
}

/// Remove the Pending changes of a queue entry that is being discarded without
/// a successful push (dropped or rejected). Call in the same transaction that
/// retires the entry. Does not touch successfully synced entries (JRST-143).
pub fn discard_pending_for_entry(conn: &Connection, entry_id: i64) -> Result<usize> {
    conn.execute("DELETE FROM issue_events WHERE sync_queue_id = ?1", params![entry_id])
}

/// Remove the Pending changes of every still-unsynced queue entry for an
/// entity that is being retired without a push (e.g. a server tombstone).
pub fn discard_pending_for_entity(conn: &Connection, entity_type: &str, entity_id: &str) -> Result<usize> {
    conn.execute(
        "DELETE FROM issue_events WHERE sync_queue_id IN
           (SELECT id FROM sync_queue
            WHERE entity_type = ?1 AND entity_id = ?2 AND synced_at IS NULL)",
        params![entity_type, entity_id],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t1','T','T',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)", params![now]).unwrap();
        for id in ["i1", "i2"] {
            conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at) VALUES (?2,'i',4,'l1','t1',?1,?1)", params![now, id]).unwrap();
        }
        conn
    }

    fn feed(body: &str) -> Vec<ActivityEntry> {
        serde_json::from_str(body).unwrap()
    }

    const BATCH: &str = r#"[
      {"type":"version","id":5,"cursor":"version:5","created_at":"2026-07-06T00:40:47Z",
       "actor":{"id":2,"name":"ryan"},"event":"update",
       "changes":[{"field":"priority","from":"high","to":"urgent"},
                  {"field":"lane_id","from":{"id":3,"name":"Backlog"},"to":null}],
       "description":"changed priority"},
      {"type":"dependency","id":5,"cursor":"dependency:5","created_at":"2026-07-06T01:00:00Z",
       "event":"create","kind":"blocks","direction":"blocked_by",
       "other_issue":{"id":2,"identifier":"MTK-2","title":"X"},"actor":null,"description":"d"},
      {"type":"reference","id":5,"cursor":"reference:5","created_at":"2026-07-06T02:00:00Z",
       "source_issue":{"id":2,"identifier":"MTK-2","title":"X"},"source_type":"comment",
       "user":{"id":3,"name":"ann"}}
    ]"#;

    #[test]
    fn stores_and_reads_back_with_all_fields() {
        let conn = mem();
        store_batch(&conn, "i1", &feed(BATCH), None, false).unwrap();
        let got = entries_for_issue(&conn, "i1").unwrap();
        assert_eq!(got.len(), 3);

        assert_eq!(got[0].cursor, "version:5");
        assert_eq!(got[0].entry_type, "version");
        assert_eq!(got[0].created_at, "2026-07-06T00:40:47.000Z");
        assert_eq!(got[0].actor_remote_id, Some(2));
        assert_eq!(got[0].actor_name.as_deref(), Some("ryan"));
        assert_eq!(got[0].description, "changed priority");
        assert_eq!(got[0].payload["changes"][0]["to"], "urgent");
        assert_eq!(got[0].payload["changes"][1]["from"]["name"], "Backlog");
        assert!(got[0].payload["changes"][1]["to"].is_null());

        assert_eq!(got[1].entry_type, "dependency");
        assert_eq!(got[1].actor_remote_id, None);
        assert_eq!(got[1].payload["direction"], "blocked_by");
        assert_eq!(got[1].payload["other_issue"]["identifier"], "MTK-2");

        assert_eq!(got[2].entry_type, "reference");
        assert_eq!(got[2].actor_name.as_deref(), Some("ann"));
        assert_eq!(got[2].payload["source_type"], "comment");
    }

    #[test]
    fn redelivery_is_idempotent() {
        let conn = mem();
        let all = feed(BATCH);
        store_batch(&conn, "i1", &all, None, false).unwrap();
        store_batch(&conn, "i1", &all[1..], None, false).unwrap();
        store_batch(&conn, "i1", &all, None, false).unwrap();
        assert_eq!(entries_for_issue(&conn, "i1").unwrap().len(), 3);
    }

    #[test]
    fn same_numeric_id_different_types_kept_separately() {
        let conn = mem();
        store_batch(&conn, "i1", &feed(BATCH), None, false).unwrap();
        let cursors: Vec<_> = entries_for_issue(&conn, "i1")
            .unwrap()
            .into_iter()
            .map(|e| e.cursor)
            .collect();
        assert_eq!(cursors, ["version:5", "dependency:5", "reference:5"]);
    }

    #[test]
    fn entries_are_scoped_per_issue() {
        let conn = mem();
        store_batch(&conn, "i1", &feed(BATCH), None, false).unwrap();
        store_batch(&conn, "i2", &feed(BATCH), None, false).unwrap();
        assert_eq!(entries_for_issue(&conn, "i1").unwrap().len(), 3);
        assert_eq!(entries_for_issue(&conn, "i2").unwrap().len(), 3);
    }

    #[test]
    fn deleting_the_issue_removes_its_entries() {
        let conn = mem();
        store_batch(&conn, "i1", &feed(BATCH), Some("t1"), true).unwrap();
        conn.execute("DELETE FROM issues WHERE id = 'i1'", []).unwrap();
        assert!(entries_for_issue(&conn, "i1").unwrap().is_empty());
        assert_eq!(fetch_state(&conn, "i1").unwrap(), FetchState::default());
    }

    #[test]
    fn fetch_state_defaults_when_never_fetched() {
        let conn = mem();
        assert_eq!(fetch_state(&conn, "i1").unwrap(), FetchState::default());
    }

    #[test]
    fn batch_and_state_written_together() {
        let conn = mem();
        store_batch(&conn, "i1", &feed(BATCH), Some("t1"), false).unwrap();
        let st = fetch_state(&conn, "i1").unwrap();
        assert_eq!(st.cursor.as_deref(), Some("t1"));
        assert!(!st.fully_fetched);
        assert_eq!(fetch_state(&conn, "i2").unwrap(), FetchState::default());

        store_batch(&conn, "i1", &[], Some("t2"), true).unwrap();
        let st = fetch_state(&conn, "i1").unwrap();
        assert_eq!(st.cursor.as_deref(), Some("t2"));
        assert!(st.fully_fetched);
    }

    #[test]
    fn none_cursor_keeps_cursor_and_full_flag_never_regresses() {
        let conn = mem();
        store_batch(&conn, "i1", &[], Some("t1"), true).unwrap();
        store_batch(&conn, "i1", &[], None, false).unwrap();
        let st = fetch_state(&conn, "i1").unwrap();
        assert_eq!(st.cursor.as_deref(), Some("t1"));
        assert!(st.fully_fetched);
    }

    #[test]
    fn failed_batch_leaves_state_untouched() {
        let conn = mem();
        store_batch(&conn, "i1", &[], Some("t1"), false).unwrap();
        // Unknown issue violates the FK: the whole transaction rolls back.
        assert!(store_batch(&conn, "nope", &feed(BATCH), Some("t9"), true).is_err());
        assert_eq!(fetch_state(&conn, "i1").unwrap().cursor.as_deref(), Some("t1"));
        assert_eq!(fetch_state(&conn, "nope").unwrap(), FetchState::default());
    }
}

#[cfg(test)]
mod display_tests {
    use super::*;
    use crate::db::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t1','T','T',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at) VALUES ('i1','i',4,'l1','t1',?1,?1)", params![now]).unwrap();
        conn
    }

    fn store(conn: &Connection, body: &str) {
        let entries: Vec<ActivityEntry> = serde_json::from_str(body).unwrap();
        store_batch(conn, "i1", &entries, None, false).unwrap();
    }

    #[test]
    fn each_plain_change_is_one_line_with_desktop_wording() {
        let conn = mem();
        store(&conn, r#"[{"type":"version","id":1,"cursor":"version:1","created_at":"2026-07-06T00:40:47Z",
          "actor":{"id":2,"name":"ryan"},"event":"update","description":"x",
          "changes":[{"field":"priority","from":"low","to":"high"},
                     {"field":"title","from":"a","to":"b"},
                     {"field":"description","from":"<p>a</p>","to":"<p>b</p>"},
                     {"field":"estimate","from":null,"to":3},
                     {"field":"due_date","from":"2026-01-01","to":null},
                     {"field":"priority","from":"high","to":"no_priority"}]}]"#);
        let items = activity_for_issue(&conn, "i1").unwrap()[1..].to_vec();
        let texts: Vec<_> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, [
            "changed priority from Low to High",
            "changed the title",
            "updated the description",
            "changed estimate from none to 3",
            "changed due date from 2026-01-01 to none",
            "changed priority from High to None",
        ]);
        assert_eq!(items[0].at, "2026-07-06T00:40:47.000Z");
        assert_eq!(items[0].actor.name.as_deref(), Some("ryan"));
        assert_eq!(items[0].key, "version:1#0");
        assert_eq!(items[0].origin, ItemOrigin::Server);
    }

    #[test]
    fn lines_are_ordered_by_server_created_at() {
        let conn = mem();
        store(&conn, r#"[
          {"type":"version","id":2,"cursor":"version:2","created_at":"2026-07-07T00:00:00Z","actor":null,"event":"update","description":"",
           "changes":[{"field":"title","from":"a","to":"b"}]},
          {"type":"version","id":1,"cursor":"version:1","created_at":"2026-07-06T00:00:00Z","actor":null,"event":"update","description":"",
           "changes":[{"field":"priority","from":"low","to":"urgent"}]}]"#);
        let items = activity_for_issue(&conn, "i1").unwrap()[1..].to_vec();
        assert_eq!(items[0].text, "changed priority from Low to Urgent");
        assert_eq!(items[1].text, "changed the title");
        assert_eq!(items[0].actor, ActorSlot { remote_id: None, name: None });
    }

    fn one_change(actor: &str) -> String {
        format!(r#"[{{"type":"version","id":1,"cursor":"version:1","created_at":"2026-07-06T00:00:00Z","actor":{actor},"event":"update","description":"","changes":[{{"field":"title","from":"a","to":"b"}}]}}]"#)
    }

    #[test]
    fn actor_prefers_mirrored_member_name() {
        let conn = mem();
        conn.execute("INSERT INTO members (id, remote_id, team_id, name, email, created_at, updated_at) VALUES ('m1',2,'t1','Ryan V','r@x.com','n','n')", []).unwrap();
        store(&conn, &one_change(r#"{"id":2,"name":"ryan"}"#));
        let items = activity_for_issue(&conn, "i1").unwrap()[1..].to_vec();
        assert_eq!(items[0].actor, ActorSlot { remote_id: Some(2), name: Some("Ryan V".into()) });
    }

    #[test]
    fn actor_not_mirrored_uses_server_name() {
        let conn = mem();
        store(&conn, &one_change(r#"{"id":9,"name":"ann"}"#));
        let items = activity_for_issue(&conn, "i1").unwrap()[1..].to_vec();
        assert_eq!(items[0].actor, ActorSlot { remote_id: Some(9), name: Some("ann".into()) });
    }

    #[test]
    fn null_actor_has_no_name() {
        let conn = mem();
        store(&conn, &one_change("null"));
        let items = activity_for_issue(&conn, "i1").unwrap();
        assert_eq!(items[0].actor, ActorSlot { remote_id: None, name: None });
    }

    #[test]
    fn empty_changes_yield_no_lines() {
        let conn = mem();
        store(&conn, r#"[{"type":"version","id":1,"cursor":"version:1","created_at":"2026-07-06T00:00:00Z","actor":null,"event":"update","description":"","changes":[]}]"#);
        assert_eq!(activity_for_issue(&conn, "i1").unwrap().len(), 1); // created only
    }
}

#[cfg(test)]
mod jrst_140_tests {
    use super::*;
    use crate::db::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t1','T','T',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO members (id, team_id, name, email, remote_id, created_at, updated_at) VALUES ('m1','t1','Ann','a@x',9,?1,?1)", params![now]).unwrap();
        conn
    }

    fn issue(conn: &Connection, id: &str, creator: Option<&str>, remote: Option<i64>) {
        conn.execute(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, creator_id, remote_id, created_at, updated_at)
             VALUES (?1,'i',4,'l1','t1',?2,?3,'2026-02-02T00:00:00.000Z','2026-02-02T00:00:00.000Z')",
            params![id, creator, remote],
        ).unwrap();
    }

    #[test]
    fn created_entry_is_first_with_member_name() {
        let conn = mem();
        issue(&conn, "i1", Some("m1"), Some(5));
        let batch: Vec<ActivityEntry> = serde_json::from_str(r#"[{"type":"version","id":1,"cursor":"version:1","created_at":"2026-01-01T00:00:00Z","actor":null,"event":"update","description":"","changes":[{"field":"title","from":"a","to":"b"}]}]"#).unwrap();
        store_batch(&conn, "i1", &batch, None, false).unwrap();
        let items = activity_for_issue(&conn, "i1").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "created the issue");
        assert_eq!(items[0].at, "2026-02-02T00:00:00.000Z");
        assert_eq!(items[0].actor, ActorSlot { remote_id: Some(9), name: Some("Ann".into()) });
        assert_eq!(items[0].key, "created#0");
        assert_eq!(items[1].text, "changed the title");
    }

    #[test]
    fn missing_creator_and_local_only_issue_still_get_entry() {
        let conn = mem();
        issue(&conn, "i2", None, None);
        let items = activity_for_issue(&conn, "i2").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].actor, ActorSlot { remote_id: None, name: None });
    }

    #[test]
    fn unknown_issue_has_no_entry() {
        assert!(activity_for_issue(&mem(), "nope").unwrap().is_empty());
    }
}

#[cfg(test)]
mod jrst139_tests {
    use super::*;
    use crate::db::migrations;

    fn conn_with(body: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t1','T','T',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at) VALUES ('i1','i',4,'l1','t1',?1,?1)", params![now]).unwrap();
        let entries: Vec<ActivityEntry> = serde_json::from_str(body).unwrap();
        store_batch(&conn, "i1", &entries, None, false).unwrap();
        conn
    }

    #[test]
    fn reference_reads_mentioned_in_with_title_and_user() {
        let conn = conn_with(r#"[{"type":"reference","id":1,"cursor":"reference:1","created_at":"2026-07-08T13:35:39Z",
          "source_issue":{"id":2,"identifier":"MTK-2","title":"Add reset"},"source_type":"comment","user":{"id":2,"name":"ryan"}}]"#);
        let items = activity_for_issue(&conn, "i1").unwrap()[1..].to_vec();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "mentioned in MTK-2: Add reset");
        assert_eq!(items[0].key, "reference:1#0");
        assert_eq!(items[0].actor, ActorSlot { remote_id: Some(2), name: Some("ryan".into()) });
    }

    #[test]
    fn reference_with_missing_source_or_user_still_renders() {
        let conn = conn_with(r#"[{"type":"reference","id":1,"cursor":"reference:1","created_at":"2026-07-08T13:35:39Z",
          "source_issue":null,"source_type":"issue","user":null},
          {"type":"reference","id":2,"cursor":"reference:2","created_at":"2026-07-08T13:36:00Z",
          "source_issue":{"id":3,"identifier":"MTK-3","title":""},"source_type":"issue","user":null}]"#);
        let items = activity_for_issue(&conn, "i1").unwrap()[1..].to_vec();
        assert_eq!(items[0].text, "mentioned in another issue");
        assert_eq!(items[0].actor, ActorSlot { remote_id: None, name: None });
        assert_eq!(items[1].text, "mentioned in MTK-3");
    }
}

#[cfg(test)]
mod jrst137_tests {
    use super::*;
    use crate::db::migrations;

    fn lines(direction: &str, event: &str, other: &str) -> Vec<String> {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t1','T','T',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at) VALUES ('i1','i',4,'l1','t1',?1,?1)", params![now]).unwrap();
        let body = format!(
            r#"[{{"type":"dependency","id":1,"cursor":"dependency:1","created_at":"2026-07-06T00:00:00Z",
            "event":"{event}","kind":"blocks","direction":"{direction}",{other},
            "actor":{{"id":2,"name":"ryan"}},"description":"added dependency: <strong>MTK-9</strong> blocking <strong>MTK-3</strong>"}}]"#
        );
        let entries: Vec<ActivityEntry> = serde_json::from_str(&body).unwrap();
        store_batch(&conn, "i1", &entries, None, false).unwrap();
        activity_for_issue(&conn, "i1").unwrap().into_iter().skip(1).map(|i| i.text).collect()
    }

    const OTHER: &str = r#""other_issue":{"id":2,"identifier":"MTK-2","title":"X"}"#;

    #[test]
    fn blocking_and_blocked_by_word_from_this_issues_side() {
        assert_eq!(lines("blocking", "create", OTHER), ["now blocks MTK-2"]);
        assert_eq!(lines("blocking", "destroy", OTHER), ["no longer blocks MTK-2"]);
        assert_eq!(lines("blocked_by", "create", OTHER), ["now blocked by MTK-2"]);
        assert_eq!(lines("blocked_by", "destroy", OTHER), ["no longer blocked by MTK-2"]);
    }

    #[test]
    fn unsupported_direction_or_event_yields_no_line() {
        assert!(lines("sideways", "create", OTHER).is_empty());
        assert!(lines("blocking", "weird", OTHER).is_empty());
        assert!(lines("relates", "weird", OTHER).is_empty());
    }

    #[test]
    fn missing_other_issue_uses_generic_wording() {
        assert_eq!(lines("blocking", "create", r#""other_issue":null"#), ["changed blocking issues"]);
    }
}

#[cfg(test)]
mod jrst138_tests {
    use super::*;
    use crate::db::migrations;

    fn lines(direction: &str, event: &str, other: &str) -> Vec<String> {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t1','T','T',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at) VALUES ('i1','i',4,'l1','t1',?1,?1)", params![now]).unwrap();
        let body = format!(
            r#"[{{"type":"dependency","id":1,"cursor":"dependency:1","created_at":"2026-07-06T00:00:00Z",
            "event":"{event}","kind":"relates","direction":"{direction}",{other},
            "actor":{{"id":2,"name":"ryan"}},"description":"x"}}]"#
        );
        let entries: Vec<ActivityEntry> = serde_json::from_str(&body).unwrap();
        store_batch(&conn, "i1", &entries, None, false).unwrap();
        activity_for_issue(&conn, "i1").unwrap().into_iter().skip(1).map(|i| i.text).collect()
    }

    const OTHER: &str = r#""other_issue":{"id":2,"identifier":"MTK-2","title":"X"}"#;

    #[test]
    fn all_six_direction_event_combinations() {
        assert_eq!(lines("relates", "create", OTHER), ["now related to MTK-2"]);
        assert_eq!(lines("relates", "destroy", OTHER), ["no longer related to MTK-2"]);
        assert_eq!(lines("duplicates", "create", OTHER), ["now a duplicate of MTK-2"]);
        assert_eq!(lines("duplicates", "destroy", OTHER), ["no longer a duplicate of MTK-2"]);
        assert_eq!(lines("duplicated_by", "create", OTHER), ["now duplicated by MTK-2"]);
        assert_eq!(lines("duplicated_by", "destroy", OTHER), ["no longer duplicated by MTK-2"]);
    }

    #[test]
    fn missing_other_issue_uses_generic_wording() {
        assert_eq!(lines("relates", "create", r#""other_issue":null"#), ["changed related issues"]);
        assert_eq!(lines("duplicated_by", "destroy", r#""other_issue":null"#), ["changed duplicate issues"]);
    }
}

#[cfg(test)]
mod jrst_134_tests {
    use super::*;
    use crate::db::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at) VALUES ('t1','Core','COR',4,?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, remote_id, created_at, updated_at) VALUES ('l1','Doing',0,'t1',10,?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, team_number, remote_id, created_at, updated_at) VALUES ('i1','i',4,'l1','t1',7,70,?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO members (id, team_id, name, email, remote_id, created_at, updated_at) VALUES ('m1','t1','Ann','a@x',5,?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO projects (id, team_id, name, remote_id, created_at, updated_at) VALUES ('p1','t1','Renamed',20,?1,?1)", params![now]).unwrap();
        conn
    }

    fn lines(conn: &Connection, changes: &str) -> Vec<String> {
        let body = format!(
            r#"[{{"type":"version","id":1,"cursor":"version:1","created_at":"2026-07-06T00:00:00Z","actor":null,"event":"update","description":"","changes":{changes}}}]"#
        );
        let entries: Vec<ActivityEntry> = serde_json::from_str(&body).unwrap();
        store_batch(conn, "i1", &entries, None, false).unwrap();
        activity_for_issue(conn, "i1").unwrap().into_iter().skip(1).map(|i| i.text).collect()
    }

    #[test]
    fn lane_resolves_current_local_name_and_falls_back() {
        let conn = mem();
        let t = lines(&conn, r#"[{"field":"lane_id","from":{"id":10,"name":"Old Name"},"to":{"id":99,"name":"Sent"}}]"#);
        assert_eq!(t, ["changed status from Doing to Sent"]);
    }

    #[test]
    fn assignee_and_null_sides() {
        let conn = mem();
        let t = lines(&conn, r#"[{"field":"assignee_id","from":null,"to":{"id":5,"name":"Anne"}},
            {"field":"assignee_id","from":{"id":5,"name":"x"},"to":null},
            {"field":"assignee_id","from":{"id":5,"name":"x"},"to":{"id":6,"name":"Bob"}}]"#);
        assert_eq!(t, ["assigned to Ann", "unassigned Ann", "reassigned from Ann to Bob"]);
    }

    #[test]
    fn project_renamed_added_and_removed() {
        let conn = mem();
        let t = lines(&conn, r#"[{"field":"project_id","from":null,"to":{"id":20,"name":"Old"}},
            {"field":"project_id","from":{"id":20,"name":"Old"},"to":null},
            {"field":"project_id","from":{"id":88,"name":"Gone"},"to":null}]"#);
        assert_eq!(t, ["moved to project Renamed", "removed from project Renamed", "removed from project Gone"]);
    }

    #[test]
    fn parent_uses_identifier() {
        let conn = mem();
        let t = lines(&conn, r#"[{"field":"parent_issue_id","from":null,"to":{"id":70,"identifier":"OLD-1"}},
            {"field":"parent_issue_id","from":{"id":70,"identifier":"OLD-1"},"to":{"id":71,"identifier":"ZZZ-3"}},
            {"field":"parent_issue_id","from":{"id":71,"identifier":"ZZZ-3"},"to":null}]"#);
        assert_eq!(t, ["set parent to COR-7", "changed parent from COR-7 to ZZZ-3", "removed parent ZZZ-3"]);
    }

    #[test]
    fn team_resolves_and_falls_back() {
        let conn = mem();
        let t = lines(&conn, r#"[{"field":"team_id","from":{"id":4,"name":"Old"},"to":{"id":9,"name":"Other"}}]"#);
        assert_eq!(t, ["moved from team Core to team Other"]);
    }
}

#[cfg(test)]
mod jrst141_tests {
    use super::*;
    use crate::db::{migrations, queries};

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t1','T','T',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at) VALUES ('i1','i',4,'l1','t1',?1,?1)", params![now]).unwrap();
        conn
    }

    fn edit_priority(conn: &Connection) -> i64 {
        let mut issue = queries::get_issue(conn, "i1").unwrap().unwrap();
        issue.priority = 1;
        queries::update_issue_queued(conn, &issue).unwrap()
    }

    #[test]
    fn edit_links_pending_change_to_its_queue_entry() {
        let conn = mem();
        let qid = edit_priority(&conn);
        let linked: i64 = conn
            .query_row("SELECT sync_queue_id FROM issue_events WHERE issue_id='i1' AND kind='priority'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(linked, qid);
    }

    #[test]
    fn pending_item_shows_as_you_with_server_wording() {
        let conn = mem();
        edit_priority(&conn);
        let items = activity_for_issue(&conn, "i1").unwrap()[1..].to_vec();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].origin, ItemOrigin::Pending);
        assert_eq!(items[0].actor.name.as_deref(), Some("You"));
        assert_eq!(items[0].text, "changed priority from None to High");
    }

    #[test]
    fn unqueued_edits_are_not_pending() {
        let conn = mem();
        let mut issue = queries::get_issue(&conn, "i1").unwrap().unwrap();
        issue.priority = 1;
        queries::update_issue(&conn, &issue).unwrap();
        assert_eq!(activity_for_issue(&conn, "i1").unwrap().len(), 1); // only the derived created item
    }

    #[test]
    fn pending_merges_by_time_after_server_entries() {
        let conn = mem();
        let body = r#"[{"type":"version","id":1,"cursor":"version:1","created_at":"2026-01-02T00:00:00Z","actor":null,"event":"update","description":"",
          "changes":[{"field":"title","from":"a","to":"b"}]}]"#;
        let entries: Vec<ActivityEntry> = serde_json::from_str(body).unwrap();
        store_batch(&conn, "i1", &entries, None, false).unwrap();
        edit_priority(&conn);
        let items = activity_for_issue(&conn, "i1").unwrap()[1..].to_vec();
        assert_eq!(items[0].origin, ItemOrigin::Server);
        assert_eq!(items[1].origin, ItemOrigin::Pending);
    }

    #[test]
    fn pending_change_in_the_same_second_sorts_after_the_server_entry() {
        let conn = mem();
        // The server stores whole seconds; local times carry milliseconds.
        let body = r#"[{"type":"version","id":1,"cursor":"version:1","created_at":"2026-01-02T00:00:47Z","actor":null,"event":"update","description":"",
          "changes":[{"field":"title","from":"a","to":"b"}]}]"#;
        let entries: Vec<ActivityEntry> = serde_json::from_str(body).unwrap();
        store_batch(&conn, "i1", &entries, None, false).unwrap();
        conn.execute("INSERT INTO sync_queue (entity_type, entity_id, action, created_at) VALUES ('issue','i1','update','x')", []).unwrap();
        conn.execute(
            "INSERT INTO issue_events (issue_id, source, kind, old_value, new_value, created_at, sync_queue_id)
             VALUES ('i1','local','priority','4','1','2026-01-02T00:00:47.123Z', last_insert_rowid())",
            [],
        )
        .unwrap();
        let items = activity_for_issue(&conn, "i1").unwrap()[1..].to_vec();
        assert_eq!(items[0].origin, ItemOrigin::Server);
        assert_eq!(items[0].at, "2026-01-02T00:00:47.000Z");
        assert_eq!(items[1].origin, ItemOrigin::Pending);
    }

    #[test]
    fn local_only_issue_reads_as_derived_created_plus_pending() {
        let conn = mem();
        let mut issue = queries::get_issue(&conn, "i1").unwrap().unwrap();
        issue.id = "i2".into();
        issue.remote_id = None;
        issue.team_number = Some(2);
        queries::insert_issue(&conn, &issue).unwrap();
        let mut issue = queries::get_issue(&conn, "i2").unwrap().unwrap();
        issue.priority = 1;
        queries::update_issue_queued(&conn, &issue).unwrap();
        let got: Vec<_> = activity_for_issue(&conn, "i2").unwrap().into_iter().map(|i| (i.text, i.origin)).collect();
        assert_eq!(got, [
            ("created the issue".to_string(), ItemOrigin::Server),
            ("changed priority from None to High".to_string(), ItemOrigin::Pending),
        ]);
    }
}

#[cfg(test)]
mod jrst143_tests {
    use super::*;
    use crate::db::{migrations, queries, sync_queue};

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t1','T','T',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at) VALUES ('i1','i',4,'l1','t1',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at) VALUES ('i2','i',4,'l1','t1',?1,?1)", params![now]).unwrap();
        conn
    }

    fn edit(conn: &Connection, id: &str) -> i64 {
        let mut issue = queries::get_issue(conn, id).unwrap().unwrap();
        issue.priority = 1;
        queries::update_issue_queued(conn, &issue).unwrap()
    }

    fn pending(conn: &Connection, id: &str) -> usize {
        activity_for_issue(conn, id).unwrap().iter().filter(|i| i.origin == ItemOrigin::Pending).count()
    }

    fn synced_at(conn: &Connection, qid: i64, at: &str) {
        conn.execute("UPDATE sync_queue SET synced_at = ?1 WHERE id = ?2", params![at, qid]).unwrap();
    }

    #[test]
    fn synced_then_fetched_after_is_pruned() {
        let conn = mem();
        let q = edit(&conn, "i1");
        synced_at(&conn, q, "2026-02-01T00:00:00.000Z");
        store_batch_at(&conn, "i1", &[], None, false, "2026-02-01T00:00:01.000Z").unwrap();
        assert_eq!(pending(&conn, "i1"), 0);
    }

    #[test]
    fn synced_but_fetch_started_earlier_is_kept() {
        let conn = mem();
        let q = edit(&conn, "i1");
        synced_at(&conn, q, "2026-02-01T00:00:05.000Z");
        store_batch_at(&conn, "i1", &[], None, false, "2026-02-01T00:00:01.000Z").unwrap();
        assert_eq!(pending(&conn, "i1"), 1);
        // A later fetch settles it.
        store_batch_at(&conn, "i1", &[], None, false, "2026-02-01T00:00:09.000Z").unwrap();
        assert_eq!(pending(&conn, "i1"), 0);
    }

    #[test]
    fn unsynced_change_is_kept_whatever_the_fetch_time() {
        let conn = mem();
        edit(&conn, "i1");
        store_batch_at(&conn, "i1", &[], None, false, "2999-01-01T00:00:00.000Z").unwrap();
        assert_eq!(pending(&conn, "i1"), 1);
    }

    #[test]
    fn only_the_fetched_issue_is_pruned() {
        let conn = mem();
        let q1 = edit(&conn, "i1");
        let q2 = edit(&conn, "i2");
        synced_at(&conn, q1, "2026-02-01T00:00:00.000Z");
        synced_at(&conn, q2, "2026-02-01T00:00:00.000Z");
        store_batch_at(&conn, "i1", &[], None, false, "2026-03-01T00:00:00.000Z").unwrap();
        assert_eq!(pending(&conn, "i1"), 0);
        assert_eq!(pending(&conn, "i2"), 1);
    }

    #[test]
    fn mark_synced_then_plain_store_batch_prunes() {
        let conn = mem();
        let q = edit(&conn, "i1");
        sync_queue::mark_synced(&conn, q).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        store_batch(&conn, "i1", &[], None, false).unwrap();
        assert_eq!(pending(&conn, "i1"), 0);
    }

    #[test]
    fn unqueued_edits_are_untouched() {
        let conn = mem();
        let mut issue = queries::get_issue(&conn, "i1").unwrap().unwrap();
        issue.priority = 1;
        queries::update_issue(&conn, &issue).unwrap();
        store_batch_at(&conn, "i1", &[], None, false, "2999-01-01T00:00:00.000Z").unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM issue_events WHERE issue_id='i1' AND sync_queue_id IS NULL", [], |r| r.get(0)).unwrap();
        assert!(n >= 1);
    }
}


#[cfg(test)]
mod jrst142_tests {
    use super::*;
    use crate::db::models::IssueDependency;
    use crate::db::{migrations, queries};

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t1','T','T',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)", params![now]).unwrap();
        for id in ["a", "b"] {
            conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at) VALUES (?1,?1,4,'l1','t1',?2,?2)", params![id, now]).unwrap();
        }
        conn
    }

    fn dep(remote_id: Option<i64>) -> IssueDependency {
        IssueDependency {
            id: "d1".into(),
            remote_id,
            blocking_issue_id: "a".into(),
            blocked_issue_id: "b".into(),
            kind: "blocks".into(),
            direction: "blocking".into(),
            created_at: "2026-01-02T00:00:00.000Z".into(),
            updated_at: "2026-01-02T00:00:00.000Z".into(),
        }
    }

    fn pending(conn: &Connection, id: &str) -> usize {
        activity_for_issue(conn, id).unwrap().iter().filter(|i| i.origin == ItemOrigin::Pending).count()
    }

    #[test]
    fn add_is_pending_on_both_issues() {
        let conn = mem();
        let q = queries::insert_issue_dependency_queued(&conn, &dep(None)).unwrap();
        assert_eq!((pending(&conn, "a"), pending(&conn, "b")), (1, 1));
        let linked: i64 = conn
            .query_row("SELECT COUNT(*) FROM issue_events WHERE sync_queue_id = ?1", [q], |r| r.get(0))
            .unwrap();
        assert_eq!(linked, 2);
    }

    #[test]
    fn remove_of_synced_edge_is_pending_on_both_issues() {
        let conn = mem();
        queries::insert_issue_dependency(&conn, &dep(Some(9))).unwrap();
        conn.execute("UPDATE teams SET remote_id = 1", []).unwrap();
        conn.execute("UPDATE issues SET remote_id = 5 WHERE id = 'a'", []).unwrap();
        let q = queries::delete_issue_dependency_queued(&conn, "d1").unwrap();
        assert!(q.is_some());
        assert_eq!((pending(&conn, "a"), pending(&conn, "b")), (1, 1));
    }

    #[test]
    fn unqueued_remove_is_not_pending() {
        let conn = mem();
        queries::insert_issue_dependency(&conn, &dep(None)).unwrap();
        assert_eq!(queries::delete_issue_dependency_queued(&conn, "d1").unwrap(), None);
        assert_eq!((pending(&conn, "a"), pending(&conn, "b")), (0, 0));
    }

    #[test]
    fn failed_add_leaves_nothing_linked() {
        let conn = mem();
        queries::insert_issue_dependency_queued(&conn, &dep(None)).unwrap();
        assert!(queries::insert_issue_dependency_queued(&conn, &dep(None)).is_err());
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM sync_queue", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}

#[cfg(test)]
mod jrst135_tests {
    use super::*;
    use crate::db::migrations;

    fn texts(description: &str, changes: &str) -> Vec<String> {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t1','T','T',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at) VALUES ('i1','i',4,'l1','t1',?1,?1)", params![now]).unwrap();
        let body = serde_json::json!([{"type":"version","id":1,"cursor":"version:1","created_at":"2026-07-06T00:00:00Z",
            "actor":null,"event":"update","description":description,
            "changes":serde_json::from_str::<Value>(changes).unwrap()}]);
        let entries: Vec<ActivityEntry> = serde_json::from_value(body).unwrap();
        store_batch(&conn, "i1", &entries, None, false).unwrap();
        activity_for_issue(&conn, "i1").unwrap()[1..].iter().map(|i| i.text.clone()).collect()
    }

    #[test]
    fn only_unknown_field_shows_stripped_description() {
        let t = texts("<p>Ryan set <b>story points</b> &amp; more</p>", r#"[{"field":"story_points","from":1,"to":2}]"#);
        assert_eq!(t, ["Ryan set story points & more"]);
    }

    #[test]
    fn mixed_known_and_unknown_surfaces_both() {
        let t = texts("<p>changed sprint</p>", r#"[{"field":"title","from":"a","to":"b"},{"field":"sprint","from":null,"to":"S1"}]"#);
        assert_eq!(t, ["changed the title", "changed sprint"]);
    }

    #[test]
    fn several_unknown_changes_emit_one_fallback() {
        let t = texts("<p>did things</p>", r#"[{"field":"a","from":1,"to":2},{"field":"b","from":1,"to":2}]"#);
        assert_eq!(t, ["did things"]);
    }

    #[test]
    fn all_known_has_no_fallback_and_empty_description_adds_nothing() {
        assert_eq!(texts("<p>x</p>", r#"[{"field":"title","from":"a","to":"b"}]"#), ["changed the title"]);
        assert!(texts("<p> </p>", r#"[{"field":"zzz","from":1,"to":2}]"#).is_empty());
    }
}

#[cfg(test)]
mod jrst144_tests {
    use super::*;
    use crate::db::{migrations, queries, sync_queue};

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t1','T','T',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at) VALUES ('i1','i',4,'l1','t1',?1,?1)", params![now]).unwrap();
        conn
    }

    fn edit(conn: &Connection, priority: i32) -> i64 {
        let mut issue = queries::get_issue(conn, "i1").unwrap().unwrap();
        issue.priority = priority;
        queries::update_issue_queued(conn, &issue).unwrap()
    }

    fn pending_count(conn: &Connection) -> usize {
        activity_for_issue(conn, "i1").unwrap().iter().filter(|i| i.origin == ItemOrigin::Pending).count()
    }

    #[test]
    fn dropped_entry_removes_only_its_pending_change() {
        let conn = mem();
        let a = edit(&conn, 1);
        let _b = edit(&conn, 2);
        assert_eq!(pending_count(&conn), 2);
        sync_queue::discard(&conn, a).unwrap();
        assert_eq!(pending_count(&conn), 1);
        let synced: Option<String> = conn.query_row("SELECT synced_at FROM sync_queue WHERE id=?1", params![a], |r| r.get(0)).unwrap();
        assert!(synced.is_some());
    }

    #[test]
    fn successful_sync_keeps_its_pending_change() {
        let conn = mem();
        let a = edit(&conn, 1);
        sync_queue::mark_synced(&conn, a).unwrap();
        assert_eq!(pending_count(&conn), 1);
    }

    #[test]
    fn retiring_an_entity_discards_its_unsynced_pending_changes() {
        let conn = mem();
        edit(&conn, 1);
        discard_pending_for_entity(&conn, "issue", "i1").unwrap();
        assert_eq!(pending_count(&conn), 0);
    }
}

#[cfg(test)]
mod jrst151_tests {
    use super::*;
    use crate::db::models::Comment;

    fn item(k: &str, at: &str) -> ActivityItem {
        ActivityItem {
            key: k.into(),
            at: at.into(),
            actor: ActorSlot { remote_id: None, name: None },
            text: k.into(),
            origin: ItemOrigin::Server,
        }
    }

    fn comment(id: &str, parent: Option<&str>, at: &str) -> Comment {
        Comment {
            id: id.into(),
            remote_id: None,
            issue_id: Some("i1".into()),
            project_id: None,
            author_id: None,
            body: id.into(),
            parent_id: parent.map(Into::into),
            created_at: at.into(),
            updated_at: at.into(),
        }
    }

    #[test]
    fn timeline_interleaves_threads_by_time_and_keeps_created_first() {
        let items = [item("created", "2026-01-01"), item("a", "2026-01-03"), item("b", "2026-01-05")];
        let comments = [
            comment("c2", None, "2026-01-05"),
            comment("c1", None, "2026-01-02"),
            comment("r1", Some("c1"), "2026-01-06"),
        ];
        let order: Vec<String> = timeline(&items, &comments)
            .iter()
            .map(|t| match t {
                TimelineItem::Activity(i) => i.key.clone(),
                TimelineItem::Thread(t) => {
                    t.iter().map(|(_, c)| c.id.as_str()).collect::<Vec<_>>().join("+")
                }
            })
            .collect();
        assert_eq!(order, ["created", "c1+r1", "a", "b", "c2"]);
        assert!(timeline(&[], &[]).is_empty());
    }
}
