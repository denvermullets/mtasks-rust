//! Local cache of server Activity entries for an issue (`issue_activity_entries`).
//! Pure over a `rusqlite::Connection`: no Iced, no HTTP.
//!
//! Entries are keyed by `(issue_id, entry_cursor)` where the cursor is the full
//! `<type>:<id>` string: numeric ids collide across entry types.
// Unused until the fetch routine and display layer land; drop then.
#![allow(dead_code)]

use rusqlite::{params, Connection, Result};
use serde_json::{json, Value};

use super::events::{self, EventKind};
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
pub fn store_batch(
    conn: &Connection,
    issue_id: &str,
    entries: &[ActivityEntry],
    cursor: Option<&str>,
    fully_fetched: bool,
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
    tx.commit()
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
        created_at: created_at.to_string(),
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
    for entry in entries_for_issue(conn, issue_id)? {
        let actor = ActorSlot {
            remote_id: entry.actor_remote_id,
            name: entry.actor_name.clone(),
        };
        let texts = match entry.entry_type.as_str() {
            "version" => version_lines(&entry),
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

/// Add the issue's Pending changes (local edits linked to a sync-queue entry),
/// attributed to "You", each after the last item not later than itself. A
/// local-only issue (no remote id) also gets its created line, since the
/// server has no entry for it.
fn merge_pending(conn: &Connection, issue_id: &str, items: &mut Vec<ActivityItem>) -> Result<()> {
    let local_only: bool = conn
        .query_row(
            "SELECT remote_id IS NULL FROM issues WHERE id = ?1",
            params![issue_id],
            |r| r.get(0),
        )
        .unwrap_or(false);
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
        let created = kind == EventKind::Created;
        if !(linked || (created && local_only)) {
            continue;
        }
        let item = ActivityItem {
            key: format!("pending:{id}#0"),
            text: events::describe(kind, old.as_deref(), new.as_deref()),
            at,
            actor: ActorSlot { remote_id: None, name: Some("You".to_string()) },
            origin: ItemOrigin::Pending,
        };
        let pos = items.iter().rposition(|i| i.at <= item.at).map_or(0, |p| p + 1);
        items.insert(pos, item);
    }
    Ok(())
}

/// One line per change in a version entry. Changes this layer can't word yet
/// produce no line.
fn version_lines(entry: &StoredEntry) -> Vec<String> {
    let Some(changes) = entry.payload["changes"].as_array() else {
        return Vec::new();
    };
    changes.iter().filter_map(plain_change_line).collect()
}

/// Wording for a plain (non-reference) field, reusing `events::describe`.
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
    Some(events::describe(
        kind,
        value(&change["from"]).as_deref(),
        value(&change["to"]).as_deref(),
    ))
}

/// Server priority name (`"no_priority"`, `"high"`, ...) as the desktop label.
fn priority_name_label(name: &str) -> String {
    match Priority::from_rails_str(name) {
        Ok(p) => events::priority_label(p.to_i32()),
        Err(_) => name.to_string(),
    }
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
        assert_eq!(got[0].created_at, "2026-07-06T00:40:47Z");
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
        let items = activity_for_issue(&conn, "i1").unwrap();
        let texts: Vec<_> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, [
            "changed priority from Low to High",
            "changed the title",
            "updated the description",
            "changed estimate from none to 3",
            "changed due date from 2026-01-01 to none",
            "changed priority from High to None",
        ]);
        assert_eq!(items[0].at, "2026-07-06T00:40:47Z");
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
        let items = activity_for_issue(&conn, "i1").unwrap();
        assert_eq!(items[0].text, "changed priority from Low to Urgent");
        assert_eq!(items[1].text, "changed the title");
        assert_eq!(items[0].actor, ActorSlot { remote_id: None, name: None });
    }

    #[test]
    fn empty_changes_yield_no_lines() {
        let conn = mem();
        store(&conn, r#"[{"type":"version","id":1,"cursor":"version:1","created_at":"2026-07-06T00:00:00Z","actor":null,"event":"update","description":"","changes":[]}]"#);
        assert!(activity_for_issue(&conn, "i1").unwrap().is_empty());
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
        let items = activity_for_issue(&conn, "i1").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].origin, ItemOrigin::Pending);
        assert_eq!(items[0].actor.name.as_deref(), Some("You"));
        assert_eq!(items[0].text, "changed priority from None to High");
    }

    #[test]
    fn unqueued_local_events_are_not_pending() {
        let conn = mem();
        let mut issue = queries::get_issue(&conn, "i1").unwrap().unwrap();
        issue.priority = 1;
        queries::update_issue(&conn, &issue).unwrap();
        assert!(activity_for_issue(&conn, "i1").unwrap().is_empty());
    }

    #[test]
    fn pending_merges_by_time_after_server_entries() {
        let conn = mem();
        let body = r#"[{"type":"version","id":1,"cursor":"version:1","created_at":"2026-01-02T00:00:00Z","actor":null,"event":"update","description":"",
          "changes":[{"field":"title","from":"a","to":"b"}]}]"#;
        let entries: Vec<ActivityEntry> = serde_json::from_str(body).unwrap();
        store_batch(&conn, "i1", &entries, None, false).unwrap();
        edit_priority(&conn);
        let items = activity_for_issue(&conn, "i1").unwrap();
        assert_eq!(items[0].origin, ItemOrigin::Server);
        assert_eq!(items[1].origin, ItemOrigin::Pending);
    }

    #[test]
    fn local_only_issue_reads_as_created_plus_pending() {
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
            ("created the issue".to_string(), ItemOrigin::Pending),
            ("changed priority from None to High".to_string(), ItemOrigin::Pending),
        ]);
    }
}
