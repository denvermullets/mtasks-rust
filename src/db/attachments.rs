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
    // A comment's files ride on its comment-create (`files: [signed_id]`).
    if new.kind == UploadKind::Attachment && new.owner_kind != OwnerKind::Comment {
        crate::db::sync_queue::enqueue(&tx, ATTACHMENT_ENTITY, &id, "create", None)?;
    }
    tx.commit()?;
    Ok(get(conn, &id)?.expect("row just inserted"))
}

/// Drop a staged (still pending) row, e.g. its chip was removed from the
/// composer. Returns its outbox path for the caller to delete; its queued
/// upload then finishes as a no-op. `None` when there is no such pending row.
pub fn remove_staged(conn: &Connection, id: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    let path: Option<Option<String>> = conn
        .query_row(
            "SELECT outbox_path FROM attachments WHERE id = ?1 AND upload_state != 'synced'",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(path) = path else { return Ok(None) };
    conn.execute("DELETE FROM attachments WHERE id = ?1", params![id])?;
    Ok(Some(path.unwrap_or_default()))
}

/// The comment-create carrying a comment's files succeeded: its pending rows
/// are replaced by the server's list (when it sent one). Returns the outbox
/// paths of the removed rows for the caller to delete.
pub fn finish_comment_files(
    conn: &Connection,
    comment_id: &str,
    dtos: Option<&[AttachmentDto]>,
) -> Result<Vec<String>> {
    let tx = conn.unchecked_transaction()?;
    let mut paths = Vec::new();
    for a in list_for_owner(&tx, OwnerKind::Comment, comment_id)? {
        if a.upload_state == UploadState::Pending && a.upload_kind != UploadKind::InlineImage {
            if let Some(p) = a.outbox_path {
                paths.push(p);
            }
            tx.execute("DELETE FROM attachments WHERE id = ?1", params![a.id])?;
        }
    }
    if let Some(d) = dtos {
        replace_synced(&tx, OwnerKind::Comment, comment_id, d)?;
    }
    tx.commit()?;
    Ok(paths)
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

/// A permanent upload error: the row stays (with its outbox copy) as `failed`,
/// showing `reason`, until the user retries or removes it.
pub fn mark_failed(conn: &Connection, id: &str, reason: &str) -> Result<()> {
    conn.execute(
        "UPDATE attachments SET upload_state = 'failed', failure_reason = ?2
         WHERE id = ?1 AND upload_state = 'pending'",
        params![id, reason],
    )?;
    Ok(())
}

/// Retry a `failed` row: back to `pending` and requeue what is still missing
/// (the upload unless it already finished, plus the attach for
/// [`UploadKind::Attachment`]), skipping anything still queued. False when the
/// row is not failed.
pub fn retry_failed(conn: &Connection, id: &str) -> Result<bool> {
    use crate::db::sync_queue::{enqueue, has_pending_entry};
    let Some(a) = get(conn, id)? else { return Ok(false) };
    if a.upload_state != UploadState::Failed {
        return Ok(false);
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE attachments SET upload_state = 'pending', failure_reason = NULL WHERE id = ?1",
        params![id],
    )?;
    if a.signed_id.is_none() && !has_pending_entry(&tx, UPLOAD_ENTITY, id)? {
        enqueue(&tx, UPLOAD_ENTITY, id, "create", None)?;
    }
    if a.upload_kind == UploadKind::Attachment && !has_pending_entry(&tx, ATTACHMENT_ENTITY, id)? {
        enqueue(&tx, ATTACHMENT_ENTITY, id, "create", None)?;
    }
    tx.commit()?;
    Ok(true)
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

/// Files for each of the given comments, keyed by the comment's LOCAL id.
/// Comments without files get no entry.
pub fn list_for_comments(
    conn: &Connection,
    comment_ids: &[&str],
) -> Result<HashMap<String, Vec<Attachment>>> {
    let mut map = HashMap::new();
    for id in comment_ids {
        let files = list_for_owner(conn, OwnerKind::Comment, id)?;
        if !files.is_empty() {
            map.insert((*id).to_string(), files);
        }
    }
    Ok(map)
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
            // A delete the server has not seen yet: the row is already gone
            // locally, so a pull must not resurrect it.
            if has_pending_delete(conn, kind, f.id)? {
                continue;
            }
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

/// True while an `attachment/delete` for `remote_id` (of this owner kind) is
/// queued and unpushed.
fn has_pending_delete(conn: &Connection, kind: OwnerKind, remote_id: i64) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sync_queue
         WHERE entity_type = 'attachment' AND action = 'delete' AND synced_at IS NULL
           AND json_extract(payload, '$.owner_kind') = ?1
           AND json_extract(payload, '$.remote_id') = ?2)",
        params![kind.as_str(), remote_id],
        |r| r.get(0),
    )
}

/// One attachment by local id.
pub fn get(conn: &Connection, id: &str) -> Result<Option<Attachment>> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM attachments WHERE id = ?1"),
        params![id],
        from_row,
    )
    .optional()
}

/// Remove a synced attachment (issue, project or comment owner) locally and
/// return the `attachment/delete` queue payload, captured before the row goes:
/// `{owner_kind, team_remote_id, owner_remote_id, remote_id, restore}` where
/// `restore` is the row snapshot used to put it back if the server answers 403.
/// Comment owners add `parent_kind` ("issue"|"project") and `parent_remote_id`
/// (`owner_remote_id` is then the comment's).
/// `None` (nothing removed) for a missing row, a pending/failed one (JRST-202
/// cancels those) or an owner (or comment parent) the server has not seen.
pub fn remove_synced_attachment(conn: &Connection, id: &str) -> Result<Option<String>> {
    use crate::db::queries;
    let Some(a) = get(conn, id)? else { return Ok(None) };
    let (Some(remote_id), UploadState::Synced) = (a.remote_id, a.upload_state) else {
        return Ok(None);
    };
    // (team local id, owner remote id, extra payload fields)
    let (team_local, owner_remote_id, extra) = match a.owner_kind.as_str() {
        "issue" => {
            let Some(issue) = queries::get_issue(conn, &a.owner_id)? else { return Ok(None) };
            let Some(r) = issue.remote_id else { return Ok(None) };
            (issue.team_id, r, serde_json::json!({}))
        }
        "project" => {
            let Some(project) = queries::get_project(conn, &a.owner_id)? else { return Ok(None) };
            let Some(r) = project.remote_id else { return Ok(None) };
            (project.team_id, r, serde_json::json!({}))
        }
        "comment" => {
            let Some(comment) = queries::get_comment(conn, &a.owner_id)? else { return Ok(None) };
            let Some(comment_remote) = comment.remote_id else { return Ok(None) };
            let (kind, team, parent_remote) = match comment.owner() {
                crate::db::models::CommentOwner::Issue(iid) => {
                    let Some(i) = queries::get_issue(conn, &iid)? else { return Ok(None) };
                    ("issue", i.team_id, i.remote_id)
                }
                crate::db::models::CommentOwner::Project(pid) => {
                    let Some(p) = queries::get_project(conn, &pid)? else { return Ok(None) };
                    ("project", p.team_id, p.remote_id)
                }
            };
            let Some(parent_remote) = parent_remote else { return Ok(None) };
            (
                team,
                comment_remote,
                serde_json::json!({ "parent_kind": kind, "parent_remote_id": parent_remote }),
            )
        }
        _ => return Ok(None),
    };
    let Some(team_remote_id) = queries::remote_id_for(conn, "teams", &team_local)? else {
        return Ok(None);
    };
    conn.execute("DELETE FROM attachments WHERE id = ?1", params![id])?;
    let mut payload = serde_json::json!({
        "owner_kind": a.owner_kind,
        "team_remote_id": team_remote_id,
        "owner_remote_id": owner_remote_id,
        "remote_id": remote_id,
        "restore": {
            "id": a.id, "owner_kind": a.owner_kind, "owner_id": a.owner_id,
            "blob_id": a.blob_id, "filename": a.filename, "content_type": a.content_type,
            "byte_size": a.byte_size, "checksum": a.checksum, "created_at": a.created_at,
        },
    });
    if let (Some(obj), Some(more)) = (payload.as_object_mut(), extra.as_object()) {
        obj.extend(more.clone());
    }
    Ok(Some(payload.to_string()))
}

/// Cancel / remove a not-yet-uploaded (pending or failed) row: delete it, drop
/// its unsent `upload`/`attachment` queue entries and the outbox copy. Returns
/// whether a row was removed; synced rows are refused (use the delete path).
pub fn remove_local(conn: &Connection, id: &str) -> Result<bool> {
    let Some(a) = get(conn, id)? else { return Ok(false) };
    if a.upload_state == UploadState::Synced {
        return Ok(false);
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM sync_queue WHERE entity_id = ?1 AND entity_type IN (?2, ?3) AND synced_at IS NULL",
        params![id, UPLOAD_ENTITY, ATTACHMENT_ENTITY],
    )?;
    tx.execute("DELETE FROM attachments WHERE id = ?1", params![id])?;
    tx.commit()?;
    if let Some(path) = a.outbox_path.as_deref() {
        let _ = crate::files::outbox::remove(std::path::Path::new(path));
    }
    Ok(true)
}

/// A comment draft was dropped (reply cancelled, composer discarded, user
/// navigated away): cancel every unsent staged file / pasted image owned by the
/// given future-comment ids so their uploads never send. Rows whose comment was
/// already submitted (a `comments` row with that id exists) are left alone.
/// Returns how many rows were removed.
pub fn discard_unsent_for_drafts(conn: &Connection, draft_ids: &[&str]) -> Result<usize> {
    let mut removed = 0;
    for draft in draft_ids {
        let submitted: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM comments WHERE id = ?1)",
            params![draft],
            |r| r.get(0),
        )?;
        if submitted {
            continue;
        }
        for a in list_for_owner(conn, OwnerKind::Comment, draft)? {
            if remove_local(conn, &a.id)? {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// Inline-image uploads of an owner that are ready for their body to be sent:
/// placeholder uuid -> (signed id, blob filename). FAILED rows are left out on
/// purpose, so a body waiting on one stays blocked until Retry/Remove.
pub fn inline_resolutions(
    conn: &Connection,
    kind: OwnerKind,
    owner_id: &str,
) -> Result<HashMap<String, (String, String)>> {
    Ok(list_for_owner(conn, kind, owner_id)?
        .into_iter()
        .filter(|a| a.upload_kind == UploadKind::InlineImage && a.upload_state == UploadState::Pending)
        .filter_map(|a| Some((a.placeholder_uuid?, (a.signed_id?, a.blob_filename?))))
        .collect())
}

/// After a body carrying `resolved` placeholders was pushed: rewrite the
/// owner's LOCAL body to the web form too (so it keeps no stale placeholder),
/// then drop those inline rows and their outbox files. A bare blob has no
/// attach step, so the row's job is done. Rows for placeholders not in
/// `resolved` (e.g. pasted after the push was prepared) are left alone.
pub fn settle_inline_images(
    conn: &Connection,
    kind: OwnerKind,
    owner_id: &str,
    resolved: &HashMap<String, (String, String)>,
) -> Result<()> {
    if resolved.is_empty() {
        return Ok(());
    }
    let (table, column) = match kind {
        OwnerKind::Issue => ("issues", "description"),
        OwnerKind::Project => ("projects", "description"),
        OwnerKind::Comment => ("comments", "body"),
    };
    let current: Option<String> = conn
        .query_row(&format!("SELECT {column} FROM {table} WHERE id = ?1"), params![owner_id], |r| r.get(0))
        .ok()
        .flatten();
    if let Some(text) = current {
        let rewritten = crate::mentions::rewrite_placeholders(&text, resolved).body;
        if rewritten != text {
            conn.execute(
                &format!("UPDATE {table} SET {column} = ?1 WHERE id = ?2"),
                params![rewritten, owner_id],
            )?;
        }
    }
    for row in list_for_owner(conn, kind, owner_id)? {
        let used = row.upload_kind == UploadKind::InlineImage
            && row.placeholder_uuid.as_ref().is_some_and(|u| resolved.contains_key(u));
        if used {
            conn.execute("DELETE FROM attachments WHERE id = ?1", params![row.id])?;
            if let Some(path) = row.outbox_path.as_deref() {
                let _ = crate::files::outbox::remove(std::path::Path::new(path));
            }
        }
    }
    Ok(())
}

/// Remove the `jait-pending://<uuid>` placeholder from the owner's LOCAL body
/// (JRST-214: Remove on a failed inline image). Returns the new body when it
/// changed, `None` when the owner or placeholder was not there.
pub fn strip_inline_placeholder(
    conn: &Connection,
    kind: OwnerKind,
    owner_id: &str,
    uuid: &str,
) -> Result<Option<String>> {
    let (table, column) = match kind {
        OwnerKind::Issue => ("issues", "description"),
        OwnerKind::Project => ("projects", "description"),
        OwnerKind::Comment => ("comments", "body"),
    };
    let current: Option<String> = conn
        .query_row(&format!("SELECT {column} FROM {table} WHERE id = ?1"), params![owner_id], |r| r.get(0))
        .ok()
        .flatten();
    let Some(text) = current else { return Ok(None) };
    let stripped = crate::mentions::strip_placeholder(&text, uuid);
    if stripped == text {
        return Ok(None);
    }
    conn.execute(
        &format!("UPDATE {table} SET {column} = ?1 WHERE id = ?2"),
        params![stripped, owner_id],
    )?;
    Ok(Some(stripped))
}

/// Put back a row captured in a delete payload's `restore` (after a 403).
/// A no-op when the row (by remote id) is already there again.
pub fn restore_from_snapshot(conn: &Connection, snap: &serde_json::Value, remote_id: i64) -> Result<()> {
    let s = |k: &str| snap.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let (Some(id), Some(kind), Some(owner_id), Some(filename), Some(created_at)) =
        (s("id"), s("owner_kind"), s("owner_id"), s("filename"), s("created_at"))
    else {
        return Ok(());
    };
    conn.execute(
        "INSERT INTO attachments (id, remote_id, owner_kind, owner_id, blob_id,
             filename, content_type, byte_size, checksum, created_at, upload_state)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'synced'
         WHERE NOT EXISTS (SELECT 1 FROM attachments WHERE remote_id = ?2)",
        params![
            id,
            remote_id,
            kind,
            owner_id,
            snap.get("blob_id").and_then(|v| v.as_i64()),
            filename,
            s("content_type").unwrap_or_default(),
            snap.get("byte_size").and_then(|v| v.as_i64()).unwrap_or(0),
            s("checksum"),
            created_at,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod comment_file_tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&c).unwrap();
        c
    }

    fn new_comment_file(owner: &str) -> NewPending {
        NewPending {
            owner_kind: OwnerKind::Comment,
            owner_id: owner.into(),
            filename: "log.txt".into(),
            content_type: "text/plain".into(),
            byte_size: 3,
            checksum: "abc==".into(),
            outbox_path: "/tmp/outbox/c".into(),
            kind: UploadKind::Attachment,
            placeholder_uuid: None,
        }
    }

    #[test]
    fn comment_files_queue_only_an_upload_because_comment_create_carries_them() {
        let c = conn();
        let a = add_pending(&c, &new_comment_file("c1")).unwrap();
        let kinds: Vec<String> = c
            .prepare("SELECT entity_type FROM sync_queue WHERE entity_id = ?1")
            .unwrap()
            .query_map([&a.id], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(kinds, vec![UPLOAD_ENTITY.to_string()]);
    }

    #[test]
    fn removing_a_staged_file_returns_its_outbox_path_and_deletes_the_row() {
        let c = conn();
        let a = add_pending(&c, &new_comment_file("c1")).unwrap();
        assert_eq!(remove_staged(&c, &a.id).unwrap().as_deref(), Some("/tmp/outbox/c"));
        assert!(get(&c, &a.id).unwrap().is_none());
        assert_eq!(remove_staged(&c, &a.id).unwrap(), None);
    }

    #[test]
    fn discarding_a_draft_cancels_unsent_rows_but_not_submitted_comments() {
        let c = conn();
        let staged = add_pending(&c, &new_comment_file("draft")).unwrap();
        let kept = add_pending(&c, &new_comment_file("sent")).unwrap();
        c.execute("PRAGMA foreign_keys=OFF", []).ok();
        c.execute(
            "INSERT INTO comments (id, issue_id, body, created_at, updated_at) VALUES ('sent', 'i1', 'b', 'x', 'x')",
            [],
        )
        .unwrap();
        assert_eq!(discard_unsent_for_drafts(&c, &["draft", "sent"]).unwrap(), 1);
        assert!(get(&c, &staged.id).unwrap().is_none());
        assert!(get(&c, &kept.id).unwrap().is_some());
        let queued: i64 = c
            .query_row("SELECT COUNT(*) FROM sync_queue WHERE entity_id = ?1", params![staged.id], |r| r.get(0))
            .unwrap();
        assert_eq!(queued, 0);
    }

    #[test]
    fn finishing_comment_files_swaps_pending_rows_for_the_servers_list() {
        let c = conn();
        add_pending(&c, &new_comment_file("c1")).unwrap();
        let dto: AttachmentDto = serde_json::from_value(serde_json::json!({
            "id": 9, "blob_id": 109, "filename": "log.txt", "content_type": "text/plain",
            "byte_size": 3, "checksum": "abc==", "created_at": "2026-01-01T00:00:00.000Z",
            "download_url": "/api/v1/attachments/9/download"
        }))
        .unwrap();
        let paths = finish_comment_files(&c, "c1", Some(&[dto])).unwrap();
        assert_eq!(paths, vec!["/tmp/outbox/c".to_string()]);
        let rows = list_for_owner(&c, OwnerKind::Comment, "c1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].remote_id, Some(9));
        assert_eq!(rows[0].upload_state, UploadState::Synced);
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
    fn pull_does_not_resurrect_attachment_with_queued_delete() {
        let c = conn();
        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(5, "a.png"), dto(6, "b.png")]).unwrap();
        let a = list_for_owner(&c, OwnerKind::Issue, "i1").unwrap().remove(0);
        assert_eq!(a.remote_id, Some(5));
        c.execute("DELETE FROM attachments WHERE id = ?1", params![a.id]).unwrap();
        c.execute(
            "INSERT INTO sync_queue (entity_type, entity_id, action, payload, created_at)
             VALUES ('attachment', ?1, 'delete', '{\"owner_kind\":\"issue\",\"remote_id\":5}', 'now')",
            params![a.id],
        )
        .unwrap();
        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(5, "a.png"), dto(6, "b.png")]).unwrap();
        assert_eq!(names(&c), vec!["b.png"]);
        // Once the delete is pushed, the server list decides again.
        c.execute("UPDATE sync_queue SET synced_at = 'x' WHERE entity_type = 'attachment'", []).unwrap();
        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(5, "a.png"), dto(6, "b.png")]).unwrap();
        assert_eq!(names(&c).len(), 2);
    }

    #[test]
    fn strip_inline_placeholder_cleans_owner_body() {
        let c = conn();
        let uuid = "123e4567-e89b-12d3-a456-426614174000";
        c.execute("PRAGMA foreign_keys=OFF", []).ok();
        c.execute(
            "INSERT INTO issues (id, title, description, lane_id, team_id, created_at, updated_at) VALUES ('i1','t',?1,'l','t','x','x')",
            params![format!("a ![image](jait-pending://{uuid}) b")],
        )
        .unwrap();
        let new = strip_inline_placeholder(&c, OwnerKind::Issue, "i1", uuid).unwrap();
        assert_eq!(new.as_deref(), Some("a  b"));
        let stored: String =
            c.query_row("SELECT description FROM issues WHERE id='i1'", [], |r| r.get(0)).unwrap();
        assert_eq!(stored, "a  b");
        assert_eq!(strip_inline_placeholder(&c, OwnerKind::Issue, "i1", uuid).unwrap(), None);
        assert_eq!(strip_inline_placeholder(&c, OwnerKind::Issue, "nope", uuid).unwrap(), None);
    }

    fn inline_row(c: &Connection, id: &str, uuid: &str, state: &str, signed: Option<&str>) {
        c.execute(
            "INSERT INTO attachments (id, owner_kind, owner_id, filename, created_at, upload_state, upload_kind, placeholder_uuid, signed_id, blob_filename)
             VALUES (?1, 'issue', 'i1', 'p.png', '2026-02-01T00:00:00.000Z', ?2, 'inline_image', ?3, ?4, ?5)",
            params![id, state, uuid, signed, signed.map(|_| "p.png")],
        )
        .unwrap();
    }

    #[test]
    fn inline_resolutions_skip_unuploaded_and_failed() {
        let c = conn();
        inline_row(&c, "a", "u-ok", "pending", Some("sig"));
        inline_row(&c, "b", "u-wait", "pending", None);
        inline_row(&c, "c", "u-bad", "failed", Some("sig2"));
        let m = inline_resolutions(&c, OwnerKind::Issue, "i1").unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m["u-ok"], ("sig".to_string(), "p.png".to_string()));
    }

    #[test]
    fn settle_rewrites_local_body_and_drops_used_rows() {
        let c = conn();
        c.execute("PRAGMA foreign_keys=OFF", []).ok();
        inline_row(&c, "a", "11111111-1111-4111-8111-111111111111", "pending", Some("sig"));
        inline_row(&c, "b", "22222222-2222-4222-8222-222222222222", "pending", None);
        let body = "x ![image](jait-pending://11111111-1111-4111-8111-111111111111) ![image](jait-pending://22222222-2222-4222-8222-222222222222)";
        c.execute("INSERT INTO issues (id, title, description, lane_id, team_id, created_at, updated_at) VALUES ('i1','t',?1,'l','t','x','x')", params![body]).unwrap();
        let resolved = inline_resolutions(&c, OwnerKind::Issue, "i1").unwrap();
        settle_inline_images(&c, OwnerKind::Issue, "i1", &resolved).unwrap();
        let d: String = c.query_row("SELECT description FROM issues WHERE id='i1'", [], |r| r.get(0)).unwrap();
        assert_eq!(d, "x ![image](/rails/active_storage/blobs/redirect/sig/p.png) ![image](jait-pending://22222222-2222-4222-8222-222222222222)");
        let left: Vec<_> = list_for_owner(&c, OwnerKind::Issue, "i1").unwrap().into_iter().map(|a| a.id).collect();
        assert_eq!(left, ["b"]);
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

    fn seed_issue(c: &Connection, remote: Option<i64>) {
        let now = "2026-01-01T00:00:00.000Z";
        c.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at) VALUES ('t1','T','T',5,?1,?1)",
            params![now],
        )
        .unwrap();
        c.execute(
            "INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)",
            params![now],
        )
        .unwrap();
        c.execute(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, remote_id, created_at, updated_at)
             VALUES ('i1','i',4,'l1','t1',?2,?1,?1)",
            params![now, remote],
        )
        .unwrap();
    }

    #[test]
    fn removing_a_synced_issue_attachment_yields_a_restorable_payload() {
        let c = conn();
        seed_issue(&c, Some(40));
        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(7, "a.png")]).unwrap();
        let local = list_for_owner(&c, OwnerKind::Issue, "i1").unwrap()[0].id.clone();

        let payload = remove_synced_attachment(&c, &local).unwrap().expect("payload");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["owner_kind"], "issue");
        assert_eq!(v["team_remote_id"], 5);
        assert_eq!(v["owner_remote_id"], 40);
        assert_eq!(v["remote_id"], 7);
        assert!(names(&c).is_empty());

        restore_from_snapshot(&c, &v["restore"], 7).unwrap();
        restore_from_snapshot(&c, &v["restore"], 7).unwrap(); // idempotent
        assert_eq!(names(&c), ["a.png"]);
        assert_eq!(list_for_owner(&c, OwnerKind::Issue, "i1").unwrap()[0].id, local);
    }

    #[test]
    fn pending_failed_and_unknown_attachments_are_not_removed_this_way() {
        let c = conn();
        seed_issue(&c, Some(40));
        local_row(&c, "p", "pending");
        local_row(&c, "f", "failed");
        assert!(remove_synced_attachment(&c, "p").unwrap().is_none());
        assert!(remove_synced_attachment(&c, "f").unwrap().is_none());
        assert!(remove_synced_attachment(&c, "nope").unwrap().is_none());
        assert_eq!(names(&c).len(), 2);
    }

    #[test]
    fn an_issue_the_server_has_not_seen_keeps_the_attachment() {
        let c = conn();
        seed_issue(&c, None);
        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(7, "a.png")]).unwrap();
        let local = list_for_owner(&c, OwnerKind::Issue, "i1").unwrap()[0].id.clone();
        assert!(remove_synced_attachment(&c, &local).unwrap().is_none());
        assert_eq!(names(&c).len(), 1);
    }

    fn seed_project(c: &Connection, remote: Option<i64>) {
        let now = "2026-01-01T00:00:00.000Z";
        c.execute(
            "INSERT INTO projects (id, remote_id, team_id, name, created_at, updated_at)
             VALUES ('p1', ?2, 't1', 'P', ?1, ?1)",
            params![now, remote],
        )
        .unwrap();
    }

    fn seed_comment(c: &Connection, id: &str, issue: Option<&str>, project: Option<&str>, remote: Option<i64>) {
        let now = "2026-01-01T00:00:00.000Z";
        c.execute(
            "INSERT INTO comments (id, remote_id, issue_id, project_id, body, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'b', ?5, ?5)",
            params![id, remote, issue, project, now],
        )
        .unwrap();
    }

    #[test]
    fn removing_a_project_attachment_yields_a_restorable_payload() {
        let c = conn();
        seed_issue(&c, Some(40));
        seed_project(&c, Some(60));
        replace_synced(&c, OwnerKind::Project, "p1", &[dto(8, "p.png")]).unwrap();
        let local = list_for_owner(&c, OwnerKind::Project, "p1").unwrap()[0].id.clone();

        let payload = remove_synced_attachment(&c, &local).unwrap().expect("payload");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(v["owner_kind"], "project");
        assert_eq!(v["team_remote_id"], 5);
        assert_eq!(v["owner_remote_id"], 60);
        assert_eq!(v["remote_id"], 8);
        assert!(list_for_owner(&c, OwnerKind::Project, "p1").unwrap().is_empty());

        restore_from_snapshot(&c, &v["restore"], 8).unwrap();
        assert_eq!(list_for_owner(&c, OwnerKind::Project, "p1").unwrap()[0].id, local);
    }

    #[test]
    fn removing_a_comment_attachment_records_the_comment_and_its_parent() {
        let c = conn();
        seed_issue(&c, Some(40));
        seed_project(&c, Some(60));
        seed_comment(&c, "ci", Some("i1"), None, Some(71));
        seed_comment(&c, "cp", None, Some("p1"), Some(72));
        replace_synced(&c, OwnerKind::Comment, "ci", &[dto(9, "a")]).unwrap();
        replace_synced(&c, OwnerKind::Comment, "cp", &[dto(10, "b")]).unwrap();

        let id_of = |owner: &str| list_for_owner(&c, OwnerKind::Comment, owner).unwrap()[0].id.clone();
        let v: serde_json::Value =
            serde_json::from_str(&remove_synced_attachment(&c, &id_of("ci")).unwrap().unwrap()).unwrap();
        assert_eq!(v["owner_kind"], "comment");
        assert_eq!(v["parent_kind"], "issue");
        assert_eq!(v["parent_remote_id"], 40);
        assert_eq!(v["owner_remote_id"], 71);
        assert_eq!(v["team_remote_id"], 5);
        assert_eq!(v["remote_id"], 9);

        let v: serde_json::Value =
            serde_json::from_str(&remove_synced_attachment(&c, &id_of("cp")).unwrap().unwrap()).unwrap();
        assert_eq!(v["parent_kind"], "project");
        assert_eq!(v["parent_remote_id"], 60);
        assert_eq!(v["owner_remote_id"], 72);
    }

    #[test]
    fn a_comment_the_server_has_not_seen_keeps_its_attachment() {
        let c = conn();
        seed_issue(&c, Some(40));
        seed_comment(&c, "ci", Some("i1"), None, None);
        replace_synced(&c, OwnerKind::Comment, "ci", &[dto(9, "a")]).unwrap();
        let id = list_for_owner(&c, OwnerKind::Comment, "ci").unwrap()[0].id.clone();
        assert!(remove_synced_attachment(&c, &id).unwrap().is_none());
        assert_eq!(list_for_owner(&c, OwnerKind::Comment, "ci").unwrap().len(), 1);
    }

    #[test]
    fn list_for_comments_keys_by_comment_and_skips_empty() {
        let c = conn();
        replace_synced(&c, OwnerKind::Comment, "c1", &[dto(1, "a")]).unwrap();
        replace_synced(&c, OwnerKind::Comment, "c2", &[dto(2, "b"), dto(3, "c")]).unwrap();
        replace_synced(&c, OwnerKind::Issue, "c3", &[dto(4, "issue-file")]).unwrap();
        let map = list_for_comments(&c, &["c1", "c2", "c3"]).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map["c1"].len(), 1);
        assert_eq!(map["c2"].iter().map(|a| a.filename.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        assert!(!map.contains_key("c3"), "issue files are not comment files");
    }

    #[test]
    fn mark_failed_keeps_the_row_with_a_reason() {
        let c = conn();
        let a = add_pending(&c, &new_pending("big.bin")).unwrap();
        mark_failed(&c, &a.id, "validation: too big").unwrap();
        let row = get(&c, &a.id).unwrap().unwrap();
        assert_eq!(row.upload_state, UploadState::Failed);
        assert_eq!(row.failure_reason.as_deref(), Some("validation: too big"));
        assert_eq!(row.outbox_path.as_deref(), Some("/tmp/outbox/x"), "file is never dropped");
    }

    #[test]
    fn retry_requeues_upload_and_attach_once() {
        let c = conn();
        let a = add_pending(&c, &new_pending("big.bin")).unwrap();
        mark_failed(&c, &a.id, "nope").unwrap();
        for e in crate::db::sync_queue::pending_entries(&c).unwrap() {
            crate::db::sync_queue::mark_synced(&c, e.id.unwrap()).unwrap();
        }
        assert!(retry_failed(&c, &a.id).unwrap());
        let row = get(&c, &a.id).unwrap().unwrap();
        assert_eq!((row.upload_state, row.failure_reason), (UploadState::Pending, None));
        assert_eq!(
            queue(&c),
            vec![
                ("upload".into(), "create".into(), a.id.clone()),
                ("attachment".into(), "create".into(), a.id.clone()),
            ]
        );
        assert!(!retry_failed(&c, &a.id).unwrap(), "only failed rows retry");
        assert_eq!(queue(&c).len(), 2);
    }

    #[test]
    fn retry_after_the_upload_finished_only_requeues_the_attach() {
        let c = conn();
        let a = add_pending(&c, &new_pending("big.bin")).unwrap();
        set_uploaded(&c, &a.id, "signed", "big.bin").unwrap();
        mark_failed(&c, &a.id, "owner gone").unwrap();
        for e in crate::db::sync_queue::pending_entries(&c).unwrap() {
            crate::db::sync_queue::mark_synced(&c, e.id.unwrap()).unwrap();
        }
        assert!(retry_failed(&c, &a.id).unwrap());
        assert_eq!(queue(&c), vec![("attachment".into(), "create".into(), a.id)]);
    }

    #[test]
    fn remove_local_cancels_a_pending_upload_row_queue_and_outbox_file() {
        let c = conn();
        let dir = std::env::temp_dir().join(format!("rm-local-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("blob");
        std::fs::write(&file, b"x").unwrap();
        let mut n = new_pending("spec.pdf");
        n.outbox_path = file.to_string_lossy().into_owned();
        let a = add_pending(&c, &n).unwrap();
        let keep = add_pending(&c, &new_pending("keep.pdf")).unwrap();
        assert!(remove_local(&c, &a.id).unwrap());
        assert_eq!(names(&c), vec!["keep.pdf"]);
        assert!(!file.exists());
        assert!(queue(&c).iter().all(|(_, _, id)| *id == keep.id));
        assert!(!remove_local(&c, &a.id).unwrap(), "already gone");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_local_refuses_synced_rows() {
        let c = conn();
        replace_synced(&c, OwnerKind::Issue, "i1", &[dto(9, "a.png")]).unwrap();
        let id = list_for_owner(&c, OwnerKind::Issue, "i1").unwrap()[0].id.clone();
        assert!(!remove_local(&c, &id).unwrap());
        assert_eq!(names(&c), vec!["a.png"]);
    }
}
