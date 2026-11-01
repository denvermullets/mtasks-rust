//! Local mirror of files attached to issues, projects and comments. Pure over
//! a `rusqlite::Connection`: no Iced, no HTTP.
//!
//! `owner_id` is the owner's LOCAL id. Synced rows are replaced from each
//! pull of the owner's `files` ([`replace_synced`]); pending and failed rows
//! are local-only (upload tickets own them) and are never touched by it.
// The read side (list_for_owner, Attachment, UploadState) is consumed by the
// display and upload tickets; only replace_synced is wired so far.
#![allow(dead_code)]

use std::collections::HashMap;

use rusqlite::{params, Connection, Result, Row};
use uuid::Uuid;

use crate::api::AttachmentDto;

/// What an attachment hangs off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    Issue,
    Project,
    Comment,
}

impl OwnerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OwnerKind::Issue => "issue",
            OwnerKind::Project => "project",
            OwnerKind::Comment => "comment",
        }
    }
}

/// Where an attachment is in its life: on the server, waiting to upload, or
/// refused for good (see `failure_reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadState {
    Synced,
    Pending,
    Failed,
}

impl UploadState {
    pub fn as_str(self) -> &'static str {
        match self {
            UploadState::Synced => "synced",
            UploadState::Pending => "pending",
            UploadState::Failed => "failed",
        }
    }

    fn from_db(s: &str) -> Self {
        match s {
            "pending" => UploadState::Pending,
            "failed" => UploadState::Failed,
            _ => UploadState::Synced,
        }
    }
}

/// A row of the `attachments` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Local uuid.
    pub id: String,
    pub remote_id: Option<i64>,
    pub owner_kind: String,
    /// Owner's local id.
    pub owner_id: String,
    pub blob_id: Option<i64>,
    pub filename: String,
    pub content_type: String,
    pub byte_size: i64,
    pub checksum: Option<String>,
    pub created_at: String,
    pub upload_state: UploadState,
    pub failure_reason: Option<String>,
}

const COLUMNS: &str = "id, remote_id, owner_kind, owner_id, blob_id, filename, content_type,
    byte_size, checksum, created_at, upload_state, failure_reason";

fn from_row(r: &Row) -> Result<Attachment> {
    Ok(Attachment {
        id: r.get(0)?,
        remote_id: r.get(1)?,
        owner_kind: r.get(2)?,
        owner_id: r.get(3)?,
        blob_id: r.get(4)?,
        filename: r.get(5)?,
        content_type: r.get(6)?,
        byte_size: r.get(7)?,
        checksum: r.get(8)?,
        created_at: r.get(9)?,
        upload_state: UploadState::from_db(&r.get::<_, String>(10)?),
        failure_reason: r.get(11)?,
    })
}

/// An owner's attachments: server-known ones in remote `id` order, then
/// local pending/failed ones (no remote id yet) oldest first.
pub fn list_for_owner(conn: &Connection, kind: OwnerKind, owner_id: &str) -> Result<Vec<Attachment>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM attachments WHERE owner_kind = ?1 AND owner_id = ?2
         ORDER BY remote_id IS NULL, remote_id, created_at, id"
    ))?;
    let rows = stmt.query_map(params![kind.as_str(), owner_id], from_row)?;
    rows.collect()
}

/// Make the owner's `synced` rows match the server's list. Unchanged files
/// keep their local id and are refreshed in place, new ones are inserted,
/// missing ones are deleted. Pending and failed rows are left alone.
pub fn replace_synced(
    conn: &Connection,
    kind: OwnerKind,
    owner_id: &str,
    files: &[AttachmentDto],
) -> Result<()> {
    conn.execute_batch("SAVEPOINT replace_attachments")?;
    let result = (|| -> Result<()> {
        let existing: HashMap<i64, String> = {
            let mut stmt = conn.prepare(
                "SELECT remote_id, id FROM attachments
                 WHERE owner_kind = ?1 AND owner_id = ?2 AND upload_state = 'synced'
                   AND remote_id IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![kind.as_str(), owner_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<_>>()?
        };
        for (remote_id, local_id) in &existing {
            if !files.iter().any(|f| f.id == *remote_id) {
                conn.execute("DELETE FROM attachments WHERE id = ?1", params![local_id])?;
            }
        }
        for f in files {
            let content_type = f.content_type.clone().unwrap_or_default();
            match existing.get(&f.id) {
                Some(local_id) => {
                    conn.execute(
                        "UPDATE attachments SET blob_id = ?2, filename = ?3, content_type = ?4,
                             byte_size = ?5, checksum = ?6, created_at = ?7
                         WHERE id = ?1",
                        params![local_id, f.blob_id, f.filename, content_type, f.byte_size, f.checksum, f.created_at],
                    )?;
                }
                None => {
                    // The unique remote_id index would reject a duplicate, so
                    // clear a stale synced row that moved owner.
                    conn.execute(
                        "DELETE FROM attachments WHERE remote_id = ?1 AND upload_state = 'synced'",
                        params![f.id],
                    )?;
                    conn.execute(
                        "INSERT INTO attachments (id, remote_id, owner_kind, owner_id, blob_id,
                             filename, content_type, byte_size, checksum, created_at, upload_state)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'synced')",
                        params![
                            Uuid::new_v4().to_string(),
                            f.id,
                            kind.as_str(),
                            owner_id,
                            f.blob_id,
                            f.filename,
                            content_type,
                            f.byte_size,
                            f.checksum,
                            f.created_at
                        ],
                    )?;
                }
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("RELEASE replace_attachments"),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO replace_attachments; RELEASE replace_attachments");
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&c).unwrap();
        c
    }

    fn dto(id: i64, name: &str) -> AttachmentDto {
        serde_json::from_value(serde_json::json!({
            "id": id, "blob_id": id + 100, "filename": name,
            "content_type": "image/png", "byte_size": 10, "checksum": "abc==",
            "created_at": "2026-01-01T00:00:00.000Z",
            "download_url": format!("/api/v1/attachments/{id}/download")
        }))
        .unwrap()
    }

    fn local_row(c: &Connection, id: &str, state: &str) {
        c.execute(
            "INSERT INTO attachments (id, owner_kind, owner_id, filename, created_at, upload_state, failure_reason)
             VALUES (?1, 'issue', 'i1', ?1, ?4, ?2, ?3)",
            params![id, state, (state == "failed").then_some("too big"),
                if state == "failed" { "2026-03-01T00:00:00.000Z" } else { "2026-02-01T00:00:00.000Z" }],
        )
        .unwrap();
    }

    fn names(c: &Connection) -> Vec<String> {
        list_for_owner(c, OwnerKind::Issue, "i1").unwrap().into_iter().map(|a| a.filename).collect()
    }

    #[test]
    fn dto_parses_payload_fields() {
        let d = dto(7, "a.png");
        assert_eq!((d.id, d.blob_id, d.byte_size), (7, Some(107), 10));
        assert_eq!(d.download_url.as_deref(), Some("/api/v1/attachments/7/download"));
    }

    #[test]
    fn replace_adds_removes_and_keeps_unchanged() {
        let c = conn();
        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(2, "b"), dto(1, "a")]).unwrap();
        assert_eq!(names(&c), ["a", "b"], "remote id order");
        let before = list_for_owner(&c, OwnerKind::Issue, "i1").unwrap();

        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(1, "a"), dto(3, "c")]).unwrap();
        let after = list_for_owner(&c, OwnerKind::Issue, "i1").unwrap();
        assert_eq!(names(&c), ["a", "c"]);
        assert_eq!(after[0].id, before[0].id, "unchanged file keeps its local id");
    }

    #[test]
    fn replace_updates_changed_fields_and_empty_list_clears() {
        let c = conn();
        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(1, "a")]).unwrap();
        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(1, "renamed")]).unwrap();
        assert_eq!(names(&c), ["renamed"]);
        replace_synced(&c, OwnerKind::Issue, "i1", &[]).unwrap();
        assert!(names(&c).is_empty());
    }

    #[test]
    fn replace_keeps_pending_and_failed_rows() {
        let c = conn();
        local_row(&c, "p", "pending");
        local_row(&c, "f", "failed");
        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(1, "a")]).unwrap();
        assert_eq!(names(&c), ["a", "p", "f"]);
        replace_synced(&c, OwnerKind::Issue, "i1", &[]).unwrap();
        let rows = list_for_owner(&c, OwnerKind::Issue, "i1").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].upload_state, UploadState::Failed);
        assert_eq!(rows[1].failure_reason.as_deref(), Some("too big"));
    }

    #[test]
    fn replace_is_scoped_to_one_owner() {
        let c = conn();
        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(1, "a")]).unwrap();
        replace_synced(&c, OwnerKind::Issue, "i2", &[dto(2, "b")]).unwrap();
        replace_synced(&c, OwnerKind::Project, "i1", &[dto(3, "c")]).unwrap();
        replace_synced(&c, OwnerKind::Issue, "i1", &[]).unwrap();
        assert!(names(&c).is_empty());
        assert_eq!(list_for_owner(&c, OwnerKind::Issue, "i2").unwrap().len(), 1);
        assert_eq!(list_for_owner(&c, OwnerKind::Project, "i1").unwrap().len(), 1);
    }
}
