//! Sync engine — drains `sync_queue` by pushing pending entries to Rails.
//!
//! On each call to [`SyncEngine::drain`] we read every row where
//! `synced_at IS NULL` (already ordered by `created_at`), re-read the current
//! local state for the referenced row, dispatch to the matching
//! `crate::api::*` call, and either mark the entry synced or leave it pending
//! depending on the outcome. We re-read the local row instead of trusting
//! `entry.payload` because today's enqueue sites sometimes pass `None`, and a
//! fresh read is always authoritative anyway.
//!
//! ## Send + async constraint
//!
//! `rusqlite::Connection` is `!Sync`, so it cannot be held across an `.await`
//! point in a `Send` future (which iced's `Task::perform` requires). We
//! therefore never pass a `&Connection` into an async function. All DB work
//! happens in short synchronous helpers that open and drop their own
//! connections; async dispatchers only hold owned data across awaits.

use std::collections::HashSet;

use rusqlite::Connection;

use crate::api::deleted::{self, TombstoneDto};
use crate::api::dto::{CommentDto, IssueDto, LabelDto, PulledDependency, ProjectDto};
use crate::api::recurring_issues::RecurringIssueDto;
use crate::api::{
    comments, dependencies, issues, labels, lanes, members, projects, recurring_issues, teams,
    users, ApiClient, ApiError, CommentParams, CommentTarget, Conditional, Direction, IssueParams,
    LabelParams, MeDto, ProjectParams, RecurringIssueParams,
};
use crate::db::models::{CommentOwner, Issue, Priority, Project, SyncQueueEntry};
use crate::db::{connection, queries, sync_queue};
use crate::sync::cursor;

#[derive(Debug, Default, Clone)]
pub struct SyncReport {
    pub processed: usize,
    pub failed: usize,
    pub last_error: Option<String>,
    /// User-actionable API failures encountered this sync (auth/validation/etc.),
    /// carrying enough structure for the UI to route them (reprompt, reopen the
    /// offending form, …). Local/transient failures (db errors, ordering waits)
    /// are intentionally omitted — they only populate `last_error`.
    pub failures: Vec<SyncFailure>,
    /// Set when a push outcome asked to stop pushing for the rest of this
    /// drain (e.g. a 403). The offending entry and every unattempted entry stay
    /// pending; pulls are unaffected. The UI can read this to show a banner.
    pub push_paused: Option<PushPause>,
}

/// Why pushes were paused for the rest of a drain. Extend with new variants
/// (e.g. lost team access) rather than adding new outcomes.
#[derive(Debug, Clone, PartialEq)]
pub enum PushPause {
    /// 403 — the token lacks the scope for writes.
    Forbidden,
    /// `/me` reported no `write` scope, so no push was attempted at all.
    ReadOnly,
}

/// A classified, user-actionable sync failure surfaced back to the UI layer.
#[derive(Debug, Clone)]
pub struct SyncFailure {
    /// Entity type of the offending queue entry (`"issue"`, `"project"`, …),
    /// or `"auth"` for a pull-side authentication failure not tied to an entity.
    pub entity_type: String,
    pub entity_id: String,
    pub kind: SyncErrorKind,
    pub message: String,
}

/// Coarse classification the UI routes on.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncErrorKind {
    /// 401 — expired/revoked/missing token. Triggers the re-auth prompt.
    Auth,
    /// 403 — token lacks the required scope.
    Forbidden,
    /// 422 — server-side validation errors (full messages).
    Validation(Vec<String>),
    /// 404 "Team not found or access denied" — the team was archived or removed
    /// from the token. The edit stays pending; the status bar shows the team.
    TeamAccess,
    /// Transient connectivity failure — surfaced in the status bar, not a toast.
    Network,
    /// Any other server-side error.
    Other,
}

impl SyncErrorKind {
    /// Classify a push failure `reason` string (as produced by [`classify`])
    /// into a structured kind. Returns `None` for local/transient reasons
    /// (db errors, ordering waits) that shouldn't surface as API failures.
    fn from_push_reason(reason: &str) -> Option<SyncErrorKind> {
        if reason.starts_with("unauthorized:") {
            Some(SyncErrorKind::Auth)
        } else if reason.starts_with("team access lost:") {
            Some(SyncErrorKind::TeamAccess)
        } else if reason.starts_with("forbidden:") {
            Some(SyncErrorKind::Forbidden)
        } else if let Some(rest) = reason.strip_prefix("validation: ") {
            Some(SyncErrorKind::Validation(
                rest.split("; ").map(str::to_string).collect(),
            ))
        } else if reason.starts_with("network:") {
            Some(SyncErrorKind::Network)
        } else if reason.starts_with("server ")
            || reason.starts_with("not found:")
            || reason.starts_with("decode:")
            || reason.starts_with("enum:")
        {
            Some(SyncErrorKind::Other)
        } else {
            None
        }
    }
}

/// Combined report for one full bidirectional sync (push then pull).
#[derive(Debug, Default, Clone)]
pub struct FullSyncReport {
    pub pull: SyncReport,
    pub push: SyncReport,
    /// Local UUID of the orphan seed team that was cleaned up during this
    /// pull, if any. The app uses this to repoint `selected_team_id` so the
    /// board doesn't render against a row that no longer exists.
    pub deleted_orphan_team: Option<String>,
    /// Signed-in user's `(rails_id, name)` from the pull's `/me` call. The app
    /// persists this to config for stamping `creator_id` on new issues.
    pub current_user: Option<(i64, String)>,
    /// Write scope reported by this sync's `/me` call (`None` if `/me` failed).
    pub can_write: Option<bool>,
    /// True for a push-only cycle (local-write trigger): nothing was pulled,
    /// so the app must not treat it as a completed pull.
    pub push_only: bool,
}

impl FullSyncReport {
    /// True when the token is read-only: `/me` showed no write scope, or a
    /// push hit a 403. Drives the status-bar banner.
    pub fn read_only(&self) -> bool {
        self.can_write == Some(false)
            || matches!(
                self.push.push_paused,
                Some(PushPause::ReadOnly | PushPause::Forbidden)
            )
    }

    /// The most actionable error to surface to the user. Push errors take
    /// precedence (they're active operations the user just initiated); pull
    /// errors are reported only when push succeeded.
    pub fn last_error(&self) -> Option<&str> {
        self.push
            .last_error
            .as_deref()
            .or(self.pull.last_error.as_deref())
    }

    /// All structured failures from both sides of the sync.
    pub fn failures(&self) -> impl Iterator<Item = &SyncFailure> {
        self.pull.failures.iter().chain(self.push.failures.iter())
    }

    /// Whether the server looked healthy this cycle, for the scheduler's
    /// backoff. Network/server errors and unclassified pull errors count as
    /// failures; per-entry rejections (422/403) and 401 (which pauses sync
    /// instead) do not.
    pub fn cycle_succeeded(&self) -> bool {
        let unhealthy = self
            .failures()
            .any(|f| matches!(f.kind, SyncErrorKind::Network | SyncErrorKind::Other));
        let unclassified = self.pull.last_error.is_some() && self.pull.failures.is_empty();
        !unhealthy && !unclassified && !self.auth_failed()
    }

    /// True if either side hit a 401 — the trigger for the re-auth prompt.
    pub fn auth_failed(&self) -> bool {
        self.failures().any(|f| f.kind == SyncErrorKind::Auth)
    }
}

#[derive(Debug)]
enum Outcome {
    /// Entry pushed successfully, or didn't need pushing.
    Success,
    /// Create pushed; the remote id and the synced mark were already saved in
    /// one transaction, so `record` must not mark the entry again.
    Created,
    /// Permanent failure for this entry — mark synced so we don't retry forever.
    DropAndLog(String),
    /// Transient failure — leave pending so the next drain retries.
    LeavePending(String),
    /// Keep this entry pending AND skip the remaining pushes this drain.
    PausePushes(PushPause, String),
    /// Team-access 404: keep the entry pending, flag the team (by Rails id) as
    /// inaccessible, and carry on with other teams' entries.
    TeamAccessLost(i64, String),
}

/// `Done` short-circuits the API call (e.g. idempotency hit, missing FK).
/// `Ready` carries the prepared payload for the API call.
enum Prep<T> {
    Ready(T),
    Done(Outcome),
}

pub struct SyncEngine;

impl SyncEngine {
    pub fn new() -> Self {
        Self
    }

    /// One full Data sync cycle: drain the push queue first, then pull, so the
    /// pull reflects the user's own edits. One `/me` call opens the cycle; its
    /// result ([`CycleCtx`]) feeds the write-scope gate, the push half, the
    /// reconciliation lookups and the pull. A read-only token skips the push
    /// half (entries stay pending, [`PushPause::ReadOnly`]); the pull still
    /// runs. A 401 on the push half pauses the whole cycle (the pull is
    /// skipped).
    pub async fn full_cycle(client: &ApiClient) -> FullSyncReport {
        let ctx = CycleCtx::fetch(client).await;
        run_cycle(
            ctx.can_write(),
            Self::drain_if_writable(client, &ctx),
            || Self::pull(client, &ctx),
        )
        .await
    }

    /// Push-only cycle for the local-write trigger: the same `/me` write-scope
    /// gate as [`Self::full_cycle`], then drain the queue. No pull, so a burst
    /// of edits costs one `/me` plus the pushes.
    pub async fn push_cycle(client: &ApiClient) -> FullSyncReport {
        let ctx = CycleCtx::fetch(client).await;
        let push = Self::drain_if_writable(client, &ctx).await;
        push_only_report(ctx.can_write(), push)
    }

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

/// What one sync cycle learned from its single `/me` call. Every part of the
/// cycle (write-scope gate, push, create reconciliation, pull) reads this
/// instead of calling `/me` again.
pub struct CycleCtx {
    me: Result<MeDto, ApiError>,
}

impl CycleCtx {
    pub async fn fetch(client: &ApiClient) -> Self {
        Self {
            me: users::me(client).await,
        }
    }

    pub fn from_me(me: Result<MeDto, ApiError>) -> Self {
        Self { me }
    }

    /// `Some(true/false)` from the token's write scope; `None` when `/me` failed.
    pub fn can_write(&self) -> Option<bool> {
        self.me.as_ref().ok().map(|m| m.token.has_write_scope())
    }
}

/// Cycle ordering, separated from the HTTP-bound halves so it can be tested.
/// `preflight_can_write` is the scope read before the push; the pull's own
/// `/me` result overrides it when the pull runs.
async fn run_cycle<D, P, PF>(
    preflight_can_write: Option<bool>,
    drain: D,
    pull: P,
) -> FullSyncReport
where
    D: std::future::Future<Output = SyncReport>,
    P: FnOnce() -> PF,
    PF: std::future::Future<Output = PullOutcome>,
{
    let push = drain.await;
    if push.failures.iter().any(|f| f.kind == SyncErrorKind::Auth) {
        return FullSyncReport {
            pull: SyncReport::default(),
            push,
            deleted_orphan_team: None,
            current_user: None,
            can_write: preflight_can_write,
            push_only: false,
        };
    }
    let outcome = pull().await;
    FullSyncReport {
        pull: outcome.report,
        push,
        deleted_orphan_team: outcome.deleted_orphan_team,
        current_user: outcome.current_user,
        can_write: outcome.can_write.or(preflight_can_write),
        push_only: false,
    }
}

fn push_only_report(can_write: Option<bool>, push: SyncReport) -> FullSyncReport {
    FullSyncReport {
        push,
        can_write,
        push_only: true,
        ..FullSyncReport::default()
    }
}

fn read_only_report() -> SyncReport {
    SyncReport {
        push_paused: Some(PushPause::ReadOnly),
        ..SyncReport::default()
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
        ("project", "delete") => project_delete(client, entry.payload.as_deref()).await,
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
        ("comment", "create") => comment_create(client, &entry.entity_id, entry_id).await,
        ("comment", "delete") => comment_delete(client, entry.payload.as_deref()).await,
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
        (et, ac) => Outcome::DropAndLog(format!("unsupported ({et},{ac})")),
    }
}

// ── Sync log lines ────────────────────────────────────────────────────────
//
// Every `[sync]` line has one shape: `[sync] <scope> outcome=<o> reason="<r>"`.
// The scope is `entry_id=<n> type=<t> action=<a>` for a queue entry or
// `phase=<p> [team=<remote id>]` for a pull step. A queue entry gets exactly
// one line per drain, whatever happened to it:
//   synced  - pushed (or nothing to push); the entry is done
//   pending - will be retried on the next drain
//   parked  - kept pending without being retried (paused pushes, read-only
//             token, lost team access)
//   dropped - permanently rejected; marked synced so it is not retried

const READ_ONLY_REASON: &str = "read-only token; push skipped";
const PAUSED_REASON: &str = "pushes paused earlier in this drain";

fn pull_mode(since: &Option<String>) -> &'static str {
    if since.is_some() {
        "incremental"
    } else {
        "full"
    }
}

fn log_line(scope: &str, outcome: &str, reason: &str) -> String {
    format!(
        "[sync] {scope} outcome={outcome} reason=\"{}\"",
        reason.replace('"', "'")
    )
}

fn sync_log(scope: &str, outcome: &str, reason: &str) {
    eprintln!("{}", log_line(scope, outcome, reason));
}

fn entry_scope(entry_id: i64, entry: &SyncQueueEntry) -> String {
    format!(
        "entry_id={entry_id} type={} action={}",
        entry.entity_type, entry.action
    )
}

fn phase_scope(phase: &str, team_remote_id: Option<i64>) -> String {
    match team_remote_id {
        Some(t) => format!("phase={phase} team={t}"),
        None => format!("phase={phase}"),
    }
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

/// Record an auth failure from a stringified pull error, if present. Pull is
/// read-only (all GETs), so the only user-actionable error we route is a 401 —
/// `ApiError::Unauthorized`'s Display always contains `"unauthorized:"`, which
/// pull helpers embed verbatim (e.g. `"/me: unauthorized: …"`). A revoked token
/// trips the first call (`/me`), but scanning every pull error also catches a
/// mid-sync revoke.
fn note_pull_failure(report: &mut SyncReport, msg: &str) {
    if msg.contains("unauthorized:") {
        report.failures.push(SyncFailure {
            entity_type: "auth".into(),
            entity_id: String::new(),
            kind: SyncErrorKind::Auth,
            message: msg.to_string(),
        });
    }
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

// ── Synchronous DB helpers (each opens its own short-lived connection) ────

fn open() -> Result<Connection, String> {
    connection::open().map_err(|e| format!("db open: {e}"))
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

// ── Issues ────────────────────────────────────────────────────────────────

struct IssueCreatePrep {
    team_remote_id: i64,
    params: IssueParams,
    local_id: String,
}

struct IssueUpdatePrep {
    team_remote_id: i64,
    remote_id: i64,
    params: IssueParams,
}

fn prep_issue_create(entity_id: &str) -> Prep<IssueCreatePrep> {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Prep::Done(Outcome::LeavePending(e)),
    };
    let issue = match queries::get_issue(&conn, entity_id) {
        Ok(Some(i)) => i,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "issue {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };

    if issue.remote_id.is_some() {
        // Idempotency: Rails already has this row.
        return Prep::Done(Outcome::Success);
    }

    let team_remote_id = match queries::remote_id_for(&conn, "teams", &issue.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return Prep::Done(Outcome::LeavePending(format!(
                "team {} not synced yet",
                issue.team_id
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };

    let params = match build_issue_params(&conn, &issue) {
        Ok(p) => p,
        Err(o) => return Prep::Done(o),
    };

    Prep::Ready(IssueCreatePrep {
        team_remote_id,
        params,
        local_id: issue.id,
    })
}

fn prep_issue_update(entity_id: &str, entry_id: i64) -> Prep<IssueUpdatePrep> {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Prep::Done(Outcome::LeavePending(e)),
    };
    prep_issue_update_in(&conn, entity_id, entry_id)
}

fn prep_issue_update_in(conn: &Connection, entity_id: &str, entry_id: i64) -> Prep<IssueUpdatePrep> {
    let issue = match queries::get_issue(conn, entity_id) {
        Ok(Some(i)) => i,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "issue {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };

    let remote_id = match issue.remote_id {
        Some(id) => id,
        None => {
            return Prep::Done(Outcome::LeavePending(format!(
                "issue {entity_id} has no remote_id — create not yet drained"
            )))
        }
    };

    let source_team = match move_order(conn, entity_id, entry_id) {
        Ok(s) => s,
        Err(o) => return Prep::Done(o),
    };

    let team_remote_id = match source_team {
        Some(src) => src,
        None => match queries::remote_id_for(conn, "teams", &issue.team_id) {
            Ok(Some(id)) => id,
            Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
            Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
        },
    };

    let mut params = match build_issue_params(conn, &issue) {
        Ok(p) => p,
        Err(o) => return Prep::Done(o),
    };
    if source_team.is_some() {
        // Queued before a move: the row already shows the post-move lane,
        // project, labels, assignee and parent, which the source team doesn't
        // know. The move resets those server-side anyway, so send only the
        // team-independent fields.
        params.lane_id = None;
        params.project_id = None;
        params.assignee_id = None;
        params.parent_issue_id = None;
        params.label_ids = None;
    }

    Prep::Ready(IssueUpdatePrep {
        team_remote_id,
        remote_id,
        params,
    })
}

/// Where an entry for `issue_id` stands relative to the issue's queued
/// `issue/move` entries. The drain is FIFO but does not stop at a pending
/// entry, and the issue's local row already shows the post-move team, so
/// per-issue entries must check:
/// - an earlier move is still pending: wait (`LeavePending`), so the entry goes
///   to the new team's URL only after the server has moved the issue;
/// - a later move is pending: `Ok(Some(source team remote id))`, so the entry
///   still goes to the team the server has the issue in;
/// - otherwise `Ok(None)`: use the row's team.
///
/// A move that was dropped (404/422) is no longer pending, so later entries
/// proceed against the local row's team; if the server still has the issue
/// elsewhere they 404 and are dropped, and the next full pull restores the
/// issue to its server state.
fn move_order(conn: &Connection, issue_id: &str, entry_id: i64) -> Result<Option<i64>, Outcome> {
    let pending = sync_queue::pending_entries(conn)
        .map_err(|e| Outcome::LeavePending(format!("db: {e}")))?;
    let mut moves = pending
        .iter()
        .filter(|e| e.entity_type == "issue" && e.action == "move" && e.entity_id == issue_id);
    if moves.clone().any(|m| m.id.is_some_and(|id| id < entry_id)) {
        return Err(Outcome::LeavePending(
            "waiting for an earlier move of this issue".into(),
        ));
    }
    let Some(next) = moves.find(|m| m.id.is_some_and(|id| id > entry_id)) else {
        return Ok(None);
    };
    let payload: serde_json::Value = next
        .payload
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    if let Some(id) = payload.get("source_team_remote_id").and_then(|v| v.as_i64()) {
        return Ok(Some(id));
    }
    let source_local = payload
        .get("source_team_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    match queries::remote_id_for(conn, "teams", source_local) {
        Ok(Some(id)) => Ok(Some(id)),
        Ok(None) => Err(Outcome::LeavePending("source team not synced yet".into())),
        Err(e) => Err(Outcome::LeavePending(format!("db: {e}"))),
    }
}

async fn issue_create(
    client: &ApiClient,
    ctx: &CycleCtx,
    entity_id: &str,
    entry_id: i64,
) -> Outcome {
    let prep = match prep_issue_create(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };

    let attempt = match record_attempt(entry_id, "issues") {
        Ok(a) => a,
        Err(o) => return o,
    };
    if let Some(since) = &attempt.previous {
        match find_issue_on_server(
            client,
            ctx,
            prep.team_remote_id,
            &prep.params,
            since,
            &attempt,
        )
        .await
        {
            Ok(Some(remote_id)) => {
                return commit_create(entry_id, "issues", &prep.local_id, remote_id)
            }
            Ok(None) => {}
            Err(o) => return o,
        }
    }

    match issues::create(client, prep.team_remote_id, &prep.params).await {
        Ok(dto) => commit_create(entry_id, "issues", &prep.local_id, dto.id),
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

async fn issue_update(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match prep_issue_update(entity_id, entry_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };

    match issues::update(client, prep.team_remote_id, prep.remote_id, &prep.params).await {
        Ok(_) => Outcome::Success,
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

async fn issue_delete(
    client: &ApiClient,
    local_id: &str,
    payload: Option<&str>,
    entry_id: i64,
) -> Outcome {
    // The payload's team was captured at enqueue; after a pending move it is
    // the new team, which the server doesn't have the issue in yet.
    match open().map(|c| move_order(&c, local_id, entry_id)) {
        Ok(Err(o)) => return o,
        Ok(Ok(_)) => {}
        Err(e) => return Outcome::LeavePending(e),
    }
    let raw = match payload {
        Some(s) => s,
        None => return Outcome::DropAndLog("issue delete: no payload (legacy entry)".into()),
    };
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => return Outcome::DropAndLog(format!("issue delete: bad payload: {e}")),
    };
    let remote_id = match parsed.get("remote_id").and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => return Outcome::DropAndLog("issue delete: missing remote_id in payload".into()),
    };
    let team_remote_id = match parsed.get("team_remote_id").and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => {
            return Outcome::DropAndLog("issue delete: missing team_remote_id in payload".into())
        }
    };
    let result = issues::delete(client, team_remote_id, remote_id).await;
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Outcome::LeavePending(e),
    };
    issue_delete_outcome(&conn, local_id, team_remote_id, result)
}

/// Map the DELETE result to an outcome; on success (or already gone) purge the
/// local tombstone and its attached rows before the entry is marked synced.
/// A resource 404 ("Issue not found") means the issue is already gone, which
/// is what the delete wanted. A team-access 404 keeps the tombstone pending
/// and flags the team (via `classify_in_team`).
fn issue_delete_outcome(
    conn: &Connection,
    local_id: &str,
    team_remote_id: i64,
    result: Result<(), ApiError>,
) -> Outcome {
    match result {
        Ok(_) => purge_local_issue(conn, local_id),
        Err(ApiError::NotFound(m)) if !is_team_access_message(&m) => {
            purge_local_issue(conn, local_id)
        }
        Err(e) => classify_in_team(&e, team_remote_id),
    }
}

// ── Issue move ────────────────────────────────────────────────────────────

struct IssueMovePrep {
    source_team_remote_id: i64,
    issue_remote_id: i64,
    target_team_remote_id: i64,
}

/// Resolve the move push from the payload captured at enqueue (the issue's row
/// has already left the source team). A remote id that was unknown then is
/// looked up again from the local row; the entry waits until all three exist.
fn prep_issue_move_in(
    conn: &Connection,
    entity_id: &str,
    payload: Option<&str>,
) -> Prep<IssueMovePrep> {
    let Some(raw) = payload else {
        return Prep::Done(Outcome::DropAndLog("issue move: no payload".into()));
    };
    let p: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => return Prep::Done(Outcome::DropAndLog(format!("issue move: bad payload: {e}"))),
    };
    match queries::get_issue(conn, entity_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "issue {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    }
    // Captured remote id if present, else the current remote id of the local row.
    let resolve = |key: &str, table: &str, local_id: &str, what: &str| -> Result<i64, Outcome> {
        if let Some(id) = p.get(key).and_then(|v| v.as_i64()) {
            return Ok(id);
        }
        match queries::remote_id_for(conn, table, local_id) {
            Ok(Some(id)) => Ok(id),
            Ok(None) => Err(Outcome::LeavePending(format!("{what} not synced yet"))),
            Err(e) => Err(Outcome::LeavePending(format!("db: {e}"))),
        }
    };
    let local = |key: &str| p.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let source = resolve("source_team_remote_id", "teams", &local("source_team_id"), "source team");
    let issue = resolve("issue_remote_id", "issues", entity_id, "issue");
    let target = resolve("target_team_remote_id", "teams", &local("target_team_id"), "target team");
    match (source, issue, target) {
        (Ok(s), Ok(i), Ok(t)) => Prep::Ready(IssueMovePrep {
            source_team_remote_id: s,
            issue_remote_id: i,
            target_team_remote_id: t,
        }),
        (Err(o), _, _) | (_, Err(o), _) | (_, _, Err(o)) => Prep::Done(o),
    }
}

async fn issue_move(client: &ApiClient, entity_id: &str, payload: Option<&str>) -> Outcome {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Outcome::LeavePending(e),
    };
    let prep = match prep_issue_move_in(&conn, entity_id, payload) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    let result = issues::move_to_team(
        client,
        prep.source_team_remote_id,
        prep.issue_remote_id,
        prep.target_team_remote_id,
    )
    .await;
    issue_move_outcome(&conn, entity_id, prep.source_team_remote_id, result)
}

/// 200 applies the response; 404 (target outside the token, or issue gone) and
/// 422 (already there / not a member of both) drop the entry with the server's
/// message. The next full pull restores the issue's server state.
fn issue_move_outcome(
    conn: &Connection,
    entity_id: &str,
    source_team_remote_id: i64,
    result: Result<IssueDto, ApiError>,
) -> Outcome {
    match result {
        Ok(dto) => match apply_move_response(conn, entity_id, &dto) {
            Ok(()) => Outcome::Success,
            Err(e) => Outcome::LeavePending(format!("apply move: {e}")),
        },
        Err(ApiError::NotFound(m)) => Outcome::DropAndLog(format!("not found: {m}")),
        Err(e) => classify_in_team(&e, source_team_remote_id),
    }
}

/// Apply a move response as a pull would, but only the fields the move
/// decides: the server's team number (from `identifier`), lane, assignee and
/// parent. Other local edits are left alone. A ref that isn't known locally
/// keeps the local value.
fn apply_move_response(conn: &Connection, entity_id: &str, dto: &IssueDto) -> Result<(), String> {
    let issue = queries::get_issue(conn, entity_id)
        .map_err(|e| format!("db: {e}"))?
        .ok_or("issue missing locally")?;
    let team_number = dto
        .identifier
        .rsplit_once('-')
        .and_then(|(_, n)| n.parse::<i32>().ok())
        .or(issue.team_number);
    let lane_id = match &dto.lane {
        Some(r) => queries::find_lane_by_remote_id(conn, r.id)
            .map_err(|e| format!("db: {e}"))?
            .map(|l| l.id)
            .unwrap_or_else(|| issue.lane_id.clone()),
        None => issue.lane_id.clone(),
    };
    let assignee_id = match &dto.assignee {
        Some(r) => queries::find_member_by_remote_id(conn, r.id)
            .map_err(|e| format!("db: {e}"))?
            .map(|m| m.id)
            .or(issue.assignee_id.clone()),
        None => None,
    };
    let parent_issue_id = match &dto.parent_issue {
        Some(r) => queries::find_issue_by_remote_id(conn, r.id)
            .map_err(|e| format!("db: {e}"))?
            .map(|i| i.id)
            .or(issue.parent_issue_id.clone()),
        None => None,
    };
    conn.execute(
        "UPDATE issues SET team_number = ?1, lane_id = ?2, assignee_id = ?3, parent_issue_id = ?4
         WHERE id = ?5",
        rusqlite::params![team_number, lane_id, assignee_id, parent_issue_id, entity_id],
    )
    .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

fn purge_local_issue(conn: &Connection, local_id: &str) -> Outcome {
    match queries::purge_tombstoned_issue(conn, local_id) {
        Ok(()) => Outcome::Success,
        Err(e) => Outcome::LeavePending(format!("purge issue: {e}")),
    }
}


/// Save a freshly created record's server id and mark its queue entry synced
/// in one SQLite transaction. If either write fails both roll back, so the
/// entry stays pending and is retried on the next drain.
fn save_remote_id_and_mark_synced(
    conn: &mut Connection,
    table: &str,
    local_id: &str,
    remote_id: i64,
    entry_id: i64,
) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| format!("begin: {e}"))?;
    match table {
        "issues" => queries::set_issue_remote_id(&tx, local_id, remote_id),
        "projects" => queries::set_project_remote_id(&tx, local_id, remote_id),
        "labels" => queries::set_label_remote_id(&tx, local_id, remote_id),
        "comments" => queries::set_comment_remote_id(&tx, local_id, remote_id),
        "issue_dependencies" => queries::set_issue_dependency_remote_id(&tx, local_id, remote_id),
        other => return Err(format!("unknown create table {other}")),
    }
    .map_err(|e| format!("write remote_id: {e}"))?;
    sync_queue::mark_synced(&tx, entry_id).map_err(|e| format!("mark_synced: {e}"))?;
    tx.commit().map_err(|e| format!("commit: {e}"))
}

fn commit_create(entry_id: i64, table: &str, local_id: &str, remote_id: i64) -> Outcome {
    let mut conn = match open() {
        Ok(c) => c,
        Err(e) => return Outcome::LeavePending(e),
    };
    match save_remote_id_and_mark_synced(&mut conn, table, local_id, remote_id, entry_id) {
        Ok(()) => Outcome::Created,
        Err(e) => Outcome::LeavePending(e),
    }
}

// ── Create reconciliation (JRST-23) ───────────────────────────────────────
//
// If a create's POST reached Rails but we never saw the response (crash, lost
// connection), the entry is still pending. Before re-POSTing we look for the
// record the earlier attempt made and adopt its id. `sync_queue.attempted_at`
// is written before the first POST and never moved forward, so a retry's
// window covers every earlier attempt.
//
// Rails has no creator/created_at filter and no server-side idempotency for
// issue/project/label creates, so each type uses the strongest rule its
// payload allows. Every rule also skips server ids already linked to another
// local row, so two identical local creates cannot adopt the same record.
//
//  - Issues:   `GET issues?updated_since=<attempt>`; match `creator.id` ==
//              token user (`/me`), exact `title`, and `created_at` >= attempt.
//              Strongest (creator + title + time).
//  - Projects: `GET projects?updated_since=<attempt>`; rows carry no creator,
//              so match exact `name` and `created_at` >= attempt. Cannot
//              verify the creator.
//  - Recurring issues: `GET recurring_issues?updated_since=<attempt>`; match
//              `creator.id` == token user (`/me`), exact `title`, the same
//              frequency, interval, weekday and day of month, and
//              `created_at` >= attempt. The adopted row is applied whole
//              (`commit_created`), so a create whose POST succeeded but whose
//              local commit failed is finished, not repeated.
//  - Labels:   rows carry only `updated_at` (no created_at/creator, no
//              updated_since filter), which says nothing about creation. List
//              the team's labels and match `name` case-insensitively (Rails
//              enforces per-team case-insensitive uniqueness, so the match is
//              unambiguous, and a plain POST would 422 on it anyway).
//
// `created_at` is compared with CLOCK_SKEW_SLACK_SECS of tolerance because it
// is the server's clock and the attempt time is ours; a small slack beats a
// missed adoption (which would duplicate). Issue lookups fetch one page of up
// to RECONCILE_PAGE_LIMIT rows. A lookup error leaves the entry pending
// rather than POSTing blind.

const CLOCK_SKEW_SLACK_SECS: i64 = 60;
const RECONCILE_PAGE_LIMIT: u32 = 500;

/// State read before a create's POST: when an earlier attempt began (if any)
/// and the server ids already linked locally for the entity's table.
struct Attempt {
    previous: Option<String>,
    taken: HashSet<i64>,
}

fn record_attempt(entry_id: i64, table: &str) -> Result<Attempt, Outcome> {
    let conn = open().map_err(Outcome::LeavePending)?;
    record_attempt_on(&conn, entry_id, table).map_err(Outcome::LeavePending)
}

fn record_attempt_on(conn: &Connection, entry_id: i64, table: &str) -> Result<Attempt, String> {
    let taken = linked_remote_ids(conn, table)?;
    let previous =
        sync_queue::begin_attempt(conn, entry_id).map_err(|e| format!("begin_attempt: {e}"))?;
    Ok(Attempt { previous, taken })
}

fn linked_remote_ids(conn: &Connection, table: &str) -> Result<HashSet<i64>, String> {
    if !matches!(table, "issues" | "projects" | "labels" | "recurring_issues") {
        return Err(format!("unknown reconcile table {table}"));
    }
    let mut stmt = conn
        .prepare(&format!(
            "SELECT remote_id FROM {table} WHERE remote_id IS NOT NULL"
        ))
        .map_err(|e| format!("db: {e}"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, i64>(0))
        .map_err(|e| format!("db: {e}"))?;
    rows.collect::<Result<_, _>>().map_err(|e| format!("db: {e}"))
}

/// True when `created_at` is at or after `attempted_at` (minus clock slack).
/// Unparseable timestamps never match.
fn created_after_attempt(created_at: &str, attempted_at: &str) -> bool {
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok();
    match (parse(created_at), parse(attempted_at)) {
        (Some(c), Some(a)) => c >= a - chrono::Duration::seconds(CLOCK_SKEW_SLACK_SECS),
        _ => false,
    }
}

/// Earliest-created matching issue: same creator, exact title, created after
/// the attempt, not already linked to a local row.
fn match_issue(
    candidates: &[IssueDto],
    me_id: i64,
    title: &str,
    attempted_at: &str,
    taken: &HashSet<i64>,
) -> Option<i64> {
    candidates
        .iter()
        .filter(|d| {
            d.title == title
                && d.creator.as_ref().map(|c| c.id) == Some(me_id)
                && created_after_attempt(&d.created_at, attempted_at)
                && !taken.contains(&d.id)
        })
        .min_by(|a, b| a.created_at.cmp(&b.created_at))
        .map(|d| d.id)
}

/// Earliest-created project with the exact name created after the attempt.
fn match_project(
    candidates: &[ProjectDto],
    name: &str,
    attempted_at: &str,
    taken: &HashSet<i64>,
) -> Option<i64> {
    candidates
        .iter()
        .filter(|d| {
            d.name == name
                && created_after_attempt(&d.created_at, attempted_at)
                && !taken.contains(&d.id)
        })
        .min_by(|a, b| a.created_at.cmp(&b.created_at))
        .map(|d| d.id)
}

/// Label with the same name (case-insensitive), last written after the
/// attempt, not already linked. Labels carry no creator, so the timestamp is
/// what tells our own create from an older label that shares the name.
fn match_label(
    candidates: &[LabelDto],
    name: &str,
    attempted_at: &str,
    taken: &HashSet<i64>,
) -> Option<i64> {
    let wanted = name.trim().to_lowercase();
    candidates
        .iter()
        .find(|d| {
            d.name.trim().to_lowercase() == wanted
                && d.updated_at
                    .as_deref()
                    .is_some_and(|u| created_after_attempt(u, attempted_at))
                && !taken.contains(&d.id)
        })
        .map(|d| d.id)
}

async fn find_issue_on_server(
    client: &ApiClient,
    ctx: &CycleCtx,
    team_remote_id: i64,
    params: &IssueParams,
    attempted_at: &str,
    attempt: &Attempt,
) -> Result<Option<i64>, Outcome> {
    let Some(title) = params.title.as_deref() else {
        return Ok(None);
    };
    // The cycle's one `/me` result. Without it the creator can't be matched,
    // so keep the entry pending rather than POSTing blind.
    let me = match &ctx.me {
        Ok(me) => me,
        Err(e) => return Err(Outcome::LeavePending(format!("/me unavailable: {e}"))),
    };
    let query = issues::IssuePageQuery {
        updated_since: Some(attempted_at.to_string()),
        limit: Some(RECONCILE_PAGE_LIMIT),
        ..Default::default()
    };
    let candidates = issues::list_page(client, team_remote_id, &query)
        .await
        .map_err(|e| classify(&e))?
        .rows;
    Ok(match_issue(
        &candidates,
        me.id,
        title,
        attempted_at,
        &attempt.taken,
    ))
}

/// Earliest-created recurring issue by this user with the same title and
/// schedule, created after the attempt, not already linked to a local row.
fn match_recurring_issue<'a>(
    candidates: &'a [RecurringIssueDto],
    me_id: i64,
    params: &RecurringIssueParams,
    attempted_at: &str,
    taken: &HashSet<i64>,
) -> Option<&'a RecurringIssueDto> {
    candidates
        .iter()
        .filter(|d| {
            Some(&d.title) == params.title.as_ref()
                && Some(&d.frequency) == params.frequency.as_ref()
                && Some(d.interval) == params.interval
                && d.weekday == params.weekday
                && d.day_of_month == params.day_of_month
                && d.creator.as_ref().map(|c| c.id) == Some(me_id)
                && created_after_attempt(&d.created_at, attempted_at)
                && !taken.contains(&d.id)
        })
        .min_by(|a, b| a.created_at.cmp(&b.created_at))
}

async fn find_recurring_issue_on_server(
    client: &ApiClient,
    ctx: &CycleCtx,
    team_remote_id: i64,
    params: &RecurringIssueParams,
    attempted_at: &str,
    attempt: &Attempt,
) -> Result<Option<RecurringIssueDto>, Outcome> {
    // Without `/me` the creator can't be matched: stay pending, don't POST blind.
    let me = match &ctx.me {
        Ok(me) => me,
        Err(e) => return Err(Outcome::LeavePending(format!("/me unavailable: {e}"))),
    };
    let candidates = recurring_issues::list_page(client, team_remote_id, Some(attempted_at))
        .await
        .map_err(|e| classify(&e))?
        .rows;
    Ok(match_recurring_issue(&candidates, me.id, params, attempted_at, &attempt.taken).cloned())
}

async fn find_project_on_server(
    client: &ApiClient,
    team_remote_id: i64,
    params: &ProjectParams,
    attempted_at: &str,
    attempt: &Attempt,
) -> Result<Option<i64>, ApiError> {
    let Some(name) = params.name.as_deref() else {
        return Ok(None);
    };
    let candidates = projects::list_page(client, team_remote_id, Some(attempted_at))
        .await?
        .rows;
    Ok(match_project(&candidates, name, attempted_at, &attempt.taken))
}

async fn find_label_on_server(
    client: &ApiClient,
    team_remote_id: i64,
    params: &LabelParams,
    attempted_at: &str,
    attempt: &Attempt,
) -> Result<Option<i64>, ApiError> {
    let candidates = match labels::list_conditional(client, team_remote_id, None).await? {
        Conditional::Modified { data, .. } => data,
        Conditional::NotModified => return Ok(None),
    };
    Ok(match_label(
        &candidates,
        &params.name,
        attempted_at,
        &attempt.taken,
    ))
}

fn build_issue_params(conn: &Connection, issue: &Issue) -> Result<IssueParams, Outcome> {
    let lane_id = require_remote_id(conn, "lanes", &issue.lane_id)?;
    let project_id = optional_remote_id(conn, "projects", issue.project_id.as_deref())?;
    let assignee_id = optional_remote_id(conn, "members", issue.assignee_id.as_deref())?;
    let parent_issue_id = optional_remote_id(conn, "issues", issue.parent_issue_id.as_deref())?;
    let label_ids = build_label_ids(conn, &issue.id)?;

    Ok(IssueParams {
        title: Some(issue.title.clone()),
        description: issue.description.clone(),
        lane_id: Some(lane_id),
        priority: Some(
            Priority::from_i32(issue.priority)
                .as_rails_str()
                .to_string(),
        ),
        estimate: issue.estimate.map(|e| e as i64),
        due_date: issue.due_date.clone(),
        assignee_id,
        project_id,
        parent_issue_id,
        label_ids,
    })
}

/// Resolve the issue's locally-attached labels to their Rails remote ids.
///
/// Always returns `Some(..)` (possibly empty) so the push asserts the full
/// label set — Rails' `label_ids=` setter treats `[]` as "detach all", which is
/// exactly what we want when the user clears an issue's labels locally. If any
/// attached label hasn't been pushed yet (no `remote_id`), we leave the entry
/// pending so the label create drains first and ordering is preserved.
fn build_label_ids(conn: &Connection, issue_id: &str) -> Result<Option<Vec<i64>>, Outcome> {
    let local_ids = queries::label_ids_for_issue(conn, issue_id)
        .map_err(|e| Outcome::LeavePending(format!("db (issue labels): {e}")))?;
    let mut remote_ids = Vec::with_capacity(local_ids.len());
    for local in &local_ids {
        remote_ids.push(require_remote_id(conn, "labels", local)?);
    }
    Ok(Some(remote_ids))
}

fn require_remote_id(conn: &Connection, table: &str, id: &str) -> Result<i64, Outcome> {
    match queries::remote_id_for(conn, table, id) {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Err(Outcome::LeavePending(format!(
            "{table} {id} not synced yet"
        ))),
        Err(e) => Err(Outcome::LeavePending(format!("db: {e}"))),
    }
}

fn optional_remote_id(
    conn: &Connection,
    table: &str,
    id: Option<&str>,
) -> Result<Option<i64>, Outcome> {
    match id {
        Some(uuid) => require_remote_id(conn, table, uuid).map(Some),
        None => Ok(None),
    }
}

// ── Projects ──────────────────────────────────────────────────────────────

struct ProjectCreatePrep {
    team_remote_id: i64,
    params: ProjectParams,
    local_id: String,
}

struct ProjectUpdatePrep {
    team_remote_id: i64,
    remote_id: i64,
    params: ProjectParams,
}

fn prep_project_create(entity_id: &str) -> Prep<ProjectCreatePrep> {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Prep::Done(Outcome::LeavePending(e)),
    };
    let project = match queries::get_project(&conn, entity_id) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "project {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    if project.remote_id.is_some() {
        return Prep::Done(Outcome::Success);
    }
    let team_remote_id = match queries::remote_id_for(&conn, "teams", &project.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let params = match build_project_params(&conn, &project) {
        Ok(p) => p,
        Err(o) => return Prep::Done(o),
    };
    Prep::Ready(ProjectCreatePrep {
        team_remote_id,
        params,
        local_id: project.id,
    })
}

fn prep_project_update(entity_id: &str) -> Prep<ProjectUpdatePrep> {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Prep::Done(Outcome::LeavePending(e)),
    };
    let project = match queries::get_project(&conn, entity_id) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "project {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let remote_id = match project.remote_id {
        Some(id) => id,
        None => {
            return Prep::Done(Outcome::LeavePending(
                "project create not yet drained".into(),
            ))
        }
    };
    let team_remote_id = match queries::remote_id_for(&conn, "teams", &project.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let params = match build_project_params(&conn, &project) {
        Ok(p) => p,
        Err(o) => return Prep::Done(o),
    };
    Prep::Ready(ProjectUpdatePrep {
        team_remote_id,
        remote_id,
        params,
    })
}

async fn project_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match prep_project_create(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    let attempt = match record_attempt(entry_id, "projects") {
        Ok(a) => a,
        Err(o) => return o,
    };
    if let Some(since) = &attempt.previous {
        match find_project_on_server(client, prep.team_remote_id, &prep.params, since, &attempt)
            .await
        {
            Ok(Some(remote_id)) => {
                return commit_create(entry_id, "projects", &prep.local_id, remote_id)
            }
            Ok(None) => {}
            Err(e) => return classify(&e),
        }
    }
    match projects::create(client, prep.team_remote_id, &prep.params).await {
        Ok(dto) => commit_create(entry_id, "projects", &prep.local_id, dto.id),
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

async fn project_update(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match prep_project_update(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    match projects::update(client, prep.team_remote_id, prep.remote_id, &prep.params).await {
        Ok(_) => Outcome::Success,
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

async fn project_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let raw = match payload {
        Some(s) => s,
        None => return Outcome::DropAndLog("project delete: no payload (legacy entry)".into()),
    };
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => return Outcome::DropAndLog(format!("project delete: bad payload: {e}")),
    };
    let remote_id = match parsed.get("remote_id").and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => return Outcome::DropAndLog("project delete: missing remote_id in payload".into()),
    };
    let team_remote_id = match parsed.get("team_remote_id").and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => {
            return Outcome::DropAndLog("project delete: missing team_remote_id in payload".into())
        }
    };
    match projects::delete(client, team_remote_id, remote_id).await {
        Ok(_) => Outcome::Success,
        Err(ApiError::NotFound(m)) if !is_team_access_message(&m) => Outcome::Success,
        Err(e) => classify_in_team(&e, team_remote_id),
    }
}

fn build_project_params(conn: &Connection, project: &Project) -> Result<ProjectParams, Outcome> {
    let lead_remote_id = match &project.lead_id {
        Some(uuid) => match queries::remote_id_for(conn, "members", uuid) {
            Ok(Some(id)) => Some(id),
            Ok(None) => {
                return Err(Outcome::LeavePending(format!(
                    "project lead member {uuid} not synced yet"
                )))
            }
            Err(e) => return Err(Outcome::LeavePending(format!("db: {e}"))),
        },
        None => None,
    };
    Ok(ProjectParams {
        name: Some(project.name.clone()),
        description: project.description.clone(),
        priority: Some(
            Priority::from_i32(project.priority)
                .as_rails_str()
                .to_string(),
        ),
        status: project.status.map(|s| s.as_str().to_string()),
        lead_id: lead_remote_id,
        start_date: project.start_date.clone(),
        due_date: project.due_date.clone(),
        roadmap_commitment: project
            .roadmap_commitment
            .as_ref()
            .map(|c| c.as_str().to_string()),
        label_ids: None,
    })
}

// ── Labels ────────────────────────────────────────────────────────────────

struct LabelCreatePrep {
    team_remote_id: i64,
    params: LabelParams,
    local_id: String,
}

fn prep_label_create(entity_id: &str) -> Prep<LabelCreatePrep> {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Prep::Done(Outcome::LeavePending(e)),
    };
    let label = match queries::get_label(&conn, entity_id) {
        Ok(Some(l)) => l,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "label {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    if label.remote_id.is_some() {
        return Prep::Done(Outcome::Success);
    }
    let team_remote_id = match queries::remote_id_for(&conn, "teams", &label.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    Prep::Ready(LabelCreatePrep {
        team_remote_id,
        params: LabelParams {
            name: label.name,
            color: label.color,
        },
        local_id: label.id,
    })
}

async fn label_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match prep_label_create(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    let attempt = match record_attempt(entry_id, "labels") {
        Ok(a) => a,
        Err(o) => return o,
    };
    if let Some(since) = &attempt.previous {
        match find_label_on_server(client, prep.team_remote_id, &prep.params, since, &attempt)
            .await
        {
            Ok(Some(remote_id)) => {
                return commit_create(entry_id, "labels", &prep.local_id, remote_id)
            }
            Ok(None) => {}
            Err(e) => return classify(&e),
        }
    }
    match labels::create(client, prep.team_remote_id, &prep.params).await {
        Ok(dto) => commit_create(entry_id, "labels", &prep.local_id, dto.id),
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

struct LabelUpdatePrep {
    team_remote_id: i64,
    remote_id: i64,
    params: LabelParams,
}

fn prep_label_update(entity_id: &str) -> Prep<LabelUpdatePrep> {
    match open() {
        Ok(conn) => label_update_prep(&conn, entity_id),
        Err(e) => Prep::Done(Outcome::LeavePending(e)),
    }
}

/// Re-reads the label row so the newest name and color go out, however many
/// edits were queued.
fn label_update_prep(conn: &Connection, entity_id: &str) -> Prep<LabelUpdatePrep> {
    let label = match queries::get_label(conn, entity_id) {
        Ok(Some(l)) => l,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "label {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let Some(remote_id) = label.remote_id else {
        return Prep::Done(Outcome::LeavePending("label create not yet drained".into()));
    };
    let team_remote_id = match queries::remote_id_for(conn, "teams", &label.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    Prep::Ready(LabelUpdatePrep {
        team_remote_id,
        remote_id,
        params: LabelParams {
            name: label.name,
            color: label.color,
        },
    })
}

async fn label_update(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match prep_label_update(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    // A 422 (duplicate name on the server) is dropped; the next label pull
    // restores the server value.
    match labels::update(client, prep.team_remote_id, prep.remote_id, &prep.params).await {
        Ok(_) => Outcome::Success,
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

/// What a recurring issue create sends. `time_zone` is absent when the row's
/// zone is null (the server then uses the creator's preference) and `label_ids`
/// when no label is attached.
struct RecurringIssueCreatePrep {
    team_remote_id: i64,
    local_id: String,
    params: RecurringIssueParams,
}

fn prep_recurring_issue_create(
    entity_id: &str,
    payload: Option<&str>,
) -> Prep<RecurringIssueCreatePrep> {
    match open() {
        Ok(conn) => recurring_issue_create_prep(&conn, entity_id, payload),
        Err(e) => Prep::Done(Outcome::LeavePending(e)),
    }
}

/// Re-reads the recurring issue row so the newest values go out, and waits
/// (leaves the entry pending) while the team, or any assignee, project, lane or
/// label the row references, has no remote id yet. `starts_on` comes from the
/// queued payload.
fn recurring_issue_create_prep(
    conn: &Connection,
    entity_id: &str,
    payload: Option<&str>,
) -> Prep<RecurringIssueCreatePrep> {
    use crate::db::recurring_issues as db;
    let row = match db::get(conn, entity_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "recurring issue {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    if row.remote_id.is_some() {
        // Idempotency: Rails already has this row.
        return Prep::Done(Outcome::Success);
    }
    let team_remote_id = match queries::remote_id_for(conn, "teams", &row.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let refs = (|| -> Result<_, Outcome> {
        let assignee_id = optional_remote_id(conn, "members", row.assignee_id.as_deref())?;
        let project_id = optional_remote_id(conn, "projects", row.project_id.as_deref())?;
        let lane_id = optional_remote_id(conn, "lanes", row.lane_id.as_deref())?;
        let local_labels = db::label_ids(conn, &row.id)
            .map_err(|e| Outcome::LeavePending(format!("db (labels): {e}")))?;
        let mut label_ids = Vec::with_capacity(local_labels.len());
        for local in &local_labels {
            label_ids.push(require_remote_id(conn, "labels", local)?);
        }
        Ok((assignee_id, project_id, lane_id, label_ids))
    })();
    let (assignee_id, project_id, lane_id, label_ids) = match refs {
        Ok(r) => r,
        Err(o) => return Prep::Done(o),
    };
    let params = RecurringIssueParams {
        title: Some(row.title),
        description: row.description,
        priority: Some(row.priority.as_rails_str().to_string()),
        assignee_id: assignee_id.map(Some),
        project_id: project_id.map(Some),
        lane_id: lane_id.map(Some),
        frequency: Some(row.frequency),
        interval: Some(row.interval),
        weekday: row.weekday,
        day_of_month: row.day_of_month,
        time_zone: row.time_zone,
        label_ids: (!label_ids.is_empty()).then_some(label_ids),
        starts_on: db::payload_starts_on(payload),
        ..Default::default()
    };
    Prep::Ready(RecurringIssueCreatePrep {
        team_remote_id,
        local_id: row.id,
        params,
    })
}

async fn recurring_issue_create(
    client: &ApiClient,
    ctx: &CycleCtx,
    entity_id: &str,
    payload: Option<&str>,
    entry_id: i64,
) -> Outcome {
    let prep = match prep_recurring_issue_create(entity_id, payload) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    // A retry (an earlier attempt began but the entry never finished) first
    // looks for the row that attempt created, so a POST that succeeded but
    // whose local commit failed is adopted instead of repeated.
    let attempt = match record_attempt(entry_id, "recurring_issues") {
        Ok(a) => a,
        Err(o) => return o,
    };
    if let Some(since) = &attempt.previous {
        match find_recurring_issue_on_server(
            client,
            ctx,
            prep.team_remote_id,
            &prep.params,
            since,
            &attempt,
        )
        .await
        {
            Ok(Some(dto)) => return commit_recurring_create(&prep.local_id, entry_id, &dto),
            Ok(None) => {}
            Err(o) => return o,
        }
    }
    // A 422 is dropped by `classify`; the row stays visible without a remote id.
    match recurring_issues::create(client, prep.team_remote_id, &prep.params).await {
        Ok(dto) => commit_recurring_create(&prep.local_id, entry_id, &dto),
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

fn commit_recurring_create(local_id: &str, entry_id: i64, dto: &RecurringIssueDto) -> Outcome {
    let committed = open().and_then(|mut conn| {
        crate::db::recurring_issues::commit_created(&mut conn, local_id, entry_id, dto)
            .map_err(|e| format!("commit recurring issue create: {e}"))
    });
    match committed {
        Ok(()) => Outcome::Created,
        Err(e) => Outcome::LeavePending(e),
    }
}

/// Remote ids of a recurring issue's labels; waits (leave pending) while any
/// has none yet.
fn recurring_label_remote_ids(conn: &Connection, id: &str) -> Result<Vec<i64>, Outcome> {
    let local = crate::db::recurring_issues::label_ids(conn, id)
        .map_err(|e| Outcome::LeavePending(format!("db (labels): {e}")))?;
    local
        .iter()
        .map(|l| require_remote_id(conn, "labels", l))
        .collect()
}

struct RecurringIssueUpdatePrep {
    team_remote_id: i64,
    remote_id: i64,
    local_id: String,
    params: RecurringIssueParams,
}

fn prep_recurring_issue_update(
    entity_id: &str,
    payload: Option<&str>,
) -> Prep<RecurringIssueUpdatePrep> {
    match open() {
        Ok(conn) => recurring_issue_update_prep(&conn, entity_id, payload),
        Err(e) => Prep::Done(Outcome::LeavePending(e)),
    }
}

/// Re-reads the recurring issue row so the newest values go out, however many
/// edits were queued. The payload's `changed` list picks which fields are sent
/// (unknown names are ignored), and `starts_on` goes out only if the payload
/// carries one (schedule changes), so a pause/resume leaves the server's
/// `next_run_on` alone. Edit tickets add their fields to the match below.
fn recurring_issue_update_prep(
    conn: &Connection,
    entity_id: &str,
    payload: Option<&str>,
) -> Prep<RecurringIssueUpdatePrep> {
    use crate::db::recurring_issues as db;
    let row = match db::get(conn, entity_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "recurring issue {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let Some(remote_id) = row.remote_id else {
        return Prep::Done(Outcome::LeavePending(
            "recurring issue create not yet drained".into(),
        ));
    };
    let team_remote_id = match queries::remote_id_for(conn, "teams", &row.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let (changed, starts_on) = db::parse_update_payload(payload);
    let mut params = RecurringIssueParams {
        starts_on,
        ..Default::default()
    };
    // Referenced assignee, project, lane and labels must have remote ids first.
    // A cleared reference is sent as an explicit null.
    for field in &changed {
        let r = match field.as_str() {
            "active" => {
                params.active = Some(row.active);
                Ok(())
            }
            "title" => {
                params.title = Some(row.title.clone());
                Ok(())
            }
            "description" => {
                // Rails treats "" as blank, which is how a cleared description goes out.
                params.description = Some(row.description.clone().unwrap_or_default());
                Ok(())
            }
            "priority" => {
                params.priority = Some(row.priority.as_rails_str().to_string());
                Ok(())
            }
            "time_zone" => {
                params.time_zone = row.time_zone.clone();
                Ok(())
            }
            "assignee_id" => optional_remote_id(conn, "members", row.assignee_id.as_deref())
                .map(|v| params.assignee_id = Some(v)),
            "project_id" => optional_remote_id(conn, "projects", row.project_id.as_deref())
                .map(|v| params.project_id = Some(v)),
            "lane_id" => optional_remote_id(conn, "lanes", row.lane_id.as_deref())
                .map(|v| params.lane_id = Some(v)),
            "label_ids" => recurring_label_remote_ids(conn, &row.id)
                .map(|v| params.label_ids = Some(v)),
            "frequency" => {
                params.frequency = Some(row.frequency.clone());
                Ok(())
            }
            "interval" => {
                params.interval = Some(row.interval);
                Ok(())
            }
            "weekday" => {
                params.weekday = row.weekday;
                Ok(())
            }
            "day_of_month" => {
                params.day_of_month = row.day_of_month;
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(o) = r {
            return Prep::Done(o);
        }
    }
    Prep::Ready(RecurringIssueUpdatePrep {
        team_remote_id,
        remote_id,
        local_id: row.id,
        params,
    })
}

async fn recurring_issue_update(
    client: &ApiClient,
    entity_id: &str,
    payload: Option<&str>,
) -> Outcome {
    let prep = match prep_recurring_issue_update(entity_id, payload) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    match recurring_issues::update(client, prep.team_remote_id, prep.remote_id, &prep.params).await
    {
        Ok(dto) => {
            let applied = open().and_then(|conn| {
                crate::db::recurring_issues::apply_response(&conn, &prep.local_id, &dto)
                    .map_err(|e| format!("apply recurring issue: {e}"))
            });
            match applied {
                Ok(()) => Outcome::Success,
                Err(e) => Outcome::LeavePending(e),
            }
        }
        // A 422 is dropped by `classify`; the next pull restores the server
        // row. "Recurring issue not found" means it is already gone (the next
        // pull's tombstone removes the local row); a team-access 404 is
        // handled by `classify_in_team`.
        Err(ApiError::NotFound(m)) if !is_team_access_message(&m) => Outcome::Success,
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

/// The local row is gone by drain time, so the payload captured at enqueue time
/// (`{team_remote_id, remote_id}`) is all a recurring issue delete has.
fn prep_recurring_issue_delete(payload: Option<&str>) -> Prep<(i64, i64)> {
    let parsed = payload.and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok());
    let ids = parsed.as_ref().and_then(|v| {
        Some((
            v.get("team_remote_id")?.as_i64()?,
            v.get("remote_id")?.as_i64()?,
        ))
    });
    match ids {
        Some(ids) => Prep::Ready(ids),
        None => Prep::Done(Outcome::DropAndLog(
            "recurring issue delete: missing or bad payload".into(),
        )),
    }
}

async fn recurring_issue_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let (team_remote_id, remote_id) = match prep_recurring_issue_delete(payload) {
        Prep::Ready(ids) => ids,
        Prep::Done(o) => return o,
    };
    // 204 and a "not found" 404 both mean it is gone; a team-access 404 is not.
    match recurring_issues::delete(client, team_remote_id, remote_id).await {
        Ok(()) => Outcome::Success,
        Err(ApiError::NotFound(m)) if !is_team_access_message(&m) => Outcome::Success,
        Err(e) => classify_in_team(&e, team_remote_id),
    }
}

/// The local row is gone by drain time, so the payload captured at enqueue time
/// (`{team_remote_id, remote_id}`) is all a label delete has.
fn prep_label_delete(payload: Option<&str>) -> Prep<(i64, i64)> {
    let Some(raw) = payload else {
        return Prep::Done(Outcome::DropAndLog(
            "label delete: no payload (legacy entry)".into(),
        ));
    };
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "label delete: bad payload: {e}"
            )))
        }
    };
    match (
        parsed.get("team_remote_id").and_then(|v| v.as_i64()),
        parsed.get("remote_id").and_then(|v| v.as_i64()),
    ) {
        (Some(team), Some(id)) => Prep::Ready((team, id)),
        _ => Prep::Done(Outcome::DropAndLog(
            "label delete: missing ids in payload".into(),
        )),
    }
}

async fn label_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let (team_remote_id, remote_id) = match prep_label_delete(payload) {
        Prep::Ready(ids) => ids,
        Prep::Done(o) => return o,
    };
    // 204 and 404 both mean the label is gone.
    match labels::delete(client, team_remote_id, remote_id).await {
        Ok(_) => Outcome::Success,
        Err(ApiError::NotFound(m)) if !is_team_access_message(&m) => Outcome::Success,
        Err(e) => classify_in_team(&e, team_remote_id),
    }
}

// ── Comments ──────────────────────────────────────────────────────────────

struct CommentCreatePrep {
    team_remote_id: i64,
    target: CommentTarget,
    params: CommentParams,
    local_id: String,
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
    Prep::Ready(CommentCreatePrep {
        team_remote_id,
        target,
        params: CommentParams {
            body: comment.body,
            parent_id: parent_remote_id,
        },
        local_id: comment.id,
    })
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

async fn comment_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
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
fn comment_delete_outcome(result: Result<(), ApiError>, team_remote_id: i64) -> Outcome {
    match result {
        Ok(_) => Outcome::Success,
        Err(ApiError::NotFound(m)) if !is_team_access_message(&m) => Outcome::Success,
        Err(ApiError::Forbidden(m)) if !is_token_scope_message(&m) => {
            Outcome::DropAndLog(format!("forbidden: {m}"))
        }
        Err(e) => classify_in_team(&e, team_remote_id),
    }
}

/// `PATCH /notifications/:id/read`. Idempotent, so a retry is safe.
async fn notification_read(client: &ApiClient, entity_id: &str) -> Outcome {
    let Ok(remote_id) = entity_id.parse::<i64>() else {
        return Outcome::DropAndLog(format!("bad notification id {entity_id}"));
    };
    notification_read_outcome(crate::api::notifications::mark_read(client, remote_id).await)
}

/// `PATCH /notifications/read_all` with the queued `before` timestamp, then
/// correct the badge from the response.
async fn notification_read_all(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let Some(before) = notification_read_all_before(payload) else {
        return Outcome::DropAndLog("read_all without a before timestamp".into());
    };
    let result = crate::api::notifications::mark_all_read(client, &before).await;
    if let Ok(resp) = &result {
        if let Ok(conn) = open() {
            let _ = crate::db::notifications::set_unread_count(&conn, resp.unread_count);
        }
    }
    notification_read_outcome(result.map(|_| ()))
}

fn notification_read_all_before(payload: Option<&str>) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(payload?).ok()?;
    v.get("before")?.as_str().map(str::to_string)
}

/// A 404 (not yours, or gone) means there is nothing left to mark: done.
fn notification_read_outcome(result: Result<(), ApiError>) -> Outcome {
    done_if_gone(result, |()| {})
}

/// Outcome for a write whose target may already be gone on the server: success
/// (running `on_ok` with the response) and a 404 both mean done; anything else
/// is classified as usual.
fn done_if_gone<T>(result: Result<T, ApiError>, on_ok: impl FnOnce(T)) -> Outcome {
    match result {
        Ok(v) => {
            on_ok(v);
            Outcome::Success
        }
        Err(ApiError::NotFound(_)) => Outcome::Success,
        Err(e) => classify(&e),
    }
}

/// `PUT /me/home_page`. A 422 drops the entry (logged by the drain); the next
/// `/me` read restores the server value.
async fn home_page_set(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let resolved = match open() {
        Ok(conn) => crate::db::home_page::resolve_payload(&conn, payload),
        Err(e) => return Outcome::LeavePending(e),
    };
    let (kind, id) = match resolved {
        crate::db::home_page::Resolved::Ready(k, i) => (k, i),
        crate::db::home_page::Resolved::Waiting => {
            return Outcome::LeavePending("home view not synced yet".into())
        }
        crate::db::home_page::Resolved::Bad => {
            return Outcome::DropAndLog("home_page set: bad payload".into())
        }
    };
    match crate::api::users::set_home_page(client, &kind, id).await {
        Ok(()) => Outcome::Success,
        Err(e) => classify(&e),
    }
}

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
fn commit_saved_view_create(
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
async fn saved_view_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
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

fn is_legacy_rename_payload(payload: Option<&str>) -> bool {
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
async fn saved_view_update(client: &ApiClient, entity_id: &str, rename: bool) -> Outcome {
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
async fn saved_view_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let Some(remote_id) = saved_view_delete_id(payload) else {
        return Outcome::DropAndLog("saved_view delete: bad payload".into());
    };
    saved_view_delete_outcome(crate::api::saved_views::delete(client, remote_id).await)
}

fn saved_view_delete_id(payload: Option<&str>) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_str(payload?).ok()?;
    v.get("remote_id")?.as_i64()
}

fn saved_view_delete_outcome(result: Result<(), ApiError>) -> Outcome {
    done_if_gone(result, |()| {})
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

async fn comment_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
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
        Ok(dto) => commit_create(entry_id, "comments", &prep.local_id, dto.id),
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

// ── Issue dependencies ────────────────────────────────────────────────────

struct DependencyCreatePrep {
    team_remote_id: i64,
    issue_remote_id: i64,
    target_remote_id: i64,
    direction: Direction,
    local_id: String,
}

fn prep_dependency_create(entity_id: &str, entry_id: i64) -> Prep<DependencyCreatePrep> {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Prep::Done(Outcome::LeavePending(e)),
    };
    let dep = match queries::get_issue_dependency(&conn, entity_id) {
        Ok(Some(d)) => d,
        Ok(None) => {
            return Prep::Done(Outcome::DropAndLog(format!(
                "dependency {entity_id} missing locally"
            )))
        }
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    if dep.remote_id.is_some() {
        return Prep::Done(Outcome::Success);
    }
    let blocking = match queries::get_issue(&conn, &dep.blocking_issue_id) {
        Ok(Some(i)) => i,
        Ok(None) => return Prep::Done(Outcome::DropAndLog("blocking issue missing".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let blocked = match queries::get_issue(&conn, &dep.blocked_issue_id) {
        Ok(Some(i)) => i,
        Ok(None) => return Prep::Done(Outcome::DropAndLog("blocked issue missing".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let (issue_remote_id, target_remote_id) = match (blocking.remote_id, blocked.remote_id) {
        (Some(b), Some(t)) => (b, t),
        _ => {
            return Prep::Done(Outcome::LeavePending(
                "dependency endpoints not yet synced".into(),
            ))
        }
    };
    let team_remote_id = match move_order(&conn, &blocking.id, entry_id) {
        Ok(Some(src)) => src,
        Ok(None) => match queries::remote_id_for(&conn, "teams", &blocking.team_id) {
            Ok(Some(id)) => id,
            Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
            Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
        },
        Err(o) => return Prep::Done(o),
    };
    Prep::Ready(DependencyCreatePrep {
        team_remote_id,
        issue_remote_id,
        target_remote_id,
        direction: dependencies::direction_for_edge(&dep.kind),
        local_id: dep.id,
    })
}

async fn dependency_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match prep_dependency_create(entity_id, entry_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    match dependencies::create(
        client,
        prep.team_remote_id,
        prep.issue_remote_id,
        prep.target_remote_id,
        prep.direction,
    )
    .await
    {
        Ok(resp) => {
            // The create is posted from the source side, so the response
            // direction is already relative to it; store what Rails echoed.
            let conn = match open() {
                Ok(c) => c,
                Err(e) => return Outcome::LeavePending(e),
            };
            if let Err(e) = queries::set_issue_dependency_sync_fields(
                &conn,
                &prep.local_id,
                resp.id,
                &resp.kind,
                &resp.direction,
            ) {
                return Outcome::LeavePending(format!("write dependency fields: {e}"));
            }
            commit_create(entry_id, "issue_dependencies", &prep.local_id, resp.id)
        }
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

/// Deletes a dependency's Rails record from the queue payload.
///
/// The local row (and its remote_id) is gone by drain time, so — like
/// issue_delete / project_delete — this handler relies entirely on the payload,
/// NOT on re-reading local state. The enqueue site (JAIT-114) must capture, before
/// deleting the local row:
///   { "team_remote_id":  <blocking issue's team remote_id>,
///     "issue_remote_id": <blocking issue's remote_id>,
///     "remote_id":       <dependency row's own remote_id / Rails dep id> }
/// and must skip enqueueing entirely when the dependency has no remote_id.
async fn dependency_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
    let raw = match payload {
        Some(s) => s,
        None => return Outcome::DropAndLog("dependency delete: no payload (legacy entry)".into()),
    };
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => return Outcome::DropAndLog(format!("dependency delete: bad payload: {e}")),
    };
    let team_remote_id = match parsed.get("team_remote_id").and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => {
            return Outcome::DropAndLog(
                "dependency delete: missing team_remote_id in payload".into(),
            )
        }
    };
    let issue_remote_id = match parsed.get("issue_remote_id").and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => {
            return Outcome::DropAndLog(
                "dependency delete: missing issue_remote_id in payload".into(),
            )
        }
    };
    let remote_id = match parsed.get("remote_id").and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => {
            return Outcome::DropAndLog("dependency delete: missing remote_id in payload".into())
        }
    };
    match dependencies::delete(client, team_remote_id, issue_remote_id, remote_id).await {
        Ok(_) => Outcome::Success,
        Err(e) => classify_in_team(&e, team_remote_id),
    }
}

// ── Error classification ──────────────────────────────────────────────────

/// Rails' message for a 404 on the team scope (unknown, archived, or outside
/// the token). Resource 404s ("Issue not found", ...) use different messages.
const TEAM_ACCESS_MESSAGE: &str = "Team not found or access denied";

fn is_team_access_message(m: &str) -> bool {
    m.trim() == TEAM_ACCESS_MESSAGE
}

/// Rails' 403 message when the API token lacks the scope for the request
/// (base_controller `authorize_api_token_scope!`). Other 403s carry
/// per-record messages.
const TOKEN_SCOPE_MESSAGE: &str = "Token lacks required scope";

fn is_token_scope_message(m: &str) -> bool {
    m.trim() == TOKEN_SCOPE_MESSAGE
}

/// Like [`classify`], but a team-access 404 flags `team_remote_id` as lost.
fn classify_in_team(err: &ApiError, team_remote_id: i64) -> Outcome {
    match err {
        ApiError::NotFound(m) if is_team_access_message(m) => {
            Outcome::TeamAccessLost(team_remote_id, format!("team access lost: {m}"))
        }
        other => classify(other),
    }
}

/// True when the entry belongs to a team currently flagged inaccessible.
fn entry_team_is_inaccessible(entry: &SyncQueueEntry) -> bool {
    let Ok(conn) = open() else { return false };
    match entry_team_remote_id(&conn, entry) {
        Some(rid) => queries::team_access_lost(&conn, rid).unwrap_or(false),
        None => false,
    }
}

fn entry_team_remote_id(conn: &Connection, entry: &SyncQueueEntry) -> Option<i64> {
    let team_remote = |team_id: &str| {
        queries::remote_id_for(conn, "teams", team_id)
            .ok()
            .flatten()
    };
    let id = entry.entity_id.as_str();
    match (entry.entity_type.as_str(), entry.action.as_str()) {
        (_, "delete") => {
            let v: serde_json::Value = serde_json::from_str(entry.payload.as_deref()?).ok()?;
            v.get("team_remote_id")?.as_i64()
        }
        ("issue", _) => team_remote(&queries::get_issue(conn, id).ok()??.team_id),
        ("project", _) => team_remote(&queries::get_project(conn, id).ok()??.team_id),
        ("label", _) => team_remote(&queries::get_label(conn, id).ok()??.team_id),
        ("recurring_issue", _) => {
            team_remote(&crate::db::recurring_issues::get(conn, id).ok()??.team_id)
        }
        ("saved_view", "create") => {
            let team_id: String = conn
                .query_row("SELECT team_id FROM saved_views WHERE id = ?1", [id], |r| r.get(0))
                .ok()?;
            team_remote(&team_id)
        }
        ("comment", _) => {
            let c = queries::get_comment(conn, id).ok()??;
            let team_id = match (&c.issue_id, &c.project_id) {
                (Some(i), _) => queries::get_issue(conn, i).ok()??.team_id,
                (None, Some(p)) => queries::get_project(conn, p).ok()??.team_id,
                (None, None) => return None,
            };
            team_remote(&team_id)
        }
        ("dependency", _) => {
            let d = queries::get_issue_dependency(conn, id).ok()??;
            team_remote(
                &queries::get_issue(conn, &d.blocking_issue_id)
                    .ok()??
                    .team_id,
            )
        }
        _ => None,
    }
}

fn classify(err: &ApiError) -> Outcome {
    match err {
        ApiError::Server { status, message } => {
            if *status >= 500 {
                Outcome::LeavePending(format!("server {status}: {message}"))
            } else {
                Outcome::DropAndLog(format!("server {status}: {message}"))
            }
        }
        ApiError::Network(e) => Outcome::LeavePending(format!("network: {e}")),
        ApiError::MissingToken => Outcome::LeavePending("missing api token".into()),
        ApiError::Validation { errors } => {
            Outcome::DropAndLog(format!("validation: {}", errors.join("; ")))
        }
        // Keep the entry pending: an expired/revoked token is recoverable — the
        // user re-enters one in Settings (sync pauses meanwhile) and the next
        // drain pushes the still-queued edit rather than silently losing it.
        ApiError::Unauthorized(m) => Outcome::LeavePending(format!("unauthorized: {m}")),
        // Only Rails' "Token lacks required scope" 403 means the whole token
        // cannot write: keep it queued and stop pushing; a better token can
        // send it later. Any other 403 (e.g. "Only the author or a team admin
        // can ...") is about this one entry: keep it pending, never drop it,
        // and carry on with the rest of the queue.
        ApiError::Forbidden(m) if is_token_scope_message(m) => {
            Outcome::PausePushes(PushPause::Forbidden, format!("forbidden: {m}"))
        }
        ApiError::Forbidden(m) => Outcome::LeavePending(format!("forbidden: {m}")),
        ApiError::NotFound(m) => Outcome::DropAndLog(format!("not found: {m}")),
        ApiError::Decode(m) => Outcome::DropAndLog(format!("decode: {m}")),
        ApiError::EnumMapping(m) => Outcome::DropAndLog(format!("enum: {m}")),
    }
}

// ── Pull side ─────────────────────────────────────────────────────────────
//
// Walks resources in FK-dependency order and merges each row into local SQLite
// using a last-write-wins policy keyed on `updated_at`.
//
// Order: teams → lanes / labels / members → projects → issues. Issue
// dependencies come from each issue-detail payload. Rails also has
// `GET .../issues/:id/issue_dependencies` (see docs/rails-api.md), but the
// detail payload already carries the same links, so pull does not call it.
//
// The merge rule per row (see `decide`):
// - No local row with this `remote_id` → INSERT.
// - Local row exists, no pending sync_queue entry → OVERWRITE from remote.
// - Local row exists, pending entry, local.updated_at > remote.updated_at →
//   keep local; the next push will overwrite Rails.
// - Otherwise → OVERWRITE from remote.

#[derive(Debug, PartialEq, Eq)]
enum MergeDecision {
    Overwrite,
    KeepLocal,
}

fn decide(local_updated: &str, remote_updated: &str, has_pending: bool) -> MergeDecision {
    if !has_pending {
        return MergeDecision::Overwrite;
    }
    let l = chrono::DateTime::parse_from_rfc3339(local_updated);
    let r = chrono::DateTime::parse_from_rfc3339(remote_updated);
    match (l, r) {
        (Ok(l), Ok(r)) if l > r => MergeDecision::KeepLocal,
        // If either timestamp doesn't parse, treat remote as authoritative —
        // the data store is the source of truth when local clocks are off.
        _ => MergeDecision::Overwrite,
    }
}

/// Result of a pull that includes optional bookkeeping outside the row counts.
pub struct PullOutcome {
    pub report: SyncReport,
    pub deleted_orphan_team: Option<String>,
    /// Signed-in user's `(rails_id, name)` from `/me`, when the call succeeded.
    /// The app persists this to config to stamp `creator_id` on new issues.
    pub current_user: Option<(i64, String)>,
    /// Whether `/me` reported `write` scope (`None` if `/me` failed).
    pub can_write: Option<bool>,
}

impl SyncEngine {
    /// Pull all resources from Rails into the local DB. The set of teams to
    /// fully sync is determined by the token's scope:
    ///
    /// - **Unscoped token** (`token.team_ids` null/absent, no `team_id`) — pull
    ///   every local non-archived team, then for each team pull lanes/labels/
    ///   members/projects/issues.
    /// - **Scoped token** (`token.team_ids` list, or legacy `team_id`) — pull
    ///   the intersection of that list with the local teams.
    ///
    /// Returns a [`PullOutcome`] summarizing row counts plus the local UUID of
    /// the locally-seeded "My Project" team if pull determined it was redundant
    /// (the caller uses this to repoint `selected_team_id`).
    pub async fn pull(client: &ApiClient, ctx: &CycleCtx) -> PullOutcome {
        let mut report = SyncReport::default();
        let mut deleted_orphan_team = None;

        // Phase 1: the token's scope, from the cycle's single `/me` call.
        let me = match &ctx.me {
            Ok(m) => m,
            Err(e) => {
                let msg = format!("/me: {e}");
                sync_log("phase=pull", "fatal", &msg);
                report.failed += 1;
                note_pull_failure(&mut report, &msg);
                report.last_error = Some(msg);
                return PullOutcome {
                    report,
                    deleted_orphan_team,
                    current_user: None,
                    can_write: None,
                };
            }
        };
        let current_user = Some((me.id, me.name.clone()));
        let can_write = Some(me.token.has_write_scope());

        // The Home page rides on the same `/me` call; a failure here must not
        // block the rest of the pull.
        if let Err(e) = open().and_then(|c| store_home_page(&c, me).map_err(|e| e.to_string())) {
            sync_log("phase=pull", "home_page", &e);
        }

        // Phase 2: refresh teams (and discover any new ones the token can see).
        match pull_teams(client).await {
            Ok(orphan) => {
                deleted_orphan_team = orphan;
                report.processed += 1;
            }
            Err(msg) => {
                report.failed += 1;
                note_pull_failure(&mut report, &msg);
                report.last_error = Some(msg);
                return PullOutcome {
                    report,
                    deleted_orphan_team,
                    current_user,
                    can_write,
                };
            }
        }

        // Phase 2b: the user's Saved views (cross-team, own cursor). Needs the
        // teams just refreshed; a failure doesn't block the per-team pulls.
        match pull_saved_views_step(client).await {
            Ok(()) => report.processed += 1,
            Err(msg) => {
                report.failed += 1;
                note_pull_failure(&mut report, &msg);
                report.last_error = Some(msg);
            }
        }

        // Phase 2c: Saved views deleted on the server (user-level feed).
        match pull_saved_view_tombstones_step(client).await {
            Ok(()) => report.processed += 1,
            Err(msg) => {
                report.failed += 1;
                note_pull_failure(&mut report, &msg);
                report.last_error = Some(msg);
            }
        }

        // Phase 3: figure out which teams to deeply sync.
        let targets = match collect_pull_targets(me.token.scoped_team_ids().as_deref()) {
            Ok(t) => t,
            Err(msg) => {
                report.last_error = Some(msg);
                report.failed += 1;
                return PullOutcome {
                    report,
                    deleted_orphan_team,
                    current_user,
                    can_write,
                };
            }
        };
        if targets.is_empty() {
            // Token has access but no teams visible (or team-scoped to a team
            // we couldn't find locally after pull_teams — shouldn't happen).
            return PullOutcome {
                report,
                deleted_orphan_team,
                current_user,
                can_write,
            };
        }

        // Phase 4: per-team resources. Each helper logs and updates the report
        // independently so one team's failure doesn't block the others.
        let mut issue_pulls: Vec<IssueTeamPull> = Vec::new();
        let mut all_issue_pulls_ok = true;
        for target in &targets {
            let mut steps = vec![
                pull_lanes_step(client, target.remote_id, &target.local_id).await,
                pull_labels_step(client, target.remote_id, &target.local_id).await,
                pull_members_step(client, target.remote_id, &target.local_id).await,
                pull_projects_step(client, target.remote_id, &target.local_id).await,
                // Before issues, so a spawned issue can resolve its recurring issue.
                pull_recurring_issues_step(client, target.remote_id, &target.local_id).await,
            ];
            match pull_issues_step(client, target.remote_id, &target.local_id).await {
                Ok(pull) => {
                    issue_pulls.push(pull);
                    steps.push(Ok(()));
                }
                Err(e) => {
                    all_issue_pulls_ok = false;
                    steps.push(Err(e));
                }
            }
            // Tombstones run before comments; the comment ids they removed
            // are handed on so a feed row for one cannot resurrect it.
            let mut deleted_comments = HashSet::new();
            match pull_tombstones_step(client, target.remote_id, &target.local_id).await {
                Ok(ids) => {
                    deleted_comments = ids;
                    steps.push(Ok(()));
                }
                Err(e) => steps.push(Err(e)),
            }
            steps.push(
                pull_comments_step(
                    client,
                    target.remote_id,
                    &target.local_id,
                    &deleted_comments,
                )
                .await,
            );
            for step in steps {
                match step {
                    Ok(()) => report.processed += 1,
                    Err(msg) => {
                        report.failed += 1;
                        note_pull_failure(&mut report, &msg);
                        report.last_error = Some(msg);
                    }
                }
            }
        }

        // A targeted pull (some teams not covered) can't tell "moved to an
        // unpulled team" from "gone", so archiving needs every team pulled.
        let covers_all_teams = collect_pull_targets(None)
            .map(|all| all.len() == targets.len())
            .unwrap_or(false);
        if let Ok(conn) = open() {
            archive_unlisted_issues(&conn, &issue_pulls, all_issue_pulls_ok && covers_all_teams);
        }

        PullOutcome {
            report,
            deleted_orphan_team,
            current_user,
            can_write,
        }
    }
}

/// Persist the Home page `/me` reported (null clears it).
fn store_home_page(conn: &Connection, me: &MeDto) -> rusqlite::Result<()> {
    crate::db::home_page::apply_me(conn, me.home_page.as_ref())
}

struct PullTarget {
    local_id: String,
    remote_id: i64,
}

fn collect_pull_targets(scoped_ids: Option<&[i64]>) -> Result<Vec<PullTarget>, String> {
    let conn = open()?;
    select_pull_targets(&conn, scoped_ids)
}

/// Choose the teams to deep-sync. `None` (unscoped token) covers every local
/// non-archived Rails-backed team; `Some(ids)` covers the intersection of those
/// remote ids with the local non-archived teams (empty list means none).
fn select_pull_targets(
    conn: &Connection,
    scoped_ids: Option<&[i64]>,
) -> Result<Vec<PullTarget>, String> {
    let teams = queries::all_teams(conn).map_err(|e| format!("db (all_teams): {e}"))?;
    Ok(teams
        .into_iter()
        .filter(|t| t.archived_at.is_none())
        .filter_map(|t| {
            let rid = t.remote_id?;
            if let Some(ids) = scoped_ids {
                if !ids.contains(&rid) {
                    return None;
                }
            }
            Some(PullTarget {
                local_id: t.id,
                remote_id: rid,
            })
        })
        .collect())
}

/// Returns the local UUID of the orphan seed team if one was cleaned up.
async fn pull_teams(client: &ApiClient) -> Result<Option<String>, String> {
    let dtos = teams::list(client)
        .await
        .map_err(|e| format!("teams: {e}"))?;

    let active_ids: Vec<i64> = dtos.iter().map(|d| d.id).collect();
    let inserted_any_remote_team = !dtos.is_empty();

    let conn = open()?;
    // A team Rails lists is accessible again: lift any inaccessible flag.
    for id in &active_ids {
        let _ = queries::set_team_access_lost(&conn, *id, false);
    }
    for dto in dtos {
        let existing = queries::find_team_by_remote_id(&conn, dto.id)
            .map_err(|e| format!("db (find team): {e}"))?;
        let has_pending = match &existing {
            Some(t) => sync_queue::has_pending_entry(&conn, "team", &t.id)
                .map_err(|e| format!("db: {e}"))?,
            None => false,
        };

        let remote_updated = dto.updated_at.clone().unwrap_or_default();

        if let Some(local) = &existing {
            if decide(&local.updated_at, &remote_updated, has_pending) == MergeDecision::KeepLocal {
                continue;
            }
        }

        let newly_mirrored = existing.is_none();
        let team = crate::db::models::Team::from(dto.clone());
        let team = if let Some(local) = existing {
            // Reuse the local UUID so foreign keys (lanes/issues/etc.) stay valid.
            crate::db::models::Team {
                id: local.id,
                ..team
            }
        } else {
            team
        };
        queries::upsert_team(&conn, &team).map_err(|e| format!("db (upsert team): {e}"))?;
        if newly_mirrored {
            // Saved views pulled for it earlier were skipped; refetch them all.
            crate::db::saved_views::on_team_mirrored(&conn)
                .map_err(|e| format!("db (saved views cursor): {e}"))?;
        }
    }
    drop(conn);

    let conn = open()?;
    let _ = queries::archive_missing_teams(&conn, &active_ids);

    // Once we've confirmed Rails has at least one team for this token, the
    // locally-seeded "My Project" placeholder is redundant. Cleanup is gated
    // on "no issues attached" so we never destroy real local-only work.
    let deleted_orphan = if inserted_any_remote_team {
        match queries::cleanup_orphan_seed_team(&conn) {
            Ok(opt) => opt,
            Err(e) => {
                sync_log("phase=pull_teams", "cleanup_orphan_failed", &format!("db: {e}"));
                None
            }
        }
    } else {
        None
    };

    Ok(deleted_orphan)
}

/// Conditional (ETag) pull shared by the lane, label and member lists: send
/// the stored ETag, skip on 304, merge each row by the usual pending-edit rule
/// (`decide`), then store the new ETag only after every row was applied.
///
/// - `entity` is the sync-queue entity type (`"lane"`), `list` the list and
///   ETag name (`"lanes"`).
/// - `fetch` performs the conditional GET with the stored ETag.
/// - `find` returns the local row for a DTO as `(local id, local updated_at)`.
/// - `store` converts the DTO (reusing the local id when there is one) and
///   upserts it under `local_team_id`.
#[allow(clippy::too_many_arguments)]
async fn pull_etag_list<D, Fut>(
    entity: &str,
    list: &str,
    team_remote_id: i64,
    local_team_id: &str,
    fetch: impl FnOnce(Option<String>) -> Fut,
    set_team: impl Fn(&mut D, i64),
    remote_updated: impl Fn(&D) -> String,
    find: impl Fn(&Connection, &D) -> Result<Option<(String, String)>, String>,
    store: impl Fn(&Connection, D, Option<String>, &str) -> Result<(), String>,
) -> Result<(), String>
where
    Fut: std::future::Future<Output = Result<Conditional<Vec<D>>, ApiError>>,
{
    let stored = {
        let conn = open()?;
        queries::get_list_etag(&conn, local_team_id, list).unwrap_or(None)
    };
    let (dtos, new_etag) = match fetch(stored)
        .await
        .map_err(|e| format!("{list}: {e}"))?
    {
        Conditional::NotModified => {
            sync_log(
                &phase_scope(&format!("pull_{list}"), Some(team_remote_id)),
                "not_modified",
                "304",
            );
            return Ok(());
        }
        Conditional::Modified { data, etag } => (data, etag),
    };

    let conn = open()?;
    for mut dto in dtos {
        // Rails omits team_id on the wire; inject from the URL path.
        set_team(&mut dto, team_remote_id);

        let existing = find(&conn, &dto)?;
        let has_pending = match &existing {
            Some((local_id, _)) => sync_queue::has_pending_entry(&conn, entity, local_id)
                .map_err(|e| format!("db: {e}"))?,
            None => false,
        };

        if let Some((_, local_updated)) = &existing {
            if decide(local_updated, &remote_updated(&dto), has_pending)
                == MergeDecision::KeepLocal
            {
                continue;
            }
        }
        store(&conn, dto, existing.map(|(id, _)| id), local_team_id)?;
    }
    store_list_etag(&conn, local_team_id, list, new_etag.as_deref());
    Ok(())
}

async fn pull_lanes_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    // Lanes table has no archived_at column, so missing lanes are left in
    // place. Hard-deleting would risk cascading FK violations on issues that
    // still reference the lane locally. Revisit when the schema gains a
    // soft-delete column.
    pull_etag_list(
        "lane",
        "lanes",
        team_remote_id,
        local_team_id,
        |etag| async move { lanes::list_conditional(client, team_remote_id, etag.as_deref()).await },
        |dto, team| dto.team_id = team,
        |dto| dto.updated_at.clone().unwrap_or_default(),
        |conn, dto| {
            let found = queries::find_lane_by_remote_id(conn, dto.id)
                .map_err(|e| format!("db (find lane): {e}"))?;
            Ok(found.map(|l| (l.id, l.updated_at)))
        },
        |conn, dto, existing, local_team_id| {
            let mut lane = dto
                .into_model(conn, existing)
                .map_err(|e| format!("convert lane: {e}"))?;
            lane.team_id = local_team_id.to_string();
            queries::upsert_lane(conn, &lane).map_err(|e| format!("db (upsert lane): {e}"))
        },
    )
    .await
}

async fn pull_labels_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    pull_etag_list(
        "label",
        "labels",
        team_remote_id,
        local_team_id,
        |etag| async move { labels::list_conditional(client, team_remote_id, etag.as_deref()).await },
        |dto, team| dto.team_id = team,
        |dto| dto.updated_at.clone().unwrap_or_default(),
        |conn, dto| {
            let found = queries::find_label_by_remote_id(conn, dto.id)
                .map_err(|e| format!("db (find label): {e}"))?;
            Ok(found.map(|l| (l.id, l.updated_at)))
        },
        |conn, dto, existing, local_team_id| {
            let mut label = dto
                .into_model(conn, existing)
                .map_err(|e| format!("convert label: {e}"))?;
            label.team_id = local_team_id.to_string();
            queries::upsert_label(conn, &label).map_err(|e| format!("db (upsert label): {e}"))
        },
    )
    .await
}

async fn pull_members_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    pull_etag_list(
        "member",
        "members",
        team_remote_id,
        local_team_id,
        |etag| async move { members::list_conditional(client, team_remote_id, etag.as_deref()).await },
        |dto, team| dto.team_id = team,
        |dto| dto.updated_at.clone().unwrap_or_default(),
        |conn, dto| {
            let found = queries::find_member_by_remote_id(conn, dto.id)
                .map_err(|e| format!("db (find member): {e}"))?;
            Ok(found.map(|m| (m.id, m.updated_at)))
        },
        |conn, dto, existing, local_team_id| {
            let mut member = dto
                .into_model(conn, existing)
                .map_err(|e| format!("convert member: {e}"))?;
            member.team_id = local_team_id.to_string();
            queries::upsert_member(conn, &member).map_err(|e| format!("db (upsert member): {e}"))
        },
    )
    .await
}

/// Persist the ETag only after the list was fully applied, so a failed apply
/// is retried with a full fetch. A response without an ETag clears the stored one.
fn store_list_etag(conn: &Connection, team_id: &str, list: &str, etag: Option<&str>) {
    let res = match etag {
        Some(tag) => queries::set_list_etag(conn, team_id, list, tag),
        None => queries::clear_list_etag(conn, team_id, list),
    };
    if let Err(e) = res {
        sync_log(
            &format!("phase=pull_{list}"),
            "etag_store_failed",
            &e.to_string(),
        );
    }
}

/// Pull the user's Saved views (cross-team). Incremental from the Saved-views
/// cursor minus the overlap; the cursor advances to `X-Server-Time` only when
/// every row was applied.
async fn pull_saved_views_step(client: &ApiClient) -> Result<(), String> {
    let since = {
        let conn = open()?;
        crate::db::saved_views::cursor(&conn)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };
    let page = crate::api::saved_views::list_page(client, since.as_deref())
        .await
        .map_err(|e| format!("saved_views: {e}"))?;
    let conn = open()?;
    let applied = crate::db::saved_views::apply_pulled(&conn, &page.rows, page.server_time.as_deref())
        .map_err(|e| format!("db (saved views): {e}"))?;
    sync_log(
        "phase=pull_saved_views",
        "ok",
        &format!(
            "mode={} rows={} upserted={} skipped_unmirrored_team={} skipped_pending={}",
            pull_mode(&since),
            page.rows.len(),
            applied.upserted,
            applied.skipped_unmirrored,
            applied.skipped_pending
        ),
    );
    Ok(())
}

/// Pull the user-level tombstone feed and remove Saved views deleted on the
/// server. Own cursor; advances to `X-Server-Time` once the page is applied.
async fn pull_saved_view_tombstones_step(client: &ApiClient) -> Result<(), String> {
    let since = {
        let conn = open()?;
        crate::db::saved_views::deleted_cursor(&conn)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };
    let page = deleted::list_user(client, since.as_deref())
        .await
        .map_err(|e| format!("deleted (user): {e}"))?;
    let conn = open()?;
    let removed = crate::db::saved_views::apply_tombstones(&conn, &page.rows, page.server_time.as_deref())
        .map_err(|e| format!("db (saved view tombstones): {e}"))?;
    sync_log(
        "phase=pull_deleted_user",
        "ok",
        &format!("mode={} rows={} removed={removed}", pull_mode(&since), page.rows.len()),
    );
    Ok(())
}

/// Pull a team's recurring issues. `updated_since` = stored cursor minus the
/// overlap (the server filter is strict `>`); the first pull omits it. The
/// cursor advances to `X-Server-Time` only when the batch was applied.
async fn pull_recurring_issues_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    let since = {
        let conn = open()?;
        crate::db::recurring_issues::cursor(&conn, local_team_id)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };
    let page = crate::api::recurring_issues::list_page(client, team_remote_id, since.as_deref())
        .await
        .map_err(|e| format!("recurring_issues: {e}"))?;
    let conn = open()?;
    let applied = crate::db::recurring_issues::apply_pulled(
        &conn,
        local_team_id,
        &page.rows,
        page.server_time.as_deref(),
    )
    .map_err(|e| format!("db (recurring issues): {e}"))?;
    sync_log(
        &phase_scope("pull_recurring_issues", Some(team_remote_id)),
        "ok",
        &format!(
            "mode={} rows={} upserted={} skipped_pending={}",
            pull_mode(&since),
            page.rows.len(),
            applied.upserted,
            applied.skipped_pending
        ),
    );
    Ok(())
}

/// Cursor resource name for the project index in `pull_cursors`.
const PROJECTS_CURSOR: &str = "projects";

/// Pull a team's projects. With a stored cursor only rows changed since
/// (cursor minus a small overlap) are fetched; the first sync of a team has
/// no cursor and fetches every project. The cursor advances to the response's
/// `X-Server-Time`, and only when every row was applied.
async fn pull_projects_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    let since = {
        let conn = open()?;
        queries::get_pull_cursor(&conn, local_team_id, PROJECTS_CURSOR)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };
    let page = projects::list_page(client, team_remote_id, since.as_deref())
        .await
        .map_err(|e| format!("projects: {e}"))?;
    sync_log(
        &phase_scope("pull_projects", Some(team_remote_id)),
        "ok",
        &format!("mode={} rows={}", pull_mode(&since), page.rows.len()),
    );

    let conn = open()?;
    apply_pulled_projects(&conn, local_team_id, page.rows, page.server_time.as_deref())
}

/// Write pulled project rows (idempotent, so overlap re-delivery is safe),
/// then advance the cursor. An error leaves the cursor so rows are retried.
fn apply_pulled_projects(
    conn: &Connection,
    local_team_id: &str,
    dtos: Vec<ProjectDto>,
    server_time: Option<&str>,
) -> Result<(), String> {
    for dto in dedupe_by_id(dtos, |d| d.id) {
        let existing = queries::find_project_by_remote_id(conn, dto.id)
            .map_err(|e| format!("db (find project): {e}"))?;
        let has_pending = match &existing {
            Some(p) => sync_queue::has_pending_entry(conn, "project", &p.id)
                .map_err(|e| format!("db: {e}"))?,
            None => false,
        };

        if let Some(local) = &existing {
            if decide(&local.updated_at, &dto.updated_at, has_pending) == MergeDecision::KeepLocal {
                continue;
            }
        }

        let existing_uuid = existing.as_ref().map(|p| p.id.clone());
        let project: Project = dto
            .into_model(conn, local_team_id.to_string(), existing_uuid)
            .map_err(|e| format!("convert project: {e}"))?;
        queries::upsert_project(conn, &project)
            .map_err(|e| format!("db (upsert project): {e}"))?;
    }
    // project_labels join not populated in this ticket; documented as TODO.
    if let Some(t) = server_time {
        queries::set_pull_cursor(conn, local_team_id, PROJECTS_CURSOR, t)
            .map_err(|e| format!("db (pull cursor): {e}"))?;
    }
    Ok(())
}

/// Cursor resource name for the issue index in `pull_cursors`.
const ISSUES_CURSOR: &str = "issues";

/// The request for one page of the issue pull. A full pull (no cursor) is the
/// same request as an incremental one minus `updated_since`: detail rows,
/// archived issues included, so `archived_at` on the rows drives archive state.
fn issue_pull_query(since: Option<&str>, after_id: Option<i64>) -> issues::IssuePageQuery {
    issues::IssuePageQuery {
        updated_since: since.map(str::to_string),
        include_detail: true,
        include_archived: true,
        limit: Some(cursor::PAGE_LIMIT),
        after_id,
    }
}

/// Pull a team's issues with full detail in paged requests. With a stored
/// cursor only rows changed since (cursor minus a small overlap) are fetched;
/// without one the whole team is fetched the same way. The cursor advances to
/// the first page's `X-Server-Time`, and only when every row was applied.
async fn pull_issues_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<IssueTeamPull, String> {
    let since = {
        let conn = open()?;
        queries::get_pull_cursor(&conn, local_team_id, ISSUES_CURSOR)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };

    let collected = cursor::collect_pages(|after_id| {
        let query = issue_pull_query(since.as_deref(), after_id);
        async move { issues::list_page(client, team_remote_id, &query).await }
    })
    .await
    .map_err(|e| format!("issues: {e}"))?;
    sync_log(
        &phase_scope("pull_issues", Some(team_remote_id)),
        "ok",
        &format!(
            "mode={} requests={} rows={}",
            pull_mode(&since),
            collected.requests,
            collected.rows.len()
        ),
    );

    let dtos = dedupe_by_id(collected.rows, |d| d.id);
    let total = dtos.len();
    let conn = open()?;
    let skipped = apply_pulled_issues(&conn, local_team_id, dtos.clone());

    if skipped > 0 {
        // Leave the cursor where it was so the failed rows are retried.
        return Err(format!(
            "issues: {skipped}/{total} skipped (see [sync] log)"
        ));
    }
    if let Some(t) = &collected.server_time {
        queries::set_pull_cursor(&conn, local_team_id, ISSUES_CURSOR, t)
            .map_err(|e| format!("db (pull cursor): {e}"))?;
    }
    Ok(IssueTeamPull {
        local_team_id: local_team_id.to_string(),
        full_listing: since.is_none(),
        seen: dtos.iter().map(|d| d.id).collect(),
    })
}

/// What one team's issue pull saw in this cycle.
struct IssueTeamPull {
    local_team_id: String,
    /// True when the whole team was listed (no cursor), so absence means gone.
    full_listing: bool,
    seen: Vec<i64>,
}

/// Archive local issues a full listing no longer returns (deleted or out of
/// reach; full listings carry their own `archived_at` for archived rows). An
/// issue moved to another team on the web vanishes from its source listing, so
/// absence is judged against every issue seen this cycle, and only when every
/// team's issue pull succeeded (`complete`): otherwise the target team may
/// simply not have been pulled yet.
fn archive_unlisted_issues(conn: &Connection, pulls: &[IssueTeamPull], complete: bool) {
    if !complete {
        return;
    }
    let seen: Vec<i64> = pulls.iter().flat_map(|p| p.seen.iter().copied()).collect();
    for p in pulls.iter().filter(|p| p.full_listing) {
        let _ = queries::archive_missing_issues(conn, &p.local_team_id, &seen);
    }
}

/// Cursor resource name for the tombstone feed in `pull_cursors`.
const DELETED_CURSOR: &str = "deleted";

/// Pull the team's deleted-records feed and apply it. Runs after the issue
/// pull. The cursor advances to the response's `X-Server-Time` only when every
/// entry was applied or deliberately ignored.
async fn pull_tombstones_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<HashSet<i64>, String> {
    let since = {
        let conn = open()?;
        queries::get_pull_cursor(&conn, local_team_id, DELETED_CURSOR)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };
    let page = deleted::list(client, team_remote_id, since.as_deref())
        .await
        .map_err(|e| format!("deleted: {e}"))?;
    sync_log(
        &phase_scope("pull_deleted", Some(team_remote_id)),
        "ok",
        &format!("mode={} rows={}", pull_mode(&since), page.rows.len()),
    );
    let conn = open()?;
    let failed = apply_tombstones(&conn, local_team_id, &page.rows);
    let deleted_comments = tombstoned_comment_ids(&page.rows);
    if failed > 0 {
        return Err(format!("deleted: {failed} failed (see [sync] log)"));
    }
    if let Some(t) = &page.server_time {
        queries::set_pull_cursor(&conn, local_team_id, DELETED_CURSOR, t)
            .map_err(|e| format!("db (pull cursor): {e}"))?;
    }
    Ok(deleted_comments)
}

/// Remote ids of the comments this feed page deleted.
fn tombstoned_comment_ids(rows: &[TombstoneDto]) -> HashSet<i64> {
    rows.iter()
        .filter(|r| r.record_type == "Comment")
        .map(|r| r.record_id)
        .collect()
}

/// Cursor resource name for the team comments feed in `pull_cursors`.
const COMMENTS_CURSOR: &str = "comments";

/// Pull a team's comments from the flat team feed. Same cursor discipline as
/// the issue pull: overlap on the stored cursor, dedupe, idempotent apply, and
/// the cursor moves to the response's `X-Server-Time` only if nothing failed.
/// Issues are pulled first so comments find their issue.
async fn pull_comments_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
    deleted_comments: &HashSet<i64>,
) -> Result<(), String> {
    let since = {
        let conn = open()?;
        queries::get_pull_cursor(&conn, local_team_id, COMMENTS_CURSOR)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };
    let page = comments::list_team_updated_since(client, team_remote_id, since.as_deref())
        .await
        .map_err(|e| format!("comments: {e}"))?;
    sync_log(
        &phase_scope("pull_comments", Some(team_remote_id)),
        "ok",
        &format!("mode={} rows={}", pull_mode(&since), page.rows.len()),
    );
    let dtos = drop_deleted_comments(dedupe_by_id(page.rows, |d| d.id), deleted_comments);
    let total = dtos.len();
    let conn = open()?;
    let failed = apply_pulled_comments(&conn, dtos);
    if failed > 0 {
        return Err(format!("comments: {failed}/{total} failed (see [sync] log)"));
    }
    if let Some(t) = &page.server_time {
        queries::set_pull_cursor(&conn, local_team_id, COMMENTS_CURSOR, t)
            .map_err(|e| format!("db (pull cursor): {e}"))?;
    }
    Ok(())
}

/// Drop feed comments the tombstone pass just deleted, along with replies to
/// them (their parent is gone, so they would come back as top-level).
fn drop_deleted_comments(dtos: Vec<CommentDto>, deleted: &HashSet<i64>) -> Vec<CommentDto> {
    if deleted.is_empty() {
        return dtos;
    }
    let mut gone = deleted.clone();
    loop {
        let before = gone.len();
        for d in &dtos {
            if d.parent_id.is_some_and(|p| gone.contains(&p)) {
                gone.insert(d.id);
            }
        }
        if gone.len() == before {
            break;
        }
    }
    dtos.into_iter().filter(|d| !gone.contains(&d.id)).collect()
}

/// Apply feed entries to the local DB (idempotent, so the cursor overlap is
/// safe). Handles Issue, Comment, Label, Project and
/// RecurringIssue; other record types are ignored. Returns how many entries
/// failed to apply.
fn apply_tombstones(conn: &Connection, local_team_id: &str, rows: &[TombstoneDto]) -> usize {
    let mut failed = 0;
    for row in rows {
        let id = row.record_id;
        let result = match row.record_type.as_str() {
            "Issue" => queries::apply_server_issue_tombstone(conn, local_team_id, id),
            "Comment" => queries::apply_server_comment_tombstone(conn, local_team_id, id),
            "Label" => queries::apply_server_label_tombstone(conn, local_team_id, id),
            "Project" => queries::apply_server_project_tombstone(conn, local_team_id, id),
            "IssueDependency" => {
                queries::apply_server_dependency_tombstone(conn, local_team_id, id)
            }
            "RecurringIssue" => {
                crate::db::recurring_issues::apply_tombstone(conn, local_team_id, id)
            }
            _ => continue,
        };
        if let Err(e) = result {
            sync_log(
                &format!("phase=pull_deleted type={} remote_id={id}", row.record_type),
                "apply_failed",
                &e.to_string(),
            );
            failed += 1;
        }
    }
    failed
}

/// Write feed comments locally (idempotent). Comments on issues or projects we
/// do not have (or issues we have tombstoned) are ignored. Replies are applied
/// after their parents, however the feed ordered them; a parent that cannot be
/// found locally leaves the reply top-level rather than blocking the cursor.
/// Returns how many rows hit a DB error.
fn apply_pulled_comments(conn: &Connection, dtos: Vec<CommentDto>) -> usize {
    let mut failed = 0;
    let mut remaining: Vec<CommentDto> = dtos.into_iter().filter(|d| d.issue_id.is_some() || d.project_id.is_some()).collect();
    loop {
        let before = remaining.len();
        let mut deferred = Vec::new();
        for dto in remaining {
            // Wait for a parent that is in this batch but not applied yet.
            let parent_local = match dto.parent_id {
                None => None,
                Some(pid) => match queries::find_comment_by_remote_id(conn, pid) {
                    Ok(Some(p)) => Some(p.id),
                    Ok(None) => {
                        deferred.push(dto);
                        continue;
                    }
                    Err(_) => None,
                },
            };
            if apply_one_comment(conn, dto, parent_local).is_err() {
                failed += 1;
            }
        }
        remaining = deferred;
        if remaining.is_empty() || remaining.len() == before {
            break;
        }
    }
    // Whatever is left has a parent we never saw: store as top-level.
    for dto in remaining {
        sync_log(
            &format!("phase=pull_comments comment_remote_id={}", dto.id),
            "stored_top_level",
            &format!("parent {:?} not found locally", dto.parent_id),
        );
        if apply_one_comment(conn, dto, None).is_err() {
            failed += 1;
        }
    }
    failed
}

fn apply_one_comment(
    conn: &Connection,
    dto: CommentDto,
    parent_local: Option<String>,
) -> Result<(), String> {
    let owner = if let Some(issue_remote) = dto.issue_id {
        let issue = match queries::find_issue_by_remote_id(conn, issue_remote) {
            Ok(Some(i)) => i,
            Ok(None) => return Ok(()),
            Err(e) => return Err(format!("db (find issue): {e}")),
        };
        if queries::is_issue_tombstoned_by_remote_id(conn, issue_remote)
            .map_err(|e| format!("db (tombstone): {e}"))?
        {
            return Ok(());
        }
        CommentOwner::Issue(issue.id)
    } else if let Some(project_remote) = dto.project_id {
        match queries::find_project_by_remote_id(conn, project_remote) {
            Ok(Some(p)) => CommentOwner::Project(p.id),
            Ok(None) => return Ok(()),
            Err(e) => return Err(format!("db (find project): {e}")),
        }
    } else {
        return Ok(());
    };
    let existing = queries::find_comment_by_remote_id(conn, dto.id)
        .map_err(|e| format!("db (find comment): {e}"))?
        .map(|c| c.id);
    let comment = dto.into_model(conn, owner, existing, parent_local);
    queries::upsert_comment(conn, &comment).map_err(|e| format!("db (upsert comment): {e}"))
}

/// Keep one row per key (the last delivered wins), in first-seen order. The
/// cursor overlap re-delivers rows, and paging can repeat one across pages.
fn dedupe_by_id<T>(rows: Vec<T>, key: impl Fn(&T) -> i64) -> Vec<T> {
    let mut index: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    let mut out: Vec<T> = Vec::with_capacity(rows.len());
    for row in rows {
        match index.get(&key(&row)) {
            Some(&i) => out[i] = row,
            None => {
                index.insert(key(&row), out.len());
                out.push(row);
            }
        }
    }
    out
}

/// Write detailed issue rows to the local DB (idempotent, so overlap
/// re-delivery is safe). Returns how many rows failed to apply.
fn apply_pulled_issues(conn: &Connection, local_team_id: &str, dtos: Vec<IssueDto>) -> usize {
    let mut skipped = 0;
    // Dependencies are persisted after every issue in this batch exists
    // locally, since an edge may point at an issue later in the batch.
    let mut pending_links: Vec<(String, Option<Vec<PulledDependency>>)> = Vec::new();
    for dto in dtos {
        let issue_remote_id = dto.id;
        let result = (|| -> Result<bool, String> {
            if queries::is_issue_tombstoned_by_remote_id(conn, issue_remote_id)
                .map_err(|e| format!("db (tombstone): {e}"))?
            {
                return Ok(false);
            }
            let existing = queries::find_issue_by_remote_id(conn, issue_remote_id)
                .map_err(|e| format!("db (find issue): {e}"))?;
            if let Some(local) = &existing {
                let has_pending = sync_queue::has_pending_entry(conn, "issue", &local.id)
                    .map_err(|e| format!("db: {e}"))?;
                if decide(&local.updated_at, &dto.updated_at, has_pending)
                    == MergeDecision::KeepLocal
                {
                    return Ok(false);
                }
            }
            let links = dto.dependency_links();
            // Only a detail payload carries the full edge set; a row with no
            // dependency lists at all must not prune anything.
            let is_detail = dto.blocking_issues.is_some()
                || dto.blocked_issues.is_some()
                || dto.related_issues.is_some()
                || dto.duplicates.is_some();
            let dto = relax_issue_fks(conn, dto);
            let existing_uuid = existing.map(|i| i.id);
            let recurring_remote_id = dto.recurring_issue_id;
            // Capture the remote label ids before `into_model` consumes dto.
            let label_remote_ids: Vec<i64> = dto.labels.iter().map(|l| l.id).collect();
            let issue: Issue = dto
                .into_model(conn, local_team_id.to_string(), existing_uuid)
                .map_err(|e| format!("convert: {e}"))?;
            queries::upsert_issue(conn, &issue).map_err(|e| format!("db (upsert): {e}"))?;
            // Recurring issues are pulled before issues, so the origin
            // resolves locally; an unresolved id keeps the stored value.
            crate::db::recurring_issues::set_issue_origin(conn, &issue.id, recurring_remote_id)
                .map_err(|e| format!("db (recurring origin): {e}"))?;
            // Reflect the remote label set into the issue_labels join. Labels
            // are pulled before issues, so their local rows already exist; any
            // remote id we can't resolve locally is skipped defensively.
            let local_label_ids = resolve_local_label_ids(conn, &label_remote_ids)?;
            queries::set_issue_labels(conn, &issue.id, &local_label_ids)
                .map_err(|e| format!("db (set issue labels): {e}"))?;
            pending_links.push((issue.id.clone(), is_detail.then_some(links)));
            Ok(true)
        })();
        if let Err(e) = result {
            sync_log(
                &format!("phase=pull_issues issue_remote_id={issue_remote_id}"),
                "skipped",
                &e.to_string(),
            );
            skipped += 1;
        }
    }
    for (issue_id, links) in &pending_links {
        let Some(links) = links else { continue };
        persist_pulled_dependencies(conn, links);
        // The server's detail set is authoritative: drop synced edges it no
        // longer lists (Rails deletes blocking edges on completion).
        let keep: Vec<i64> = links.iter().map(|l| l.dependency_id).collect();
        if let Err(e) = queries::prune_issue_dependencies(conn, issue_id, &keep) {
            sync_log(
                &format!("phase=pull_issues issue_id={issue_id}"),
                "prune_dependencies_failed",
                &e.to_string(),
            );
        }
    }
    skipped
}

/// Apply one freshly fetched issue (detail row) through the same path the
/// pull uses, so local tombstones and pending local edits are respected
/// (`decide`). A row the pull would skip as unresolvable is an error.
fn apply_fetched_issue(conn: &Connection, local_team_id: &str, dto: IssueDto) -> Result<(), String> {
    match apply_pulled_issues(conn, local_team_id, vec![dto]) {
        0 => Ok(()),
        _ => Err("issue could not be applied locally (see [sync] log)".into()),
    }
}

impl SyncEngine {
    /// Opening an issue does one fresh fetch of that issue (with detail) and
    /// merges it exactly like a pulled row. Fetch errors are returned for the
    /// caller to ignore or surface; local state is untouched on error.
    pub async fn fetch_issue(
        client: &ApiClient,
        team_remote_id: i64,
        local_team_id: &str,
        issue_remote_id: i64,
    ) -> Result<(), String> {
        let dto = issues::get(client, team_remote_id, issue_remote_id)
            .await
            .map_err(|e| format!("issue: {e}"))?;
        let conn = open()?;
        apply_fetched_issue(&conn, local_team_id, dto)
    }
}

/// Store pulled dependency edges. An edge whose endpoint issue isn't present
/// locally (e.g. an issue in another team) is skipped and logged.
fn persist_pulled_dependencies(conn: &Connection, links: &[PulledDependency]) {
    for l in links {
        let find = |remote| queries::find_issue_by_remote_id(conn, remote);
        let (source, target) = match (find(l.source_remote_id), find(l.target_remote_id)) {
            (Ok(Some(s)), Ok(Some(t))) => (s, t),
            _ => {
                sync_log(
                    &format!("phase=pull_issues dependency_remote_id={}", l.dependency_id),
                    "skipped",
                    "endpoint not found locally",
                );
                continue;
            }
        };
        if let Err(e) = queries::upsert_pulled_dependency(
            conn,
            l.dependency_id,
            &source.id,
            &target.id,
            l.kind,
            l.direction,
        ) {
            sync_log(
                &format!("phase=pull_issues dependency_remote_id={}", l.dependency_id),
                "write_failed",
                &e.to_string(),
            );
        }
    }
}

/// Null out optional FKs on an `IssueDto` whose referenced rows aren't present
/// locally. Lane is intentionally NOT relaxed — an issue without a lane has
/// nowhere to render, so the caller treats unresolvable lanes as a hard error.
///
/// This is needed because Rails returns issue creators / assignees that
/// reference users who may not be members of this team (e.g. an admin who
/// created the issue but isn't on the team's roster). Our local schema only
/// stores team members, so resolving those FKs strictly would fail.
/// Map a list of remote label ids to their local UUIDs, skipping any that have
/// no local row yet (labels are pulled before issues, so this should be rare).
fn resolve_local_label_ids(conn: &Connection, remote_ids: &[i64]) -> Result<Vec<String>, String> {
    let mut local = Vec::with_capacity(remote_ids.len());
    for remote_id in remote_ids {
        match queries::find_label_by_remote_id(conn, *remote_id) {
            Ok(Some(label)) => local.push(label.id),
            Ok(None) => {
                sync_log(
                    &format!("phase=pull_issues label_remote_id={remote_id}"),
                    "skipped",
                    "not found locally",
                );
            }
            Err(e) => return Err(format!("db (find label): {e}")),
        }
    }
    Ok(local)
}

fn relax_issue_fks(conn: &Connection, mut dto: IssueDto) -> IssueDto {
    if let Some(r) = &dto.assignee {
        if matches!(queries::find_member_by_remote_id(conn, r.id), Ok(None)) {
            dto.assignee = None;
        }
    }
    if let Some(r) = &dto.creator {
        if matches!(queries::find_member_by_remote_id(conn, r.id), Ok(None)) {
            dto.creator = None;
        }
    }
    if let Some(r) = &dto.project {
        if matches!(queries::find_project_by_remote_id(conn, r.id), Ok(None)) {
            dto.project = None;
        }
    }
    if let Some(r) = &dto.parent_issue {
        if matches!(queries::find_issue_by_remote_id(conn, r.id), Ok(None)) {
            dto.parent_issue = None;
        }
    }
    dto
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #[test]
    fn read_all_before_parses_payload() {
        assert_eq!(
            notification_read_all_before(Some(r#"{"before":"2026-10-31T09:10:00.000Z"}"#)).as_deref(),
            Some("2026-10-31T09:10:00.000Z")
        );
        assert_eq!(notification_read_all_before(Some("{}")), None);
        assert_eq!(notification_read_all_before(None), None);
    }

    #[test]
    fn notification_read_404_counts_as_done() {
        assert!(matches!(notification_read_outcome(Ok(())), Outcome::Success));
        assert!(matches!(
            notification_read_outcome(Err(ApiError::NotFound("Not found".into()))),
            Outcome::Success
        ));
        assert!(matches!(
            notification_read_outcome(Err(ApiError::Unauthorized("x".into()))),
            Outcome::LeavePending(_)
        ));
    }

    use super::*;

    fn failure(kind: SyncErrorKind) -> SyncFailure {
        SyncFailure {
            entity_type: "issue".into(),
            entity_id: "x".into(),
            kind,
            message: "m".into(),
        }
    }

    fn report_with(kind: Option<SyncErrorKind>) -> FullSyncReport {
        let mut r = FullSyncReport::default();
        if let Some(k) = kind {
            r.push.failed = 1;
            r.push.last_error = Some("e".into());
            r.push.failures.push(failure(k));
        }
        r
    }

    #[test]
    fn cycle_succeeded_reflects_server_health_for_backoff() {
        assert!(report_with(None).cycle_succeeded());
        assert!(!report_with(Some(SyncErrorKind::Network)).cycle_succeeded());
        assert!(!report_with(Some(SyncErrorKind::Other)).cycle_succeeded());
        assert!(!report_with(Some(SyncErrorKind::Auth)).cycle_succeeded());
        // Per-entry rejections don't mean the server is unhealthy.
        assert!(report_with(Some(SyncErrorKind::Forbidden)).cycle_succeeded());
        assert!(report_with(Some(SyncErrorKind::Validation(vec![]))).cycle_succeeded());
        // A read-only pause is a healthy cycle, not a failure.
        let mut ro = FullSyncReport::default();
        ro.push = read_only_report();
        ro.can_write = Some(false);
        assert!(ro.cycle_succeeded());
        // Pull error with no structured failure counts as unhealthy.
        let mut r = FullSyncReport::default();
        r.pull.last_error = Some("boom".into());
        assert!(!r.cycle_succeeded());
    }

    #[test]
    fn push_only_report_does_not_claim_a_pull() {
        let r = push_only_report(Some(true), SyncReport::default());
        assert!(r.push_only);
        assert_eq!(r.pull.failed, 0);
        assert!(r.deleted_orphan_team.is_none() && r.current_user.is_none());
        assert!(r.cycle_succeeded());
        // A read-only token never pushes and shows the banner.
        let ro = push_only_report(Some(false), read_only_report());
        assert!(ro.read_only());
        assert!(ro.cycle_succeeded());
    }

    fn ctx_with_scopes(scopes: &[&str]) -> CycleCtx {
        let me: MeDto = serde_json::from_value(serde_json::json!({
            "id": 7, "name": "Me", "email": "me@example.com",
            "token": { "name": "t", "scopes": scopes }
        }))
        .unwrap();
        CycleCtx::from_me(Ok(me))
    }

    #[test]
    fn cycle_ctx_reads_write_scope_from_the_one_me_result() {
        assert_eq!(ctx_with_scopes(&["read"]).can_write(), Some(false));
        assert_eq!(ctx_with_scopes(&["read", "write"]).can_write(), Some(true));
        assert_eq!(
            CycleCtx::from_me(Err(ApiError::MissingToken)).can_write(),
            None
        );
    }

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

    #[tokio::test]
    async fn read_only_token_never_drains() {
        let client = ApiClient::new("http://127.0.0.1:9", "t").unwrap();
        let r = SyncEngine::drain_if_writable(&client, &ctx_with_scopes(&["read"])).await;
        assert_eq!(r.push_paused, Some(PushPause::ReadOnly));
    }

    fn is_drop(o: &Outcome) -> bool {
        matches!(o, Outcome::DropAndLog(_))
    }
    fn is_retry(o: &Outcome) -> bool {
        matches!(o, Outcome::LeavePending(_))
    }

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

    /// A project (remote id `project_remote`) in team t1 (remote 700) with one
    /// top-level project comment "pc".
    fn project_comment_fixture(project_remote: Option<i64>) -> Connection {
        let conn = setup_labels_db();
        let remote = project_remote.map_or("NULL".to_string(), |r| r.to_string());
        exec(
            &conn,
            &format!(
                "INSERT INTO projects (id, remote_id, team_id, name, created_at, updated_at)
                 VALUES ('p1', {remote}, 't1', 'P', 'x', 'x')"
            ),
        );
        let c = crate::db::models::Comment {
            id: "pc".into(),
            remote_id: None,
            issue_id: None,
            project_id: Some("p1".into()),
            author_id: None,
            body: "hello".into(),
            parent_id: None,
            created_at: "x".into(),
            updated_at: "x".into(),
        };
        queries::insert_comment(&conn, &c).unwrap();
        conn
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
    fn classify_5xx_is_retry() {
        assert!(is_retry(&classify(&ApiError::Server {
            status: 500,
            message: "boom".into()
        })));
        assert!(is_retry(&classify(&ApiError::Server {
            status: 503,
            message: "boom".into()
        })));
    }

    #[test]
    fn classify_4xx_is_drop() {
        assert!(is_drop(&classify(&ApiError::Server {
            status: 418,
            message: "teapot".into()
        })));
        assert!(is_drop(&classify(&ApiError::NotFound("gone".into()))));
    }

    #[test]
    fn read_only_report_pauses_without_failures() {
        let r = read_only_report();
        assert_eq!(r.push_paused, Some(PushPause::ReadOnly));
        assert_eq!((r.processed, r.failed), (0, 0));
        assert!(r.last_error.is_none());
    }

    #[test]
    fn full_report_read_only_from_scope_or_403() {
        let mk = |can_write, push: SyncReport| FullSyncReport {
            pull: SyncReport::default(),
            push,
            deleted_orphan_team: None,
            current_user: None,
            can_write,
            push_only: false,
        };
        assert!(mk(Some(false), read_only_report()).read_only());
        assert!(mk(
            Some(true),
            SyncReport {
                push_paused: Some(PushPause::Forbidden),
                ..SyncReport::default()
            }
        )
        .read_only());
        // Write scope restored and no 403: state clears.
        assert!(!mk(Some(true), SyncReport::default()).read_only());
        assert!(!mk(None, SyncReport::default()).read_only());
    }

    #[test]
    fn classify_non_scope_forbidden_stays_pending_without_pausing() {
        for m in [
            "Only the author or a team admin can delete this",
            "Bootstrap token required",
        ] {
            match classify(&ApiError::Forbidden(m.into())) {
                Outcome::LeavePending(r) => assert_eq!(r, format!("forbidden: {m}")),
                other => panic!("expected leave-pending for {m:?}, got {other:?}"),
            }
        }
        let entry = SyncQueueEntry {
            id: Some(1),
            entity_type: "comment".into(),
            entity_id: "c1".into(),
            action: "delete".into(),
            payload: None,
            created_at: String::new(),
            synced_at: None,
        };
        let mut report = SyncReport::default();
        let outcome = classify(&ApiError::Forbidden("Only the author can".into()));
        assert!(!record(&mut report, &entry, 1, outcome));
        assert_eq!(report.push_paused, None);
    }

    #[test]
    fn classify_forbidden_keeps_pending_and_pauses() {
        match classify(&ApiError::Forbidden("Token lacks required scope".into())) {
            Outcome::PausePushes(PushPause::Forbidden, m) => assert!(m.starts_with("forbidden:")),
            other => panic!("expected pause, got {other:?}"),
        }
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
    fn team_access_404_is_told_apart_from_resource_404() {
        let team = ApiError::NotFound("Team not found or access denied".into());
        match classify_in_team(&team, 42) {
            Outcome::TeamAccessLost(42, m) => assert!(m.starts_with("team access lost:")),
            other => panic!("expected team access lost, got {other:?}"),
        }
        // Resource 404s keep today's drop-and-log.
        for m in ["Issue not found", "Project not found", "Label not found"] {
            let o = classify_in_team(&ApiError::NotFound(m.into()), 42);
            assert!(is_drop(&o), "{m} should drop, got {o:?}");
        }
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
            entry_team_remote_id(&conn, &mk("issue", "i1", "update", None)),
            Some(700)
        );
        assert_eq!(
            entry_team_remote_id(
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
    fn classify_unauthorized_is_retry() {
        // A 401 is recoverable: the user re-enters a token in Settings and the
        // still-pending entry drains on the next sync, so we must not drop it.
        assert!(is_retry(&classify(&ApiError::Unauthorized("nope".into()))));
    }

    #[test]
    fn push_reason_classifies_auth_and_validation() {
        assert_eq!(
            SyncErrorKind::from_push_reason("unauthorized: bad token"),
            Some(SyncErrorKind::Auth)
        );
        assert_eq!(
            SyncErrorKind::from_push_reason("validation: Title can't be blank; Priority invalid"),
            Some(SyncErrorKind::Validation(vec![
                "Title can't be blank".into(),
                "Priority invalid".into(),
            ]))
        );
        // Local/transient reasons carry no structured failure.
        assert_eq!(SyncErrorKind::from_push_reason("team not synced yet"), None);
        assert_eq!(SyncErrorKind::from_push_reason("db: locked"), None);
    }

    #[test]
    fn classify_validation_is_drop() {
        let e = ApiError::Validation {
            errors: vec!["title can't be blank".into()],
        };
        match classify(&e) {
            Outcome::DropAndLog(m) => assert!(m.contains("title can't be blank")),
            other => panic!("expected drop, got {other:?}"),
        }
    }

    #[test]
    fn home_page_422_is_dropped_and_logged_then_me_restores() {
        use crate::db::home_page;
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        home_page::set_local(&conn, "team", 3).unwrap();
        let entry = sync_queue::pending_entries(&conn).unwrap().remove(0);
        // The drain's view of the entry: payload parses, a 422 classifies as a drop.
        assert_eq!(home_page::parse_payload(entry.payload.as_deref()), Some(("team".into(), 3)));
        let e = ApiError::Validation { errors: vec!["not found".into()] };
        assert!(matches!(classify(&e), Outcome::DropAndLog(_)));
        sync_queue::mark_synced(&conn, entry.id.unwrap()).unwrap();
        home_page::apply_me(&conn, Some(&crate::api::HomePageDto { kind: "team".into(), id: 1 })).unwrap();
        assert_eq!(home_page::get(&conn).unwrap().unwrap().target, home_page::HomeTarget::Team(1));
    }

    #[test]
    fn a_view_created_offline_then_set_as_home_waits_then_resolves_after_its_create() {
        use crate::db::home_page::{self, Resolved};
        let (mut conn, id) = saved_view_conn();
        home_page::set_view_local(&conn, &id, None).unwrap();
        let pending = sync_queue::pending_entries(&conn).unwrap();
        // FIFO: the create drains before the home write.
        assert_eq!((pending[0].entity_type.as_str(), pending[1].entity_type.as_str()), ("saved_view", "home_page"));
        assert_eq!(home_page::resolve_payload(&conn, pending[1].payload.as_deref()), Resolved::Waiting);
        // The create pushes; the queued PUT now carries the view's remote id.
        commit_saved_view_create(&mut conn, pending[0].id.unwrap(), &id, &saved_view_dto(88)).unwrap();
        assert_eq!(
            home_page::resolve_payload(&conn, pending[1].payload.as_deref()),
            Resolved::Ready("view".into(), 88)
        );
        assert_eq!(home_page::get(&conn).unwrap().unwrap().target, home_page::HomeTarget::View(88));
    }

    #[test]
    fn a_5xx_leaves_the_home_page_write_pending() {
        let e = ApiError::Server { status: 500, message: "x".into() };
        assert!(matches!(classify(&e), Outcome::LeavePending(_)));
    }

    #[test]
    fn classify_missing_token_is_retry() {
        // MissingToken is recoverable: user can fix the token in settings and
        // the next drain will pick up.
        assert!(is_retry(&classify(&ApiError::MissingToken)));
    }

    #[test]
    fn decide_no_pending_overwrites() {
        // Without a pending push, remote is always authoritative — even if
        // local is newer (which means a previous push already drained).
        assert_eq!(
            decide("2026-04-27T12:00:00Z", "2026-04-27T11:00:00Z", false),
            MergeDecision::Overwrite
        );
    }

    #[test]
    fn decide_pending_and_local_newer_keeps_local() {
        assert_eq!(
            decide("2026-04-27T12:00:00Z", "2026-04-27T11:00:00Z", true),
            MergeDecision::KeepLocal
        );
    }

    #[test]
    fn decide_pending_but_remote_newer_overwrites() {
        // Concurrent edit: someone updated remote AFTER our local edit. LWW
        // means remote wins; the queued push will then overwrite again.
        assert_eq!(
            decide("2026-04-27T11:00:00Z", "2026-04-27T12:00:00Z", true),
            MergeDecision::Overwrite
        );
    }

    #[test]
    fn decide_unparseable_timestamps_overwrites() {
        // Defensive: if either timestamp is malformed, prefer remote rather
        // than risk dropping a real update because a clock string was off.
        assert_eq!(
            decide("not-a-date", "2026-04-27T12:00:00Z", true),
            MergeDecision::Overwrite
        );
        assert_eq!(
            decide("2026-04-27T12:00:00Z", "not-a-date", true),
            MergeDecision::Overwrite
        );
    }

    #[test]
    fn pull_targets_follow_token_team_ids() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        for (id, remote, archived) in [
            ("a", Some(1), None),
            ("b", Some(2), None),
            ("c", Some(3), Some(now)),
            ("d", None, None),
        ] {
            conn.execute(
                "INSERT INTO teams (id, name, identifier, remote_id, archived_at, created_at, updated_at)
                 VALUES (?1, ?1, ?1, ?2, ?3, ?4, ?4)",
                rusqlite::params![id, remote, archived, now],
            )
            .unwrap();
        }
        let ids = |t: Vec<PullTarget>| {
            let mut v: Vec<i64> = t.into_iter().map(|t| t.remote_id).collect();
            v.sort();
            v
        };
        // Unscoped: all non-archived Rails-backed teams.
        assert_eq!(ids(select_pull_targets(&conn, None).unwrap()), vec![1, 2]);
        // List: intersection with local teams (99 unknown, 3 archived).
        assert_eq!(
            ids(select_pull_targets(&conn, Some(&[2, 3, 99])).unwrap()),
            vec![2]
        );
        // Empty list: zero teams.
        assert!(select_pull_targets(&conn, Some(&[])).unwrap().is_empty());
    }

    fn setup_labels_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES ('t1', 'T', 'T', 700, ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lanes (id, name, position, team_id, created_at, updated_at)
             VALUES ('l1', 'B', 0, 't1', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at)
             VALUES ('i1', 'Issue', 4, 'l1', 't1', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn
    }

    fn insert_test_label(conn: &Connection, id: &str, remote_id: Option<i64>) {
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute(
            "INSERT INTO labels (id, remote_id, team_id, name, color, created_at, updated_at)
             VALUES (?1, ?2, 't1', ?1, '#ef4444', ?3, ?3)",
            rusqlite::params![id, remote_id, now],
        )
        .unwrap();
    }

    #[test]
    fn label_update_prep_rereads_row_and_waits_for_remote_id() {
        let conn = setup_labels_db();
        insert_test_label(&conn, "lab1", None);
        assert!(matches!(
            label_update_prep(&conn, "lab1"),
            Prep::Done(Outcome::LeavePending(_))
        ));
        queries::set_label_remote_id(&conn, "lab1", 11).unwrap();
        queries::edit_label(&conn, "lab1", "renamed", "#3b82f6").unwrap();
        match label_update_prep(&conn, "lab1") {
            Prep::Ready(p) => {
                assert_eq!((p.team_remote_id, p.remote_id), (700, 11));
                assert_eq!(p.params.name, "renamed");
                assert_eq!(p.params.color.as_deref(), Some("#3b82f6"));
            }
            _ => panic!("expected Ready"),
        }
        assert!(matches!(
            label_update_prep(&conn, "nope"),
            Prep::Done(Outcome::DropAndLog(_))
        ));
    }

    fn insert_test_recurring(conn: &Connection, remote_id: Option<i64>) {
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute(
            "INSERT INTO recurring_issues (id, remote_id, team_id, title, priority, frequency,
                interval, active, created_at, updated_at)
             VALUES ('r1', ?1, 't1', 'R', 0, 'daily', 1, 1, ?2, ?2)",
            rusqlite::params![remote_id, now],
        )
        .unwrap();
    }

    #[test]
    fn recurring_issue_update_prep_rereads_row_and_waits_for_remote_ids() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, None);
        let payload = crate::db::recurring_issues::update_payload(&["active"], None);
        assert!(matches!(
            recurring_issue_update_prep(&conn, "r1", Some(&payload)),
            Prep::Done(Outcome::LeavePending(_))
        ));
        conn.execute("UPDATE recurring_issues SET remote_id = 5 WHERE id = 'r1'", [])
            .unwrap();
        conn.execute("UPDATE teams SET remote_id = NULL WHERE id = 't1'", []).unwrap();
        assert!(matches!(
            recurring_issue_update_prep(&conn, "r1", Some(&payload)),
            Prep::Done(Outcome::LeavePending(_))
        ));
        conn.execute("UPDATE teams SET remote_id = 700 WHERE id = 't1'", []).unwrap();
        // The toggle is read at push time: flip it after queueing.
        crate::db::recurring_issues::set_active(&conn, "r1", false).unwrap();
        match recurring_issue_update_prep(&conn, "r1", Some(&payload)) {
            Prep::Ready(p) => {
                assert_eq!((p.team_remote_id, p.remote_id), (700, 5));
                assert_eq!(p.params.active, Some(false));
                assert_eq!(p.params.starts_on, None);
                let body = serde_json::to_value(&p.params).unwrap();
                assert_eq!(body, serde_json::json!({ "active": false }));
            }
            _ => panic!("expected Ready"),
        }
        assert!(matches!(
            recurring_issue_update_prep(&conn, "nope", Some(&payload)),
            Prep::Done(Outcome::DropAndLog(_))
        ));
    }

    #[test]
    fn recurring_issue_update_prep_sends_starts_on_only_when_the_payload_has_one() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, Some(5));
        let payload = crate::db::recurring_issues::update_payload(&["active"], Some("2026-11-05"));
        match recurring_issue_update_prep(&conn, "r1", Some(&payload)) {
            Prep::Ready(p) => assert_eq!(p.params.starts_on.as_deref(), Some("2026-11-05")),
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn recurring_issue_schedule_prep_sends_schedule_fields_and_starts_on() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, Some(5));
        conn.execute(
            "UPDATE recurring_issues SET frequency = 'weekly', interval = 2, weekday = 3
             WHERE id = 'r1'",
            [],
        )
        .unwrap();
        let payload = crate::db::recurring_issues::update_payload(
            &["frequency", "interval", "weekday"],
            Some("2026-11-10"),
        );
        match recurring_issue_update_prep(&conn, "r1", Some(&payload)) {
            Prep::Ready(p) => assert_eq!(
                serde_json::to_value(&p.params).unwrap(),
                serde_json::json!({
                    "frequency": "weekly", "interval": 2, "weekday": 3,
                    "starts_on": "2026-11-10"
                })
            ),
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn recurring_issue_edit_prep_sends_changed_fields_without_starts_on() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, Some(5));
        conn.execute(
            "UPDATE recurring_issues SET title = 'New', priority = 1, description = NULL,
                time_zone = 'Tokyo' WHERE id = 'r1'",
            [],
        )
        .unwrap();
        let payload = crate::db::recurring_issues::update_payload(
            &["title", "priority", "description", "time_zone"],
            None,
        );
        match recurring_issue_update_prep(&conn, "r1", Some(&payload)) {
            Prep::Ready(p) => {
                let body = serde_json::to_value(&p.params).unwrap();
                assert_eq!(
                    body,
                    serde_json::json!({
                        "title": "New", "priority": "high", "description": "",
                        "time_zone": "Tokyo"
                    })
                );
            }
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn recurring_issue_edit_prep_waits_for_references_and_sends_clears_as_null() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, Some(5));
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("UPDATE recurring_issues SET lane_id = 'l1' WHERE id = 'r1'", []).unwrap();
        let payload = crate::db::recurring_issues::update_payload(
            &["lane_id", "assignee_id", "project_id", "label_ids"],
            None,
        );
        let waits = |c: &Connection| {
            matches!(
                recurring_issue_update_prep(c, "r1", Some(&payload)),
                Prep::Done(Outcome::LeavePending(_))
            )
        };
        // The lane has no remote id yet.
        assert!(waits(&conn));
        conn.execute("UPDATE lanes SET remote_id = 11 WHERE id = 'l1'", []).unwrap();
        assert!(!waits(&conn));

        insert_test_label(&conn, "lb1", None);
        conn.execute(
            "INSERT INTO recurring_issue_labels (recurring_issue_id, label_id) VALUES ('r1', 'lb1')",
            [],
        )
        .unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE labels SET remote_id = 14 WHERE id = 'lb1'", []).unwrap();

        conn.execute(
            "INSERT INTO members (id, team_id, name, email, created_at, updated_at)
             VALUES ('m1', 't1', 'Ann', 'a@x.io', ?1, ?1)",
            [now],
        )
        .unwrap();
        conn.execute("UPDATE recurring_issues SET assignee_id = 'm1' WHERE id = 'r1'", []).unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE recurring_issues SET assignee_id = NULL WHERE id = 'r1'", []).unwrap();
        match recurring_issue_update_prep(&conn, "r1", Some(&payload)) {
            Prep::Ready(p) => {
                let body = serde_json::to_value(&p.params).unwrap();
                assert_eq!(
                    body,
                    serde_json::json!({
                        "lane_id": 11, "assignee_id": null, "project_id": null,
                        "label_ids": [14]
                    })
                );
            }
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn recurring_issue_create_prep_rereads_row_and_includes_starts_on() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, None);
        conn.execute("UPDATE recurring_issues SET title = 'Edited' WHERE id = 'r1'", [])
            .unwrap();
        let payload = crate::db::recurring_issues::create_payload("2026-11-04");
        match recurring_issue_create_prep(&conn, "r1", Some(&payload)) {
            Prep::Ready(p) => {
                assert_eq!(p.team_remote_id, 700);
                assert_eq!(p.local_id, "r1");
                let body = serde_json::to_value(&p.params).unwrap();
                assert_eq!(
                    body,
                    serde_json::json!({
                        "title": "Edited", "priority": "urgent", "frequency": "daily",
                        "interval": 1, "starts_on": "2026-11-04"
                    })
                );
            }
            _ => panic!("expected Ready"),
        }
        assert!(matches!(
            recurring_issue_create_prep(&conn, "nope", Some(&payload)),
            Prep::Done(Outcome::DropAndLog(_))
        ));
    }

    #[test]
    fn recurring_issue_create_prep_sends_time_zone_only_when_the_row_has_one() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, None);
        let body = |conn: &Connection| match recurring_issue_create_prep(conn, "r1", None) {
            Prep::Ready(p) => serde_json::to_value(&p.params).unwrap(),
            _ => panic!("expected Ready"),
        };
        assert!(body(&conn).get("time_zone").is_none());
        conn.execute("UPDATE recurring_issues SET time_zone = 'UTC' WHERE id = 'r1'", [])
            .unwrap();
        assert_eq!(body(&conn)["time_zone"], "UTC");
    }

    #[test]
    fn recurring_issue_create_prep_waits_for_team_and_referenced_remote_ids() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, None);
        let waits = |conn: &Connection| {
            matches!(
                recurring_issue_create_prep(conn, "r1", None),
                Prep::Done(Outcome::LeavePending(_))
            )
        };
        conn.execute("UPDATE teams SET remote_id = NULL WHERE id = 't1'", []).unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE teams SET remote_id = 700 WHERE id = 't1'", []).unwrap();

        // Lane, project, assignee and label without remote ids each block.
        conn.execute("UPDATE recurring_issues SET lane_id = 'l1' WHERE id = 'r1'", []).unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE lanes SET remote_id = 11 WHERE id = 'l1'", []).unwrap();
        assert!(!waits(&conn));

        conn.execute(
            "INSERT INTO projects (id, team_id, name, created_at, updated_at)
             VALUES ('p1', 't1', 'P', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute("UPDATE recurring_issues SET project_id = 'p1' WHERE id = 'r1'", []).unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE projects SET remote_id = 12 WHERE id = 'p1'", []).unwrap();
        assert!(!waits(&conn));

        conn.execute(
            "INSERT INTO members (id, team_id, name, email, created_at, updated_at)
             VALUES ('m1', 't1', 'Ann', 'a@x.io', '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
            [],
        )
        .unwrap();
        conn.execute("UPDATE recurring_issues SET assignee_id = 'm1' WHERE id = 'r1'", []).unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE members SET remote_id = 13 WHERE id = 'm1'", []).unwrap();
        assert!(!waits(&conn));

        insert_test_label(&conn, "lb1", None);
        conn.execute(
            "INSERT INTO recurring_issue_labels (recurring_issue_id, label_id) VALUES ('r1', 'lb1')",
            [],
        )
        .unwrap();
        assert!(waits(&conn));
        conn.execute("UPDATE labels SET remote_id = 14 WHERE id = 'lb1'", []).unwrap();
        match recurring_issue_create_prep(&conn, "r1", None) {
            Prep::Ready(p) => {
                assert_eq!(p.params.lane_id, Some(Some(11)));
                assert_eq!(p.params.project_id, Some(Some(12)));
                assert_eq!(p.params.assignee_id, Some(Some(13)));
                assert_eq!(p.params.label_ids, Some(vec![14]));
            }
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn recurring_issue_create_prep_is_a_no_op_once_the_row_has_a_remote_id() {
        let conn = setup_labels_db();
        insert_test_recurring(&conn, Some(5));
        assert!(matches!(
            recurring_issue_create_prep(&conn, "r1", None),
            Prep::Done(Outcome::Success)
        ));
    }

    #[test]
    fn recurring_issue_delete_prep_reads_only_the_payload() {
        match prep_recurring_issue_delete(Some(r#"{"team_remote_id":700,"remote_id":11}"#)) {
            Prep::Ready(ids) => assert_eq!(ids, (700, 11)),
            _ => panic!("expected Ready"),
        }
        for bad in [None, Some("not json"), Some(r#"{"remote_id":11}"#)] {
            assert!(matches!(
                prep_recurring_issue_delete(bad),
                Prep::Done(Outcome::DropAndLog(_))
            ));
        }
    }

    #[test]
    fn label_delete_prep_reads_only_the_payload() {
        match prep_label_delete(Some(r#"{"team_remote_id":700,"remote_id":11}"#)) {
            Prep::Ready(ids) => assert_eq!(ids, (700, 11)),
            _ => panic!("expected Ready"),
        }
        for bad in [None, Some("not json"), Some(r#"{"remote_id":11}"#)] {
            assert!(matches!(
                prep_label_delete(bad),
                Prep::Done(Outcome::DropAndLog(_))
            ));
        }
    }

    fn saved_view_conn() -> (Connection, String) {
        let conn = setup_labels_db();
        let sv = crate::db::saved_views::create_local(&conn, "t1", "Mine", r#"{"view_mode":"list"}"#)
            .unwrap();
        (conn, sv.id)
    }

    fn saved_view_dto(id: i64) -> crate::api::saved_views::SavedViewDto {
        serde_json::from_value(serde_json::json!({
            "id": id, "team_id": 700, "name": "Mine",
            "query": {"view_mode": "list"}, "position": 3,
            "updated_at": "2026-10-31T10:00:00.000Z"
        }))
        .unwrap()
    }

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

    #[test]
    fn done_if_gone_treats_success_and_404_as_done_and_classifies_the_rest() {
        let mut ran = false;
        assert!(matches!(done_if_gone(Ok(1), |_| ran = true), Outcome::Success));
        assert!(ran);
        assert!(matches!(
            done_if_gone(Err::<(), _>(ApiError::NotFound("gone".into())), |_| {}),
            Outcome::Success
        ));
        assert!(matches!(
            done_if_gone(Err::<(), _>(ApiError::MissingToken), |_| {}),
            Outcome::LeavePending(_)
        ));
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

    #[test]
    fn build_label_ids_resolves_remote_ids() {
        let conn = setup_labels_db();
        insert_test_label(&conn, "lab1", Some(11));
        insert_test_label(&conn, "lab2", Some(22));
        queries::set_issue_labels(&conn, "i1", &["lab1".into(), "lab2".into()]).unwrap();

        let mut ids = build_label_ids(&conn, "i1").unwrap().unwrap();
        ids.sort();
        assert_eq!(ids, vec![11, 22]);
    }

    #[test]
    fn list_etag_is_stored_per_team_and_list() {
        let conn = setup_labels_db();
        assert_eq!(queries::get_list_etag(&conn, "t1", "labels").unwrap(), None);

        store_list_etag(&conn, "t1", "labels", Some("W/\"a\""));
        store_list_etag(&conn, "t1", "lanes", Some("W/\"b\""));
        assert_eq!(
            queries::get_list_etag(&conn, "t1", "labels").unwrap().as_deref(),
            Some("W/\"a\"")
        );
        assert_eq!(
            queries::get_list_etag(&conn, "t1", "lanes").unwrap().as_deref(),
            Some("W/\"b\"")
        );
        assert_eq!(queries::get_list_etag(&conn, "t2", "labels").unwrap(), None);

        // Overwrite, then clear when the server stops sending an ETag.
        store_list_etag(&conn, "t1", "labels", Some("W/\"c\""));
        assert_eq!(
            queries::get_list_etag(&conn, "t1", "labels").unwrap().as_deref(),
            Some("W/\"c\"")
        );
        store_list_etag(&conn, "t1", "labels", None);
        assert_eq!(queries::get_list_etag(&conn, "t1", "labels").unwrap(), None);
    }

    fn setup_tombstoned_issue() -> Connection {
        let conn = setup_labels_db();
        insert_test_label(&conn, "lab1", Some(11));
        queries::set_issue_labels(&conn, "i1", &["lab1".into()]).unwrap();
        conn.execute_batch(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, parent_issue_id, created_at, updated_at)
               VALUES ('i2', 'Other', 4, 'l1', 't1', 'i1', '2026-01-01', '2026-01-01');
             INSERT INTO comments (id, issue_id, body, created_at, updated_at)
               VALUES ('c1', 'i1', 'hi', '2026-01-01', '2026-01-01');
             INSERT INTO issue_dependencies (id, blocking_issue_id, blocked_issue_id, created_at, updated_at)
               VALUES ('d1', 'i1', 'i2', '2026-01-01', '2026-01-01');",
        )
        .unwrap();
        queries::delete_issue(&conn, "i1").unwrap();
        conn
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    fn assert_purged(conn: &Connection) {
        assert_eq!(count(conn, "SELECT COUNT(*) FROM issues WHERE id='i1'"), 0);
        assert_eq!(count(conn, "SELECT COUNT(*) FROM comments"), 0);
        assert_eq!(count(conn, "SELECT COUNT(*) FROM issue_labels"), 0);
        assert_eq!(count(conn, "SELECT COUNT(*) FROM issue_dependencies"), 0);
        // Sub-issue survives, detached.
        assert_eq!(
            count(conn, "SELECT COUNT(*) FROM issues WHERE id='i2' AND parent_issue_id IS NULL"),
            1
        );
    }

    fn tomb(kind: &str, id: i64) -> TombstoneDto {
        TombstoneDto {
            record_type: kind.into(),
            record_id: id,
            deleted_at: None,
        }
    }

    fn give_i1_remote_id(conn: &Connection) {
        conn.execute("UPDATE issues SET remote_id = 500 WHERE id = 'i1'", [])
            .unwrap();
    }

    #[test]
    fn issue_tombstone_purges_live_issue_and_attachments() {
        let conn = setup_tombstoned_issue();
        conn.execute("UPDATE issues SET deleted_at = NULL WHERE id = 'i1'", [])
            .unwrap();
        give_i1_remote_id(&conn);
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Issue", 500)]), 0);
        assert_purged(&conn);
    }

    #[test]
    fn issue_tombstone_clears_local_marker_and_pending_delete() {
        let conn = setup_tombstoned_issue();
        give_i1_remote_id(&conn);
        sync_queue::enqueue(&conn, "issue", "i1", "delete", Some("{}")).unwrap();
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Issue", 500)]), 0);
        assert_purged(&conn);
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn tombstone_for_unknown_issue_or_other_team_is_a_noop() {
        let conn = setup_tombstoned_issue();
        give_i1_remote_id(&conn);
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Issue", 999)]), 0);
        assert_eq!(apply_tombstones(&conn, "other", &[tomb("Issue", 500)]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issues WHERE id='i1'"), 1);
    }

    #[test]
    fn unhandled_tombstone_types_are_ignored_and_repeats_are_safe() {
        let conn = setup_tombstoned_issue();
        give_i1_remote_id(&conn);
        let rows = [
            tomb("IssueDependency", 1),
            tomb("SomethingNew", 2),
            tomb("Issue", 500),
        ];
        assert_eq!(apply_tombstones(&conn, "t1", &rows), 0);
        assert_eq!(apply_tombstones(&conn, "t1", &rows), 0);
        assert_purged(&conn);
    }

    #[test]
    fn recurring_issue_tombstone_removes_the_local_row() {
        let conn = setup_labels_db();
        exec(
            &conn,
            "INSERT INTO recurring_issues (id, remote_id, team_id, title, priority, frequency,
                interval, active, created_at, updated_at)
               VALUES ('r1', 40, 't1', 'R', 0, 'daily', 1, 1, '2026-01-01', '2026-01-01');
             INSERT INTO recurring_issue_labels (recurring_issue_id, label_id)
               SELECT 'r1', id FROM labels LIMIT 1;",
        );
        let rows = [tomb("RecurringIssue", 999), tomb("RecurringIssue", 40)];
        assert_eq!(apply_tombstones(&conn, "t1", &rows), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM recurring_issues"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM recurring_issue_labels"), 0);
        assert_eq!(apply_tombstones(&conn, "t1", &rows), 0);
    }

    fn exec(conn: &Connection, sql: &str) {
        conn.execute_batch(sql).unwrap();
    }

    #[test]
    fn comment_tombstone_deletes_comment_and_replies_only() {
        let conn = setup_labels_db();
        exec(
            &conn,
            "INSERT INTO comments (id, remote_id, issue_id, body, parent_id, created_at, updated_at) VALUES
               ('c1', 901, 'i1', 'a', NULL, '2026-01-01', '2026-01-01'),
               ('c2', 902, 'i1', 'reply', 'c1', '2026-01-01', '2026-01-01'),
               ('c3', 903, 'i1', 'reply2', 'c2', '2026-01-01', '2026-01-01'),
               ('c4', 904, 'i1', 'other', NULL, '2026-01-01', '2026-01-01');",
        );
        sync_queue::enqueue(&conn, "comment", "c2", "create", Some("{}")).unwrap();
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Comment", 901)]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments WHERE id='c4'"), 1);
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 0);
        // Repeat, unknown id and another team are no-ops.
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Comment", 901)]), 0);
        assert_eq!(apply_tombstones(&conn, "other", &[tomb("Comment", 904)]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments"), 1);
    }

    #[test]
    fn label_tombstone_detaches_from_issues_and_projects() {
        let conn = setup_labels_db();
        insert_test_label(&conn, "lab1", Some(11));
        insert_test_label(&conn, "lab2", Some(22));
        exec(
            &conn,
            "INSERT INTO projects (id, remote_id, team_id, name, created_at, updated_at)
               VALUES ('p1', 800, 't1', 'P', '2026-01-01', '2026-01-01');
             INSERT INTO project_labels (project_id, label_id, created_at, updated_at)
               VALUES ('p1', 'lab1', '2026-01-01', '2026-01-01'), ('p1', 'lab2', '2026-01-01', '2026-01-01');",
        );
        queries::set_issue_labels(&conn, "i1", &["lab1".into(), "lab2".into()]).unwrap();
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Label", 11)]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM labels WHERE id='lab1'"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issue_labels"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM project_labels"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM labels WHERE id='lab2'"), 1);
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Label", 11)]), 0);
    }

    #[test]
    fn project_tombstone_unlinks_issues_and_deletes_project() {
        let conn = setup_labels_db();
        insert_test_label(&conn, "lab1", Some(11));
        exec(
            &conn,
            "INSERT INTO projects (id, remote_id, team_id, name, created_at, updated_at)
               VALUES ('p1', 800, 't1', 'P', '2026-01-01', '2026-01-01');
             INSERT INTO project_labels (project_id, label_id, created_at, updated_at)
               VALUES ('p1', 'lab1', '2026-01-01', '2026-01-01');
             UPDATE issues SET project_id = 'p1' WHERE id = 'i1';",
        );
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Project", 800)]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM projects"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM project_labels"), 0);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM issues WHERE id='i1' AND project_id IS NULL"),
            1
        );
        // The label itself survives.
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM labels WHERE id='lab1'"), 1);
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Project", 800)]), 0);
    }

    fn comment_dto(id: i64, parent: Option<i64>) -> CommentDto {
        CommentDto {
            id,
            body: "x".into(),
            parent_id: parent,
            user: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            replies: None,
            issue_id: Some(500),
            project_id: None,
        }
    }

    #[test]
    fn tombstoned_comment_in_feed_is_not_resurrected() {
        let conn = setup_labels_db();
        give_i1_remote_id(&conn);
        exec(
            &conn,
            "INSERT INTO comments (id, remote_id, issue_id, body, created_at, updated_at)
               VALUES ('c1', 901, 'i1', 'a', '2026-01-01', '2026-01-01');",
        );
        let rows = [tomb("Comment", 901)];
        assert_eq!(apply_tombstones(&conn, "t1", &rows), 0);
        let deleted = tombstoned_comment_ids(&rows);
        // Feed still carries the comment, a reply to it, and an unrelated one.
        let feed = vec![
            comment_dto(901, None),
            comment_dto(902, Some(901)),
            comment_dto(903, None),
        ];
        let kept = drop_deleted_comments(feed, &deleted);
        assert_eq!(kept.iter().map(|d| d.id).collect::<Vec<_>>(), vec![903]);
        assert_eq!(apply_pulled_comments(&conn, kept), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments WHERE remote_id IN (901,902)"), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments WHERE remote_id = 903"), 1);
    }

    #[test]
    fn issue_delete_204_purges_tombstone_and_attachments() {
        let conn = setup_tombstoned_issue();
        assert!(matches!(
            issue_delete_outcome(&conn, "i1", 42, Ok(())),
            Outcome::Success
        ));
        assert_purged(&conn);
    }

    #[test]
    fn issue_delete_resource_404_counts_as_gone() {
        let conn = setup_tombstoned_issue();
        let r = Err(ApiError::NotFound("Issue not found".into()));
        assert!(matches!(issue_delete_outcome(&conn, "i1", 42, r), Outcome::Success));
        assert_purged(&conn);
    }

    #[test]
    fn issue_delete_403_pauses_and_keeps_tombstone() {
        let conn = setup_tombstoned_issue();
        let r = Err(ApiError::Forbidden("Token lacks required scope".into()));
        assert!(matches!(
            issue_delete_outcome(&conn, "i1", 42, r),
            Outcome::PausePushes(PushPause::Forbidden, _)
        ));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issues WHERE id='i1'"), 1);
    }

    #[test]
    fn issue_delete_team_access_404_is_not_success_and_keeps_tombstone() {
        let conn = setup_tombstoned_issue();
        let r = Err(ApiError::NotFound("Team not found or access denied".into()));
        assert!(matches!(
            issue_delete_outcome(&conn, "i1", 42, r),
            Outcome::TeamAccessLost(42, _)
        ));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issues WHERE id='i1'"), 1);
    }

    #[test]
    fn build_label_ids_empty_when_no_labels() {
        let conn = setup_labels_db();
        // No labels attached → assert an empty set (Rails detaches all).
        assert_eq!(build_label_ids(&conn, "i1").unwrap(), Some(vec![]));
    }

    fn queue_create(conn: &Connection, entity_type: &str, entity_id: &str) -> i64 {
        sync_queue::enqueue(conn, entity_type, entity_id, "create", None).unwrap();
        conn.last_insert_rowid()
    }

    fn issue_remote(conn: &Connection) -> Option<i64> {
        conn.query_row("SELECT remote_id FROM issues WHERE id='i1'", [], |r| {
            r.get(0)
        })
        .unwrap()
    }

    #[test]
    fn create_saves_remote_id_and_marks_synced_together() {
        let mut conn = setup_labels_db();
        let entry = queue_create(&conn, "issue", "i1");
        save_remote_id_and_mark_synced(&mut conn, "issues", "i1", 99, entry).unwrap();
        assert_eq!(issue_remote(&conn), Some(99));
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 0);
    }

    #[test]
    fn create_rolls_back_remote_id_when_mark_synced_fails() {
        let mut conn = setup_labels_db();
        let entry = queue_create(&conn, "issue", "i1");
        conn.execute_batch(
            "CREATE TRIGGER fail_mark BEFORE UPDATE ON sync_queue
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();
        assert!(save_remote_id_and_mark_synced(&mut conn, "issues", "i1", 99, entry).is_err());
        assert_eq!(issue_remote(&conn), None);
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 1);
    }

    #[test]
    fn create_rolls_back_mark_synced_when_remote_id_write_fails() {
        let mut conn = setup_labels_db();
        let entry = queue_create(&conn, "issue", "i1");
        conn.execute_batch(
            "CREATE TRIGGER fail_rid BEFORE UPDATE OF remote_id ON issues
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();
        assert!(save_remote_id_and_mark_synced(&mut conn, "issues", "i1", 99, entry).is_err());
        assert_eq!(issue_remote(&conn), None);
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 1);
    }

    #[test]
    fn create_commit_covers_label_table_and_rejects_unknown() {
        let mut conn = setup_labels_db();
        insert_test_label(&conn, "lab1", None);
        let entry = queue_create(&conn, "label", "lab1");
        save_remote_id_and_mark_synced(&mut conn, "labels", "lab1", 5, entry).unwrap();
        assert_eq!(
            queries::remote_id_for(&conn, "labels", "lab1").unwrap(),
            Some(5)
        );
        assert!(save_remote_id_and_mark_synced(&mut conn, "bogus", "x", 1, entry).is_err());
    }

    #[test]
    fn build_label_ids_leaves_pending_for_unsynced_label() {
        let conn = setup_labels_db();
        // Label exists locally but hasn't been pushed yet (no remote_id).
        insert_test_label(&conn, "lab1", None);
        queries::set_issue_labels(&conn, "i1", &["lab1".into()]).unwrap();

        match build_label_ids(&conn, "i1") {
            Err(Outcome::LeavePending(msg)) => assert!(msg.contains("labels lab1")),
            other => panic!("expected LeavePending, got {other:?}"),
        }
    }

    fn setup_dependency_db() -> Connection {
        let conn = setup_labels_db();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("UPDATE issues SET remote_id = 1 WHERE id = 'i1'", [])
            .unwrap();
        for (id, remote) in [("i2", 2), ("i3", 3)] {
            conn.execute(
                "INSERT INTO issues (id, title, priority, lane_id, team_id, remote_id, created_at, updated_at)
                 VALUES (?1, 'x', 4, 'l1', 't1', ?2, ?3, ?3)",
                rusqlite::params![id, remote, now],
            )
            .unwrap();
        }
        conn
    }

    fn link(dep: i64, kind: &'static str, s: i64, t: i64) -> PulledDependency {
        PulledDependency {
            dependency_id: dep,
            kind,
            direction: if kind == "blocks" { "blocking" } else { kind },
            source_remote_id: s,
            target_remote_id: t,
        }
    }

    fn dep_row(conn: &Connection, remote: i64) -> (String, String, String, String) {
        conn.query_row(
            "SELECT blocking_issue_id, blocked_issue_id, kind, direction
             FROM issue_dependencies WHERE remote_id = ?1",
            [remote],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    }

    // ── Create reconciliation (JRST-23) ──

    const ATTEMPT: &str = "2026-10-30T12:00:00.000Z";

    fn issue_dto(id: i64, title: &str, creator: Option<i64>, created_at: &str) -> IssueDto {
        serde_json::from_value(serde_json::json!({
            "id": id, "identifier": format!("X-{id}"), "title": title,
            "priority": "no_priority", "estimate": null, "due_date": null,
            "lane": null, "assignee": null,
            "creator": creator.map(|c| serde_json::json!({"id": c, "name": "n"})),
            "project": null, "labels": [],
            "created_at": created_at, "updated_at": created_at,
        }))
        .unwrap()
    }

    fn project_dto(id: i64, name: &str, created_at: &str) -> ProjectDto {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "description": null, "status": "backlog",
            "priority": "no_priority", "lead": null, "start_date": null,
            "due_date": null, "velocity_score": 0, "completed_issues_count": 0,
            "total_issues_count": 0, "progress_percentage": 0,
            "behind_schedule": false, "roadmap_commitment": null,
            "created_at": created_at, "updated_at": created_at,
        }))
        .unwrap()
    }

    fn label_dto(id: i64, name: &str) -> LabelDto {
        serde_json::from_value(serde_json::json!({"id": id, "name": name, "color": null}))
            .unwrap()
    }

    fn label_dto_at(id: i64, name: &str, updated_at: &str) -> LabelDto {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "color": null, "updated_at": updated_at
        }))
        .unwrap()
    }

    fn none() -> HashSet<i64> {
        HashSet::new()
    }

    #[test]
    fn issue_match_requires_creator_title_and_time() {
        let after = "2026-10-30T12:00:05.000Z";
        let c = vec![
            issue_dto(1, "Fix", Some(8), after),
            issue_dto(2, "Other", Some(7), after),
            issue_dto(3, "Fix", Some(7), "2026-10-30T11:00:00.000Z"),
            issue_dto(4, "Fix", None, after),
            issue_dto(5, "Fix", Some(7), after),
        ];
        assert_eq!(match_issue(&c, 7, "Fix", ATTEMPT, &none()), Some(5));
        assert_eq!(match_issue(&c[..4], 7, "Fix", ATTEMPT, &none()), None);
    }

    #[test]
    fn issue_match_tolerates_small_clock_skew_only() {
        let near = issue_dto(1, "Fix", Some(7), "2026-10-30T11:59:30.000Z");
        let far = issue_dto(2, "Fix", Some(7), "2026-10-30T11:55:00.000Z");
        assert_eq!(match_issue(&[near], 7, "Fix", ATTEMPT, &none()), Some(1));
        assert_eq!(match_issue(&[far], 7, "Fix", ATTEMPT, &none()), None);
    }

    #[test]
    fn issue_match_picks_earliest_and_skips_linked_ids() {
        let c = vec![
            issue_dto(10, "Fix", Some(7), "2026-10-30T12:00:09.000Z"),
            issue_dto(11, "Fix", Some(7), "2026-10-30T12:00:02.000Z"),
        ];
        assert_eq!(match_issue(&c, 7, "Fix", ATTEMPT, &none()), Some(11));
        let taken: HashSet<i64> = [11].into_iter().collect();
        assert_eq!(match_issue(&c, 7, "Fix", ATTEMPT, &taken), Some(10));
    }

    fn recurring_dto(id: i64, title: &str, creator: Option<i64>, created_at: &str) -> RecurringIssueDto {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": title, "priority": "no_priority",
            "frequency": "weekly", "interval": 2, "weekday": 5, "day_of_month": null,
            "active": true,
            "creator": creator.map(|c| serde_json::json!({"id": c, "name": "n"})),
            "created_at": created_at, "updated_at": created_at,
        }))
        .unwrap()
    }

    fn recurring_params(title: &str) -> RecurringIssueParams {
        RecurringIssueParams {
            title: Some(title.into()),
            frequency: Some("weekly".into()),
            interval: Some(2),
            weekday: Some(5),
            ..Default::default()
        }
    }

    #[test]
    fn recurring_match_requires_creator_title_schedule_and_time() {
        let after = "2026-10-30T12:00:05.000Z";
        let mut other_schedule = recurring_dto(3, "Water", Some(7), after);
        other_schedule.interval = 1;
        let c = vec![
            recurring_dto(1, "Water", Some(8), after),
            recurring_dto(2, "Other", Some(7), after),
            other_schedule,
            recurring_dto(4, "Water", Some(7), "2026-10-30T11:00:00.000Z"),
            recurring_dto(5, "Water", None, after),
            recurring_dto(6, "Water", Some(7), after),
        ];
        let p = recurring_params("Water");
        assert_eq!(match_recurring_issue(&c, 7, &p, ATTEMPT, &none()).map(|d| d.id), Some(6));
        assert!(match_recurring_issue(&c[..5], 7, &p, ATTEMPT, &none()).is_none());
    }

    #[test]
    fn recurring_match_picks_earliest_and_skips_linked_ids() {
        let c = vec![
            recurring_dto(10, "Water", Some(7), "2026-10-30T12:00:09.000Z"),
            recurring_dto(11, "Water", Some(7), "2026-10-30T12:00:02.000Z"),
        ];
        let p = recurring_params("Water");
        assert_eq!(match_recurring_issue(&c, 7, &p, ATTEMPT, &none()).map(|d| d.id), Some(11));
        let taken: HashSet<i64> = [11].into_iter().collect();
        assert_eq!(match_recurring_issue(&c, 7, &p, ATTEMPT, &taken).map(|d| d.id), Some(10));
    }

    #[test]
    fn project_pull_dedupes_and_advances_cursor() {
        let conn = setup_labels_db();
        let a = project_dto(1, "A", "2026-10-30T10:00:00.000Z");
        let a2 = project_dto(1, "A renamed", "2026-10-30T10:00:01.000Z");
        apply_pulled_projects(&conn, "t1", vec![a, a2], Some("2026-10-30T12:00:00.000000Z"))
            .unwrap();
        let p = queries::find_project_by_remote_id(&conn, 1).unwrap().unwrap();
        assert_eq!(p.name, "A renamed");
        assert_eq!(
            queries::get_pull_cursor(&conn, "t1", "projects").unwrap().as_deref(),
            Some("2026-10-30T12:00:00.000000Z")
        );
        // The issue cursor is independent.
        assert_eq!(queries::get_pull_cursor(&conn, "t1", "issues").unwrap(), None);
    }

    #[test]
    fn project_pull_redelivery_is_idempotent_and_no_header_keeps_cursor() {
        let conn = setup_labels_db();
        queries::set_pull_cursor(&conn, "t1", "projects", "OLD").unwrap();
        for _ in 0..2 {
            apply_pulled_projects(
                &conn,
                "t1",
                vec![project_dto(1, "A", "2026-10-30T10:00:00.000Z")],
                None,
            )
            .unwrap();
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            queries::get_pull_cursor(&conn, "t1", "projects").unwrap().as_deref(),
            Some("OLD")
        );
    }

    #[test]
    fn project_match_is_exact_name_after_attempt() {
        let after = "2026-10-30T12:01:00.000Z";
        let c = vec![
            project_dto(1, "Roadmap", "2026-10-29T00:00:00.000Z"),
            project_dto(2, "roadmap", after),
            project_dto(3, "Roadmap", after),
        ];
        assert_eq!(match_project(&c, "Roadmap", ATTEMPT, &none()), Some(3));
        let taken: HashSet<i64> = [3].into_iter().collect();
        assert_eq!(match_project(&c, "Roadmap", ATTEMPT, &taken), None);
    }

    #[test]
    fn label_match_is_case_insensitive_name_and_skips_linked() {
        let at = "2026-10-30T12:00:00Z";
        let c = vec![
            label_dto_at(1, "Bug", "2026-10-30T12:00:05Z"),
            label_dto_at(2, "Feature", "2026-10-30T12:00:05Z"),
        ];
        assert_eq!(match_label(&c, "bug", at, &none()), Some(1));
        assert_eq!(match_label(&c, " FEATURE ", at, &none()), Some(2));
        assert_eq!(match_label(&c, "Chore", at, &none()), None);
        let taken: HashSet<i64> = [1].into_iter().collect();
        assert_eq!(match_label(&c, "Bug", at, &taken), None);
    }

    #[test]
    fn label_match_requires_update_after_the_attempt() {
        let at = "2026-10-30T12:00:00Z";
        // Same name, but the label was last touched well before the attempt:
        // an unrelated pre-existing label, not our create.
        let old = vec![label_dto_at(1, "Bug", "2026-10-29T08:00:00Z")];
        assert_eq!(match_label(&old, "Bug", at, &none()), None);
        // Within the clock-skew slack still matches.
        let skewed = vec![label_dto_at(2, "Bug", "2026-10-30T11:59:30Z")];
        assert_eq!(match_label(&skewed, "Bug", at, &none()), Some(2));
        // No timestamp at all never matches.
        assert_eq!(match_label(&[label_dto(3, "Bug")], "Bug", at, &none()), None);
    }

    fn stored_attempt(conn: &Connection, entry: i64) -> String {
        conn.query_row(
            "SELECT attempted_at FROM sync_queue WHERE id=?1",
            [entry],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn pull_persists_dependencies_with_remote_id_kind_direction() {
        let conn = setup_dependency_db();
        persist_pulled_dependencies(
            &conn,
            &[link(11, "blocks", 2, 1), link(12, "relates", 1, 3)],
        );
        assert_eq!(
            dep_row(&conn, 11),
            ("i2".into(), "i1".into(), "blocks".into(), "blocking".into())
        );
        assert_eq!(
            dep_row(&conn, 12),
            ("i1".into(), "i3".into(), "relates".into(), "relates".into())
        );
    }

    #[test]
    fn pull_is_idempotent_and_does_not_duplicate_across_both_issues() {
        let conn = setup_dependency_db();
        persist_pulled_dependencies(&conn, &[link(12, "relates", 1, 3)]);
        // The other issue's detail reports the same edge with flipped orientation.
        persist_pulled_dependencies(&conn, &[link(12, "relates", 3, 1)]);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM issue_dependencies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn pull_adopts_remote_id_for_locally_created_edge() {
        let conn = setup_dependency_db();
        conn.execute(
            "INSERT INTO issue_dependencies (id, blocking_issue_id, blocked_issue_id, created_at, updated_at)
             VALUES ('d1', 'i1', 'i2', 'x', 'x')",
            [],
        )
        .unwrap();
        persist_pulled_dependencies(&conn, &[link(40, "blocks", 1, 2)]);
        let remote: Option<i64> = conn
            .query_row(
                "SELECT remote_id FROM issue_dependencies WHERE id = 'd1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remote, Some(40));
    }

    #[test]
    fn pull_skips_edges_to_unknown_issues() {
        let conn = setup_dependency_db();
        persist_pulled_dependencies(&conn, &[link(50, "blocks", 1, 999)]);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM issue_dependencies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn create_response_fields_are_stored() {
        let conn = setup_dependency_db();
        conn.execute(
            "INSERT INTO issue_dependencies (id, blocking_issue_id, blocked_issue_id, created_at, updated_at)
             VALUES ('d1', 'i1', 'i2', 'x', 'x')",
            [],
        )
        .unwrap();
        queries::set_issue_dependency_sync_fields(&conn, "d1", 77, "blocks", "blocking").unwrap();
        let d = queries::get_issue_dependency(&conn, "d1").unwrap().unwrap();
        assert_eq!(
            (d.remote_id, d.kind.as_str(), d.direction.as_str()),
            (Some(77), "blocks", "blocking")
        );
    }

    fn detail_dto(id: i64, title: &str, updated_at: &str) -> IssueDto {
        serde_json::from_value(serde_json::json!({
            "id": id, "identifier": format!("T-{id}"), "title": title,
            "priority": "medium", "estimate": null, "due_date": null,
            "lane": {"id": 10, "name": "B"}, "assignee": null, "creator": null,
            "project": null, "labels": [],
            "created_at": "2026-01-01T00:00:00.000Z", "updated_at": updated_at,
            "description": format!("desc {id}"),
            "parent_issue": {"id": 1, "identifier": "T-1"},
            "blocking_issues": [{"id": 3, "identifier": "T-3", "title": "x", "dependency_id": 55}],
            "blocked_issues": [],
            "started_at": "2026-02-01T00:00:00.000Z",
            "completed_at": "2026-03-01T00:00:00.000Z",
            "canceled_at": null
        }))
        .unwrap()
    }

    fn feed_comment(id: i64, parent: Option<i64>, issue: Option<i64>, body: &str) -> CommentDto {
        CommentDto {
            id,
            body: body.into(),
            parent_id: parent,
            user: None,
            created_at: "2026-02-01T00:00:00.000Z".into(),
            updated_at: "2026-02-01T00:00:00.000Z".into(),
            replies: None,
            issue_id: issue,
            project_id: None,
        }
    }

    #[test]
    fn feed_comments_link_issue_and_parent_regardless_of_order() {
        let conn = setup_labels_db();
        conn.execute("UPDATE issues SET remote_id = 50 WHERE id = 'i1'", []).unwrap();
        // Reply delivered before its parent.
        let dtos = vec![
            feed_comment(2, Some(1), Some(50), "reply"),
            feed_comment(1, None, Some(50), "top"),
        ];
        assert_eq!(apply_pulled_comments(&conn, dtos), 0);
        let all = queries::comments_for_issue(&conn, "i1").unwrap();
        assert_eq!(all.len(), 2);
        let top = all.iter().find(|c| c.remote_id == Some(1)).unwrap();
        let reply = all.iter().find(|c| c.remote_id == Some(2)).unwrap();
        assert_eq!(reply.parent_id.as_deref(), Some(top.id.as_str()));
    }

    #[test]
    fn feed_comments_are_idempotent_and_update_edits() {
        let conn = setup_labels_db();
        conn.execute("UPDATE issues SET remote_id = 50 WHERE id = 'i1'", []).unwrap();
        let dtos = vec![feed_comment(1, None, Some(50), "v1")];
        assert_eq!(apply_pulled_comments(&conn, dtos.clone()), 0);
        assert_eq!(apply_pulled_comments(&conn, dtos), 0);
        assert_eq!(apply_pulled_comments(&conn, vec![feed_comment(1, None, Some(50), "v2")]), 0);
        let all = queries::comments_for_issue(&conn, "i1").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].body, "v2");
    }

    #[test]
    fn feed_comments_skip_ownerless_and_unknown_issue_without_failing() {
        let conn = setup_labels_db();
        conn.execute("UPDATE issues SET remote_id = 50 WHERE id = 'i1'", []).unwrap();
        let dtos = vec![
            feed_comment(1, None, None, "ownerless"),
            feed_comment(2, None, Some(999), "unknown issue"),
            // Parent never seen: stored top-level, not a failure.
            feed_comment(3, Some(77), Some(50), "orphan reply"),
        ];
        assert_eq!(apply_pulled_comments(&conn, dtos), 0);
        let all = queries::comments_for_issue(&conn, "i1").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].parent_id, None);
    }

    fn feed_project_comment(id: i64, parent: Option<i64>, project: i64, body: &str) -> CommentDto {
        CommentDto {
            issue_id: None,
            project_id: Some(project),
            ..feed_comment(id, parent, None, body)
        }
    }

    fn insert_test_project(conn: &Connection, id: &str, remote: i64) {
        exec(
            conn,
            &format!(
                "INSERT INTO projects (id, remote_id, team_id, name, created_at, updated_at)
                 VALUES ('{id}', {remote}, 't1', 'P', '2026-01-01', '2026-01-01')"
            ),
        );
    }

    #[test]
    fn feed_applies_project_comments_parents_first_and_skips_unknown_project() {
        let conn = setup_labels_db();
        insert_test_project(&conn, "p1", 80);
        // Reply delivered before its parent; one row for a project we lack.
        let dtos = vec![
            feed_project_comment(2, Some(1), 80, "reply"),
            feed_project_comment(1, None, 80, "top"),
            feed_project_comment(3, None, 999, "unknown project"),
        ];
        assert_eq!(apply_pulled_comments(&conn, dtos), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments"), 1 + 1);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM comments WHERE project_id='p1' AND issue_id IS NULL"),
            2
        );
        let top = queries::find_comment_by_remote_id(&conn, 1).unwrap().unwrap();
        let reply = queries::find_comment_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(top.parent_id, None);
        assert_eq!(reply.parent_id.as_deref(), Some(top.id.as_str()));
        assert_eq!(top.owner(), crate::db::models::CommentOwner::Project("p1".into()));
    }

    #[test]
    fn comment_tombstone_deletes_project_comment_and_replies() {
        let conn = setup_labels_db();
        insert_test_project(&conn, "p1", 80);
        let dtos = vec![
            feed_project_comment(1, None, 80, "top"),
            feed_project_comment(2, Some(1), 80, "reply"),
        ];
        assert_eq!(apply_pulled_comments(&conn, dtos), 0);
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("Comment", 1)]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments"), 0);
    }

    #[test]
    fn pull_cursor_is_stored_per_team_and_resource() {
        let conn = setup_labels_db();
        assert_eq!(queries::get_pull_cursor(&conn, "t1", "issues").unwrap(), None);
        queries::set_pull_cursor(&conn, "t1", "issues", "A").unwrap();
        queries::set_pull_cursor(&conn, "t1", "issues", "B").unwrap();
        queries::set_pull_cursor(&conn, "t1", "comments", "C").unwrap();
        assert_eq!(
            queries::get_pull_cursor(&conn, "t1", "issues").unwrap().as_deref(),
            Some("B")
        );
        assert_eq!(
            queries::get_pull_cursor(&conn, "t2", "issues").unwrap(),
            None
        );
        queries::clear_pull_cursor(&conn, "t1", "issues").unwrap();
        assert_eq!(queries::get_pull_cursor(&conn, "t1", "issues").unwrap(), None);
        assert!(queries::get_pull_cursor(&conn, "t1", "comments").unwrap().is_some());
    }

    #[test]
    fn full_issue_pull_is_the_incremental_request_without_updated_since() {
        let full = issue_pull_query(None, Some(9));
        let incremental = issue_pull_query(Some("2026-10-30T12:00:00Z"), Some(9));
        assert!(full.include_archived && full.include_detail);
        assert_eq!(full.updated_since, None);
        assert_eq!(
            issues::IssuePageQuery {
                updated_since: None,
                ..incremental
            }
            .query_string(),
            full.query_string()
        );
        assert!(full.query_string().contains("include_archived=1"));
    }

    #[test]
    fn dedupe_keeps_last_delivery_in_first_seen_order() {
        let rows = vec![(1, "a"), (2, "b"), (1, "c")];
        assert_eq!(dedupe_by_id(rows, |r| r.0), vec![(1, "c"), (2, "b")]);
    }

    #[test]
    fn detail_rows_set_description_parent_dependencies_and_lifecycle() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        let dtos = vec![
            detail_dto(2, "two", "2026-05-01T00:00:00.000Z"),
            // Overlap re-delivery of the same row is idempotent.
            detail_dto(2, "two", "2026-05-01T00:00:00.000Z"),
        ];
        let dtos = dedupe_by_id(dtos, |d| d.id);
        assert_eq!(apply_pulled_issues(&conn, "t1", dtos.clone()), 0);
        assert_eq!(apply_pulled_issues(&conn, "t1", dtos), 0);
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(i.description.as_deref(), Some("desc 2"));
        assert_eq!(i.parent_issue_id.as_deref(), Some("i1"));
        assert_eq!(i.started_at.as_deref(), Some("2026-02-01T00:00:00.000Z"));
        assert_eq!(i.completed_at.as_deref(), Some("2026-03-01T00:00:00.000Z"));
        assert_eq!(i.canceled_at, None);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issues WHERE remote_id = 2"), 1);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM issue_dependencies WHERE remote_id = 55"),
            1
        );
    }

    fn dep_count(conn: &Connection) -> i64 {
        count(conn, "SELECT COUNT(*) FROM issue_dependencies")
    }

    fn insert_dep(conn: &Connection, id: &str, remote: Option<i64>, blocking: &str, blocked: &str) {
        conn.execute(
            "INSERT INTO issue_dependencies
                (id, remote_id, blocking_issue_id, blocked_issue_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'x', 'x')",
            rusqlite::params![id, remote, blocking, blocked],
        )
        .unwrap();
    }

    #[test]
    fn dependency_tombstone_removes_edge_and_retires_pending_sync() {
        let conn = setup_dependency_db();
        insert_dep(&conn, "d1", Some(6), "i1", "i2");
        insert_dep(&conn, "d2", Some(7), "i1", "i3");
        conn.execute(
            "INSERT INTO sync_queue (entity_type, entity_id, action, created_at)
             VALUES ('dependency', 'd1', 'delete', 'x')",
            [],
        )
        .unwrap();
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("IssueDependency", 6)]), 0);
        assert_eq!(dep_count(&conn), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issue_dependencies WHERE id = 'd2'"), 1);
        assert_eq!(
            count(
                &conn,
                "SELECT COUNT(*) FROM sync_queue WHERE entity_id = 'd1' AND synced_at IS NULL"
            ),
            0
        );
        // Repeat and unknown ids are safe.
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("IssueDependency", 6)]), 0);
        assert_eq!(apply_tombstones(&conn, "t1", &[tomb("IssueDependency", 999)]), 0);
        assert_eq!(dep_count(&conn), 1);
    }

    fn detail_without_links(id: i64, updated_at: &str) -> IssueDto {
        let mut dto = detail_dto(id, "two", updated_at);
        dto.blocking_issues = Some(vec![]);
        dto
    }

    #[test]
    fn pulled_detail_missing_an_edge_removes_it_on_either_side() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        // i2 (remote 2) is pulled. Stale edges on both sides, mixed kinds.
        insert_dep(&conn, "out", Some(61), "i2", "i3");
        insert_dep(&conn, "in", Some(62), "i1", "i2");
        conn.execute(
            "UPDATE issue_dependencies SET kind='relates', direction='relates' WHERE id='in'",
            [],
        )
        .unwrap();
        // An edge not touching i2 must survive.
        insert_dep(&conn, "other", Some(63), "i1", "i3");
        let dto = detail_without_links(2, "2026-05-01T00:00:00.000Z");
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![dto]), 0);
        assert_eq!(dep_count(&conn), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issue_dependencies WHERE id='other'"), 1);
    }

    #[test]
    fn pulled_detail_keeps_pending_local_edges_without_remote_id() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        insert_dep(&conn, "pending", None, "i2", "i3");
        let dto = detail_without_links(2, "2026-05-01T00:00:00.000Z");
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![dto]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issue_dependencies WHERE id='pending'"), 1);
    }

    #[test]
    fn pulled_detail_keeps_edges_the_server_still_has() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        // detail_dto(2) reports remote issue 3 blocking it via dependency 55.
        insert_dep(&conn, "kept", Some(55), "i3", "i2");
        insert_dep(&conn, "stale", Some(56), "i2", "i1");
        let dto = detail_dto(2, "two", "2026-05-01T00:00:00.000Z");
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![dto]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issue_dependencies WHERE id='kept'"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issue_dependencies WHERE id='stale'"), 0);
    }

    // ── Issues moved to another team on the web (JRST-69) ──

    /// i1..i3 (remote 1..3) live in t1; t2 is a second team with lane l2.
    fn setup_move_db() -> Connection {
        let conn = setup_dependency_db();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES ('t2', 'U', 'U', 701, ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lanes (id, name, position, team_id, remote_id, created_at, updated_at)
             VALUES ('l2', 'B', 0, 't2', 20, ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn
    }

    /// Remote issue 2 as the target team lists it after the move.
    fn moved_dto() -> IssueDto {
        let mut dto = detail_dto(2, "two", "2026-05-01T00:00:00.000Z");
        dto.identifier = "U-9".into();
        dto.lane = Some(serde_json::from_value(serde_json::json!({"id": 20, "name": "B"})).unwrap());
        dto
    }

    fn team_pull(team: &str, full_listing: bool, seen: &[i64]) -> IssueTeamPull {
        IssueTeamPull {
            local_team_id: team.into(),
            full_listing,
            seen: seen.to_vec(),
        }
    }

    fn assert_moved_not_archived_or_duplicated(conn: &Connection) {
        let i = queries::find_issue_by_remote_id(conn, 2).unwrap().unwrap();
        assert_eq!((i.id.as_str(), i.team_id.as_str(), i.lane_id.as_str()), ("i2", "t2", "l2"));
        assert_eq!(i.team_number, Some(9));
        assert_eq!(i.archived_at, None);
        assert_eq!(count(conn, "SELECT COUNT(*) FROM issues WHERE remote_id = 2"), 1);
        assert_eq!(count(conn, "SELECT COUNT(*) FROM issues WHERE team_id='t1' AND id='i2'"), 0);
    }

    #[test]
    fn moved_issue_follows_when_source_is_pulled_before_target() {
        let conn = setup_move_db();
        let pulls = [team_pull("t1", true, &[1, 3]), team_pull("t2", true, &[2])];
        assert_eq!(apply_pulled_issues(&conn, "t2", vec![moved_dto()]), 0);
        archive_unlisted_issues(&conn, &pulls, true);
        assert_moved_not_archived_or_duplicated(&conn);
    }

    #[test]
    fn moved_issue_follows_when_target_is_pulled_before_source() {
        let conn = setup_move_db();
        let pulls = [team_pull("t2", true, &[2]), team_pull("t1", true, &[1, 3])];
        assert_eq!(apply_pulled_issues(&conn, "t2", vec![moved_dto()]), 0);
        archive_unlisted_issues(&conn, &pulls, true);
        assert_moved_not_archived_or_duplicated(&conn);
    }

    #[test]
    fn issue_missing_from_only_the_source_listing_is_not_archived() {
        let conn = setup_move_db();
        // Only the source team was pulled (target skipped or failed).
        let pulls = [team_pull("t1", true, &[1, 3])];
        archive_unlisted_issues(&conn, &pulls, false);
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!((i.team_id.as_str(), i.archived_at), ("t1", None));
    }

    #[test]
    fn issue_gone_from_every_listing_is_still_archived() {
        let conn = setup_move_db();
        let pulls = [team_pull("t1", true, &[1, 3]), team_pull("t2", true, &[])];
        archive_unlisted_issues(&conn, &pulls, true);
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert!(i.archived_at.is_some());
        // Incremental pulls never archive.
        let j = queries::find_issue_by_remote_id(&conn, 3).unwrap().unwrap();
        assert_eq!(j.archived_at, None);
    }

    #[test]
    fn pulled_issue_records_the_recurring_issue_that_spawned_it() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        crate::db::recurring_issues::apply_pulled(
            &conn,
            "t1",
            &[serde_json::from_value(serde_json::json!({
                "id": 5, "title": "Weekly", "description": null, "priority": "no_priority",
                "frequency": "weekly", "interval": 1, "weekday": 1, "day_of_month": null,
                "time_zone": null, "active": true, "next_run_on": null, "last_run_at": null,
                "assignee": null, "project": null, "lane": null, "label_ids": [],
                "creator": null, "created_at": "2026-01-01T00:00:00.000Z",
                "updated_at": "2026-01-01T00:00:00.000Z",
            }))
            .unwrap()],
            Some("s"),
        )
        .unwrap();
        let mut dto = detail_dto(2, "two", "2026-05-01T00:00:00.000Z");
        dto.recurring_issue_id = Some(5);
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![dto]), 0);
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        let origin = crate::db::recurring_issues::origin_of_issue(&conn, &i.id).unwrap().unwrap();
        assert_eq!(origin.title, "Weekly");

        // Server detached it: cleared.
        let back = detail_dto(2, "two", "2026-06-01T00:00:00.000Z");
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![back]), 0);
        assert_eq!(crate::db::recurring_issues::origin_id(&conn, &i.id).unwrap(), None);
    }

    #[test]
    fn archived_at_on_pulled_rows_archives_and_unarchives() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        let mut dto = detail_dto(2, "two", "2026-05-01T00:00:00.000Z");
        dto.archived_at = Some("2026-05-01T00:00:00.000Z".into());
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![dto]), 0);
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(i.archived_at.as_deref(), Some("2026-05-01T00:00:00.000Z"));

        let back = detail_dto(2, "two", "2026-06-01T00:00:00.000Z");
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![back]), 0);
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(i.archived_at, None);
    }

    #[test]
    fn pending_local_edit_survives_incremental_apply() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        conn.execute(
            "UPDATE issues SET title = 'local', updated_at = '2026-09-01T00:00:00.000Z' WHERE id = 'i2'",
            [],
        )
        .unwrap();
        sync_queue::enqueue(&conn, "issue", "i2", "update", None).unwrap();
        let skipped = apply_pulled_issues(
            &conn,
            "t1",
            vec![detail_dto(2, "remote", "2026-05-01T00:00:00.000Z")],
        );
        assert_eq!(skipped, 0);
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(i.title, "local");
    }

    #[test]
    fn fetched_issue_updates_a_clean_local_row() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        apply_fetched_issue(&conn, "t1", detail_dto(2, "fresh", "2026-05-01T00:00:00.000Z"))
            .unwrap();
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(i.title, "fresh");
    }

    #[test]
    fn fetched_issue_keeps_pending_local_edit() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        conn.execute(
            "UPDATE issues SET title = 'local', updated_at = '2026-09-01T00:00:00.000Z' WHERE id = 'i2'",
            [],
        )
        .unwrap();
        sync_queue::enqueue(&conn, "issue", "i2", "update", None).unwrap();
        apply_fetched_issue(&conn, "t1", detail_dto(2, "remote", "2026-05-01T00:00:00.000Z"))
            .unwrap();
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(i.title, "local");
    }

    #[test]
    fn fetched_issue_does_not_resurrect_a_local_tombstone() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        queries::delete_issue(&conn, "i2").unwrap();
        apply_fetched_issue(&conn, "t1", detail_dto(2, "remote", "2026-12-01T00:00:00.000Z"))
            .unwrap();
        let (title, deleted): (String, Option<String>) = conn
            .query_row("SELECT title, deleted_at FROM issues WHERE id = 'i2'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_ne!(title, "remote");
        assert!(deleted.is_some());
    }

    #[test]
    fn fetched_issue_that_cannot_be_applied_is_an_error() {
        let conn = setup_dependency_db(); // lane has no remote id
        assert!(apply_fetched_issue(&conn, "t1", detail_dto(2, "x", "2026-05-01T00:00:00.000Z"))
            .is_err());
    }

    #[test]
    fn unresolvable_row_counts_as_skipped() {
        let conn = setup_dependency_db(); // lane has no remote id -> lane lookup fails
        let skipped = apply_pulled_issues(
            &conn,
            "t1",
            vec![detail_dto(2, "x", "2026-05-01T00:00:00.000Z")],
        );
        assert_eq!(skipped, 1);
    }

    #[test]
    fn record_attempt_records_time_once_and_reports_earlier_attempt() {
        let conn = setup_labels_db();
        let entry = queue_create(&conn, "issue", "i1");
        let first = record_attempt_on(&conn, entry, "issues").unwrap();
        assert!(first.previous.is_none());
        let stored = stored_attempt(&conn, entry);
        let second = record_attempt_on(&conn, entry, "issues").unwrap();
        assert_eq!(second.previous.as_deref(), Some(stored.as_str()));
        assert_eq!(stored_attempt(&conn, entry), stored);
    }

    #[test]
    fn record_attempt_collects_linked_remote_ids() {
        let conn = setup_labels_db();
        insert_test_label(&conn, "a", Some(41));
        insert_test_label(&conn, "b", None);
        let entry = queue_create(&conn, "label", "b");
        let a = record_attempt_on(&conn, entry, "labels").unwrap();
        assert_eq!(a.taken, [41].into_iter().collect::<HashSet<_>>());
        assert!(record_attempt_on(&conn, entry, "comments").is_err());
    }

    #[test]
    fn adopting_a_match_links_and_marks_synced_without_posting() {
        let mut conn = setup_labels_db();
        let entry = queue_create(&conn, "issue", "i1");
        let _ = record_attempt_on(&conn, entry, "issues").unwrap();
        let retry = record_attempt_on(&conn, entry, "issues").unwrap();
        let since = retry.previous.unwrap();
        let c = vec![issue_dto(55, "Issue", Some(7), "2999-01-01T00:00:00.000Z")];
        let id = match_issue(&c, 7, "Issue", &since, &retry.taken).unwrap();
        save_remote_id_and_mark_synced(&mut conn, "issues", "i1", id, entry).unwrap();
        assert_eq!(issue_remote(&conn), Some(55));
        assert_eq!(sync_queue::pending_count(&conn).unwrap(), 0);
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

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    fn auth_push_report() -> SyncReport {
        let mut r = SyncReport::default();
        r.failed = 1;
        r.failures.push(SyncFailure {
            entity_type: "issue".into(),
            entity_id: "x".into(),
            kind: SyncErrorKind::Auth,
            message: "unauthorized: nope".into(),
        });
        r
    }

    fn pull_outcome(processed: usize) -> PullOutcome {
        let mut report = SyncReport::default();
        report.processed = processed;
        PullOutcome {
            report,
            deleted_orphan_team: None,
            current_user: None,
            can_write: None,
        }
    }

    #[test]
    fn cycle_pushes_before_pulling_and_reports_both() {
        use std::cell::RefCell;
        let order = RefCell::new(Vec::new());
        let report = block_on(run_cycle(
            None,
            async {
                order.borrow_mut().push("push");
                let mut r = SyncReport::default();
                r.processed = 2;
                r
            },
            || async {
                order.borrow_mut().push("pull");
                pull_outcome(5)
            },
        ));
        assert_eq!(*order.borrow(), vec!["push", "pull"]);
        assert_eq!(report.push.processed, 2);
        assert_eq!(report.pull.processed, 5);
    }

    #[test]
    fn cycle_401_on_push_skips_pull() {
        let mut pulled = false;
        let report = block_on(run_cycle(None, async { auth_push_report() }, || {
            pulled = true;
            async { pull_outcome(5) }
        }));
        assert!(!pulled);
        assert!(report.auth_failed());
        assert_eq!(report.pull.processed, 0);
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

    /// i2 (remote 2) moved locally t1 -> t2 and queued; returns the queued payload.
    fn moved_locally() -> (Connection, String) {
        let conn = setup_move_db();
        queries::move_issue_to_team(&conn, "i2", "t2").unwrap();
        assert!(queries::queue_issue_move(&conn, "i2", "t1").unwrap());
        let payload = sync_queue::pending_entries(&conn)
            .unwrap()
            .into_iter()
            .find(|e| e.action == "move")
            .and_then(|e| e.payload)
            .unwrap();
        (conn, payload)
    }

    #[test]
    fn move_prep_ready_with_the_ids_captured_at_enqueue() {
        let (conn, payload) = moved_locally();
        match prep_issue_move_in(&conn, "i2", Some(&payload)) {
            Prep::Ready(p) => {
                assert_eq!(
                    (p.source_team_remote_id, p.issue_remote_id, p.target_team_remote_id),
                    (700, 2, 701)
                );
            }
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
    }

    #[test]
    fn move_prep_waits_until_the_target_team_has_a_remote_id() {
        let conn = setup_move_db();
        conn.execute("UPDATE teams SET remote_id = NULL WHERE id = 't2'", []).unwrap();
        queries::move_issue_to_team(&conn, "i2", "t2").unwrap();
        queries::queue_issue_move(&conn, "i2", "t1").unwrap();
        let payload = sync_queue::pending_entries(&conn).unwrap().remove(0).payload.unwrap();
        match prep_issue_move_in(&conn, "i2", Some(&payload)) {
            Prep::Done(o) => assert!(is_retry(&o)),
            Prep::Ready(_) => panic!("target team has no remote id yet"),
        }
        // The team create drains; the same payload now resolves it.
        conn.execute("UPDATE teams SET remote_id = 701 WHERE id = 't2'", []).unwrap();
        match prep_issue_move_in(&conn, "i2", Some(&payload)) {
            Prep::Ready(p) => assert_eq!(p.target_team_remote_id, 701),
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
    }

    #[test]
    fn move_prep_drops_a_missing_payload_or_issue() {
        let (conn, payload) = moved_locally();
        assert!(matches!(prep_issue_move_in(&conn, "i2", None), Prep::Done(o) if is_drop(&o)));
        assert!(
            matches!(prep_issue_move_in(&conn, "gone", Some(&payload)), Prep::Done(o) if is_drop(&o))
        );
    }

    #[test]
    fn move_response_sets_the_server_identifier_and_lane() {
        let (conn, _) = moved_locally();
        let before = queries::get_issue(&conn, "i2").unwrap().unwrap();
        assert_ne!(before.team_number, Some(9));
        let out = issue_move_outcome(&conn, "i2", 700, Ok(moved_dto()));
        assert!(matches!(out, Outcome::Success), "{out:?}");
        let i = queries::get_issue(&conn, "i2").unwrap().unwrap();
        assert_eq!((i.team_id.as_str(), i.lane_id.as_str(), i.team_number), ("t2", "l2", Some(9)));
        // Only identifier/lane/assignee/parent come from the response.
        assert_eq!(i.title, before.title);
    }

    #[test]
    fn move_rejections_drop_the_entry_and_server_errors_retry() {
        let (conn, _) = moved_locally();
        let nf = ApiError::NotFound("Team not found or access denied".into());
        assert!(is_drop(&issue_move_outcome(&conn, "i2", 700, Err(nf))));
        let inv = ApiError::Validation { errors: vec!["Issue is already in that team".into()] };
        match issue_move_outcome(&conn, "i2", 700, Err(inv)) {
            Outcome::DropAndLog(m) => assert!(m.contains("already in that team"), "{m}"),
            o => panic!("{o:?}"),
        }
        let down = ApiError::Server { status: 503, message: "x".into() };
        assert!(is_retry(&issue_move_outcome(&conn, "i2", 700, Err(down))));
    }

    // ── Per-issue ordering around a queued move ──────────────────────────

    fn queue_update(conn: &Connection) -> i64 {
        sync_queue::enqueue(conn, "issue", "i2", "update", None).unwrap();
        conn.last_insert_rowid()
    }

    fn move_entry_id(conn: &Connection) -> i64 {
        sync_queue::pending_entries(conn)
            .unwrap()
            .into_iter()
            .find(|e| e.action == "move")
            .and_then(|e| e.id)
            .unwrap()
    }

    #[test]
    fn update_queued_after_a_move_waits_until_the_move_drains() {
        let (conn, _) = moved_locally();
        let update = queue_update(&conn);
        match prep_issue_update_in(&conn, "i2", update) {
            Prep::Done(o) => assert!(is_retry(&o), "{o:?}"),
            Prep::Ready(_) => panic!("update must wait for the pending move"),
        }
        // Once the move succeeded, the update goes to the new team's URL.
        sync_queue::mark_synced(&conn, move_entry_id(&conn)).unwrap();
        match prep_issue_update_in(&conn, "i2", update) {
            Prep::Ready(p) => assert_eq!((p.team_remote_id, p.remote_id), (701, 2)),
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
    }

    #[test]
    fn update_queued_before_a_move_goes_to_the_source_team() {
        let conn = setup_move_db();
        let update = queue_update(&conn);
        queries::move_issue_to_team(&conn, "i2", "t2").unwrap();
        assert!(queries::queue_issue_move(&conn, "i2", "t1").unwrap());
        assert!(update < move_entry_id(&conn));
        match prep_issue_update_in(&conn, "i2", update) {
            Prep::Ready(p) => {
                assert_eq!((p.team_remote_id, p.remote_id), (700, 2));
                // Post-move lane/labels aren't sent to the old team.
                assert_eq!(p.params.lane_id, None);
                assert_eq!(p.params.label_ids, None);
                assert!(p.params.title.is_some());
            }
            Prep::Done(o) => panic!("expected ready, got {o:?}"),
        }
    }

    #[test]
    fn a_dropped_move_does_not_block_later_updates() {
        let (conn, _) = moved_locally();
        let update = queue_update(&conn);
        // A 422 drops the entry: it is marked synced and no longer pending.
        sync_queue::mark_synced(&conn, move_entry_id(&conn)).unwrap();
        assert!(matches!(prep_issue_update_in(&conn, "i2", update), Prep::Ready(_)));
    }

    #[test]
    fn other_issues_ignore_the_move() {
        let (conn, _) = moved_locally();
        assert_eq!(move_order(&conn, "i1", i64::MAX).unwrap(), None);
    }
}
