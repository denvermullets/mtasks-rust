//! Sync engine: one Data sync cycle. [`SyncEngine::full_cycle`] pushes (see
//! `push.rs`) and then pulls (see `pull.rs`, `tombstones.rs`) from one `/me`
//! call; [`SyncEngine::push_cycle`] only pushes. This file holds the cycle
//! ordering, the reports it returns, and the log and DB helpers both halves share.
//!
//! ## Send + async constraint
//!
//! `rusqlite::Connection` is `!Sync`, so it cannot be held across an `.await`
//! point in a `Send` future (which iced's `Task::perform` requires). We
//! therefore never pass a `&Connection` into an async function. All DB work
//! happens in short synchronous helpers that open and drop their own
//! connections; async dispatchers only hold owned data across awaits.

use rusqlite::Connection;

use crate::api::{ApiClient, ApiError, MeDto, users};
use crate::db::connection;
use crate::sync::pull::PullOutcome;

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
    /// Messages the user must see even though nothing failed, such as losing
    /// access to a team.
    pub notices: Vec<String>,
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
    pub(super) fn from_push_reason(reason: &str) -> Option<SyncErrorKind> {
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
    /// Local UUIDs of teams this pull removed (purged, or the orphan seed team).
    /// The app uses this to repoint `selected_team_id` and any open view so
    /// nothing renders against a row that no longer exists.
    pub deleted_teams: Vec<String>,
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

pub struct SyncEngine;

impl SyncEngine {
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
}

/// What one sync cycle learned from its single `/me` call. Every part of the
/// cycle (write-scope gate, push, create reconciliation, pull) reads this
/// instead of calling `/me` again.
pub struct CycleCtx {
    pub(super) me: Result<MeDto, ApiError>,
}

impl CycleCtx {
    pub async fn fetch(client: &ApiClient) -> Self {
        Self {
            me: users::me(client).await,
        }
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
            deleted_teams: Vec::new(),
            current_user: None,
            can_write: preflight_can_write,
            push_only: false,
        };
    }
    let outcome = pull().await;
    FullSyncReport {
        pull: outcome.report,
        push,
        deleted_teams: outcome.deleted_teams,
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

pub(super) fn read_only_report() -> SyncReport {
    SyncReport {
        push_paused: Some(PushPause::ReadOnly),
        ..SyncReport::default()
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
//   dropped - permanently rejected, so marked synced and not retried; or
//             deleted with its team when `/teams` stopped listing that team

pub(super) fn log_line(scope: &str, outcome: &str, reason: &str) -> String {
    format!(
        "[sync] {scope} outcome={outcome} reason=\"{}\"",
        reason.replace('"', "'")
    )
}

pub(super) fn sync_log(scope: &str, outcome: &str, reason: &str) {
    eprintln!("{}", log_line(scope, outcome, reason));
}

pub(super) fn phase_scope(phase: &str, team_remote_id: Option<i64>) -> String {
    match team_remote_id {
        Some(t) => format!("phase={phase} team={t}"),
        None => format!("phase={phase}"),
    }
}

// ── Synchronous DB helpers (each opens its own short-lived connection) ────

pub(super) fn open() -> Result<Connection, String> {
    connection::open().map_err(|e| format!("db open: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::{block_on, ctx_with_scopes};

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
        let ro = FullSyncReport { push: read_only_report(), can_write: Some(false), ..Default::default() };
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
        assert!(r.deleted_teams.is_empty() && r.current_user.is_none());
        assert!(r.cycle_succeeded());
        // A read-only token never pushes and shows the banner.
        let ro = push_only_report(Some(false), read_only_report());
        assert!(ro.read_only());
        assert!(ro.cycle_succeeded());
    }

    #[test]
    fn cycle_ctx_reads_write_scope_from_the_one_me_result() {
        assert_eq!(ctx_with_scopes(&["read"]).can_write(), Some(false));
        assert_eq!(ctx_with_scopes(&["read", "write"]).can_write(), Some(true));
        assert_eq!(
            CycleCtx { me: Err(ApiError::MissingToken) }.can_write(),
            None
        );
    }

    #[tokio::test]
    async fn read_only_token_never_drains() {
        let client = ApiClient::new("http://127.0.0.1:9", "t").unwrap();
        let r = SyncEngine::drain_if_writable(&client, &ctx_with_scopes(&["read"])).await;
        assert_eq!(r.push_paused, Some(PushPause::ReadOnly));
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
            deleted_teams: Vec::new(),
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

    fn auth_push_report() -> SyncReport {
        let mut r = SyncReport { failed: 1, ..Default::default() };
        r.failures.push(SyncFailure {
            entity_type: "issue".into(),
            entity_id: "x".into(),
            kind: SyncErrorKind::Auth,
            message: "unauthorized: nope".into(),
        });
        r
    }

    fn pull_outcome(processed: usize) -> PullOutcome {
        let report = SyncReport { processed, ..Default::default() };
        PullOutcome {
            report,
            deleted_teams: Vec::new(),
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
                SyncReport { processed: 2, ..Default::default() }
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
