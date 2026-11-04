//! Push half of the sync engine: drains `sync_queue` to Rails.
//!
//! On each call to [`SyncEngine::drain`] we read every row where
//! `synced_at IS NULL` (already ordered by `created_at`), re-read the current
//! local state for the referenced row, dispatch to the matching
//! `crate::api::*` call, and either mark the entry synced or leave it pending
//! depending on the outcome. We re-read the local row instead of trusting
//! `entry.payload` because today's enqueue sites sometimes pass `None`, and a
//! fresh read is always authoritative anyway.
//!
//! One submodule per entity; this file owns the drain loop, dispatch and per-entry
//! logging.

mod attachment;
mod comment;
mod dashboard;
mod dependency;
mod home_page;
mod issue;
mod issue_move;
mod label;
mod notification;
mod outcome;
mod project;
mod reconcile;
mod recurring;
mod saved_view;
#[cfg(test)]
mod fixtures;

use crate::api::ApiClient;
use crate::db::{attachments, dashboards, queries, sync_queue};
use crate::db::models::SyncQueueEntry;
use crate::sync::engine::{
    CycleCtx, PushPause, SyncEngine, SyncErrorKind, SyncFailure, SyncReport, open,
    read_only_report, sync_log,
};
use crate::sync::push::attachment::{attachment_create, attachment_delete, upload_create};
use crate::sync::push::comment::{comment_create, comment_delete};
use crate::sync::push::dashboard::{
    dashboard_create, dashboard_delete, dashboard_group_create, dashboard_group_delete,
    dashboard_group_order, dashboard_group_update, dashboard_update,
};
use crate::sync::push::dependency::{dependency_create, dependency_delete};
use crate::sync::push::home_page::home_page_set;
use crate::sync::push::issue::{issue_create, issue_delete, issue_update};
use crate::sync::push::issue_move::issue_move;
use crate::sync::push::label::{label_create, label_delete, label_update};
use crate::sync::push::notification::{notification_read, notification_read_all};
use crate::sync::push::outcome::Outcome;
use crate::sync::push::project::{project_create, project_update};
use crate::sync::push::recurring::{
    recurring_issue_create, recurring_issue_delete, recurring_issue_update,
};
use crate::sync::push::saved_view::{
    is_legacy_rename_payload, saved_view_create, saved_view_delete, saved_view_update,
};

impl SyncEngine {
    /// Drain unless `/me` said the token cannot write; then skip every push
    /// (entries stay pending, one `parked` log line each) and report
    /// [`PushPause::ReadOnly`].
    pub async fn drain_if_writable(client: &ApiClient, ctx: &CycleCtx) -> SyncReport {
        if ctx.can_write() == Some(false) {
            park_all(read_pending().unwrap_or_default().iter(), READ_ONLY_REASON);
            return read_only_report();
        }
        Self::drain(client, ctx).await
    }

    pub async fn drain(client: &ApiClient, ctx: &CycleCtx) -> SyncReport {
        let mut report = SyncReport::default();

        let entries = match read_pending() {
            Ok(v) => v,
            Err(msg) => {
                sync_log("drain", "fatal", &msg);
                report.last_error = Some(msg);
                return report;
            }
        };

        for (i, entry) in entries.iter().enumerate() {
            let entry_id = match entry.id {
                Some(id) => id,
                None => continue,
            };

            if entry_team_is_inaccessible(entry) {
                sync_log(
                    &entry_scope(entry_id, entry),
                    "parked",
                    "team access lost; entry stays pending",
                );
                continue;
            }

            let outcome = process(client, ctx, entry, entry_id).await;
            if record(&mut report, entry, entry_id, outcome) {
                // Pushes are paused: everything after this entry is skipped
                // untouched, so say so for each of them.
                let reason = match &report.push_paused {
                    Some(PushPause::ReadOnly) => READ_ONLY_REASON,
                    _ => PAUSED_REASON,
                };
                park_all(entries[i + 1..].iter(), reason);
                break;
            }
        }

        report
    }
}

async fn process(
    client: &ApiClient,
    ctx: &CycleCtx,
    entry: &SyncQueueEntry,
    entry_id: i64,
) -> Outcome {
    match (entry.entity_type.as_str(), entry.action.as_str()) {
        ("issue", "create") => issue_create(client, ctx, &entry.entity_id, entry_id).await,
        ("issue", "update") => issue_update(client, &entry.entity_id, entry_id).await,
        ("issue", "delete") => {
            issue_delete(client, &entry.entity_id, entry.payload.as_deref(), entry_id).await
        }
        ("issue", "move") => issue_move(client, &entry.entity_id, entry.payload.as_deref()).await,
        ("project", "create") => project_create(client, &entry.entity_id, entry_id).await,
        ("project", "update") => project_update(client, &entry.entity_id).await,
        ("recurring_issue", "create") => {
            recurring_issue_create(
                client,
                ctx,
                &entry.entity_id,
                entry.payload.as_deref(),
                entry_id,
            )
            .await
        }
        ("recurring_issue", "update") => {
            recurring_issue_update(client, &entry.entity_id, entry.payload.as_deref()).await
        }
        ("recurring_issue", "delete") => {
            recurring_issue_delete(client, entry.payload.as_deref()).await
        }
        ("label", "create") => label_create(client, &entry.entity_id, entry_id).await,
        ("label", "update") => label_update(client, &entry.entity_id).await,
        ("label", "delete") => label_delete(client, entry.payload.as_deref()).await,
        (attachments::COMMENT_ENTITY, "create") => comment_create(client, &entry.entity_id, entry_id).await,
        (attachments::COMMENT_ENTITY, "delete") => comment_delete(client, entry.payload.as_deref()).await,
        (attachments::UPLOAD_ENTITY, "create") => upload_create(client, &entry.entity_id).await,
        (attachments::ATTACHMENT_ENTITY, "create") => attachment_create(client, &entry.entity_id).await,
        (attachments::ATTACHMENT_ENTITY, "delete") => attachment_delete(client, entry.payload.as_deref()).await,
        ("dependency", "create") => dependency_create(client, &entry.entity_id, entry_id).await,
        ("dependency", "delete") => dependency_delete(client, entry.payload.as_deref()).await,
        ("notification", "read") => notification_read(client, &entry.entity_id).await,
        ("notification", "read_all") => notification_read_all(client, entry.payload.as_deref()).await,
        ("home_page", "update") => home_page_set(client, entry.payload.as_deref()).await,
        ("saved_view", "create") => saved_view_create(client, &entry.entity_id, entry_id).await,
        ("saved_view", "update") => {
            let legacy_rename = is_legacy_rename_payload(entry.payload.as_deref());
            saved_view_update(client, &entry.entity_id, legacy_rename).await
        }
        ("saved_view", "rename") => saved_view_update(client, &entry.entity_id, true).await,
        ("saved_view", "delete") => saved_view_delete(client, entry.payload.as_deref()).await,
        (dashboards::ENTITY_TYPE, "create") => {
            dashboard_create(client, &entry.entity_id, entry_id).await
        }
        (dashboards::GROUP_ENTITY_TYPE, "create") => {
            dashboard_group_create(client, &entry.entity_id, entry_id).await
        }
        (dashboards::GROUP_ENTITY_TYPE, "update") => {
            dashboard_group_update(client, &entry.entity_id, entry_id).await
        }
        (dashboards::GROUP_ENTITY_TYPE, "delete") => {
            dashboard_group_delete(client, entry.payload.as_deref()).await
        }
        (dashboards::ENTITY_TYPE, "update") => dashboard_update(client, &entry.entity_id).await,
        (dashboards::ENTITY_TYPE, "delete") => {
            dashboard_delete(client, entry.payload.as_deref()).await
        }
        (dashboards::GROUP_ORDER_ENTITY_TYPE, "update") => {
            dashboard_group_order(client, &entry.entity_id, entry_id).await
        }
        (et, ac) => Outcome::DropAndLog(format!("unsupported ({et},{ac})")),
    }
}

const READ_ONLY_REASON: &str = "read-only token; push skipped";

const PAUSED_REASON: &str = "pushes paused earlier in this drain";

fn entry_scope(entry_id: i64, entry: &SyncQueueEntry) -> String {
    format!(
        "entry_id={entry_id} type={} action={}",
        entry.entity_type, entry.action
    )
}

/// Outcome label and reason for an entry's log line.
fn describe(outcome: &Outcome) -> (&'static str, &str) {
    match outcome {
        Outcome::Success | Outcome::Created => ("synced", "ok"),
        Outcome::DropAndLog(r) => ("dropped", r),
        Outcome::LeavePending(r) => ("pending", r),
        Outcome::PausePushes(_, r) | Outcome::TeamAccessLost(_, r) => ("parked", r),
    }
}

/// One `parked` line per entry that is skipped without being attempted.
fn park_all<'a>(entries: impl Iterator<Item = &'a SyncQueueEntry>, reason: &str) {
    for entry in entries {
        if let Some(id) = entry.id {
            sync_log(&entry_scope(id, entry), "parked", reason);
        }
    }
}

/// Apply `outcome` to the report/queue and log exactly one line for the entry.
/// Returns true when the drain should stop pushing (remaining entries are
/// left untouched, hence still pending).
fn record(
    report: &mut SyncReport,
    entry: &SyncQueueEntry,
    entry_id: i64,
    outcome: Outcome,
) -> bool {
    let scope = entry_scope(entry_id, entry);
    let (label, reason) = describe(&outcome);
    match outcome {
        Outcome::Success => {
            match mark_synced(entry_id) {
                Ok(()) => sync_log(&scope, label, reason),
                Err(e) => sync_log(&scope, "pending", &format!("mark_synced failed: {e}")),
            }
            report.processed += 1;
        }
        Outcome::Created => {
            sync_log(&scope, label, reason);
            report.processed += 1;
        }
        Outcome::PausePushes(pause, reason) => {
            sync_log(&scope, "parked", &reason);
            report.failed += 1;
            record_push_failure(report, entry, &reason);
            report.last_error = Some(reason);
            report.push_paused = Some(pause);
            return true;
        }
        Outcome::TeamAccessLost(team_remote_id, reason) => {
            sync_log(&scope, "parked", &reason);
            if let Ok(conn) = open() {
                let _ = queries::set_team_access_lost(&conn, team_remote_id, true);
            }
            report.failed += 1;
            record_push_failure(report, entry, &reason);
            report.last_error = Some(reason);
        }
        Outcome::DropAndLog(reason) => {
            sync_log(&scope, "dropped", &reason);
            let _ = discard_entry(entry_id);
            report.failed += 1;
            record_push_failure(report, entry, &reason);
            report.last_error = Some(reason);
        }
        Outcome::LeavePending(reason) => {
            sync_log(&scope, "pending", &reason);
            report.failed += 1;
            record_push_failure(report, entry, &reason);
            report.last_error = Some(reason);
        }
    }
    false
}

/// Push a structured [`SyncFailure`] onto the report when `reason` carries a
/// user-actionable API error (auth/validation/forbidden/server). No-op for
/// local/transient reasons.
fn record_push_failure(report: &mut SyncReport, entry: &SyncQueueEntry, reason: &str) {
    if let Some(kind) = SyncErrorKind::from_push_reason(reason) {
        // A refused move is reported apart from issue edits: a 422 on an edit
        // reopens the form, a refused move only toasts.
        let entity_type = match (entry.entity_type.as_str(), entry.action.as_str()) {
            ("issue", "move") => "issue_move".to_string(),
            _ => entry.entity_type.clone(),
        };
        report.failures.push(SyncFailure {
            entity_type,
            entity_id: entry.entity_id.clone(),
            kind,
            message: reason.to_string(),
        });
    }
}

fn read_pending() -> Result<Vec<SyncQueueEntry>, String> {
    let conn = open()?;
    sync_queue::pending_entries(&conn).map_err(|e| format!("read sync_queue: {e}"))
}

fn discard_entry(entry_id: i64) -> Result<(), String> {
    let conn = open()?;
    sync_queue::discard(&conn, entry_id).map_err(|e| format!("discard: {e}"))
}

fn mark_synced(entry_id: i64) -> Result<(), String> {
    let conn = open()?;
    sync_queue::mark_synced(&conn, entry_id).map_err(|e| format!("mark_synced: {e}"))
}

/// True when the entry belongs to a team currently flagged inaccessible.
fn entry_team_is_inaccessible(entry: &SyncQueueEntry) -> bool {
    let Ok(conn) = open() else { return false };
    match sync_queue::entry_team_remote_id(&conn, entry) {
        Some(rid) => queries::team_access_lost(&conn, rid).unwrap_or(false),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiError;
    use crate::sync::engine::{log_line, phase_scope};
    use crate::sync::push::outcome::classify;
    use crate::sync::test_support::{ctx_with_scopes, setup_labels_db};

    #[test]
    fn every_outcome_logs_one_line_naming_outcome_and_reason() {
        let entry = SyncQueueEntry {
            id: Some(4),
            entity_type: "issue".into(),
            entity_id: "i1".into(),
            action: "update".into(),
            payload: None,
            created_at: String::new(),
            synced_at: None,
        };
        let scope = entry_scope(4, &entry);
        let cases = [
            (Outcome::Success, "synced"),
            (Outcome::Created, "synced"),
            (Outcome::DropAndLog("bad".into()), "dropped"),
            (Outcome::LeavePending("later".into()), "pending"),
            (
                Outcome::PausePushes(PushPause::Forbidden, "no scope".into()),
                "parked",
            ),
            (Outcome::TeamAccessLost(3, "gone".into()), "parked"),
        ];
        for (outcome, label) in cases {
            let (l, reason) = describe(&outcome);
            assert_eq!(l, label);
            let line = log_line(&scope, l, reason);
            assert!(line.starts_with("[sync] entry_id=4 type=issue action=update outcome="));
            assert!(line.contains(&format!("outcome={label} reason=\"")));
            assert_eq!(line.lines().count(), 1);
        }
        assert_eq!(
            log_line(&phase_scope("pull_issues", Some(3)), "ok", "rows=2"),
            "[sync] phase=pull_issues team=3 outcome=ok reason=\"rows=2\""
        );
    }

    #[test]
    fn record_pause_stops_drain_and_leaves_entry_pending() {
        let entry = SyncQueueEntry {
            id: Some(1),
            entity_type: "issue".into(),
            entity_id: "i1".into(),
            action: "update".into(),
            payload: None,
            created_at: String::new(),
            synced_at: None,
        };
        let mut report = SyncReport::default();
        let outcome = classify(&ApiError::Forbidden("Token lacks required scope".into()));
        assert!(record(&mut report, &entry, 1, outcome));
        assert_eq!(report.push_paused, Some(PushPause::Forbidden));
        assert_eq!(report.processed, 0);
        assert!(report
            .failures
            .iter()
            .any(|f| f.kind == SyncErrorKind::Forbidden));
    }

    #[test]
    fn team_access_failure_surfaces_as_team_access_kind() {
        let entry = SyncQueueEntry {
            id: Some(1),
            entity_type: "issue".into(),
            entity_id: "i1".into(),
            action: "update".into(),
            payload: None,
            created_at: String::new(),
            synced_at: None,
        };
        let mut report = SyncReport::default();
        record_push_failure(
            &mut report,
            &entry,
            "team access lost: Team not found or access denied",
        );
        assert_eq!(report.failures[0].kind, SyncErrorKind::TeamAccess);
    }

    #[test]
    fn entries_resolve_to_their_team_and_flag_round_trips() {
        let conn = setup_labels_db();
        let mk =|et: &str, id: &str, action: &str, payload: Option<&str>| SyncQueueEntry {
            id: Some(1),
            entity_type: et.into(),
            entity_id: id.into(),
            action: action.into(),
            payload: payload.map(str::to_string),
            created_at: String::new(),
            synced_at: None,
        };
        assert_eq!(
            sync_queue::entry_team_remote_id(&conn, &mk("issue", "i1", "update", None)),
            Some(700)
        );
        assert_eq!(
            sync_queue::entry_team_remote_id(
                &conn,
                &mk("issue", "i1", "delete", Some("{\"team_remote_id\":700}"))
            ),
            Some(700)
        );

        assert!(!queries::team_access_lost(&conn, 700).unwrap());
        queries::set_team_access_lost(&conn, 700, true).unwrap();
        assert!(queries::team_access_lost(&conn, 700).unwrap());
        assert_eq!(
            queries::inaccessible_team_identifiers(&conn).unwrap(),
            vec!["T"]
        );
        // Visible again: flag clears.
        queries::set_team_access_lost(&conn, 700, false).unwrap();
        assert!(!queries::team_access_lost(&conn, 700).unwrap());
        assert!(queries::inaccessible_team_identifiers(&conn)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn process_unknown_routes_to_drop() {
        // Crafting a SyncQueueEntry with a bogus (entity_type, action) — process
        // should classify as DropAndLog without touching the DB or hitting the
        // network. tokio::test would require enabling the `macros` feature, so
        // use a one-shot runtime to keep deps flat.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let client = ApiClient::new("http://localhost:1", "stub-token").unwrap();
            let entry = SyncQueueEntry {
                id: Some(1),
                entity_type: "ufo".into(),
                entity_id: "x".into(),
                action: "abduct".into(),
                payload: None,
                created_at: "0".into(),
                synced_at: None,
            };
            let outcome = process(&client, &ctx_with_scopes(&["write"]), &entry, 1).await;
            assert!(matches!(outcome, Outcome::DropAndLog(_)));
        });
    }

    // ── Pushing an issue move (JRST-71) ──

    #[test]
    fn move_rejection_is_reported_apart_from_issue_edit_rejections() {
        // A plain issue 422 reopens the edit form; a move 422 must not.
        let entry = SyncQueueEntry {
            id: Some(1),
            entity_type: "issue".into(),
            entity_id: "i2".into(),
            action: "move".into(),
            payload: None,
            created_at: String::new(),
            synced_at: None,
        };
        let mut report = SyncReport::default();
        record_push_failure(&mut report, &entry, "validation: Issue is already in that team");
        assert_eq!(report.failures[0].entity_type, "issue_move");
        assert_eq!(report.failures[0].entity_id, "i2");
    }
}
