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

use crate::api::{
    dependencies, issues, labels, projects, ApiClient, ApiError, Direction, IssueParams,
    LabelParams, ProjectParams,
};
use crate::db::models::{Issue, Project, SyncQueueEntry};
use crate::db::{connection, queries, sync_queue};

#[derive(Debug, Default, Clone)]
pub struct SyncReport {
    pub processed: usize,
    pub failed: usize,
    pub last_error: Option<String>,
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
        ("issue", "delete") => issue_delete(&entry.entity_id),
        ("project", "create") => project_create(client, &entry.entity_id).await,
        ("project", "update") => project_update(client, &entry.entity_id).await,
        ("project", "delete") => project_delete(&entry.entity_id),
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

fn issue_delete(_entity_id: &str) -> Outcome {
    // Local row is gone by the time delete drains. Today's enqueue sites
    // don't capture the pre-deletion remote_id in the payload, so we can't
    // hit Rails with the right id. Drop with a log; revisit when DESK-12
    // captures remote_id at enqueue time.
    Outcome::DropAndLog(
        "issue delete unsupported: pre-deletion remote_id not captured in payload".into(),
    )
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
    Prep::Ready(ProjectCreatePrep {
        team_remote_id,
        params: build_project_params(&project),
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
    Prep::Ready(ProjectUpdatePrep {
        team_remote_id,
        remote_id,
        params: build_project_params(&project),
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

fn project_delete(_entity_id: &str) -> Outcome {
    Outcome::DropAndLog(
        "project delete unsupported: pre-deletion remote_id not captured in payload".into(),
    )
}

fn build_project_params(project: &Project) -> ProjectParams {
    ProjectParams {
        name: Some(project.name.clone()),
        description: project.description.clone(),
        priority: Some(crate::api::dto::priority_to_string(project.priority).to_string()),
        status: Some(project.status.as_str().to_string()),
        // lead_id (member uuid → remote_id) deferred until projects are
        // creatable from the UI.
        lead_id: None,
        start_date: project.start_date.clone(),
        due_date: project.due_date.clone(),
        roadmap_commitment: project
            .roadmap_commitment
            .as_ref()
            .map(|c| c.as_str().to_string()),
        label_ids: None,
    }
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
