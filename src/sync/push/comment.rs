//! Pushing comment creates and deletes.

use rusqlite::Connection;

use crate::api::{ApiClient, ApiError, CommentParams, CommentTarget, comments};
use crate::db::{attachments, queries};
use crate::db::attachments::{Attachment, UploadState};
use crate::db::models::CommentOwner;
use crate::sync::engine::open;
use super::attachment::{InlineImages, prepare_body, settle_inline};
use super::issue::move_order;
use super::outcome::{
    Outcome, Prep, classify_in_team, is_team_access_message, is_token_scope_message,
};
use super::reconcile::commit_create;

// ── Comments ──────────────────────────────────────────────────────────────

struct CommentCreatePrep {
    team_remote_id: i64,
    target: CommentTarget,
    params: CommentParams,
    local_id: String,
    inline: InlineImages,
}

fn prep_comment_create(entity_id: &str, entry_id: i64) -> Prep<CommentCreatePrep> {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Prep::Done(Outcome::LeavePending(e)),
    };
    prep_comment_create_in(&conn, entity_id, entry_id)
}

fn prep_comment_create_in(
    conn: &Connection,
    entity_id: &str,
    entry_id: i64,
) -> Prep<CommentCreatePrep> {
    let comment = match queries::get_comment(conn, entity_id) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "comment {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    if comment.remote_id.is_some() {
        return Prep::Done(Outcome::Success);
    }
    // The comment is nested under its issue or project; the owner and its team
    // must be synced before we can address the endpoint.
    let (target, team_local_id, source_team) = match comment.owner() {
        CommentOwner::Issue(id) => {
            let issue = match queries::get_issue(conn, &id) {
                Ok(Some(i)) => i,
                Ok(None) => {
                    return Prep::Done(Outcome::DropAndLog("comment's issue missing".into()))
                }
                Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
            };
            match issue.remote_id {
                Some(r) => match move_order(conn, &issue.id, entry_id) {
                    Ok(src) => (CommentTarget::Issue(r), issue.team_id, src),
                    Err(o) => return Prep::Done(o),
                },
                None => return Prep::Done(Outcome::LeavePending("issue not synced yet".into())),
            }
        }
        CommentOwner::Project(id) => {
            let project = match queries::get_project(conn, &id) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    return Prep::Done(Outcome::DropAndLog("comment's project missing".into()))
                }
                Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
            };
            match project.remote_id {
                Some(r) => (CommentTarget::Project(r), project.team_id, None),
                None => {
                    return Prep::Done(Outcome::LeavePending("project not synced yet".into()))
                }
            }
        }
    };
    // An issue with a pending move is addressed under its source team.
    let team_remote_id = match source_team {
        Some(id) => id,
        None => match queries::remote_id_for(conn, "teams", &team_local_id) {
            Ok(Some(id)) => id,
            Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
            Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
        },
    };
    let parent_remote_id = match resolve_comment_parent(conn, &comment) {
        Ok(p) => p,
        Err(o) => return Prep::Done(o),
    };
    let files = match comment_create_files(conn, &comment.id) {
        Ok(f) => f,
        Err(o) => return Prep::Done(o),
    };
    let (body, inline) =
        match prepare_body(conn, attachments::OwnerKind::Comment, &comment.id, Some(comment.body)) {
            Ok((b, i)) => (b.unwrap_or_default(), i),
            Err(o) => return Prep::Done(o),
        };
    Prep::Ready(CommentCreatePrep {
        team_remote_id,
        target,
        params: CommentParams { body, parent_id: parent_remote_id, files },
        local_id: comment.id,
        inline,
    })
}

/// Signed ids of the comment's staged files, in staging order. The create waits
/// (`LeavePending`) while any file's upload has no signed id yet.
fn comment_create_files(conn: &Connection, comment_id: &str) -> Result<Vec<String>, Outcome> {
    let files = attachments::list_for_owner(conn, attachments::OwnerKind::Comment, comment_id)
        .map_err(|e| Outcome::LeavePending(format!("db: {e}")))?;
    let mut ids = Vec::new();
    // Pasted inline images ride in the body (see `prepare_body`), not `files`.
    let staged = |f: &&Attachment| {
        f.upload_state != UploadState::Synced
            && f.upload_kind != attachments::UploadKind::InlineImage
    };
    for f in files.iter().filter(staged) {
        match &f.signed_id {
            Some(s) => ids.push(s.clone()),
            None => return Err(Outcome::LeavePending("comment files still uploading".into())),
        }
    }
    Ok(ids)
}

/// Remote id of a reply's parent. A parent that hasn't synced yet leaves the
/// reply pending (the queue is FIFO so the parent normally goes first).
struct CommentDeletePrep {
    team_remote_id: i64,
    issue_remote_id: i64,
    remote_id: i64,
}

/// Like the other deletes, relies entirely on the payload captured at enqueue
/// time (`{team_remote_id, issue_remote_id, remote_id}`): the local row is gone.
fn prep_comment_delete(payload: Option<&str>) -> Prep<CommentDeletePrep> {
    let Some(raw) = payload else {
        return Prep::Done(Outcome::DropAndLog(
            "comment delete: no payload (legacy entry)".into(),
        ));
    };
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => return Prep::Done(Outcome::DropAndLog(format!("comment delete: bad payload: {e}"))),
    };
    let field = |k: &str| parsed.get(k).and_then(|v| v.as_i64());
    match (field("team_remote_id"), field("issue_remote_id"), field("remote_id")) {
        (Some(team_remote_id), Some(issue_remote_id), Some(remote_id)) => {
            Prep::Ready(CommentDeletePrep {
                team_remote_id,
                issue_remote_id,
                remote_id,
            })
        }
        _ => Prep::Done(Outcome::DropAndLog(
            "comment delete: incomplete payload".into(),
        )),
    }
}

pub(super) async fn comment_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let prep = match prep_comment_delete(payload) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    let result =
        comments::delete(client, prep.team_remote_id, prep.issue_remote_id, prep.remote_id).await;
    comment_delete_outcome(result, prep.team_remote_id)
}

/// 204 and a resource 404 both mean the comment is gone. A 403 (not the token
/// scope one) is dropped with the server's message so the user sees a toast;
/// the next per-issue comment fetch upserts the comment back.
pub(super) fn comment_delete_outcome(result: Result<(), ApiError>, team_remote_id: i64) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(ApiError::NotFound(m)) if !is_team_access_message(&m) => Outcome::Success,
        Err(ApiError::Forbidden(m)) if !is_token_scope_message(&m) => {
            Outcome::DropAndLog(format!("forbidden: {m}"))
        }
        Err(e) => classify_in_team(&e, team_remote_id),
    }
}

fn resolve_comment_parent(
    conn: &rusqlite::Connection,
    comment: &crate::db::models::Comment,
) -> Result<Option<i64>, Outcome> {
    let Some(parent_local) = comment.parent_id.as_deref() else {
        return Ok(None);
    };
    match queries::get_comment(conn, parent_local) {
        Ok(Some(p)) => match p.remote_id {
            Some(id) => Ok(Some(id)),
            None => Err(Outcome::LeavePending(
                "parent comment not synced yet".into(),
            )),
        },
        Ok(None) => Err(Outcome::DropAndLog("parent comment missing locally".into())),
        Err(e) => Err(Outcome::LeavePending(format!("db: {e}"))),
    }
}

pub(super) async fn comment_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match prep_comment_create(entity_id, entry_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    // Send the local UUID as the idempotency key so a re-drain (e.g. after a
    // crash between POST and mark_synced) returns the existing comment instead
    // of creating a duplicate.
    match comments::create(
        client,
        prep.team_remote_id,
        prep.target,
        &prep.params,
        &prep.local_id,
    )
    .await
    {
        Ok(dto) => {
            if !prep.params.files.is_empty() {
                if let Ok(conn) = open() {
                    if let Ok(paths) =
                        attachments::finish_comment_files(&conn, &prep.local_id, dto.files.as_deref())
                    {
                        for p in paths {
                            let _ = crate::files::outbox::remove(std::path::Path::new(&p));
                        }
                    }
                }
            }
            let out = commit_create(entry_id, "comments", &prep.local_id, dto.id);
            settle_inline(attachments::OwnerKind::Comment, &prep.local_id, &prep.inline);
            out
        }
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::engine::{PushPause, SyncErrorKind};
    use crate::sync::push::fixtures::{PH, is_retry, project_comment_fixture, staged_comment_file};
    use crate::sync::test_support::{exec, setup_labels_db};

    fn reply_fixture(
        parent_remote: Option<i64>,
    ) -> (rusqlite::Connection, crate::db::models::Comment) {
        use crate::db::models::Comment;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        let mk = |id: &str, remote: Option<i64>, parent: Option<&str>| Comment {
            id: id.into(),
            remote_id: remote,
            issue_id: Some("i1".into()),
            project_id: None,
            author_id: None,
            body: id.into(),
            parent_id: parent.map(String::from),
            created_at: "x".into(),
            updated_at: "x".into(),
        };
        queries::insert_comment(&conn, &mk("p", parent_remote, None)).unwrap();
        let reply = mk("r", None, Some("p"));
        queries::insert_comment(&conn, &reply).unwrap();
        (conn, reply)
    }

    #[test]
    fn project_comment_create_posts_to_the_project_path_once_synced() {
        let conn = project_comment_fixture(Some(80));
        match prep_comment_create_in(&conn, "pc", i64::MAX) {
            Prep::Ready(p) => {
                assert_eq!(p.team_remote_id, 700);
                assert_eq!(p.target, CommentTarget::Project(80));
                assert_eq!(p.params.body, "hello");
                assert_eq!(p.params.parent_id, None);
            }
            Prep::Done(_) => panic!("expected Ready"),
        }
    }

    /// Adds a reply "pr" to the fixture's project comment "pc", whose remote id is
    /// `parent_remote`.
    fn add_project_reply(conn: &Connection, parent_remote: Option<i64>) {
        if let Some(r) = parent_remote {
            queries::set_comment_remote_id(conn, "pc", r).unwrap();
        }
        let reply = crate::db::models::Comment {
            id: "pr".into(),
            remote_id: None,
            issue_id: None,
            project_id: Some("p1".into()),
            author_id: None,
            body: "reply".into(),
            parent_id: Some("pc".into()),
            created_at: "x".into(),
            updated_at: "x".into(),
        };
        queries::insert_comment(conn, &reply).unwrap();
    }

    #[test]
    fn project_reply_waits_for_an_unsynced_parent() {
        let conn = project_comment_fixture(Some(80));
        add_project_reply(&conn, None);
        match prep_comment_create_in(&conn, "pr", i64::MAX) {
            Prep::Done(o) => assert!(is_retry(&o)),
            Prep::Ready(_) => panic!("expected the reply to wait"),
        }
    }

    #[test]
    fn project_reply_sends_the_parent_remote_id_to_the_project_path() {
        let conn = project_comment_fixture(Some(80));
        add_project_reply(&conn, Some(31));
        match prep_comment_create_in(&conn, "pr", i64::MAX) {
            Prep::Ready(p) => {
                assert_eq!(p.target, CommentTarget::Project(80));
                assert_eq!(p.params.parent_id, Some(31));
            }
            Prep::Done(_) => panic!("expected Ready"),
        }
    }

    #[test]
    fn project_comment_create_waits_for_the_project() {
        let conn = project_comment_fixture(None);
        match prep_comment_create_in(&conn, "pc", i64::MAX) {
            Prep::Done(o) => assert!(is_retry(&o)),
            Prep::Ready(_) => panic!("expected the create to wait"),
        }
    }

    #[test]
    fn project_comment_create_waits_for_the_team() {
        let conn = project_comment_fixture(Some(80));
        exec(&conn, "UPDATE teams SET remote_id = NULL WHERE id = 't1'");
        match prep_comment_create_in(&conn, "pc", i64::MAX) {
            Prep::Done(o) => assert!(is_retry(&o)),
            Prep::Ready(_) => panic!("expected the create to wait"),
        }
    }

    #[test]
    fn issue_comment_create_still_posts_to_the_issue_path() {
        let conn = setup_labels_db();
        exec(&conn, "UPDATE issues SET remote_id = 55 WHERE id = 'i1'");
        let c = crate::db::models::Comment {
            id: "ic".into(),
            remote_id: None,
            issue_id: Some("i1".into()),
            project_id: None,
            author_id: None,
            body: "hi".into(),
            parent_id: None,
            created_at: "x".into(),
            updated_at: "x".into(),
        };
        queries::insert_comment(&conn, &c).unwrap();
        match prep_comment_create_in(&conn, "ic", i64::MAX) {
            Prep::Ready(p) => assert_eq!(p.target, CommentTarget::Issue(55)),
            Prep::Done(_) => panic!("expected Ready"),
        }
    }

    #[test]
    fn reply_push_carries_parent_remote_id() {
        let (conn, reply) = reply_fixture(Some(77));
        assert_eq!(resolve_comment_parent(&conn, &reply).unwrap(), Some(77));
        let params = CommentParams {
            body: "r".into(),
            parent_id: Some(77),
            files: vec![],
        };
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["parent_id"], 77);
    }

    #[test]
    fn reply_waits_for_unsynced_parent() {
        let (conn, reply) = reply_fixture(None);
        assert!(is_retry(
            &resolve_comment_parent(&conn, &reply).unwrap_err()
        ));
    }

    #[test]
    fn comment_delete_prep_proceeds_only_on_complete_payload() {
        let full = r#"{"team_remote_id":7,"issue_remote_id":50,"remote_id":9}"#;
        match prep_comment_delete(Some(full)) {
            Prep::Ready(p) => assert_eq!((p.team_remote_id, p.issue_remote_id, p.remote_id), (7, 50, 9)),
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
        for bad in [None, Some("not json"), Some(r#"{"team_remote_id":7,"remote_id":9}"#)] {
            assert!(matches!(
                prep_comment_delete(bad),
                Prep::Done(Outcome::DropAndLog(_))
            ));
        }
    }

    #[test]
    fn comment_delete_204_and_resource_404_are_success() {
        assert!(matches!(comment_delete_outcome(Ok(()), 7), Outcome::Success));
        let gone = Err(ApiError::NotFound("Comment not found".into()));
        assert!(matches!(comment_delete_outcome(gone, 7), Outcome::Success));
    }

    #[test]
    fn comment_delete_403_drops_with_server_message() {
        let msg = "Only the author or a team admin can delete this";
        match comment_delete_outcome(Err(ApiError::Forbidden(msg.into())), 7) {
            Outcome::DropAndLog(r) => {
                assert_eq!(r, format!("forbidden: {msg}"));
                assert_eq!(SyncErrorKind::from_push_reason(&r), Some(SyncErrorKind::Forbidden));
            }
            other => panic!("expected drop, got {other:?}"),
        }
    }

    #[test]
    fn comment_delete_scope_403_and_team_404_keep_existing_handling() {
        let scope = Err(ApiError::Forbidden("Token lacks required scope".into()));
        assert!(matches!(
            comment_delete_outcome(scope, 7),
            Outcome::PausePushes(PushPause::Forbidden, _)
        ));
        let team = Err(ApiError::NotFound("Team not found or access denied".into()));
        assert!(matches!(
            comment_delete_outcome(team, 7),
            Outcome::TeamAccessLost(7, _)
        ));
    }

    #[test]
    fn top_level_comment_has_no_parent() {
        let (conn, _) = reply_fixture(Some(1));
        let top = queries::get_comment(&conn, "p").unwrap().unwrap();
        assert_eq!(resolve_comment_parent(&conn, &top).unwrap(), None);
    }

    #[test]
    fn comment_create_waits_for_its_files_then_sends_their_signed_ids() {
        let conn = project_comment_fixture(Some(80));
        let a = staged_comment_file(&conn, "pc");
        assert!(matches!(
            prep_comment_create_in(&conn, "pc", i64::MAX),
            Prep::Done(Outcome::LeavePending(_))
        ));
        attachments::set_uploaded(&conn, &a.id, "signed-1", "log.txt").unwrap();
        match prep_comment_create_in(&conn, "pc", i64::MAX) {
            Prep::Ready(p) => assert_eq!(p.params.files, vec!["signed-1".to_string()]),
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
    }

    fn pasted_inline_image(conn: &Connection, owner: &str, signed: Option<&str>) {
        conn.execute(
            "INSERT INTO attachments (id, owner_kind, owner_id, filename, created_at, upload_state, upload_kind, placeholder_uuid, signed_id, blob_filename)
             VALUES ('inl','comment',?1,'p.png','2026-02-01T00:00:00.000Z','pending','inline_image',?2,?3,'p.png')",
            rusqlite::params![owner, PH, signed],
        )
        .unwrap();
    }

    #[test]
    fn comment_create_waits_for_pasted_image_then_rewrites_body_without_attaching_it() {
        let conn = project_comment_fixture(Some(80));
        exec(&conn, &format!("UPDATE comments SET body = '![image](jait-pending://{PH})' WHERE id = 'pc'"));
        pasted_inline_image(&conn, "pc", None);
        assert!(matches!(
            prep_comment_create_in(&conn, "pc", i64::MAX),
            Prep::Done(Outcome::LeavePending(_))
        ));
        attachments::set_uploaded(&conn, "inl", "sig", "p.png").unwrap();
        match prep_comment_create_in(&conn, "pc", i64::MAX) {
            Prep::Ready(p) => {
                assert_eq!(p.params.body, "![image](/rails/active_storage/blobs/redirect/sig/p.png)");
                assert!(p.params.files.is_empty(), "inline images are not file attachments");
                assert!(p.inline.contains_key(PH));
            }
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
    }

    #[test]
    fn comment_without_files_sends_none() {
        let conn = project_comment_fixture(Some(80));
        match prep_comment_create_in(&conn, "pc", i64::MAX) {
            Prep::Ready(p) => assert!(p.params.files.is_empty()),
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
    }
}
