//! Pushing project creates and updates.

use rusqlite::Connection;

use crate::api::{ApiClient, ProjectParams, projects};
use crate::db::{attachments, queries};
use crate::db::models::{Priority, Project};
use crate::sync::engine::open;
use super::attachment::{InlineImages, prepare_body, settle_inline};
use super::outcome::{Outcome, Prep, classify, classify_in_team};
use super::reconcile::{commit_create, find_project_on_server, record_attempt};

// ── Projects ──────────────────────────────────────────────────────────────

struct ProjectCreatePrep {
    team_remote_id: i64,
    params: ProjectParams,
    local_id: String,
    inline: InlineImages,
}

struct ProjectUpdatePrep {
    team_remote_id: i64,
    remote_id: i64,
    params: ProjectParams,
    local_id: String,
    inline: InlineImages,
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
    let mut params = match build_project_params(&conn, &project) {
        Ok(p) => p,
        Err(o) => return Prep::Done(o),
    };
    let inline = match prepare_body(&conn, attachments::OwnerKind::Project, &project.id, params.description.take()) {
        Ok((d, i)) => {
            params.description = d;
            i
        }
        Err(o) => return Prep::Done(o),
    };
    Prep::Ready(ProjectCreatePrep {
        team_remote_id,
        params,
        local_id: project.id,
        inline,
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
    let mut params = match build_project_params(&conn, &project) {
        Ok(p) => p,
        Err(o) => return Prep::Done(o),
    };
    let inline = match prepare_body(&conn, attachments::OwnerKind::Project, &project.id, params.description.take()) {
        Ok((d, i)) => {
            params.description = d;
            i
        }
        Err(o) => return Prep::Done(o),
    };
    Prep::Ready(ProjectUpdatePrep {
        team_remote_id,
        remote_id,
        params,
        local_id: project.id,
        inline,
    })
}

pub(super) async fn project_create(client: &ApiClient, entity_id: &str, entry_id: i64) -> Outcome {
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
        Ok(dto) => {
            let out = commit_create(entry_id, "projects", &prep.local_id, dto.id);
            settle_inline(attachments::OwnerKind::Project, &prep.local_id, &prep.inline);
            out
        }
        Err(e) => classify_in_team(&e, prep.team_remote_id),
    }
}

pub(super) async fn project_update(client: &ApiClient, entity_id: &str) -> Outcome {
    let prep = match prep_project_update(entity_id) {
        Prep::Ready(p) => p,
        Prep::Done(o) => return o,
    };
    match projects::update(client, prep.team_remote_id, prep.remote_id, &prep.params).await {
        Ok(_) => {
            settle_inline(attachments::OwnerKind::Project, &prep.local_id, &prep.inline);
            Outcome::Success
        }
        Err(e) => classify_in_team(&e, prep.team_remote_id),
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
