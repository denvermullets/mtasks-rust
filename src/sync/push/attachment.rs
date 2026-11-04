//! Pushing attachment uploads and deletes, and inline images in bodies.

use rusqlite::Connection;

use crate::api::{ApiClient, ApiError};
use crate::api::attachments as attachments_api;
use crate::db::{attachments, queries};
use crate::db::attachments::{Attachment, UploadState};
use crate::sync::engine::open;
use super::comment::comment_delete_outcome;
use super::outcome::{Outcome, Prep, classify_in_team, is_token_scope_message};

/// Inline-image uploads a body was rewritten with: placeholder uuid ->
/// (signed id, blob filename). Settled (local body rewritten, rows dropped)
/// once the push succeeded.
pub(super) type InlineImages = std::collections::HashMap<String, (String, String)>;

/// Pure decision for a body that may hold `jait-pending://` placeholders:
/// the web-form body once every placeholder is resolved, else `LeavePending`
/// (an upload still running, or a FAILED one, which stays blocking until the
/// user retries or removes it). Reused by comment/project body writers.
fn rewrite_body_or_wait(body: &str, resolved: &InlineImages) -> Result<String, Outcome> {
    let rw = crate::mentions::rewrite_placeholders(body, resolved);
    if rw.unresolved.is_empty() {
        Ok(rw.body)
    } else {
        Err(Outcome::LeavePending(format!(
            "waiting for {} inline image upload(s)",
            rw.unresolved.len()
        )))
    }
}

/// DB side of [`rewrite_body_or_wait`]: look up the owner's inline-image
/// uploads, then rewrite `body`. Returns the body to send plus the map to hand
/// to [`settle_inline`] after a successful push.
pub(super) fn prepare_body(
    conn: &Connection,
    kind: attachments::OwnerKind,
    owner_id: &str,
    body: Option<String>,
) -> Result<(Option<String>, InlineImages), Outcome> {
    let Some(text) = body else { return Ok((None, InlineImages::new())) };
    if !text.contains(crate::mentions::PENDING_SCHEME) {
        return Ok((Some(text), InlineImages::new()));
    }
    let resolved = attachments::inline_resolutions(conn, kind, owner_id)
        .map_err(|e| Outcome::LeavePending(format!("db: {e}")))?;
    let rewritten = rewrite_body_or_wait(&text, &resolved)?;
    Ok((Some(rewritten), resolved))
}

/// Best-effort cleanup after the body was accepted by Rails.
pub(super) fn settle_inline(kind: attachments::OwnerKind, owner_id: &str, inline: &InlineImages) {
    if inline.is_empty() {
        return;
    }
    if let Ok(conn) = open() {
        if let Err(e) = attachments::settle_inline_images(&conn, kind, owner_id, inline) {
            eprintln!("settle inline images: {e}");
        }
    }
}

/// What an `attachment/delete` entry needs, parsed from its payload (the local
/// row is gone by drain time). See `db::attachments::remove_synced_attachment`.
#[derive(Debug, PartialEq)]
struct AttachmentDeletePrep {
    team_remote_id: i64,
    owner: attachments_api::AttachmentOwner,
    remote_id: i64,
}

fn prep_attachment_delete(payload: Option<&str>) -> Prep<AttachmentDeletePrep> {
    let Some(raw) = payload else {
        return Prep::Done(Outcome::DropAndLog("attachment delete: no payload".into()));
    };
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            return Prep::Done(Outcome::DropAndLog(format!("attachment delete: bad payload: {e}")))
        }
    };
    let field = |k: &str| parsed.get(k).and_then(|v| v.as_i64());
    let owner = match (
        parsed.get("owner_kind").and_then(|v| v.as_str()),
        parsed.get("parent_kind").and_then(|v| v.as_str()),
        field("owner_remote_id"),
        field("parent_remote_id"),
    ) {
        (Some("issue"), _, Some(issue_id), _) => attachments_api::AttachmentOwner::Issue { issue_id },
        (Some("project"), _, Some(project_id), _) => {
            attachments_api::AttachmentOwner::Project { project_id }
        }
        (Some("comment"), Some("issue"), Some(comment_id), Some(issue_id)) => {
            attachments_api::AttachmentOwner::IssueComment { issue_id, comment_id }
        }
        (Some("comment"), Some("project"), Some(comment_id), Some(project_id)) => {
            attachments_api::AttachmentOwner::ProjectComment { project_id, comment_id }
        }
        _ => {
            return Prep::Done(Outcome::DropAndLog(
                "attachment delete: unsupported or incomplete owner".into(),
            ))
        }
    };
    match (field("team_remote_id"), field("remote_id")) {
        (Some(team_remote_id), Some(remote_id)) => {
            Prep::Ready(AttachmentDeletePrep { team_remote_id, owner, remote_id })
        }
        _ => Prep::Done(Outcome::DropAndLog("attachment delete: incomplete payload".into())),
    }
}

/// Same rules as `comment/delete` (204/404 = gone, 403 dropped with a toast),
/// and a 403 also puts the row back from the payload's `restore` snapshot so
/// the attachment reappears without waiting for the owner to change upstream.
pub(super) async fn attachment_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let prep = match prep_attachment_delete(payload) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    let result = attachments_api::delete(client, prep.team_remote_id, prep.owner, prep.remote_id).await;
    let forbidden = matches!(&result, Err(ApiError::Forbidden(m)) if !is_token_scope_message(m));
    if forbidden {
        restore_deleted_attachment(payload, prep.remote_id);
    }
    comment_delete_outcome(result, prep.team_remote_id)
}

fn restore_deleted_attachment(payload: Option<&str>, remote_id: i64) {
    let snap = payload
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        .and_then(|v| v.get("restore").cloned());
    if let (Some(snap), Ok(conn)) = (snap, open()) {
        let _ = crate::db::attachments::restore_from_snapshot(&conn, &snap, remote_id);
    }
}

// ── Attachment uploads ────────────────────────────────────────────────────
//
// `upload/create` and `attachment/create` entries (entity_id = the attachment's
// local id), queued together by `db::attachments::add_pending`. Both wait while
// the owner has no remote id; the attach also waits for the upload's
// `signed_id`. Errors are split by [`classify_upload_error`]: permanent ones mark
// the row `failed` (Retry/Remove on the card), temporary ones keep retrying.

/// What an `upload/create` entry needs to run.
struct UploadPrep {
    attachment_id: String,
    outbox_path: String,
    blob: attachments_api::BlobParams,
}

/// What an `attachment/create` entry needs to run.
struct AttachPrep {
    attachment_id: String,
    team_remote_id: i64,
    owner: attachments_api::AttachmentOwner,
    signed_id: String,
    outbox_path: Option<String>,
}

/// Remote id of an attachment's owner; `Err` is the wait/drop outcome.
fn owner_remote_id(conn: &Connection, a: &Attachment) -> Result<i64, Outcome> {
    match attachments::owner_ref(conn, a) {
        Ok(Some(o)) => o
            .remote_id
            .ok_or_else(|| Outcome::LeavePending(format!("{} not synced yet", a.owner_kind))),
        Ok(None) => Err(Outcome::DropAndLog(format!(
            "attachment owner ({}) missing locally",
            a.owner_kind
        ))),
        Err(e) => Err(Outcome::LeavePending(format!("db: {e}"))),
    }
}

fn prep_upload_create_in(conn: &Connection, entity_id: &str) -> Prep<UploadPrep> {
    let a = match attachments::get(conn, entity_id) {
        Ok(Some(a)) => a,
        // Removed before it ran (cancel).
        Ok(None) => return Prep::Done(Outcome::Success),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    if a.signed_id.is_some() || a.upload_state != UploadState::Pending {
        return Prep::Done(Outcome::Success);
    }
    // A comment's files upload while it is still being written (and while it
    // has no remote id): its create waits for them, not the other way round.
    if a.owner_kind != attachments::OwnerKind::Comment {
        if let Err(o) = owner_remote_id(conn, &a) {
            return Prep::Done(o);
        }
    }
    let Some(outbox_path) = a.outbox_path.clone() else {
        return Prep::Done(Outcome::LeavePending("no outbox copy".into()));
    };
    Prep::Ready(UploadPrep {
        attachment_id: a.id,
        outbox_path,
        blob: attachments_api::BlobParams {
            filename: a.filename,
            byte_size: a.byte_size.max(0) as u64,
            checksum: a.checksum.unwrap_or_default(),
            content_type: a.content_type,
        },
    })
}

fn prep_attachment_create_in(conn: &Connection, entity_id: &str) -> Prep<AttachPrep> {
    let a = match attachments::get(conn, entity_id) {
        Ok(Some(a)) => a,
        Ok(None) => return Prep::Done(Outcome::Success),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    if a.upload_state != UploadState::Pending {
        return Prep::Done(Outcome::Success);
    }
    let Some(signed_id) = a.signed_id.clone() else {
        return Prep::Done(Outcome::LeavePending("upload not finished yet".into()));
    };
    let owner_remote = match owner_remote_id(conn, &a) {
        Ok(r) => r,
        Err(o) => return Prep::Done(o),
    };
    use attachments_api::AttachmentOwner;
    let owner = match a.owner_kind {
        attachments::OwnerKind::Issue => AttachmentOwner::Issue { issue_id: owner_remote },
        attachments::OwnerKind::Project => AttachmentOwner::Project { project_id: owner_remote },
        // Comment files ride on the comment's own create (JRST-206).
        attachments::OwnerKind::Comment => {
            return Prep::Done(Outcome::LeavePending(
                "attaching to a comment is not supported yet".into(),
            ))
        }
    };
    let team_local_id = attachments::owner_ref(conn, &a).ok().flatten().map(|o| o.team_id);
    let team_remote_id = match team_local_id
        .and_then(|t| queries::remote_id_for(conn, "teams", &t).ok().flatten())
    {
        Some(t) => t,
        None => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
    };
    Prep::Ready(AttachPrep {
        attachment_id: a.id,
        team_remote_id,
        owner,
        signed_id,
        outbox_path: a.outbox_path,
    })
}

/// How an upload/attach error is treated.
#[derive(Debug, PartialEq, Eq)]
enum UploadFailure {
    /// Will not succeed unchanged: the row becomes `failed` with this reason.
    Permanent(String),
    /// Offline, timeout, 5xx, auth: keep the entry pending and retry.
    Temporary(String),
}

/// Pure split of upload errors. Permanent: 422 (`Validation`), a 403 from the
/// API (`Forbidden`) and a 404 (the owner is gone), plus other 4xx from the
/// storage target. Temporary: network (offline/timeout), 5xx, 401/missing token,
/// and a 403 from the PUT (`Server { 403 }`, an expired target: JRST-201
/// restarts the direct upload, which every retry does).
fn classify_upload_error(err: &ApiError) -> UploadFailure {
    use UploadFailure::{Permanent, Temporary};
    match err {
        ApiError::Validation { errors } => Permanent(format!("rejected: {}", errors.join("; "))),
        ApiError::Forbidden(m) => Permanent(format!("not allowed: {m}")),
        ApiError::NotFound(m) => Permanent(format!("not found: {m}")),
        ApiError::Server { status, message } if matches!(status, 403 | 408 | 429) => {
            Temporary(format!("server {status}: {message}"))
        }
        ApiError::Server { status, message } if (400..500).contains(status) => {
            Permanent(format!("server {status}: {message}"))
        }
        ApiError::Server { status, message } => Temporary(format!("server {status}: {message}")),
        other => Temporary(other.to_string()),
    }
}

/// Outcome for an upload/attach error. A permanent one marks the row `failed`
/// and consumes the entry (Retry requeues it); the user's file is kept.
fn upload_outcome(entity_id: &str, err: &ApiError) -> Outcome {
    match classify_upload_error(err) {
        UploadFailure::Temporary(reason) => Outcome::LeavePending(reason),
        UploadFailure::Permanent(reason) => {
            match open().and_then(|c| {
                attachments::mark_failed(&c, entity_id, &reason).map_err(|e| format!("db: {e}"))
            }) {
                Ok(()) => Outcome::DropAndLog(reason),
                Err(e) => Outcome::LeavePending(e),
            }
        }
    }
}

/// Outcome for an error from the PUT to the upload target. Kept apart from
/// [`upload_outcome`] (the direct-upload-create step) because the storage host
/// answers with its own statuses: a 403 there means the signed target expired,
/// never that the file is refused, so it must never become a permanent failure.
/// The target isn't stored, so every retry starts a fresh direct upload.
fn put_outcome(err: &ApiError) -> Outcome {
    Outcome::LeavePending(format!("upload target failed, will restart the upload: {err:?}"))
}

pub(super) async fn upload_create(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match open() {
        Ok(conn) => match prep_upload_create_in(&conn, entity_id) {
            Prep::Ready(p) => p,
            Prep::Done(o) => return o,
        },
        Err(e) => return Outcome::LeavePending(e),
    };
    let bytes = match std::fs::read(&prep.outbox_path) {
        Ok(b) => b,
        Err(e) => return Outcome::LeavePending(format!("outbox copy unreadable: {e}")),
    };
    let direct = match attachments_api::create_direct_upload(client, &prep.blob).await {
        Ok(d) => d,
        Err(e) => return upload_outcome(entity_id, &e),
    };
    if let Err(e) = attachments_api::put_bytes(client, &direct.direct_upload, bytes).await {
        return put_outcome(&e);
    }
    let blob_filename = direct.filename.unwrap_or(prep.blob.filename);
    let saved = open().and_then(|c| {
        attachments::set_uploaded(&c, &prep.attachment_id, &direct.signed_id, &blob_filename)
            .map_err(|e| format!("db: {e}"))
    });
    match saved {
        Ok(()) => Outcome::Success,
        Err(e) => Outcome::LeavePending(e),
    }
}

pub(super) async fn attachment_create(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match open() {
        Ok(conn) => match prep_attachment_create_in(&conn, entity_id) {
            Prep::Ready(p) => p,
            Prep::Done(o) => return o,
        },
        Err(e) => return Outcome::LeavePending(e),
    };
    let ids = std::slice::from_ref(&prep.signed_id);
    let result = match prep.owner {
        attachments_api::AttachmentOwner::Project { project_id } => {
            attachments_api::attach_to_project(client, prep.team_remote_id, project_id, ids).await
        }
        attachments_api::AttachmentOwner::Issue { issue_id } => {
            attachments_api::attach_to_issue(client, prep.team_remote_id, issue_id, ids).await
        }
        _ => return Outcome::LeavePending("unsupported attachment owner".into()),
    };
    match result {
        Ok(rows) => {
            let Some(dto) = rows.into_iter().next() else {
                return Outcome::LeavePending("attach returned no attachment".into());
            };
            let saved = open().and_then(|c| {
                attachments::mark_attached(&c, &prep.attachment_id, &dto)
                    .map_err(|e| format!("db: {e}"))
            });
            match saved {
                Ok(()) => {
                    if let Some(p) = prep.outbox_path {
                        let _ = crate::files::outbox::remove(std::path::Path::new(&p));
                    }
                    Outcome::Success
                }
                Err(e) => Outcome::LeavePending(e),
            }
        }
        // Team access lost keeps the entry; anything else goes through the
        // permanent/temporary split.
        Err(e) => match classify_in_team(&e, prep.team_remote_id) {
            o @ Outcome::TeamAccessLost(..) => o,
            _ => upload_outcome(entity_id, &e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sync_queue;
    use crate::sync::push::fixtures::{PH, project_comment_fixture, staged_comment_file};
    use crate::sync::push::reconcile::save_remote_id_and_mark_synced;
    use crate::sync::test_support::{setup_dependency_db, setup_labels_db};

    #[test]
    fn body_waits_until_every_placeholder_resolved() {
        let body = format!("a ![image](jait-pending://{PH}) b");
        let none = InlineImages::new();
        assert!(matches!(rewrite_body_or_wait(&body, &none), Err(Outcome::LeavePending(_))));
        let mut ok = InlineImages::new();
        ok.insert(PH.into(), ("sig".into(), "my pic.png".into()));
        assert_eq!(
            rewrite_body_or_wait(&body, &ok).ok().unwrap(),
            "a ![image](/rails/active_storage/blobs/redirect/sig/my%20pic.png) b"
        );
        assert_eq!(rewrite_body_or_wait("plain", &none).ok().unwrap(), "plain");
    }

    #[test]
    fn prepare_body_rewrites_project_inline_upload() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        conn.execute(
            "INSERT INTO attachments (id, owner_kind, owner_id, filename, created_at, upload_state, upload_kind, placeholder_uuid, signed_id, blob_filename)
             VALUES ('a','project','p1','p.png','2026-02-01T00:00:00.000Z','pending','inline_image',?1,'sig','p.png')",
            [PH],
        )
        .unwrap();
        let body = Some(format!("x ![image](jait-pending://{PH})"));
        let (sent, map) = prepare_body(&conn, attachments::OwnerKind::Project, "p1", body.clone())
            .ok()
            .unwrap();
        assert_eq!(
            sent.unwrap(),
            "x ![image](/rails/active_storage/blobs/redirect/sig/p.png)"
        );
        assert_eq!(map.len(), 1);
        // Another owner's body still waits: the row belongs to the project.
        assert!(prepare_body(&conn, attachments::OwnerKind::Issue, "p1", body).is_err());
    }

    #[test]
    fn prepare_body_blocks_on_failed_inline_upload() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        conn.execute(
            "INSERT INTO attachments (id, owner_kind, owner_id, filename, created_at, upload_state, upload_kind, placeholder_uuid, signed_id, blob_filename)
             VALUES ('a','issue','i1','p.png','2026-02-01T00:00:00.000Z','failed','inline_image',?1,'sig','p.png')",
            [PH],
        )
        .unwrap();
        let body = Some(format!("![image](jait-pending://{PH})"));
        let r = prepare_body(&conn, attachments::OwnerKind::Issue, "i1", body);
        assert!(matches!(r, Err(Outcome::LeavePending(_))));
    }

    #[test]
    fn attachment_delete_payload_parses_for_an_issue_owner() {
        let p = r#"{"owner_kind":"issue","team_remote_id":3,"owner_remote_id":9,"remote_id":12,"restore":{}}"#;
        match prep_attachment_delete(Some(p)) {
            Prep::Ready(r) => assert_eq!(
                r,
                AttachmentDeletePrep {
                    team_remote_id: 3,
                    owner: attachments_api::AttachmentOwner::Issue { issue_id: 9 },
                    remote_id: 12
                }
            ),
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
    }

    #[test]
    fn attachment_delete_payload_parses_for_project_and_comment_owners() {
        let ready = |p: &str| match prep_attachment_delete(Some(p)) {
            Prep::Ready(r) => r.owner,
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        };
        assert_eq!(
            ready(r#"{"owner_kind":"project","team_remote_id":3,"owner_remote_id":4,"remote_id":12}"#),
            attachments_api::AttachmentOwner::Project { project_id: 4 }
        );
        assert_eq!(
            ready(r#"{"owner_kind":"comment","parent_kind":"issue","parent_remote_id":9,"team_remote_id":3,"owner_remote_id":5,"remote_id":12}"#),
            attachments_api::AttachmentOwner::IssueComment { issue_id: 9, comment_id: 5 }
        );
        assert_eq!(
            ready(r#"{"owner_kind":"comment","parent_kind":"project","parent_remote_id":4,"team_remote_id":3,"owner_remote_id":5,"remote_id":12}"#),
            attachments_api::AttachmentOwner::ProjectComment { project_id: 4, comment_id: 5 }
        );
        // A comment payload without its parent cannot be addressed.
        let no_parent = r#"{"owner_kind":"comment","team_remote_id":3,"owner_remote_id":5,"remote_id":12}"#;
        assert!(matches!(prep_attachment_delete(Some(no_parent)), Prep::Done(Outcome::DropAndLog(_))));
    }

    #[test]
    fn attachment_delete_bad_payloads_are_dropped() {
        let bad = r#"{"owner_kind":"x","owner_remote_id":1,"team_remote_id":1,"remote_id":1}"#;
        for p in [None, Some("nope"), Some(r#"{"owner_kind":"issue"}"#), Some(bad)] {
            assert!(matches!(prep_attachment_delete(p), Prep::Done(Outcome::DropAndLog(_))));
        }
    }

    // ── Attachment uploads (JRST-197) ──

    fn pending_upload(conn: &Connection, issue: &str) -> Attachment {
        attachments::add_pending(
            conn,
            &attachments::NewPending {
                owner_kind: crate::db::attachments::OwnerKind::Issue,
                owner_id: issue.into(),
                filename: "spec.pdf".into(),
                content_type: "application/pdf".into(),
                byte_size: 12,
                checksum: "abc==".into(),
                outbox_path: "/tmp/outbox/x".into(),
                kind: crate::files::validate::UploadKind::Attachment,
                placeholder_uuid: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn upload_waits_for_an_owner_with_no_remote_id() {
        let conn = setup_labels_db(); // i1 has no remote id
        let a = pending_upload(&conn, "i1");
        assert!(matches!(
            prep_upload_create_in(&conn, &a.id),
            Prep::Done(Outcome::LeavePending(_))
        ));
        assert!(matches!(
            prep_attachment_create_in(&conn, &a.id),
            Prep::Done(Outcome::LeavePending(_))
        ));
    }

    #[test]
    fn upload_is_ready_once_the_owner_is_synced_and_attach_waits_for_the_signed_id() {
        let conn = setup_dependency_db(); // i1 remote 1, team remote 700
        let a = pending_upload(&conn, "i1");
        match prep_upload_create_in(&conn, &a.id) {
            Prep::Ready(p) => {
                assert_eq!(p.blob.filename, "spec.pdf");
                assert_eq!(p.blob.checksum, "abc==");
            }
            _ => panic!("expected the upload to be ready"),
        }
        assert!(matches!(
            prep_attachment_create_in(&conn, &a.id),
            Prep::Done(Outcome::LeavePending(_))
        ));

        attachments::set_uploaded(&conn, &a.id, "signed", "spec.pdf").unwrap();
        // The upload is not redone, and the attach now has everything.
        assert!(matches!(prep_upload_create_in(&conn, &a.id), Prep::Done(Outcome::Success)));
        match prep_attachment_create_in(&conn, &a.id) {
            Prep::Ready(p) => {
                assert_eq!(
                    (p.team_remote_id, p.owner),
                    (700, attachments_api::AttachmentOwner::Issue { issue_id: 1 })
                );
                assert_eq!(p.signed_id, "signed");
            }
            _ => panic!("expected the attach to be ready"),
        }
    }

    #[test]
    fn a_project_attach_is_ready_with_its_project_remote_id() {
        let conn = setup_dependency_db(); // team t1 remote 700
        conn.execute(
            "INSERT INTO projects (id, remote_id, team_id, name, created_at, updated_at)
             VALUES ('p1', 50, 't1', 'P', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        let a = attachments::add_pending(
            &conn,
            &attachments::NewPending {
                owner_kind: crate::db::attachments::OwnerKind::Project,
                owner_id: "p1".into(),
                filename: "spec.pdf".into(),
                content_type: "application/pdf".into(),
                byte_size: 12,
                checksum: "abc==".into(),
                outbox_path: "/tmp/outbox/x".into(),
                kind: crate::files::validate::UploadKind::Attachment,
                placeholder_uuid: None,
            },
        )
        .unwrap();
        attachments::set_uploaded(&conn, &a.id, "signed", "spec.pdf").unwrap();
        match prep_attachment_create_in(&conn, &a.id) {
            Prep::Ready(p) => assert_eq!(
                (p.team_remote_id, p.owner),
                (700, attachments_api::AttachmentOwner::Project { project_id: 50 })
            ),
            _ => panic!("expected the project attach to be ready"),
        }
    }

    #[test]
    fn files_on_an_unsynced_issue_follow_its_create_in_one_drain() {
        let mut conn = setup_labels_db(); // i1 has no remote id
        sync_queue::enqueue(&conn, "issue", "i1", "create", None).unwrap();
        let a = pending_upload(&conn, "i1");
        let order: Vec<(String, String)> = sync_queue::pending_entries(&conn)
            .unwrap()
            .into_iter()
            .map(|e| (e.entity_type, e.action))
            .collect();
        let want = |t: &str, a: &str| (t.to_string(), a.to_string());
        assert_eq!(
            order,
            vec![want("issue", "create"), want("upload", "create"), want("attachment", "create")]
        );

        // Entries run in queue order against the live DB: waiting first...
        assert!(matches!(
            prep_upload_create_in(&conn, &a.id),
            Prep::Done(Outcome::LeavePending(_))
        ));
        // ...then the create lands the remote id and the same drain proceeds.
        let create_id = sync_queue::pending_entries(&conn).unwrap()[0].id.unwrap();
        save_remote_id_and_mark_synced(&mut conn, "issues", "i1", 55, create_id).unwrap();
        assert!(matches!(prep_upload_create_in(&conn, &a.id), Prep::Ready(_)));
        attachments::set_uploaded(&conn, &a.id, "signed", "spec.pdf").unwrap();
        assert!(matches!(prep_attachment_create_in(&conn, &a.id), Prep::Ready(_)));
    }

    #[test]
    fn a_removed_row_is_a_finished_entry() {
        let conn = setup_dependency_db();
        assert!(matches!(prep_upload_create_in(&conn, "gone"), Prep::Done(Outcome::Success)));
        assert!(matches!(prep_attachment_create_in(&conn, "gone"), Prep::Done(Outcome::Success)));
    }

    fn is_permanent(e: ApiError) -> bool {
        matches!(classify_upload_error(&e), UploadFailure::Permanent(_))
    }

    #[test]
    fn upload_422_403_and_404_are_permanent() {
        assert!(is_permanent(ApiError::Validation { errors: vec!["too big".into()] }));
        assert!(is_permanent(ApiError::Forbidden("no".into())));
        assert!(is_permanent(ApiError::NotFound("gone".into())));
        assert!(is_permanent(ApiError::Server { status: 400, message: "bad".into() }));
    }

    #[test]
    fn upload_5xx_auth_and_expired_put_target_are_temporary() {
        assert!(!is_permanent(ApiError::Server { status: 500, message: "x".into() }));
        assert!(!is_permanent(ApiError::Server { status: 503, message: "x".into() }));
        assert!(!is_permanent(ApiError::Server { status: 403, message: "expired".into() }));
        assert!(!is_permanent(ApiError::Server { status: 429, message: "slow".into() }));
        assert!(!is_permanent(ApiError::Unauthorized("x".into())));
        assert!(!is_permanent(ApiError::MissingToken));
    }

    #[tokio::test]
    async fn upload_offline_and_timeout_are_temporary() {
        // Offline: nothing listens on the port.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = closed.local_addr().unwrap();
        drop(closed);
        let offline = reqwest::get(format!("http://{addr}/")).await.unwrap_err();
        assert!(offline.is_connect());
        assert!(!is_permanent(ApiError::Network(offline)));
        // Timeout: the socket accepts but nobody answers.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", silent.local_addr().unwrap());
        let timeout = reqwest::Client::new()
            .get(url)
            .timeout(std::time::Duration::from_millis(50))
            .send()
            .await
            .unwrap_err();
        assert!(timeout.is_timeout());
        assert!(!is_permanent(ApiError::Network(timeout)));
    }

    #[test]
    fn a_failed_row_is_a_finished_entry_for_both_steps() {
        let conn = setup_dependency_db();
        let a = pending_upload(&conn, "i1");
        attachments::set_uploaded(&conn, &a.id, "signed", "spec.pdf").unwrap();
        attachments::mark_failed(&conn, &a.id, "nope").unwrap();
        assert!(matches!(prep_upload_create_in(&conn, &a.id), Prep::Done(Outcome::Success)));
        assert!(matches!(prep_attachment_create_in(&conn, &a.id), Prep::Done(Outcome::Success)));
    }

    #[test]
    fn comment_files_upload_before_the_comment_has_a_remote_id() {
        let conn = project_comment_fixture(Some(80));
        let a = staged_comment_file(&conn, "pc"); // comment "pc" is unsynced
        assert!(matches!(prep_upload_create_in(&conn, &a.id), Prep::Ready(_)));
        // Even before the comment row exists locally (staged in the composer).
        let b = staged_comment_file(&conn, "not-yet-sent");
        assert!(matches!(prep_upload_create_in(&conn, &b.id), Prep::Ready(_)));
    }

    #[test]
    fn a_put_step_error_is_always_temporary() {
        // An expired target (403 from the storage host) is not the user's
        // file being refused: the next retry restarts the direct upload.
        for e in [
            ApiError::Server { status: 403, message: "expired".into() },
            ApiError::Validation { errors: vec!["x".into()] },
            ApiError::NotFound("x".into()),
            ApiError::Server { status: 400, message: "bad".into() },
        ] {
            match put_outcome(&e) {
                Outcome::LeavePending(reason) => assert!(reason.contains("restart")),
                _ => panic!("PUT error must stay pending"),
            }
        }
    }
}
