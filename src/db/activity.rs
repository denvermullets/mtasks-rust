//! Local cache of server Activity entries for an issue (`issue_activity_entries`).
//! Pure over a `rusqlite::Connection`: no Iced, no HTTP.
//!
//! Entries are keyed by `(issue_id, entry_cursor)` where the cursor is the full
//! `<type>:<id>` string: numeric ids collide across entry types.
// Unused until the fetch routine and display layer land; drop then.
#![allow(dead_code)]

use rusqlite::{params, Connection, Result};
use serde_json::{json, Value};

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

/// Store a fetched batch for `issue_id`, upserting by entry cursor so entries
/// re-delivered at a cursor overlap replace themselves rather than duplicate.
pub fn store_batch(conn: &Connection, issue_id: &str, entries: &[ActivityEntry]) -> Result<()> {
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
    tx.commit()
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
        store_batch(&conn, "i1", &feed(BATCH)).unwrap();
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
        store_batch(&conn, "i1", &all).unwrap();
        store_batch(&conn, "i1", &all[1..]).unwrap();
        store_batch(&conn, "i1", &all).unwrap();
        assert_eq!(entries_for_issue(&conn, "i1").unwrap().len(), 3);
    }

    #[test]
    fn same_numeric_id_different_types_kept_separately() {
        let conn = mem();
        store_batch(&conn, "i1", &feed(BATCH)).unwrap();
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
        store_batch(&conn, "i1", &feed(BATCH)).unwrap();
        store_batch(&conn, "i2", &feed(BATCH)).unwrap();
        assert_eq!(entries_for_issue(&conn, "i1").unwrap().len(), 3);
        assert_eq!(entries_for_issue(&conn, "i2").unwrap().len(), 3);
    }

    #[test]
    fn deleting_the_issue_removes_its_entries() {
        let conn = mem();
        store_batch(&conn, "i1", &feed(BATCH)).unwrap();
        conn.execute("DELETE FROM issues WHERE id = 'i1'", []).unwrap();
        assert!(entries_for_issue(&conn, "i1").unwrap().is_empty());
    }
}
