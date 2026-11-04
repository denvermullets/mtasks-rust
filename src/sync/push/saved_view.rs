//! Pushing Saved view creates, updates and deletes.

use rusqlite::Connection;

use crate::api::{ApiClient, ApiError};
use crate::db::{queries, sync_queue};
use crate::sync::engine::open;
use super::outcome::{Outcome, Prep, classify_in_team, done_if_gone};

/// Resolve `local:<id>` placeholders in a saved view query's `label_ids` and
/// `project_ids` to remote ids. The record's own create is earlier in the FIFO
/// queue, so it has normally pushed; one that still has no remote id (or is
/// gone) is left out, and its description is returned for logging.
fn resolve_query_placeholders(
    conn: &Connection,
    query: serde_json::Value,
) -> (serde_json::Value, Vec<String>) {
    let mut query = query;
    let mut dropped = Vec::new();
    let Some(map) = query.as_object_mut() else {
        return (query, dropped);
    };
    for (key, table) in [("label_ids", "labels"), ("project_ids", "projects")] {
        let Some(serde_json::Value::String(text)) = map.get(key) else {
            continue;
        };
        if !text.contains(crate::saved_view::LOCAL_PLACEHOLDER) {
            continue;
        }
        let mut kept = Vec::new();
        for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let Some(local) = crate::saved_view::placeholder_local_id(part) else {
                kept.push(part.to_string());
                continue;
            };
            match queries::remote_id_for(conn, table, local) {
                Ok(Some(rid)) => kept.push(rid.to_string()),
                _ => dropped.push(format!("{key} {part}")),
            }
        }
        map.insert(key.to_string(), serde_json::Value::String(kept.join(",")));
    }
    (query, dropped)
}

fn log_dropped_placeholders(entity_id: &str, dropped: &[String]) {
    for d in dropped {
        eprintln!("saved_view {entity_id}: left unresolved placeholder out of push ({d})");
    }
}

struct SavedViewCreatePrep {
    team_remote_id: i64,
    local_id: String,
    name: String,
    query: serde_json::Value,
}

fn saved_view_create_prep(conn: &Connection, entity_id: &str) -> Prep<SavedViewCreatePrep> {
    use rusqlite::OptionalExtension;
    let view = match conn
        .query_row(
            "SELECT id, remote_id, team_id, name, query FROM saved_views WHERE id = ?1",
            [entity_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )
        .optional()
    {
        Ok(Some(v)) => v,
        // Deleted before it was ever pushed.
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "saved view {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let (local_id, remote_id, team_id, name, query) = view;
    if remote_id.is_some() {
        return Prep::Done(Outcome::Success);
    }
    let team_remote_id = match queries::remote_id_for(conn, "teams", &team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let query = serde_json::from_str(&query).unwrap_or_else(|_| serde_json::json!({}));
    let (query, dropped) = resolve_query_placeholders(conn, query);
    log_dropped_placeholders(&local_id, &dropped);
    Prep::Ready(SavedViewCreatePrep {
        team_remote_id,
        local_id,
        name,
        query,
    })
}

/// Adopt the server's row and retire the queue entry together.
pub(super) fn commit_saved_view_create(
    conn: &mut Connection,
    entry_id: i64,
    local_id: &str,
    dto: &crate::api::saved_views::SavedViewDto,
) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| format!("begin: {e}"))?;
    crate::db::saved_views::adopt_created(&tx, local_id, dto)
        .map_err(|e| format!("adopt saved view: {e}"))?;
    sync_queue::mark_synced(&tx, entry_id).map_err(|e| format!("mark_synced: {e}"))?;
    tx.commit().map_err(|e| format!("commit: {e}"))
}

/// `POST /teams/:team_id/saved_views`. Rails has no idempotency or lookup for
/// these, so a re-drain after a lost response creates a second server row; the
/// pull then mirrors it as a separate view the user can delete.
pub(super) async fn saved_view_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match open() {
        Ok(conn) => match saved_view_create_prep(&conn, entity_id) {
            Prep::Ready(p) => p,
            Prep::Done(o) => return o,
        },
        Err(e) => return Outcome::LeavePending(e),
    };
    match crate::api::saved_views::create(client, prep.team_remote_id, &prep.name, &prep.query)
        .await
    {
        Ok(dto) => {
            let mut conn = match open() {
                Ok(c) => c,
                Err(e) => return Outcome::LeavePending(e),
            };
            match commit_saved_view_create(&mut conn, entry_id, &prep.local_id, &dto) {
                Ok(()) => Outcome::Created,
                Err(e) => Outcome::LeavePending(e),
            }
        }
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

/// What a `saved_view` update pushes: the server id, the stored query text (to
/// guard adoption) and the query as a JSON object.
struct SavedViewUpdatePrep {
    remote_id: i64,
    stored: String,
    change: SavedViewChange,
}

/// What an update pushes: the query as a JSON object, or (for a `rename`
/// entry) the stored name alone.
enum SavedViewChange {
    Query(serde_json::Value),
    Name(String),
}

/// Older builds queued a rename as an `update` with this payload; such entries
/// still in the queue are pushed as renames.
const LEGACY_RENAME_PAYLOAD: &str = r#"{"only":"name"}"#;

pub(super) fn is_legacy_rename_payload(payload: Option<&str>) -> bool {
    payload == Some(LEGACY_RENAME_PAYLOAD)
}

/// Re-reads the row so the newest definition goes out, however many updates
/// were queued. Unknown keys and unresolvable ids ride along untouched.
/// `rename` pushes the name alone instead of the query.
fn saved_view_update_prep(
    conn: &Connection,
    entity_id: &str,
    rename: bool,
) -> Prep<SavedViewUpdatePrep> {
    let view = match crate::db::saved_views::get(conn, entity_id) {
        Ok(Some(v)) => v,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "saved_view {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let Some(remote_id) = view.remote_id else {
        return Prep::Done(Outcome::LeavePending(
            "saved_view create not yet drained".into(),
        ));
    };
    let query = match serde_json::from_str::<serde_json::Value>(&view.query) {
        Ok(q) if q.is_object() => q,
        _ => serde_json::json!({}),
    };
    // A name-only push never sends the query, so placeholders only matter on
    // the query path.
    let change = if rename {
        SavedViewChange::Name(view.name)
    } else {
        let (query, dropped) = resolve_query_placeholders(conn, query);
        log_dropped_placeholders(entity_id, &dropped);
        SavedViewChange::Query(query)
    };
    Prep::Ready(SavedViewUpdatePrep {
        remote_id,
        stored: view.query,
        change,
    })
}

/// `PATCH /saved_views/:id`. On success the server's returned query replaces
/// the local one, unless the row changed again meanwhile. A 404 means the view
/// is gone on the server: done (the tombstone feed removes the row).
pub(super) async fn saved_view_update(client: &ApiClient, entity_id: &str, rename: bool) -> Outcome {
    let prep = match open() {
        Ok(conn) => match saved_view_update_prep(&conn, entity_id, rename) {
            Prep::Ready(p) => p,
            Prep::Done(o) => return o,
        },
        Err(e) => return Outcome::LeavePending(e),
    };
    match &prep.change {
        SavedViewChange::Name(name) => {
            let result = crate::api::saved_views::update_name(client, prep.remote_id, name).await;
            done_if_gone(result, |_| {})
        }
        SavedViewChange::Query(query) => {
            let result = crate::api::saved_views::update_query(client, prep.remote_id, query).await;
            done_if_gone(result, |dto| {
                if let Ok(conn) = open() {
                    let _ = crate::db::saved_views::adopt_server_query(
                        &conn,
                        entity_id,
                        &prep.stored,
                        &dto.query,
                    );
                }
            })
        }
    }
}

/// `DELETE /saved_views/:id`. A 404 means it is already gone: done.
pub(super) async fn saved_view_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let Some(remote_id) = saved_view_delete_id(payload) else {
        return Outcome::DropAndLog("saved_view delete: bad payload".into());
    };
    saved_view_delete_outcome(crate::api::saved_views::delete(client, remote_id).await)
}

pub(super) fn saved_view_delete_id(payload: Option<&str>) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_str(payload?).ok()?;
    v.get("remote_id")?.as_i64()
}

fn saved_view_delete_outcome(result: Result<(), ApiError>) -> Outcome {
    done_if_gone(result, |()| {})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::push::fixtures::{saved_view_conn, saved_view_dto};
    use crate::sync::test_support::insert_test_label;

    #[test]
    fn saved_view_create_prep_carries_team_name_and_query() {
        let (conn, id) = saved_view_conn();
        let Prep::Ready(p) = saved_view_create_prep(&conn, &id) else {
            panic!("expected Ready");
        };
        assert_eq!(p.team_remote_id, 700);
        assert_eq!(p.name, "Mine");
        assert_eq!(p.query, serde_json::json!({"view_mode": "list"}));
        assert_eq!(p.local_id, id);
    }

    #[test]
    fn saved_view_create_prep_waits_for_the_team_and_skips_done_or_missing_views() {
        let (conn, id) = saved_view_conn();
        conn.execute("UPDATE teams SET remote_id = NULL", []).unwrap();
        assert!(matches!(
            saved_view_create_prep(&conn, &id),
            Prep::Done(Outcome::LeavePending(_))
        ));
        assert!(matches!(
            saved_view_create_prep(&conn, "nope"),
            Prep::Done(Outcome::DropAndLog(_))
        ));
        conn.execute("UPDATE saved_views SET remote_id = 5", []).unwrap();
        assert!(matches!(
            saved_view_create_prep(&conn, &id),
            Prep::Done(Outcome::Success)
        ));
    }

    #[test]
    fn saved_view_create_push_adopts_the_server_row_and_marks_synced() {
        let (mut conn, id) = saved_view_conn();
        let entry = sync_queue::pending_entries(&conn).unwrap()[0].id.unwrap();
        commit_saved_view_create(&mut conn, entry, &id, &saved_view_dto(88)).unwrap();
        let got = crate::db::saved_views::find_by_remote_id(&conn, 88).unwrap().unwrap();
        assert_eq!((got.id, got.position), (id, 3));
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn saved_view_create_push_survives_a_pull_that_already_inserted_the_row() {
        let (mut conn, id) = saved_view_conn();
        let entry = sync_queue::pending_entries(&conn).unwrap()[0].id.unwrap();
        // The POST landed earlier and a pull mirrored it as a second local row.
        crate::db::saved_views::apply_pulled(&conn, &[saved_view_dto(88)], None).unwrap();
        commit_saved_view_create(&mut conn, entry, &id, &saved_view_dto(88)).unwrap();
        let all = crate::db::saved_views::list_for_team(&conn, "t1").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].id.as_str(), all[0].remote_id), (id.as_str(), Some(88)));
    }

    #[test]
    fn saved_view_delete_reads_the_remote_id_from_the_payload() {
        assert_eq!(saved_view_delete_id(Some(r#"{"remote_id":5}"#)), Some(5));
        for bad in [None, Some("nope"), Some("{}")] {
            assert_eq!(saved_view_delete_id(bad), None);
        }
    }

    #[test]
    fn saved_view_delete_treats_success_and_404_as_done() {
        assert!(matches!(saved_view_delete_outcome(Ok(())), Outcome::Success));
        assert!(matches!(
            saved_view_delete_outcome(Err(ApiError::NotFound("gone".into()))),
            Outcome::Success
        ));
        assert!(matches!(
            saved_view_delete_outcome(Err(ApiError::MissingToken)),
            Outcome::LeavePending(_)
        ));
    }

    fn saved_view_db(remote_id: Option<i64>, query: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES ('t1', 'T', 'T', 700, ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO saved_views (id, remote_id, team_id, name, query, position, updated_at)
             VALUES ('sv1', ?1, 't1', 'V', ?2, 0, ?3)",
            rusqlite::params![remote_id, query, now],
        )
        .unwrap();
        conn
    }

    #[test]
    fn saved_view_update_pushes_the_stored_query_as_an_object() {
        let q = r#"{"lane_ids":"1,999","future_key":"x","view_mode":"list"}"#;
        let conn = saved_view_db(Some(5), q);
        let Prep::Ready(p) = saved_view_update_prep(&conn, "sv1", false) else {
            panic!("expected ready");
        };
        assert_eq!(p.remote_id, 5);
        assert_eq!(
            update_query(&p),
            &serde_json::json!({"lane_ids":"1,999","future_key":"x","view_mode":"list"})
        );
        assert_eq!(p.stored, q);
    }

    #[test]
    fn saved_view_rename_entry_pushes_the_name_alone() {
        let conn = saved_view_db(Some(5), r#"{"view_mode":"list"}"#);
        let Prep::Ready(p) = saved_view_update_prep(&conn, "sv1", true) else {
            panic!("expected ready");
        };
        assert!(matches!(&p.change, SavedViewChange::Name(n) if n == "V"));
        let Prep::Ready(plain) = saved_view_update_prep(&conn, "sv1", false) else {
            panic!("expected ready");
        };
        assert!(matches!(plain.change, SavedViewChange::Query(_)));
    }

    #[test]
    fn queued_legacy_rename_updates_are_still_recognised() {
        assert!(is_legacy_rename_payload(Some(r#"{"only":"name"}"#)));
        assert!(!is_legacy_rename_payload(Some("{}")));
        assert!(!is_legacy_rename_payload(None));
    }

    fn update_query(p: &SavedViewUpdatePrep) -> &serde_json::Value {
        match &p.change {
            SavedViewChange::Query(q) => q,
            SavedViewChange::Name(_) => panic!("expected a query change"),
        }
    }

    #[test]
    fn saved_view_update_resolves_placeholders_to_remote_ids_and_drops_unresolvable() {
        let q = r#"{"label_ids":"30,local:lab1,local:lab2,local:ghost","project_ids":"none,local:p1,local:p2","x":"1"}"#;
        let conn = saved_view_db(Some(5), q);
        let now = "2026-01-01T00:00:00.000Z";
        for (id, rid) in [("lab1", Some(77)), ("lab2", None)] {
            conn.execute(
                "INSERT INTO labels (id, remote_id, team_id, name, color, created_at, updated_at)
                 VALUES (?1, ?2, 't1', ?1, '#ef4444', ?3, ?3)",
                rusqlite::params![id, rid, now],
            )
            .unwrap();
        }
        for (id, rid) in [("p1", Some(88)), ("p2", None)] {
            conn.execute(
                "INSERT INTO projects (id, remote_id, team_id, name, created_at, updated_at)
                 VALUES (?1, ?2, 't1', ?1, ?3, ?3)",
                rusqlite::params![id, rid, now],
            )
            .unwrap();
        }
        let Prep::Ready(p) = saved_view_update_prep(&conn, "sv1", false) else {
            panic!("expected ready");
        };
        assert_eq!(update_query(&p)["label_ids"], "30,77");
        assert_eq!(update_query(&p)["project_ids"], "none,88");
        assert_eq!(update_query(&p)["x"], "1");
        // The stored query keeps the placeholders.
        assert_eq!(p.stored, q);
    }

    #[test]
    fn saved_view_create_resolves_placeholders_and_drops_unresolvable() {
        let (conn, id) = saved_view_conn();
        insert_test_label(&conn, "lab1", Some(77));
        insert_test_label(&conn, "lab2", None);
        crate::db::saved_views::update_query_local(
            &conn,
            &id,
            &[("label_ids".to_string(), "local:lab1,local:lab2".to_string())].into(),
        )
        .unwrap();
        let Prep::Ready(p) = saved_view_create_prep(&conn, &id) else {
            panic!("expected Ready");
        };
        assert_eq!(p.query["label_ids"], "77");
    }

    #[test]
    fn saved_view_update_waits_for_an_unpushed_create_and_drops_a_missing_row() {
        let conn = saved_view_db(None, "{}");
        assert!(matches!(
            saved_view_update_prep(&conn, "sv1", false),
            Prep::Done(Outcome::LeavePending(_))
        ));
        assert!(matches!(
            saved_view_update_prep(&conn, "nope", false),
            Prep::Done(Outcome::DropAndLog(_))
        ));
    }
}
