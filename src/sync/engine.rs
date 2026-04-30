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

use rusqlite::Connection;

use crate::api::dto::IssueDto;
use crate::api::{
    dependencies, issues, labels, lanes, members, projects, teams, users, ApiClient, ApiError,
    Direction, IssueParams, LabelParams, ProjectParams,
};
use crate::db::models::{Issue, Project, SyncQueueEntry};
use crate::db::{connection, queries, sync_queue};

#[derive(Debug, Default, Clone)]
pub struct SyncReport {
    pub processed: usize,
    pub failed: usize,
    pub last_error: Option<String>,
}

/// Combined report for one full bidirectional sync (pull then push).
#[derive(Debug, Default, Clone)]
pub struct FullSyncReport {
    pub pull: SyncReport,
    pub push: SyncReport,
    /// Local UUID of the orphan seed team that was cleaned up during this
    /// pull, if any. The app uses this to repoint `selected_team_id` so the
    /// board doesn't render against a row that no longer exists.
    pub deleted_orphan_team: Option<String>,
}

impl FullSyncReport {
    /// The most actionable error to surface to the user. Push errors take
    /// precedence (they're active operations the user just initiated); pull
    /// errors are reported only when push succeeded.
    pub fn last_error(&self) -> Option<&str> {
        self.push
            .last_error
            .as_deref()
            .or(self.pull.last_error.as_deref())
    }
}

#[derive(Debug)]
enum Outcome {
    /// Entry pushed successfully, or didn't need pushing.
    Success,
    /// Permanent failure for this entry — mark synced so we don't retry forever.
    DropAndLog(String),
    /// Transient failure — leave pending so the next drain retries.
    LeavePending(String),
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

            let outcome = process(client, &entry).await;
            record(&mut report, &entry, entry_id, outcome);
        }

        report
    }
}

async fn process(client: &ApiClient, entry: &SyncQueueEntry) -> Outcome {
    match (entry.entity_type.as_str(), entry.action.as_str()) {
        ("issue", "create") => issue_create(client, &entry.entity_id).await,
        ("issue", "update") => issue_update(client, &entry.entity_id).await,
        ("issue", "delete") => issue_delete(client, entry.payload.as_deref()).await,
        ("project", "create") => project_create(client, &entry.entity_id).await,
        ("project", "update") => project_update(client, &entry.entity_id).await,
        ("project", "delete") => project_delete(client, entry.payload.as_deref()).await,
        ("label", "create") => label_create(client, &entry.entity_id).await,
        ("dependency", "create") => dependency_create(client, &entry.entity_id).await,
        ("dependency", "delete") => dependency_delete(&entry.entity_id),
        (et, ac) => Outcome::DropAndLog(format!("unsupported ({et},{ac})")),
    }
}

fn record(report: &mut SyncReport, entry: &SyncQueueEntry, entry_id: i64, outcome: Outcome) {
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
        Outcome::DropAndLog(reason) => {
            eprintln!(
                "[sync] entry_id={entry_id} type={} action={} outcome=drop reason=\"{reason}\"",
                entry.entity_type, entry.action
            );
            let _ = mark_synced(entry_id);
            report.failed += 1;
            report.last_error = Some(reason);
        }
        Outcome::LeavePending(reason) => {
            eprintln!(
                "[sync] entry_id={entry_id} type={} action={} outcome=retry reason=\"{reason}\"",
                entry.entity_type, entry.action
            );
            report.failed += 1;
            report.last_error = Some(reason);
        }
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

async fn issue_create(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match prep_issue_create(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };

    match issues::create(client, prep.team_remote_id, &prep.params).await {
        Ok(dto) => write_issue_remote_id(&prep.local_id, dto.id),
        Err(e) => classify(&e),
    }
}

async fn issue_update(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match prep_issue_update(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };

    match issues::update(client, prep.team_remote_id, prep.remote_id, &prep.params).await {
        Ok(_) => Outcome::Success,
        Err(e) => classify(&e),
    }
}

async fn issue_delete(client: &ApiClient, payload: Option<&str>) -> Outcome {
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
        None => return Outcome::DropAndLog("issue delete: missing team_remote_id in payload".into()),
    };
    match issues::delete(client, team_remote_id, remote_id).await {
        Ok(_) => Outcome::Success,
        Err(ApiError::NotFound(_)) => Outcome::Success,
        Err(e) => classify(&e),
    }
}

fn write_issue_remote_id(local_id: &str, remote_id: i64) -> Outcome {
    let conn = match open() {
        Ok(c) => c,
        Err(e) => return Outcome::LeavePending(e),
    };
    if let Err(e) = queries::set_issue_remote_id(&conn, local_id, remote_id) {
        return Outcome::LeavePending(format!("write remote_id: {e}"));
    }
    Outcome::Success
}

fn build_issue_params(conn: &Connection, issue: &Issue) -> Result<IssueParams, Outcome> {
    let lane_id = require_remote_id(conn, "lanes", &issue.lane_id)?;
    let project_id = optional_remote_id(conn, "projects", issue.project_id.as_deref())?;
    let assignee_id = optional_remote_id(conn, "members", issue.assignee_id.as_deref())?;
    let parent_issue_id = optional_remote_id(conn, "issues", issue.parent_issue_id.as_deref())?;

    Ok(IssueParams {
        title: Some(issue.title.clone()),
        description: issue.description.clone(),
        lane_id: Some(lane_id),
        priority: Some(crate::api::dto::priority_to_string(issue.priority).to_string()),
        estimate: issue.estimate.map(|e| e as i64),
        due_date: issue.due_date.clone(),
        assignee_id,
        project_id,
        parent_issue_id,
        // The issue_labels join table isn't wired into the local UI yet; once
        // labels are settable on local issues, populate this with resolved
        // remote ids.
        label_ids: None,
    })
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
        None => return Prep::Done(Outcome::LeavePending("project create not yet drained".into())),
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

async fn project_create(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match prep_project_create(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    match projects::create(client, prep.team_remote_id, &prep.params).await {
        Ok(dto) => {
            let conn = match open() {
                Ok(c) => c,
                Err(e) => return Outcome::LeavePending(e),
            };
            if let Err(e) = queries::set_project_remote_id(&conn, &prep.local_id, dto.id) {
                return Outcome::LeavePending(format!("write remote_id: {e}"));
            }
            Outcome::Success
        }
        Err(e) => classify(&e),
    }
}

async fn project_update(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match prep_project_update(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    match projects::update(client, prep.team_remote_id, prep.remote_id, &prep.params).await {
        Ok(_) => Outcome::Success,
        Err(e) => classify(&e),
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
            return Outcome::DropAndLog(
                "project delete: missing team_remote_id in payload".into(),
            )
        }
    };
    match projects::delete(client, team_remote_id, remote_id).await {
        Ok(_) => Outcome::Success,
        Err(ApiError::NotFound(_)) => Outcome::Success,
        Err(e) => classify(&e),
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
        priority: Some(crate::api::dto::priority_to_string(project.priority).to_string()),
        status: Some(project.status.as_str().to_string()),
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

async fn label_create(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match prep_label_create(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    match labels::create(client, prep.team_remote_id, &prep.params).await {
        Ok(dto) => {
            let conn = match open() {
                Ok(c) => c,
                Err(e) => return Outcome::LeavePending(e),
            };
            if let Err(e) = queries::set_label_remote_id(&conn, &prep.local_id, dto.id) {
                return Outcome::LeavePending(format!("write remote_id: {e}"));
            }
            Outcome::Success
        }
        Err(e) => classify(&e),
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

async fn dependency_create(client: &ApiClient, entity_id: &str) -> Outcome {
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
            let conn = match open() {
                Ok(c) => c,
                Err(e) => return Outcome::LeavePending(e),
            };
            if let Err(e) = queries::set_issue_dependency_remote_id(&conn, &prep.local_id, resp.id)
            {
                return Outcome::LeavePending(format!("write remote_id: {e}"));
            }
            Outcome::Success
        }
        Err(e) => classify(&e),
    }
}

fn dependency_delete(_entity_id: &str) -> Outcome {
    Outcome::DropAndLog(
        "dependency delete unsupported: pre-deletion remote_id not captured in payload".into(),
    )
}

// ── Error classification ──────────────────────────────────────────────────

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
        ApiError::Unauthorized(m) => Outcome::DropAndLog(format!("unauthorized: {m}")),
        ApiError::Forbidden(m) => Outcome::DropAndLog(format!("forbidden: {m}")),
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
// dependencies are not pulled (Rails has no index endpoint for that resource).
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
}

impl SyncEngine {
    /// Pull all resources from Rails into the local DB. The set of teams to
    /// fully sync is determined by the token's scope:
    ///
    /// - **User-scoped token** (`token.team_id == null` from `/me`) — pull
    ///   every team the user is on, then for each team pull lanes/labels/
    ///   members/projects/issues.
    /// - **Team-scoped token** (`token.team_id == Some(t)`) — pull just that
    ///   team's resources.
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
                report.last_error = Some(msg);
                report.failed += 1;
                return PullOutcome { report, deleted_orphan_team };
            }
        };

        // Phase 2: refresh teams (and discover any new ones the token can see).
        match pull_teams(client).await {
            Ok(orphan) => {
                deleted_orphan_team = orphan;
                report.processed += 1;
            }
            Err(msg) => {
                report.last_error = Some(msg);
                report.failed += 1;
                return PullOutcome { report, deleted_orphan_team };
            }
        }

        // Phase 3: figure out which teams to deeply sync.
        let targets = match collect_pull_targets(me.token.team_id) {
            Ok(t) => t,
            Err(msg) => {
                report.last_error = Some(msg);
                report.failed += 1;
                return PullOutcome { report, deleted_orphan_team };
            }
        };
        if targets.is_empty() {
            // Token has access but no teams visible (or team-scoped to a team
            // we couldn't find locally after pull_teams — shouldn't happen).
            return PullOutcome { report, deleted_orphan_team };
        }

        // Phase 4: per-team resources. Each helper logs and updates the report
        // independently so one team's failure doesn't block the others.
        for target in &targets {
            for step in [
                pull_lanes_step(client, target.remote_id, &target.local_id).await,
                pull_labels_step(client, target.remote_id, &target.local_id).await,
                pull_members_step(client, target.remote_id, &target.local_id).await,
                pull_projects_step(client, target.remote_id, &target.local_id).await,
                pull_issues_step(client, target.remote_id, &target.local_id).await,
            ] {
                match step {
                    Ok(()) => report.processed += 1,
                    Err(msg) => {
                        report.failed += 1;
                        report.last_error = Some(msg);
                    }
                }
            }
        }

        PullOutcome { report, deleted_orphan_team }
    }
}

struct PullTarget {
    local_id: String,
    remote_id: i64,
}

fn collect_pull_targets(token_team_id: Option<i64>) -> Result<Vec<PullTarget>, String> {
    let conn = open()?;
    match token_team_id {
        Some(remote_id) => {
            let local = queries::find_team_by_remote_id(&conn, remote_id)
                .map_err(|e| format!("db (find team): {e}"))?;
            match local {
                Some(t) => Ok(vec![PullTarget {
                    local_id: t.id,
                    remote_id,
                }]),
                None => Err(format!(
                    "token is scoped to team remote_id={remote_id}, but it wasn't returned by /api/v1/teams"
                )),
            }
        }
        None => {
            // User-scoped token: fully sync every Rails-backed team we know
            // about. all_teams returns the local rows; we need their remote_id.
            let teams = queries::all_teams(&conn)
                .map_err(|e| format!("db (all_teams): {e}"))?;
            Ok(teams
                .into_iter()
                .filter_map(|t| {
                    t.remote_id.map(|rid| PullTarget {
                        local_id: t.id,
                        remote_id: rid,
                    })
                })
                .collect())
        }
    }
}

/// Returns the local UUID of the orphan seed team if one was cleaned up.
async fn pull_teams(client: &ApiClient) -> Result<Option<String>, String> {
    let dtos = teams::list(client)
        .await
        .map_err(|e| format!("teams: {e}"))?;

    let active_ids: Vec<i64> = dtos.iter().map(|d| d.id).collect();
    let inserted_any_remote_team = !dtos.is_empty();

    let conn = open()?;
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
    let dtos = lanes::list(client, team_remote_id)
        .await
        .map_err(|e| format!("lanes: {e}"))?;

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
    Ok(())
}

async fn pull_labels_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    let dtos = labels::list(client, team_remote_id)
        .await
        .map_err(|e| format!("labels: {e}"))?;

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
    Ok(())
}

async fn pull_members_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    let dtos = members::list(client, team_remote_id)
        .await
        .map_err(|e| format!("members: {e}"))?;

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
    Ok(())
}

async fn pull_projects_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    let dtos = projects::list(client, team_remote_id)
        .await
        .map_err(|e| format!("projects: {e}"))?;

    let conn = open()?;
    for dto in dtos {
        let existing = queries::find_project_by_remote_id(&conn, dto.id)
            .map_err(|e| format!("db (find project): {e}"))?;
        let has_pending = match &existing {
            Some(p) => sync_queue::has_pending_entry(&conn, "project", &p.id)
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
            .into_model(&conn, local_team_id.to_string(), existing_uuid)
            .map_err(|e| format!("convert project: {e}"))?;
        queries::upsert_project(&conn, &project)
            .map_err(|e| format!("db (upsert project): {e}"))?;
    }
    // project_labels join not populated in this ticket; documented as TODO.
    Ok(())
}

async fn pull_issues_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    // Rails' issue *index* serializer omits detail-only fields like
    // `description`, `parent_issue`, `blocking_issues`, `blocked_issues`, and
    // the lifecycle timestamps. We use the index to discover which issues
    // changed (cheap) and then only fetch full detail for the ones we'll
    // actually write — steady-state pulls don't refetch unchanged rows.
    let listings: Vec<IssueDto> = issues::list(client, team_remote_id, &Default::default())
        .await
        .map_err(|e| format!("issues: {e}"))?;

    let active_ids: Vec<i64> = listings.iter().map(|d| d.id).collect();

    // Phase 1 (sync): decide which remote_ids need a full detail fetch.
    let to_fetch: Vec<i64> = {
        let conn = open()?;
        let mut ids = Vec::new();
        for dto in &listings {
            let existing = queries::find_issue_by_remote_id(&conn, dto.id)
                .map_err(|e| format!("db (find issue): {e}"))?;
            let has_pending = match &existing {
                Some(i) => sync_queue::has_pending_entry(&conn, "issue", &i.id)
                    .map_err(|e| format!("db: {e}"))?,
                None => false,
            };
            let should_fetch = match &existing {
                Some(local) => {
                    decide(&local.updated_at, &dto.updated_at, has_pending)
                        != MergeDecision::KeepLocal
                }
                None => true, // new issue → always fetch
            };
            if should_fetch {
                ids.push(dto.id);
            }
        }
        ids
    };
    let total_to_fetch = to_fetch.len();

    // Phase 2 (async): fetch detail for each. Errors per-issue are logged and
    // counted; one failure doesn't block the rest.
    let mut details: Vec<IssueDto> = Vec::with_capacity(total_to_fetch);
    let mut skipped = 0usize;
    for issue_remote_id in to_fetch {
        match issues::get(client, team_remote_id, issue_remote_id).await {
            Ok(d) => details.push(d),
            Err(e) => {
                eprintln!(
                    "[sync] phase=pull_issues issue_remote_id={issue_remote_id} fetch_failed reason=\"{e}\""
                );
                skipped += 1;
            }
        }
    }

    // Phase 3 (sync): write each detail to the local DB.
    {
        let conn = open()?;
        for dto in details {
            let issue_remote_id = dto.id;
            let dto = relax_issue_fks(&conn, dto);

            let result = (|| -> Result<(), String> {
                let existing = queries::find_issue_by_remote_id(&conn, dto.id)
                    .map_err(|e| format!("db (find issue): {e}"))?;
                let existing_uuid = existing.as_ref().map(|i| i.id.clone());
                let issue: Issue = dto
                    .into_model(&conn, local_team_id.to_string(), existing_uuid)
                    .map_err(|e| format!("convert: {e}"))?;
                queries::upsert_issue(&conn, &issue).map_err(|e| format!("db (upsert): {e}"))?;
                Ok(())
            })();

            if let Err(e) = result {
                eprintln!(
                    "[sync] phase=pull_issues issue_remote_id={issue_remote_id} skipped reason=\"{e}\""
                );
                skipped += 1;
            }
            // issue_labels join intentionally not populated; the local board
            // doesn't render labels yet. Wire this when the UI gains label
            // rendering.
        }
    }

    let conn = open()?;
    let _ = queries::archive_missing_issues(&conn, local_team_id, &active_ids);

    if skipped > 0 {
        Err(format!(
            "issues: {skipped}/{total_to_fetch} skipped (see [sync] log)"
        ))
    } else {
        Ok(())
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

    fn is_drop(o: &Outcome) -> bool {
        matches!(o, Outcome::DropAndLog(_))
    }
    fn is_retry(o: &Outcome) -> bool {
        matches!(o, Outcome::LeavePending(_))
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
        assert!(is_drop(&classify(&ApiError::Forbidden("nope".into()))));
        assert!(is_drop(&classify(&ApiError::Unauthorized("nope".into()))));
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
            decide(
                "2026-04-27T12:00:00Z",
                "2026-04-27T11:00:00Z",
                false
            ),
            MergeDecision::Overwrite
        );
    }

    #[test]
    fn decide_pending_and_local_newer_keeps_local() {
        assert_eq!(
            decide(
                "2026-04-27T12:00:00Z",
                "2026-04-27T11:00:00Z",
                true
            ),
            MergeDecision::KeepLocal
        );
    }

    #[test]
    fn decide_pending_but_remote_newer_overwrites() {
        // Concurrent edit: someone updated remote AFTER our local edit. LWW
        // means remote wins; the queued push will then overwrite again.
        assert_eq!(
            decide(
                "2026-04-27T11:00:00Z",
                "2026-04-27T12:00:00Z",
                true
            ),
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
            let outcome = process(&client, &entry).await;
            assert!(matches!(outcome, Outcome::DropAndLog(_)));
        });
    }
}
