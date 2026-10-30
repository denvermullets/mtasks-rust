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
use crate::api::{
    comments, dependencies, issues, labels, lanes, members, projects, teams, users, ApiClient,
    ApiError, CommentParams, Conditional, Direction, IssueParams, LabelParams, ProjectParams,
};
use crate::db::models::{Issue, Priority, Project, SyncQueueEntry};
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
    /// pull reflects the user's own edits. A cheap `/me` preflight reads write
    /// scope so a read-only token skips the push half (entries stay pending,
    /// [`PushPause::ReadOnly`]); the pull still runs. A 401 on the push half
    /// pauses the whole cycle (the pull is skipped).
    pub async fn full_cycle(client: &ApiClient) -> FullSyncReport {
        let can_write = users::me(client)
            .await
            .ok()
            .map(|m| m.token.has_write_scope());
        run_cycle(can_write, Self::drain_if_writable(client, can_write), || {
            Self::pull(client)
        })
        .await
    }

    /// Push-only cycle for the local-write trigger: the same `/me` write-scope
    /// gate as [`Self::full_cycle`], then drain the queue. No pull, so a burst
    /// of edits costs one `/me` plus the pushes.
    pub async fn push_cycle(client: &ApiClient) -> FullSyncReport {
        let can_write = users::me(client)
            .await
            .ok()
            .map(|m| m.token.has_write_scope());
        let push = Self::drain_if_writable(client, can_write).await;
        push_only_report(can_write, push)
    }

    /// Drain unless `/me` said the token cannot write; then skip every push
    /// (entries stay pending) and report [`PushPause::ReadOnly`].
    pub async fn drain_if_writable(client: &ApiClient, can_write: Option<bool>) -> SyncReport {
        if can_write == Some(false) {
            return read_only_report();
        }
        Self::drain(client).await
    }

    pub async fn drain(client: &ApiClient) -> SyncReport {
        let mut report = SyncReport::default();

        let entries = match read_pending() {
            Ok(v) => v,
            Err(msg) => {
                eprintln!("[sync] outcome=fatal reason=\"{msg}\"");
                report.last_error = Some(msg);
                return report;
            }
        };

        for entry in entries {
            let entry_id = match entry.id {
                Some(id) => id,
                None => continue,
            };

            if entry_team_is_inaccessible(&entry) {
                continue;
            }

            let outcome = process(client, &entry, entry_id).await;
            if record(&mut report, &entry, entry_id, outcome) {
                break;
            }
        }

        report
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

async fn process(client: &ApiClient, entry: &SyncQueueEntry, entry_id: i64) -> Outcome {
    match (entry.entity_type.as_str(), entry.action.as_str()) {
        ("issue", "create") => issue_create(client, &entry.entity_id, entry_id).await,
        ("issue", "update") => issue_update(client, &entry.entity_id).await,
        ("issue", "delete") => {
            issue_delete(client, &entry.entity_id, entry.payload.as_deref()).await
        }
        ("project", "create") => project_create(client, &entry.entity_id, entry_id).await,
        ("project", "update") => project_update(client, &entry.entity_id).await,
        ("project", "delete") => project_delete(client, entry.payload.as_deref()).await,
        ("label", "create") => label_create(client, &entry.entity_id, entry_id).await,
        ("comment", "create") => comment_create(client, &entry.entity_id, entry_id).await,
        ("dependency", "create") => dependency_create(client, &entry.entity_id, entry_id).await,
        ("dependency", "delete") => dependency_delete(client, entry.payload.as_deref()).await,
        (et, ac) => Outcome::DropAndLog(format!("unsupported ({et},{ac})")),
    }
}

/// Apply `outcome` to the report/queue. Returns true when the drain should
/// stop pushing (remaining entries are left untouched, hence still pending).
fn record(
    report: &mut SyncReport,
    entry: &SyncQueueEntry,
    entry_id: i64,
    outcome: Outcome,
) -> bool {
    match outcome {
        Outcome::Success => {
            if let Err(e) = mark_synced(entry_id) {
                eprintln!(
                    "[sync] entry_id={entry_id} type={} action={} outcome=mark_synced_failed reason=\"{e}\"",
                    entry.entity_type, entry.action
                );
            }
            report.processed += 1;
        }
        Outcome::Created => report.processed += 1,
        Outcome::PausePushes(pause, reason) => {
            eprintln!(
                "[sync] entry_id={entry_id} type={} action={} outcome=pause reason=\"{reason}\"",
                entry.entity_type, entry.action
            );
            report.failed += 1;
            record_push_failure(report, entry, &reason);
            report.last_error = Some(reason);
            report.push_paused = Some(pause);
            return true;
        }
        Outcome::TeamAccessLost(team_remote_id, reason) => {
            eprintln!(
                "[sync] entry_id={entry_id} type={} action={} outcome=team_access_lost team={team_remote_id} reason=\"{reason}\"",
                entry.entity_type, entry.action
            );
            if let Ok(conn) = open() {
                let _ = queries::set_team_access_lost(&conn, team_remote_id, true);
            }
            report.failed += 1;
            record_push_failure(report, entry, &reason);
            report.last_error = Some(reason);
        }
        Outcome::DropAndLog(reason) => {
            eprintln!(
                "[sync] entry_id={entry_id} type={} action={} outcome=drop reason=\"{reason}\"",
                entry.entity_type, entry.action
            );
            let _ = mark_synced(entry_id);
            report.failed += 1;
            record_push_failure(report, entry, &reason);
            report.last_error = Some(reason);
        }
        Outcome::LeavePending(reason) => {
            eprintln!(
                "[sync] entry_id={entry_id} type={} action={} outcome=retry reason=\"{reason}\"",
                entry.entity_type, entry.action
            );
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
        report.failures.push(SyncFailure {
            entity_type: entry.entity_type.clone(),
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

fn prep_issue_update(entity_id: &str) -> Prep<IssueUpdatePrep> {
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

    let remote_id = match issue.remote_id {
        Some(id) => id,
        None => {
            return Prep::Done(Outcome::LeavePending(format!(
                "issue {entity_id} has no remote_id — create not yet drained"
            )))
        }
    };

    let team_remote_id = match queries::remote_id_for(&conn, "teams", &issue.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };

    let params = match build_issue_params(&conn, &issue) {
        Ok(p) => p,
        Err(o) => return Prep::Done(o),
    };

    Prep::Ready(IssueUpdatePrep {
        team_remote_id,
        remote_id,
        params,
    })
}

async fn issue_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match prep_issue_create(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };

    let attempt = match begin_attempt(entry_id, "issues") {
        Ok(a) => a,
        Err(o) => return o,
    };
    if let Some(since) = &attempt.previous {
        match find_issue_on_server(client, prep.team_remote_id, &prep.params, since, &attempt)
            .await
        {
            Ok(Some(remote_id)) => {
                return commit_create(entry_id, "issues", &prep.local_id, remote_id)
            }
            Ok(None) => {}
            Err(e) => return classify(&e),
        }
    }

    match issues::create(client, prep.team_remote_id, &prep.params).await {
        Ok(dto) => commit_create(entry_id, "issues", &prep.local_id, dto.id),
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

async fn issue_update(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match prep_issue_update(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };

    match issues::update(client, prep.team_remote_id, prep.remote_id, &prep.params).await {
        Ok(_) => Outcome::Success,
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

async fn issue_delete(client: &ApiClient, local_id: &str, payload: Option<&str>) -> Outcome {
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
const RECONCILE_PAGE_LIMIT: i64 = 500;

/// State read before a create's POST: when an earlier attempt began (if any)
/// and the server ids already linked locally for the entity's table.
struct Attempt {
    previous: Option<String>,
    taken: HashSet<i64>,
}

fn begin_attempt(entry_id: i64, table: &str) -> Result<Attempt, Outcome> {
    let conn = open().map_err(Outcome::LeavePending)?;
    begin_attempt_on(&conn, entry_id, table).map_err(Outcome::LeavePending)
}

fn begin_attempt_on(conn: &Connection, entry_id: i64, table: &str) -> Result<Attempt, String> {
    let taken = linked_remote_ids(conn, table)?;
    let previous =
        sync_queue::begin_attempt(conn, entry_id).map_err(|e| format!("begin_attempt: {e}"))?;
    Ok(Attempt { previous, taken })
}

fn linked_remote_ids(conn: &Connection, table: &str) -> Result<HashSet<i64>, String> {
    if !matches!(table, "issues" | "projects" | "labels") {
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

/// Label with the same name, case-insensitively, not already linked.
fn match_label(candidates: &[LabelDto], name: &str, taken: &HashSet<i64>) -> Option<i64> {
    let wanted = name.trim().to_lowercase();
    candidates
        .iter()
        .find(|d| d.name.trim().to_lowercase() == wanted && !taken.contains(&d.id))
        .map(|d| d.id)
}

async fn find_issue_on_server(
    client: &ApiClient,
    team_remote_id: i64,
    params: &IssueParams,
    attempted_at: &str,
    attempt: &Attempt,
) -> Result<Option<i64>, ApiError> {
    let Some(title) = params.title.as_deref() else {
        return Ok(None);
    };
    let me = users::me(client).await?;
    let filters = issues::IssueFilters {
        updated_since: Some(attempted_at.to_string()),
        limit: Some(RECONCILE_PAGE_LIMIT),
        ..Default::default()
    };
    let candidates = issues::list(client, team_remote_id, &filters).await?;
    Ok(match_issue(
        &candidates,
        me.id,
        title,
        attempted_at,
        &attempt.taken,
    ))
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
    let candidates = projects::list_updated_since(client, team_remote_id, attempted_at).await?;
    Ok(match_project(&candidates, name, attempted_at, &attempt.taken))
}

async fn find_label_on_server(
    client: &ApiClient,
    team_remote_id: i64,
    params: &LabelParams,
    attempt: &Attempt,
) -> Result<Option<i64>, ApiError> {
    let candidates = match labels::list_conditional(client, team_remote_id, None).await? {
        Conditional::Modified { data, .. } => data,
        Conditional::NotModified => return Ok(None),
    };
    Ok(match_label(&candidates, &params.name, &attempt.taken))
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
    let attempt = match begin_attempt(entry_id, "projects") {
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
    let attempt = match begin_attempt(entry_id, "labels") {
        Ok(a) => a,
        Err(o) => return o,
    };
    if attempt.previous.is_some() {
        match find_label_on_server(client, prep.team_remote_id, &prep.params, &attempt).await {
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

// ── Comments ──────────────────────────────────────────────────────────────

struct CommentCreatePrep {
    team_remote_id: i64,
    issue_remote_id: i64,
    params: CommentParams,
    local_id: String,
}

fn prep_comment_create(entity_id: &str) -> Prep<CommentCreatePrep> {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Prep::Done(Outcome::LeavePending(e)),
    };
    let comment = match queries::get_comment(&conn, entity_id) {
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
    // The comment is nested under its issue; both the issue and its team must be
    // synced before we can address the endpoint.
    let issue = match queries::get_issue(&conn, &comment.issue_id) {
        Ok(Some(i)) => i,
        Ok(None) => return Prep::Done(Outcome::DropAndLog("comment's issue missing".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let issue_remote_id = match issue.remote_id {
        Some(id) => id,
        None => return Prep::Done(Outcome::LeavePending("issue not synced yet".into())),
    };
    let team_remote_id = match queries::remote_id_for(&conn, "teams", &issue.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    let parent_remote_id = match resolve_comment_parent(&conn, &comment) {
        Ok(p) => p,
        Err(o) => return Prep::Done(o),
    };
    Prep::Ready(CommentCreatePrep {
        team_remote_id,
        issue_remote_id,
        params: CommentParams {
            body: comment.body,
            parent_id: parent_remote_id,
        },
        local_id: comment.id,
    })
}

/// Remote id of a reply's parent. A parent that hasn't synced yet leaves the
/// reply pending (the queue is FIFO so the parent normally goes first).
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
    let prep = match prep_comment_create(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    // Send the local UUID as the idempotency key so a re-drain (e.g. after a
    // crash between POST and mark_synced) returns the existing comment instead
    // of creating a duplicate.
    match comments::create(
        client,
        prep.team_remote_id,
        prep.issue_remote_id,
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
    local_id: String,
}

fn prep_dependency_create(entity_id: &str) -> Prep<DependencyCreatePrep> {
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
    let team_remote_id = match queries::remote_id_for(&conn, "teams", &blocking.team_id) {
        Ok(Some(id)) => id,
        Ok(None) => return Prep::Done(Outcome::LeavePending("team not synced yet".into())),
        Err(e) => return Prep::Done(Outcome::LeavePending(format!("db: {e}"))),
    };
    Prep::Ready(DependencyCreatePrep {
        team_remote_id,
        issue_remote_id,
        target_remote_id,
        local_id: dep.id,
    })
}

async fn dependency_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
    let prep = match prep_dependency_create(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    match dependencies::create(
        client,
        prep.team_remote_id,
        prep.issue_remote_id,
        prep.target_remote_id,
        Direction::Blocking,
    )
    .await
    {
        Ok(resp) => {
            // The desktop only sends `blocking`, so the response direction is
            // already relative to the source side; store what Rails echoed.
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
        ("comment", _) => {
            let c = queries::get_comment(conn, id).ok()??;
            team_remote(&queries::get_issue(conn, &c.issue_id).ok()??.team_id)
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
        // A 403 means the token lacks the scope, not that the edit is bad.
        // Keep it queued and stop pushing; a better token can send it later.
        ApiError::Forbidden(m) => {
            Outcome::PausePushes(PushPause::Forbidden, format!("forbidden: {m}"))
        }
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
    pub async fn pull(client: &ApiClient) -> PullOutcome {
        let mut report = SyncReport::default();
        let mut deleted_orphan_team = None;

        // Phase 1: identify the token's scope.
        let me = match users::me(client).await {
            Ok(m) => m,
            Err(e) => {
                let msg = format!("/me: {e}");
                eprintln!("[sync] outcome=fatal phase=pull reason=\"{msg}\"");
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
        for target in &targets {
            let mut steps = vec![
                pull_lanes_step(client, target.remote_id, &target.local_id).await,
                pull_labels_step(client, target.remote_id, &target.local_id).await,
                pull_members_step(client, target.remote_id, &target.local_id).await,
                pull_projects_step(client, target.remote_id, &target.local_id).await,
                pull_issues_step(client, target.remote_id, &target.local_id).await,
            ];
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

        PullOutcome {
            report,
            deleted_orphan_team,
            current_user,
            can_write,
        }
    }
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
                eprintln!("[sync] phase=pull_teams cleanup_orphan reason=\"db: {e}\"");
                None
            }
        }
    } else {
        None
    };

    Ok(deleted_orphan)
}

async fn pull_lanes_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    let stored = {
        let conn = open()?;
        queries::get_list_etag(&conn, local_team_id, "lanes").unwrap_or(None)
    };
    let (dtos, new_etag) =
        match lanes::list_conditional(client, team_remote_id, stored.as_deref())
            .await
            .map_err(|e| format!("lanes: {e}"))?
        {
            Conditional::NotModified => {
                eprintln!("[sync] phase=pull_lanes team={team_remote_id} 304 not modified");
                return Ok(());
            }
            Conditional::Modified { data, etag } => (data, etag),
        };

    let conn = open()?;
    for mut dto in dtos {
        // Rails omits team_id on the wire; inject from the URL path.
        dto.team_id = team_remote_id;

        let existing = queries::find_lane_by_remote_id(&conn, dto.id)
            .map_err(|e| format!("db (find lane): {e}"))?;
        let has_pending = match &existing {
            Some(l) => sync_queue::has_pending_entry(&conn, "lane", &l.id)
                .map_err(|e| format!("db: {e}"))?,
            None => false,
        };
        let remote_updated = dto.updated_at.clone().unwrap_or_default();

        if let Some(local) = &existing {
            if decide(&local.updated_at, &remote_updated, has_pending) == MergeDecision::KeepLocal {
                continue;
            }
        }

        let existing_uuid = existing.as_ref().map(|l| l.id.clone());
        let mut lane = dto
            .into_model(&conn, existing_uuid)
            .map_err(|e| format!("convert lane: {e}"))?;
        // into_model resolves team via local_uuid_for on the dto's team_id;
        // sanity-check we landed on the team we expected.
        if lane.team_id != local_team_id {
            lane.team_id = local_team_id.to_string();
        }
        queries::upsert_lane(&conn, &lane).map_err(|e| format!("db (upsert lane): {e}"))?;
    }

    // Lanes table has no archived_at column, so missing lanes are left in
    // place. Hard-deleting would risk cascading FK violations on issues that
    // still reference the lane locally. Revisit when the schema gains a
    // soft-delete column.
    store_list_etag(&conn, local_team_id, "lanes", new_etag.as_deref());
    Ok(())
}

async fn pull_labels_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    let stored = {
        let conn = open()?;
        queries::get_list_etag(&conn, local_team_id, "labels").unwrap_or(None)
    };
    let (dtos, new_etag) =
        match labels::list_conditional(client, team_remote_id, stored.as_deref())
            .await
            .map_err(|e| format!("labels: {e}"))?
        {
            Conditional::NotModified => {
                eprintln!("[sync] phase=pull_labels team={team_remote_id} 304 not modified");
                return Ok(());
            }
            Conditional::Modified { data, etag } => (data, etag),
        };

    let conn = open()?;
    for mut dto in dtos {
        dto.team_id = team_remote_id;

        let existing = queries::find_label_by_remote_id(&conn, dto.id)
            .map_err(|e| format!("db (find label): {e}"))?;
        let has_pending = match &existing {
            Some(l) => sync_queue::has_pending_entry(&conn, "label", &l.id)
                .map_err(|e| format!("db: {e}"))?,
            None => false,
        };
        let remote_updated = dto.updated_at.clone().unwrap_or_default();

        if let Some(local) = &existing {
            if decide(&local.updated_at, &remote_updated, has_pending) == MergeDecision::KeepLocal {
                continue;
            }
        }

        let existing_uuid = existing.as_ref().map(|l| l.id.clone());
        let mut label = dto
            .into_model(&conn, existing_uuid)
            .map_err(|e| format!("convert label: {e}"))?;
        if label.team_id != local_team_id {
            label.team_id = local_team_id.to_string();
        }
        queries::upsert_label(&conn, &label).map_err(|e| format!("db (upsert label): {e}"))?;
    }
    store_list_etag(&conn, local_team_id, "labels", new_etag.as_deref());
    Ok(())
}

/// Persist the ETag only after the list was fully applied, so a failed apply
/// is retried with a full fetch. A response without an ETag clears the stored one.
fn store_list_etag(conn: &Connection, team_id: &str, list: &str, etag: Option<&str>) {
    let res = match etag {
        Some(tag) => queries::set_list_etag(conn, team_id, list, tag),
        None => queries::clear_list_etag(conn, team_id, list),
    };
    if let Err(e) = res {
        eprintln!("[sync] phase=pull_{list} etag store failed: {e}");
    }
}

async fn pull_members_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    let stored = {
        let conn = open()?;
        queries::get_list_etag(&conn, local_team_id, "members").unwrap_or(None)
    };
    let (dtos, new_etag) =
        match members::list_conditional(client, team_remote_id, stored.as_deref())
            .await
            .map_err(|e| format!("members: {e}"))?
        {
            Conditional::NotModified => {
                eprintln!("[sync] phase=pull_members team={team_remote_id} 304 not modified");
                return Ok(());
            }
            Conditional::Modified { data, etag } => (data, etag),
        };

    let conn = open()?;
    for mut dto in dtos {
        dto.team_id = team_remote_id;

        let existing = queries::find_member_by_remote_id(&conn, dto.id)
            .map_err(|e| format!("db (find member): {e}"))?;
        let has_pending = match &existing {
            Some(m) => sync_queue::has_pending_entry(&conn, "member", &m.id)
                .map_err(|e| format!("db: {e}"))?,
            None => false,
        };
        let remote_updated = dto.updated_at.clone().unwrap_or_default();

        if let Some(local) = &existing {
            if decide(&local.updated_at, &remote_updated, has_pending) == MergeDecision::KeepLocal {
                continue;
            }
        }

        let existing_uuid = existing.as_ref().map(|m| m.id.clone());
        let mut member = dto
            .into_model(&conn, existing_uuid)
            .map_err(|e| format!("convert member: {e}"))?;
        if member.team_id != local_team_id {
            member.team_id = local_team_id.to_string();
        }
        queries::upsert_member(&conn, &member).map_err(|e| format!("db (upsert member): {e}"))?;
    }
    store_list_etag(&conn, local_team_id, "members", new_etag.as_deref());
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
    eprintln!(
        "[sync] phase=pull_projects team_remote_id={team_remote_id} mode={} rows={}",
        if since.is_some() { "incremental" } else { "full" },
        page.rows.len()
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

/// Pull a team's issues with full detail in paged requests. With a stored
/// cursor only rows changed since (cursor minus a small overlap) are fetched;
/// without one the whole team is fetched the same way. The cursor advances to
/// the first page's `X-Server-Time`, and only when every row was applied.
async fn pull_issues_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    let since = {
        let conn = open()?;
        queries::get_pull_cursor(&conn, local_team_id, ISSUES_CURSOR)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };

    let collected = cursor::collect_pages(|after_id| {
        let query = issues::IssuePageQuery {
            updated_since: since.clone(),
            include_detail: true,
            // Incremental pulls learn about archive/unarchive from
            // `archived_at` on the rows; a full pull stays active-only and
            // relies on the missing-row sweep below.
            include_archived: since.is_some(),
            limit: Some(cursor::PAGE_LIMIT),
            after_id,
        };
        async move { issues::list_page(client, team_remote_id, &query).await }
    })
    .await
    .map_err(|e| format!("issues: {e}"))?;
    eprintln!(
        "[sync] phase=pull_issues team_remote_id={team_remote_id} mode={} requests={} rows={}",
        if since.is_some() { "incremental" } else { "full" },
        collected.requests,
        collected.rows.len()
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
    if since.is_none() {
        // A full (unfiltered) listing is the complete set of active issues, so
        // anything missing from it was archived or removed server-side.
        let active_ids: Vec<i64> = dtos.iter().map(|d| d.id).collect();
        let _ = queries::archive_missing_issues(&conn, local_team_id, &active_ids);
    }
    if let Some(t) = &collected.server_time {
        queries::set_pull_cursor(&conn, local_team_id, ISSUES_CURSOR, t)
            .map_err(|e| format!("db (pull cursor): {e}"))?;
    }
    Ok(())
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
    eprintln!(
        "[sync] phase=pull_deleted team_remote_id={team_remote_id} mode={} rows={}",
        if since.is_some() { "incremental" } else { "full" },
        page.rows.len()
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
    eprintln!(
        "[sync] phase=pull_comments team_remote_id={team_remote_id} mode={} rows={}",
        if since.is_some() { "incremental" } else { "full" },
        page.rows.len()
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
/// safe). Handles Issue, Comment, Label and Project; other record types
/// (IssueDependency, RecurringIssue) are ignored. Returns how many entries
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
            _ => continue,
        };
        if let Err(e) = result {
            eprintln!(
                "[sync] phase=pull_deleted type={} remote_id={id} apply_failed reason=\"{e}\"",
                row.record_type
            );
            failed += 1;
        }
    }
    failed
}

/// Write feed comments locally (idempotent). Project comments and comments on
/// issues we do not have (or have tombstoned) are ignored. Replies are applied
/// after their parents, however the feed ordered them; a parent that cannot be
/// found locally leaves the reply top-level rather than blocking the cursor.
/// Returns how many rows hit a DB error.
fn apply_pulled_comments(conn: &Connection, dtos: Vec<CommentDto>) -> usize {
    let mut failed = 0;
    let mut remaining: Vec<CommentDto> = dtos.into_iter().filter(|d| d.issue_id.is_some()).collect();
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
        eprintln!(
            "[sync] phase=pull_comments comment_remote_id={} parent {:?} not found locally; storing top-level",
            dto.id, dto.parent_id
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
    let Some(issue_remote) = dto.issue_id else {
        return Ok(());
    };
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
    let existing = queries::find_comment_by_remote_id(conn, dto.id)
        .map_err(|e| format!("db (find comment): {e}"))?
        .map(|c| c.id);
    let comment = dto.into_model(conn, issue.id, existing, parent_local);
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
    let mut pending_links: Vec<Vec<PulledDependency>> = Vec::new();
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
            let dto = relax_issue_fks(conn, dto);
            let existing_uuid = existing.map(|i| i.id);
            // Capture the remote label ids before `into_model` consumes dto.
            let label_remote_ids: Vec<i64> = dto.labels.iter().map(|l| l.id).collect();
            let issue: Issue = dto
                .into_model(conn, local_team_id.to_string(), existing_uuid)
                .map_err(|e| format!("convert: {e}"))?;
            queries::upsert_issue(conn, &issue).map_err(|e| format!("db (upsert): {e}"))?;
            // Reflect the remote label set into the issue_labels join. Labels
            // are pulled before issues, so their local rows already exist; any
            // remote id we can't resolve locally is skipped defensively.
            let local_label_ids = resolve_local_label_ids(conn, &label_remote_ids)?;
            queries::set_issue_labels(conn, &issue.id, &local_label_ids)
                .map_err(|e| format!("db (set issue labels): {e}"))?;
            pending_links.push(links);
            Ok(true)
        })();
        if let Err(e) = result {
            eprintln!(
                "[sync] phase=pull_issues issue_remote_id={issue_remote_id} skipped reason=\"{e}\""
            );
            skipped += 1;
        }
    }
    for links in &pending_links {
        persist_pulled_dependencies(conn, links);
    }
    skipped
}

/// Store pulled dependency edges. An edge whose endpoint issue isn't present
/// locally (e.g. an issue in another team) is skipped and logged.
fn persist_pulled_dependencies(conn: &Connection, links: &[PulledDependency]) {
    for l in links {
        let find = |remote| queries::find_issue_by_remote_id(conn, remote);
        let (source, target) = match (find(l.source_remote_id), find(l.target_remote_id)) {
            (Ok(Some(s)), Ok(Some(t))) => (s, t),
            _ => {
                eprintln!(
                    "[sync] phase=pull_issues dependency remote_id={} endpoint not found locally; skipping",
                    l.dependency_id
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
            eprintln!(
                "[sync] phase=pull_issues dependency remote_id={} write_failed reason=\"{e}\"",
                l.dependency_id
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
                eprintln!(
                    "[sync] phase=pull_issues label remote_id={remote_id} not found locally; skipping"
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

    #[tokio::test]
    async fn read_only_token_never_drains() {
        let client = ApiClient::new("http://127.0.0.1:9", "t").unwrap();
        let r = SyncEngine::drain_if_writable(&client, Some(false)).await;
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
            issue_id: "i1".into(),
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
    fn classify_forbidden_keeps_pending_and_pauses() {
        match classify(&ApiError::Forbidden("read only".into())) {
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
        let outcome = classify(&ApiError::Forbidden("nope".into()));
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
            tomb("RecurringIssue", 2),
            tomb("Issue", 500),
        ];
        assert_eq!(apply_tombstones(&conn, "t1", &rows), 0);
        assert_eq!(apply_tombstones(&conn, "t1", &rows), 0);
        assert_purged(&conn);
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
        let c = vec![label_dto(1, "Bug"), label_dto(2, "Feature")];
        assert_eq!(match_label(&c, "bug", &none()), Some(1));
        assert_eq!(match_label(&c, " FEATURE ", &none()), Some(2));
        assert_eq!(match_label(&c, "Chore", &none()), None);
        let taken: HashSet<i64> = [1].into_iter().collect();
        assert_eq!(match_label(&c, "Bug", &taken), None);
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
    fn feed_comments_skip_project_and_unknown_issue_without_failing() {
        let conn = setup_labels_db();
        conn.execute("UPDATE issues SET remote_id = 50 WHERE id = 'i1'", []).unwrap();
        let dtos = vec![
            feed_comment(1, None, None, "project comment"),
            feed_comment(2, None, Some(999), "unknown issue"),
            // Parent never seen: stored top-level, not a failure.
            feed_comment(3, Some(77), Some(50), "orphan reply"),
        ];
        assert_eq!(apply_pulled_comments(&conn, dtos), 0);
        let all = queries::comments_for_issue(&conn, "i1").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].parent_id, None);
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
    fn begin_attempt_records_time_once_and_reports_earlier_attempt() {
        let conn = setup_labels_db();
        let entry = queue_create(&conn, "issue", "i1");
        let first = begin_attempt_on(&conn, entry, "issues").unwrap();
        assert!(first.previous.is_none());
        let stored = stored_attempt(&conn, entry);
        let second = begin_attempt_on(&conn, entry, "issues").unwrap();
        assert_eq!(second.previous.as_deref(), Some(stored.as_str()));
        assert_eq!(stored_attempt(&conn, entry), stored);
    }

    #[test]
    fn begin_attempt_collects_linked_remote_ids() {
        let conn = setup_labels_db();
        insert_test_label(&conn, "a", Some(41));
        insert_test_label(&conn, "b", None);
        let entry = queue_create(&conn, "label", "b");
        let a = begin_attempt_on(&conn, entry, "labels").unwrap();
        assert_eq!(a.taken, [41].into_iter().collect::<HashSet<_>>());
        assert!(begin_attempt_on(&conn, entry, "comments").is_err());
    }

    #[test]
    fn adopting_a_match_links_and_marks_synced_without_posting() {
        let mut conn = setup_labels_db();
        let entry = queue_create(&conn, "issue", "i1");
        let _ = begin_attempt_on(&conn, entry, "issues").unwrap();
        let retry = begin_attempt_on(&conn, entry, "issues").unwrap();
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
            let outcome = process(&client, &entry, 1).await;
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
}
