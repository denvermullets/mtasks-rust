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
    /// Staged copy of the bytes while the upload is pending/failed.
    pub outbox_path: Option<String>,
    pub upload_kind: UploadKind,
    /// Inline images only: the uuid in the body's `jait-pending://<uuid>`.
    pub placeholder_uuid: Option<String>,
    /// Set once the direct upload and its PUT completed.
    pub signed_id: Option<String>,
    /// The blob's filename as the server stored it (for the blob URL).
    pub blob_filename: Option<String>,
}

/// Attached to its owner's list, or a bare blob a body points at.
pub use crate::files::validate::UploadKind;

const COLUMNS: &str = "id, remote_id, owner_kind, owner_id, blob_id, filename, content_type,
    byte_size, checksum, created_at, upload_state, failure_reason,
    outbox_path, upload_kind, placeholder_uuid, signed_id, blob_filename";

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
        outbox_path: r.get(12)?,
        upload_kind: UploadKind::from_db(&r.get::<_, String>(13)?),
        placeholder_uuid: r.get(14)?,
        signed_id: r.get(15)?,
        blob_filename: r.get(16)?,
    })
}

/// One attachment by local id.
pub fn get(conn: &Connection, id: &str) -> Result<Option<Attachment>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM attachments WHERE id = ?1"))?;
    let mut rows = stmt.query_map(params![id], from_row)?;
    rows.next().transpose()
}

/// Everything needed to record a file the user added.
#[derive(Debug, Clone)]
pub struct NewPending {
    pub owner_kind: OwnerKind,
    pub owner_id: String,
    pub filename: String,
    pub content_type: String,
    pub byte_size: i64,
    /// Base64 MD5 of the bytes.
    pub checksum: String,
    pub outbox_path: String,
    pub kind: UploadKind,
    pub placeholder_uuid: Option<String>,
}

/// Sync queue entity for the direct upload + PUT. `entity_id` is the
/// attachment's local id.
pub const UPLOAD_ENTITY: &str = "upload";
/// Sync queue entity for attaching the uploaded blob to its owner
/// (`attachment/create`; `attachment/delete` belongs to JRST-215).
pub const ATTACHMENT_ENTITY: &str = "attachment";

/// Record a Pending upload: insert the `pending` row and enqueue `upload/create`
/// (plus `attachment/create` for [`UploadKind::Attachment`]), atomically. Queue
/// order is the run order: the attach is enqueued right behind the upload.
pub fn add_pending(conn: &Connection, new: &NewPending) -> Result<Attachment> {
    let id = Uuid::new_v4().to_string();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO attachments (id, owner_kind, owner_id, filename, content_type, byte_size,
             checksum, created_at, upload_state, outbox_path, upload_kind, placeholder_uuid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9, ?10, ?11)",
        params![
            id,
            new.owner_kind.as_str(),
            new.owner_id,
            new.filename,
            new.content_type,
            new.byte_size,
            new.checksum,
            crate::db::now_iso(),
            new.outbox_path,
            new.kind.as_str(),
            new.placeholder_uuid
        ],
    )?;
    crate::db::sync_queue::enqueue(&tx, UPLOAD_ENTITY, &id, "create", None)?;
    if new.kind == UploadKind::Attachment {
        crate::db::sync_queue::enqueue(&tx, ATTACHMENT_ENTITY, &id, "create", None)?;
    }
    tx.commit()?;
    Ok(get(conn, &id)?.expect("row just inserted"))
}

/// The direct upload and PUT finished: remember what the attach needs.
pub fn set_uploaded(conn: &Connection, id: &str, signed_id: &str, blob_filename: &str) -> Result<()> {
    conn.execute(
        "UPDATE attachments SET signed_id = ?2, blob_filename = ?3 WHERE id = ?1",
        params![id, signed_id, blob_filename],
    )?;
    Ok(())
}

/// The server attached the file: the row becomes `synced` with its remote id
/// and the pending-upload fields are cleared (the caller removes the outbox
/// copy). A different synced row already holding this remote id (a pull that
/// raced the attach) is dropped so the unique index holds.
pub fn mark_attached(conn: &Connection, id: &str, dto: &AttachmentDto) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM attachments WHERE remote_id = ?1 AND id != ?2 AND upload_state = 'synced'",
        params![dto.id, id],
    )?;
    tx.execute(
        "UPDATE attachments SET remote_id = ?2, blob_id = ?3, filename = ?4, content_type = ?5,
             byte_size = ?6, checksum = ?7, created_at = ?8, upload_state = 'synced',
             failure_reason = NULL, outbox_path = NULL, signed_id = NULL, blob_filename = NULL
         WHERE id = ?1",
        params![
            id,
            dto.id,
            dto.blob_id,
            dto.filename,
            dto.content_type.clone().unwrap_or_default(),
            dto.byte_size,
            dto.checksum,
            dto.created_at
        ],
    )?;
    tx.commit()
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

    fn new_pending(name: &str) -> NewPending {
        NewPending {
            owner_kind: OwnerKind::Issue,
            owner_id: "i1".into(),
            filename: name.into(),
            content_type: "application/pdf".into(),
            byte_size: 12,
            checksum: "abc==".into(),
            outbox_path: "/tmp/outbox/x".into(),
            kind: UploadKind::Attachment,
            placeholder_uuid: None,
        }
    }

    fn queue(c: &Connection) -> Vec<(String, String, String)> {
        crate::db::sync_queue::pending_entries(c)
            .unwrap()
            .into_iter()
            .map(|e| (e.entity_type, e.action, e.entity_id))
            .collect()
    }

    #[test]
    fn add_pending_lists_the_row_and_queues_upload_then_attach() {
        let c = conn();
        let a = add_pending(&c, &new_pending("spec.pdf")).unwrap();
        assert_eq!(a.upload_state, UploadState::Pending);
        assert_eq!(a.outbox_path.as_deref(), Some("/tmp/outbox/x"));
        assert_eq!(names(&c), vec!["spec.pdf"]);
        assert_eq!(
            queue(&c),
            vec![
                ("upload".into(), "create".into(), a.id.clone()),
                ("attachment".into(), "create".into(), a.id.clone()),
            ]
        );
    }

    #[test]
    fn inline_images_queue_only_the_upload() {
        let c = conn();
        let mut n = new_pending("p.png");
        n.kind = UploadKind::InlineImage;
        n.placeholder_uuid = Some("u-1".into());
        let a = add_pending(&c, &n).unwrap();
        assert_eq!(a.placeholder_uuid.as_deref(), Some("u-1"));
        assert_eq!(queue(&c), vec![("upload".into(), "create".into(), a.id)]);
    }

    #[test]
    fn mark_attached_turns_the_row_synced_and_clears_upload_fields() {
        let c = conn();
        let a = add_pending(&c, &new_pending("spec.pdf")).unwrap();
        set_uploaded(&c, &a.id, "signed", "spec.pdf").unwrap();
        assert_eq!(get(&c, &a.id).unwrap().unwrap().signed_id.as_deref(), Some("signed"));
        mark_attached(&c, &a.id, &dto(9, "spec.pdf")).unwrap();
        let row = get(&c, &a.id).unwrap().unwrap();
        assert_eq!((row.upload_state, row.remote_id), (UploadState::Synced, Some(9)));
        assert_eq!((row.outbox_path, row.signed_id), (None, None));
    }

    #[test]
    fn mark_attached_replaces_a_synced_row_a_pull_already_inserted() {
        let c = conn();
        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(9, "spec.pdf")]).unwrap();
        let a = add_pending(&c, &new_pending("spec.pdf")).unwrap();
        mark_attached(&c, &a.id, &dto(9, "spec.pdf")).unwrap();
        assert_eq!(list_for_owner(&c, OwnerKind::Issue, "i1").unwrap().len(), 1);
    }
}
