use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::error::ApiError;
use crate::db::models::{
    Comment, CommentOwner, Issue, IssueDependency, Label, Lane, Member, Priority, Project, ProjectStatus,
    RoadmapCommitment, Team,
};

// ── Reference shapes used inside Issue/Project/Comment serializers ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordRef {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LabelRef {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParentIssueRef {
    pub id: i64,
    pub identifier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueRef {
    pub id: i64,
    pub identifier: String,
    pub title: String,
    /// IssueDependency id; present on issue-detail dependency entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_id: Option<i64>,
    /// Link direction relative to the detailed issue; present on
    /// `related_issues` / `duplicates` entries only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
}

/// One dependency edge decoded from an issue-detail payload, oriented so
/// `source` is Rails' `blocking_issue` side. `direction` is relative to source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledDependency {
    pub dependency_id: i64,
    pub kind: &'static str,
    pub direction: &'static str,
    pub source_remote_id: i64,
    pub target_remote_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserRef {
    pub id: i64,
    pub name: String,
    pub email: String,
}

// ── IssueDto (mirrors Api::V1::IssueSerializer) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IssueDto {
    pub id: i64,
    pub identifier: String,
    pub title: String,
    pub priority: String,
    pub estimate: Option<i64>,
    pub due_date: Option<String>,
    pub lane: Option<RecordRef>,
    pub assignee: Option<RecordRef>,
    pub creator: Option<RecordRef>,
    pub project: Option<RecordRef>,
    #[serde(default)]
    pub labels: Vec<LabelRef>,
    pub created_at: String,
    pub updated_at: String,
    /// Set when the issue is archived upstream; absent/null when active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Remote id of the recurring issue that spawned this issue; null for
    /// hand-made issues. Server-set, never pushed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurring_issue_id: Option<i64>,

    // detail-only (None when the Rails serializer ran with detailed: false)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_issue: Option<ParentIssueRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_issues: Option<Vec<IssueRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_issues: Option<Vec<IssueRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_issues: Option<Vec<IssueRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicates: Option<Vec<IssueRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canceled_at: Option<String>,
    /// Files attached to the issue (detail payload only). `None` when the
    /// payload has no `files` key: that means no files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<AttachmentDto>>,
}

/// One attachment as Rails serializes it under `files` on issues, projects
/// and comments. `id` is the attachment row (what delete takes);
/// `download_url` is `/api/v1/attachments/:id/download`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentDto {
    pub id: i64,
    #[serde(default)]
    pub blob_id: Option<i64>,
    pub filename: String,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub byte_size: i64,
    #[serde(default)]
    pub checksum: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub download_url: Option<String>,
}

impl IssueDto {
    /// Build a DTO from a local model. Hydrates nested {id, name} objects by
    /// querying SQLite for the related records' remote_ids and names.
    pub fn from_model(conn: &Connection, issue: &Issue) -> Result<Self, ApiError> {
        let team = lookup_team_by_local_id(conn, &issue.team_id)?;
        let team_number = issue
            .team_number
            .ok_or_else(|| ApiError::Decode("issue missing team_number".into()))?;
        let identifier = format!("{}-{}", team.identifier, team_number);

        let lane = lookup_lane_by_local_id(conn, &issue.lane_id)?;
        let lane_ref = Some(RecordRef {
            id: require_remote(lane.remote_id, "lane")?,
            name: lane.name,
        });

        let assignee = match &issue.assignee_id {
            Some(local) => Some(member_record_ref(conn, local)?),
            None => None,
        };
        let creator = match &issue.creator_id {
            Some(local) => Some(member_record_ref(conn, local)?),
            None => None,
        };
        let project = match &issue.project_id {
            Some(local) => Some(project_record_ref(conn, local)?),
            None => None,
        };
        let parent_issue = match &issue.parent_issue_id {
            Some(local) => Some(parent_issue_ref(conn, local)?),
            None => None,
        };
        let labels = labels_for_issue(conn, &issue.id)?;
        let blocking = blocking_issue_refs(conn, &issue.id)?;
        let blocked = blocked_issue_refs(conn, &issue.id)?;

        let id = require_remote(issue.remote_id, "issue")?;
        Ok(IssueDto {
            id,
            identifier,
            title: issue.title.clone(),
            priority: Priority::from_i32(issue.priority)
                .as_rails_str()
                .to_string(),
            estimate: issue.estimate.map(|f| f as i64),
            due_date: issue.due_date.clone(),
            lane: lane_ref,
            assignee,
            creator,
            project,
            labels,
            created_at: issue.created_at.clone(),
            updated_at: issue.updated_at.clone(),
            archived_at: None,
            recurring_issue_id: None,
            description: issue.description.clone(),
            parent_issue,
            blocking_issues: Some(blocking),
            blocked_issues: Some(blocked),
            related_issues: None,
            duplicates: None,
            started_at: issue.started_at.clone(),
            completed_at: issue.completed_at.clone(),
            canceled_at: issue.canceled_at.clone(),
            files: None,
        })
    }

    /// Every dependency edge in this (detail) payload, oriented source ->
    /// target. Entries without a `dependency_id` are skipped.
    pub fn dependency_links(&self) -> Vec<PulledDependency> {
        let me = self.id;
        let mut out = Vec::new();
        let mut push = |r: &IssueRef, kind, direction, me_is_source: bool| {
            if let Some(dependency_id) = r.dependency_id {
                let (s, t) = if me_is_source { (me, r.id) } else { (r.id, me) };
                out.push(PulledDependency {
                    dependency_id,
                    kind,
                    direction,
                    source_remote_id: s,
                    target_remote_id: t,
                });
            }
        };
        // blocking_issues: these issues block me (they are the source).
        for r in self.blocking_issues.iter().flatten() {
            push(r, "blocks", "blocking", false);
        }
        for r in self.blocked_issues.iter().flatten() {
            push(r, "blocks", "blocking", true);
        }
        // `relates` is symmetric; orient with this issue as source.
        for r in self.related_issues.iter().flatten() {
            push(r, "relates", "relates", true);
        }
        for r in self.duplicates.iter().flatten() {
            // "duplicated_by": the other issue is the duplicate (source).
            let me_is_source = r.direction.as_deref() != Some("duplicated_by");
            push(r, "duplicates", "duplicates", me_is_source);
        }
        out
    }

    /// Convert a DTO into a local Issue model. Resolves nested Rails ids to
    /// local UUIDs by remote_id lookups. Caller supplies the team's local UUID
    /// (the issue endpoint is scoped by team) and an existing local UUID when
    /// updating; otherwise a fresh UUID is generated.
    ///
    /// Labels and blocking/blocked dependencies are not persisted to the Issue
    /// struct itself — they live in join tables. Callers should read
    /// `self.labels` and `self.blocking_issues` separately and write them to
    /// the issue_labels / issue_dependencies tables.
    pub fn into_model(
        self,
        conn: &Connection,
        team_id: String,
        existing_uuid: Option<String>,
    ) -> Result<Issue, ApiError> {
        let lane_remote = self
            .lane
            .as_ref()
            .ok_or_else(|| ApiError::Decode("issue payload missing lane".into()))?
            .id;
        let lane_id = local_uuid_for(conn, "lanes", lane_remote)?;

        let assignee_id = match &self.assignee {
            Some(r) => Some(local_uuid_for(conn, "members", r.id)?),
            None => None,
        };
        let creator_id = match &self.creator {
            Some(r) => Some(local_uuid_for(conn, "members", r.id)?),
            None => None,
        };
        let project_id = match &self.project {
            Some(r) => Some(local_uuid_for(conn, "projects", r.id)?),
            None => None,
        };
        let parent_issue_id = match &self.parent_issue {
            Some(r) => Some(local_uuid_for(conn, "issues", r.id)?),
            None => None,
        };

        // identifier ("PROJ-42") splits as "{team_identifier}-{team_number}".
        // The team_identifier may itself contain hyphens? Unlikely in practice
        // (Rails enforces 3-4 char alphanumeric), but split from the right to
        // be safe.
        let team_number = self
            .identifier
            .rsplit_once('-')
            .and_then(|(_, n)| n.parse::<i32>().ok());

        Ok(Issue {
            id: existing_uuid.unwrap_or_else(|| Uuid::new_v4().to_string()),
            title: self.title,
            description: self.description,
            priority: Priority::from_rails_str(&self.priority)?.to_i32(),
            team_number,
            due_date: self.due_date,
            estimate: self.estimate.map(|i| i as f64),
            started_at: self.started_at,
            completed_at: self.completed_at,
            canceled_at: self.canceled_at,
            lane_id,
            team_id,
            project_id,
            parent_issue_id,
            assignee_id,
            creator_id,
            archived_at: self.archived_at,
            remote_id: Some(self.id),
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

// ── ProjectDto (mirrors Api::V1::ProjectSerializer) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectDto {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub status: Option<ProjectStatus>,
    pub priority: String,
    pub lead: Option<RecordRef>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub velocity_score: i64,
    pub completed_issues_count: i64,
    pub total_issues_count: i64,
    pub progress_percentage: i64,
    pub behind_schedule: bool,
    pub roadmap_commitment: Option<RoadmapCommitment>,
    pub created_at: String,
    pub updated_at: String,

    // detail-only
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<LabelRef>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issues_count: Option<i64>,
}

impl ProjectDto {
    pub fn from_model(conn: &Connection, project: &Project) -> Result<Self, ApiError> {
        let lead = match &project.lead_id {
            Some(local) => Some(member_record_ref(conn, local)?),
            None => None,
        };
        let labels = labels_for_project(conn, &project.id)?;

        let progress_percentage = if project.total_issues_count == 0 {
            0
        } else {
            (project.completed_issues_count as f64 / project.total_issues_count as f64 * 100.0)
                .round() as i64
        };

        // behind_schedule on Rails depends on querying issues for the
        // effective start date (project.rb:44). Replicating that here would
        // cross out of the DTO module's responsibility; defer until a caller
        // actually needs it locally and pass a conservative default.
        let behind_schedule = false;

        Ok(ProjectDto {
            id: require_remote(project.remote_id, "project")?,
            name: project.name.clone(),
            description: project.description.clone(),
            status: project.status,
            priority: Priority::from_i32(project.priority)
                .as_rails_str()
                .to_string(),
            lead,
            start_date: project.start_date.clone(),
            due_date: project.due_date.clone(),
            velocity_score: project.velocity_score,
            completed_issues_count: project.completed_issues_count,
            total_issues_count: project.total_issues_count,
            progress_percentage,
            behind_schedule,
            roadmap_commitment: project.roadmap_commitment,
            created_at: project.created_at.clone(),
            updated_at: project.updated_at.clone(),
            labels: Some(labels),
            issues_count: None,
        })
    }

    pub fn into_model(
        self,
        conn: &Connection,
        team_id: String,
        existing_uuid: Option<String>,
    ) -> Result<Project, ApiError> {
        let lead_id = match &self.lead {
            Some(r) => Some(local_uuid_for(conn, "members", r.id)?),
            None => None,
        };

        Ok(Project {
            id: existing_uuid.unwrap_or_else(|| Uuid::new_v4().to_string()),
            remote_id: Some(self.id),
            team_id,
            name: self.name,
            description: self.description,
            status: self.status,
            priority: Priority::from_rails_str(&self.priority)?.to_i32(),
            lead_id,
            start_date: self.start_date,
            due_date: self.due_date,
            roadmap_commitment: self.roadmap_commitment,
            velocity_score: self.velocity_score,
            completed_issues_count: self.completed_issues_count,
            total_issues_count: self.total_issues_count,
            progress_percentage: self.progress_percentage,
            behind_schedule: self.behind_schedule,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

// ── CommentDto (mirrors Api::V1::CommentSerializer) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommentDto {
    pub id: i64,
    pub body: String,
    pub parent_id: Option<i64>,
    pub user: Option<UserRef>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replies: Option<Vec<CommentDto>>,
    /// Remote issue id. Only the team comments feed sends it (null for
    /// project comments); the per-issue endpoint omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<i64>,
    /// Remote project id. Only the team comments feed sends it (set for
    /// project comments, null for issue comments).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i64>,
    /// Files attached to the comment. `None` when the payload has no `files`
    /// key (the local attachments are then left alone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<AttachmentDto>>,
}

impl CommentDto {
    /// Merge a fetched comment into a local `Comment`. `owner` is the local
    /// UUID of the owning issue or project (already resolved by the caller). The
    /// author is resolved to a local member by `user.id`; if that member isn't
    /// known locally the author is left `None` (falls back to "Unknown" in the
    /// UI). `parent_local_uuid` is the local UUID of the parent comment, if any.
    pub fn into_model(
        self,
        conn: &Connection,
        owner: CommentOwner,
        existing_uuid: Option<String>,
        parent_local_uuid: Option<String>,
    ) -> Comment {
        let (issue_id, project_id) = match owner {
            CommentOwner::Issue(i) => (Some(i), None),
            CommentOwner::Project(p) => (None, Some(p)),
        };
        let author_id = self
            .user
            .as_ref()
            .and_then(|u| local_uuid_for(conn, "members", u.id).ok());
        Comment {
            id: existing_uuid.unwrap_or_else(|| Uuid::new_v4().to_string()),
            remote_id: Some(self.id),
            issue_id,
            project_id,
            author_id,
            body: self.body,
            parent_id: parent_local_uuid,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Flatten a fetched comment tree into local rows, parents before children (the
/// `parent_id` column is a FK). Nested `replies` give the parent link; a reply
/// delivered at top level falls back to resolving its `parent_id` remote id.
/// Returns the number of comments upserted.
pub fn merge_comment_tree(
    conn: &Connection,
    owner: &CommentOwner,
    dtos: Vec<CommentDto>,
    parent_local_uuid: Option<&str>,
) -> usize {
    let mut count = 0;
    for mut dto in dtos {
        let replies = dto.replies.take().unwrap_or_default();
        let existing = crate::db::queries::find_comment_by_remote_id(conn, dto.id)
            .ok()
            .flatten()
            .map(|c| c.id);
        let parent = parent_local_uuid.map(String::from).or_else(|| {
            dto.parent_id
                .and_then(|pid| local_uuid_for(conn, "comments", pid).ok())
        });
        let files = dto.files.take();
        let comment = dto.into_model(
            conn,
            owner.clone(),
            existing,
            parent,
        );
        if crate::db::queries::upsert_comment(conn, &comment).is_ok() {
            if let Some(files) = files {
                let _ = crate::db::attachments::replace_synced(
                    conn,
                    crate::db::attachments::OwnerKind::Comment,
                    &comment.id,
                    &files,
                );
            }
            count += 1;
            count += merge_comment_tree(conn, owner, replies, Some(&comment.id));
        }
    }
    count
}

// ── Flat DTOs (no Rails serializer; mirror default AR as_json output) ──
//
// Some Rails index serializers (teams, lanes, labels, members) omit
// `created_at`/`updated_at` to keep list payloads small. The fields are
// modeled as `Option<String>` with `#[serde(default)]` so deserialization
// doesn't fail when the server elides them; conversions back to local models
// fall back to `now()` if absent.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeamDto {
    pub id: i64,
    pub name: String,
    pub identifier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl From<TeamDto> for Team {
    fn from(dto: TeamDto) -> Self {
        let created_at = dto.created_at.unwrap_or_else(crate::db::now_iso);
        let updated_at = dto.updated_at.unwrap_or_else(|| created_at.clone());
        Team {
            id: Uuid::new_v4().to_string(),
            name: dto.name,
            identifier: dto.identifier,
            remote_id: Some(dto.id),
            archived_at: None,
            created_at,
            updated_at,
        }
    }
}

impl TryFrom<&Team> for TeamDto {
    type Error = ApiError;
    fn try_from(t: &Team) -> Result<Self, Self::Error> {
        Ok(TeamDto {
            id: require_remote(t.remote_id, "team")?,
            name: t.name.clone(),
            identifier: t.identifier.clone(),
            created_at: Some(t.created_at.clone()),
            updated_at: Some(t.updated_at.clone()),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaneDto {
    pub id: i64,
    /// Rails index responses omit `team_id` (the URL path scopes the request).
    /// Default to 0 on deserialize; pull-side callers should overwrite this
    /// with the team_id they used in the URL before calling [`Self::into_model`].
    #[serde(default)]
    pub team_id: i64,
    pub name: String,
    pub color: Option<String>,
    pub position: i32,
    /// Workflow category: backlog, unstarted, started, completed or canceled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl LaneDto {
    /// Resolve the Rails team_id to a local UUID and produce a Lane model.
    pub fn into_model(
        self,
        conn: &Connection,
        existing_uuid: Option<String>,
    ) -> Result<Lane, ApiError> {
        let team_id = local_uuid_for(conn, "teams", self.team_id)?;
        let created_at = self.created_at.unwrap_or_else(crate::db::now_iso);
        let updated_at = self.updated_at.unwrap_or_else(|| created_at.clone());
        Ok(Lane {
            id: existing_uuid.unwrap_or_else(|| Uuid::new_v4().to_string()),
            name: self.name,
            color: self.color,
            position: self.position,
            team_id,
            remote_id: Some(self.id),
            category: self.category,
            created_at,
            updated_at,
        })
    }
}

impl LaneDto {
    pub fn from_model(conn: &Connection, lane: &Lane) -> Result<Self, ApiError> {
        let team = lookup_team_by_local_id(conn, &lane.team_id)?;
        Ok(LaneDto {
            id: require_remote(lane.remote_id, "lane")?,
            team_id: require_remote(team.remote_id, "team")?,
            name: lane.name.clone(),
            color: lane.color.clone(),
            position: lane.position,
            category: lane.category.clone(),
            created_at: Some(lane.created_at.clone()),
            updated_at: Some(lane.updated_at.clone()),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LabelDto {
    pub id: i64,
    /// See note on [`LaneDto::team_id`] — Rails index responses omit this.
    #[serde(default)]
    pub team_id: i64,
    pub name: String,
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl LabelDto {
    pub fn into_model(
        self,
        conn: &Connection,
        existing_uuid: Option<String>,
    ) -> Result<Label, ApiError> {
        let team_id = local_uuid_for(conn, "teams", self.team_id)?;
        let created_at = self.created_at.unwrap_or_else(crate::db::now_iso);
        let updated_at = self.updated_at.unwrap_or_else(|| created_at.clone());
        Ok(Label {
            id: existing_uuid.unwrap_or_else(|| Uuid::new_v4().to_string()),
            remote_id: Some(self.id),
            team_id,
            name: self.name,
            color: self.color,
            created_at,
            updated_at,
        })
    }

    pub fn from_model(conn: &Connection, label: &Label) -> Result<Self, ApiError> {
        let team = lookup_team_by_local_id(conn, &label.team_id)?;
        Ok(LabelDto {
            id: require_remote(label.remote_id, "label")?,
            team_id: require_remote(team.remote_id, "team")?,
            name: label.name.clone(),
            color: label.color.clone(),
            created_at: Some(label.created_at.clone()),
            updated_at: Some(label.updated_at.clone()),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemberDto {
    pub id: i64,
    /// See note on [`LaneDto::team_id`] — Rails index responses omit this.
    #[serde(default)]
    pub team_id: i64,
    pub name: String,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl MemberDto {
    pub fn into_model(
        self,
        conn: &Connection,
        existing_uuid: Option<String>,
    ) -> Result<Member, ApiError> {
        let team_id = local_uuid_for(conn, "teams", self.team_id)?;
        let created_at = self.created_at.unwrap_or_else(crate::db::now_iso);
        let updated_at = self.updated_at.unwrap_or_else(|| created_at.clone());
        Ok(Member {
            id: existing_uuid.unwrap_or_else(|| Uuid::new_v4().to_string()),
            remote_id: Some(self.id),
            team_id,
            name: self.name,
            email: self.email,
            created_at,
            updated_at,
        })
    }

    pub fn from_model(conn: &Connection, member: &Member) -> Result<Self, ApiError> {
        let team = lookup_team_by_local_id(conn, &member.team_id)?;
        Ok(MemberDto {
            id: require_remote(member.remote_id, "member")?,
            team_id: require_remote(team.remote_id, "team")?,
            name: member.name.clone(),
            email: member.email.clone(),
            created_at: Some(member.created_at.clone()),
            updated_at: Some(member.updated_at.clone()),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueDependencyDto {
    pub id: i64,
    pub blocking_issue_id: i64,
    pub blocked_issue_id: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl IssueDependencyDto {
    pub fn into_model(
        self,
        conn: &Connection,
        existing_uuid: Option<String>,
    ) -> Result<IssueDependency, ApiError> {
        let blocking_issue_id = local_uuid_for(conn, "issues", self.blocking_issue_id)?;
        let blocked_issue_id = local_uuid_for(conn, "issues", self.blocked_issue_id)?;
        Ok(IssueDependency {
            id: existing_uuid.unwrap_or_else(|| Uuid::new_v4().to_string()),
            remote_id: Some(self.id),
            blocking_issue_id,
            blocked_issue_id,
            kind: "blocks".into(),
            direction: "blocking".into(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }

    pub fn from_model(conn: &Connection, dep: &IssueDependency) -> Result<Self, ApiError> {
        let blocking = lookup_issue_by_local_id(conn, &dep.blocking_issue_id)?;
        let blocked = lookup_issue_by_local_id(conn, &dep.blocked_issue_id)?;
        Ok(IssueDependencyDto {
            id: require_remote(dep.remote_id, "issue_dependency")?,
            blocking_issue_id: require_remote(blocking.remote_id, "issue")?,
            blocked_issue_id: require_remote(blocked.remote_id, "issue")?,
            created_at: dep.created_at.clone(),
            updated_at: dep.updated_at.clone(),
        })
    }
}

// ── Internal lookup helpers ──

fn require_remote(remote_id: Option<i64>, what: &str) -> Result<i64, ApiError> {
    remote_id.ok_or_else(|| {
        ApiError::Decode(format!("{what} has no remote_id; cannot serialize for API"))
    })
}

fn local_uuid_for(conn: &Connection, table: &str, remote_id: i64) -> Result<String, ApiError> {
    let sql = format!("SELECT id FROM {table} WHERE remote_id = ?1");
    conn.query_row(&sql, params![remote_id], |r| r.get::<_, String>(0))
        .map_err(|e| {
            ApiError::Decode(format!(
                "could not resolve {table} remote_id={remote_id}: {e}"
            ))
        })
}

fn lookup_team_by_local_id(conn: &Connection, id: &str) -> Result<Team, ApiError> {
    conn.query_row(
        "SELECT id, name, identifier, remote_id, archived_at, created_at, updated_at
         FROM teams WHERE id = ?1",
        params![id],
        Team::from_row,
    )
    .map_err(|e| ApiError::Decode(format!("team {id} not found: {e}")))
}

fn lookup_lane_by_local_id(conn: &Connection, id: &str) -> Result<Lane, ApiError> {
    conn.query_row(
        "SELECT id, name, color, position, team_id, remote_id, category, created_at, updated_at
         FROM lanes WHERE id = ?1",
        params![id],
        Lane::from_row,
    )
    .map_err(|e| ApiError::Decode(format!("lane {id} not found: {e}")))
}

fn lookup_member_by_local_id(conn: &Connection, id: &str) -> Result<Member, ApiError> {
    conn.query_row(
        "SELECT id, remote_id, team_id, name, email, created_at, updated_at
         FROM members WHERE id = ?1",
        params![id],
        Member::from_row,
    )
    .map_err(|e| ApiError::Decode(format!("member {id} not found: {e}")))
}

fn lookup_project_by_local_id(conn: &Connection, id: &str) -> Result<Project, ApiError> {
    conn.query_row(
        "SELECT id, remote_id, team_id, name, description, status, priority, lead_id,
                start_date, due_date, roadmap_commitment, velocity_score,
                completed_issues_count, total_issues_count, progress_percentage,
                behind_schedule, created_at, updated_at
         FROM projects WHERE id = ?1",
        params![id],
        Project::from_row,
    )
    .map_err(|e| ApiError::Decode(format!("project {id} not found: {e}")))
}

fn lookup_issue_by_local_id(conn: &Connection, id: &str) -> Result<Issue, ApiError> {
    conn.query_row(
        "SELECT id, title, description, priority, team_number, due_date, estimate,
                started_at, completed_at, canceled_at, lane_id, team_id,
                project_id, parent_issue_id, assignee_id, creator_id, archived_at,
                remote_id, created_at, updated_at
         FROM issues WHERE id = ?1",
        params![id],
        Issue::from_row,
    )
    .map_err(|e| ApiError::Decode(format!("issue {id} not found: {e}")))
}

fn member_record_ref(conn: &Connection, id: &str) -> Result<RecordRef, ApiError> {
    let m = lookup_member_by_local_id(conn, id)?;
    Ok(RecordRef {
        id: require_remote(m.remote_id, "member")?,
        name: m.name,
    })
}

fn project_record_ref(conn: &Connection, id: &str) -> Result<RecordRef, ApiError> {
    let p = lookup_project_by_local_id(conn, id)?;
    Ok(RecordRef {
        id: require_remote(p.remote_id, "project")?,
        name: p.name,
    })
}

fn parent_issue_ref(conn: &Connection, id: &str) -> Result<ParentIssueRef, ApiError> {
    let parent = lookup_issue_by_local_id(conn, id)?;
    let team = lookup_team_by_local_id(conn, &parent.team_id)?;
    let team_number = parent
        .team_number
        .ok_or_else(|| ApiError::Decode("parent issue missing team_number".into()))?;
    Ok(ParentIssueRef {
        id: require_remote(parent.remote_id, "issue")?,
        identifier: format!("{}-{}", team.identifier, team_number),
    })
}

fn labels_for_issue(conn: &Connection, issue_id: &str) -> Result<Vec<LabelRef>, ApiError> {
    let mut stmt = conn
        .prepare(
            "SELECT l.remote_id, l.name
             FROM labels l
             JOIN issue_labels il ON il.label_id = l.id
             WHERE il.issue_id = ?1
             ORDER BY l.name",
        )
        .map_err(|e| ApiError::Decode(format!("labels_for_issue prepare: {e}")))?;
    let rows = stmt
        .query_map(params![issue_id], |row| {
            let remote: Option<i64> = row.get(0)?;
            let name: String = row.get(1)?;
            Ok((remote, name))
        })
        .map_err(|e| ApiError::Decode(format!("labels_for_issue query: {e}")))?;
    rows.map(|r| {
        let (remote, name) = r.map_err(|e| ApiError::Decode(format!("labels row: {e}")))?;
        Ok(LabelRef {
            id: require_remote(remote, "label")?,
            name,
        })
    })
    .collect()
}

fn labels_for_project(conn: &Connection, project_id: &str) -> Result<Vec<LabelRef>, ApiError> {
    let mut stmt = conn
        .prepare(
            "SELECT l.remote_id, l.name
             FROM labels l
             JOIN project_labels pl ON pl.label_id = l.id
             WHERE pl.project_id = ?1
             ORDER BY l.name",
        )
        .map_err(|e| ApiError::Decode(format!("labels_for_project prepare: {e}")))?;
    let rows = stmt
        .query_map(params![project_id], |row| {
            let remote: Option<i64> = row.get(0)?;
            let name: String = row.get(1)?;
            Ok((remote, name))
        })
        .map_err(|e| ApiError::Decode(format!("labels_for_project query: {e}")))?;
    rows.map(|r| {
        let (remote, name) = r.map_err(|e| ApiError::Decode(format!("labels row: {e}")))?;
        Ok(LabelRef {
            id: require_remote(remote, "label")?,
            name,
        })
    })
    .collect()
}

fn issue_ref_query(
    conn: &Connection,
    issue_id: &str,
    join_column: &str,
    return_column: &str,
) -> Result<Vec<IssueRef>, ApiError> {
    let sql = format!(
        "SELECT i.remote_id, i.team_number, i.title, t.identifier
         FROM issue_dependencies d
         JOIN issues i ON i.id = d.{return_column}
         JOIN teams t ON t.id = i.team_id
         WHERE d.{join_column} = ?1
         ORDER BY i.created_at"
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| ApiError::Decode(format!("issue_ref_query prepare: {e}")))?;
    let rows = stmt
        .query_map(params![issue_id], |row| {
            let remote: Option<i64> = row.get(0)?;
            let team_number: Option<i32> = row.get(1)?;
            let title: String = row.get(2)?;
            let team_identifier: String = row.get(3)?;
            Ok((remote, team_number, title, team_identifier))
        })
        .map_err(|e| ApiError::Decode(format!("issue_ref_query query: {e}")))?;
    rows.map(|r| {
        let (remote, team_number, title, team_identifier) =
            r.map_err(|e| ApiError::Decode(format!("issue_ref row: {e}")))?;
        let tn = team_number
            .ok_or_else(|| ApiError::Decode("dependency issue missing team_number".into()))?;
        Ok(IssueRef {
            id: require_remote(remote, "issue")?,
            identifier: format!("{team_identifier}-{tn}"),
            title,
            dependency_id: None,
            direction: None,
        })
    })
    .collect()
}

fn blocking_issue_refs(conn: &Connection, issue_id: &str) -> Result<Vec<IssueRef>, ApiError> {
    // Issues that are BLOCKING this issue: rows where this issue is blocked.
    issue_ref_query(conn, issue_id, "blocked_issue_id", "blocking_issue_id")
}

fn blocked_issue_refs(conn: &Connection, issue_id: &str) -> Result<Vec<IssueRef>, ApiError> {
    // Issues that this issue is BLOCKING: rows where this issue is blocking.
    issue_ref_query(conn, issue_id, "blocking_issue_id", "blocked_issue_id")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use rusqlite::Connection;

    fn fresh_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        migrations::run(&conn).unwrap();
        // Migrations seed a default team + lanes; clear so tests start blank.
        conn.execute_batch(
            "DELETE FROM issue_labels;
             DELETE FROM project_labels;
             DELETE FROM issue_dependencies;
             DELETE FROM issues;
             DELETE FROM labels;
             DELETE FROM members;
             DELETE FROM projects;
             DELETE FROM lanes;
             DELETE FROM teams;",
        )
        .unwrap();
        conn
    }

    #[test]
    fn comment_dto_with_project_id_maps_to_project_owner() {
        let conn = fresh_conn();
        let json = r#"{"id":5,"body":"b","parent_id":null,"user":null,
            "created_at":"x","updated_at":"x","issue_id":null,"project_id":80}"#;
        let dto: CommentDto = serde_json::from_str(json).unwrap();
        assert_eq!(dto.project_id, Some(80));
        let c = dto.into_model(&conn, CommentOwner::Project("p1".into()), None, None);
        assert_eq!(c.owner(), CommentOwner::Project("p1".into()));
        assert_eq!(c.issue_id, None);
    }

    #[test]
    fn merge_comment_tree_flattens_replies_with_parent_links() {
        let conn = fresh_conn();
        conn.execute_batch(
            "INSERT INTO teams (id,name,identifier,remote_id,created_at,updated_at) VALUES ('t1','T','TT',1,'x','x');
             INSERT INTO lanes (id,name,color,position,team_id,created_at,updated_at) VALUES ('l1','L','#fff',0,'t1','x','x');
             INSERT INTO issues (id,title,priority,team_number,lane_id,team_id,created_at,updated_at) VALUES ('i1','I',0,1,'l1','t1','x','x');",
        )
        .unwrap();
        let mk = |id: i64, parent: Option<i64>, replies: Option<Vec<CommentDto>>| CommentDto {
            id,
            body: format!("c{id}"),
            parent_id: parent,
            user: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
            replies,
            issue_id: None,
            project_id: None,
            files: None,
        };
        let tree = vec![mk(
            10,
            None,
            Some(vec![mk(11, Some(10), Some(vec![mk(12, Some(11), None)]))]),
        )];
        assert_eq!(merge_comment_tree(&conn, &CommentOwner::Issue("i1".into()), tree.clone(), None), 3);
        let all = crate::db::queries::comments_for_issue(&conn, "i1").unwrap();
        let by_remote = |r: i64| all.iter().find(|c| c.remote_id == Some(r)).unwrap();
        assert_eq!(by_remote(10).parent_id, None);
        assert_eq!(
            by_remote(11).parent_id.as_deref(),
            Some(by_remote(10).id.as_str())
        );
        assert_eq!(
            by_remote(12).parent_id.as_deref(),
            Some(by_remote(11).id.as_str())
        );
        // Idempotent re-merge.
        assert_eq!(merge_comment_tree(&conn, &CommentOwner::Issue("i1".into()), tree, None), 3);
        assert_eq!(
            crate::db::queries::comments_for_issue(&conn, "i1")
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn merge_comment_tree_stores_comment_files_and_replaces_on_refetch() {
        use crate::db::attachments::{list_for_owner, OwnerKind};
        let conn = fresh_conn();
        conn.execute_batch(
            "INSERT INTO teams (id,name,identifier,remote_id,created_at,updated_at) VALUES ('t1','T','TT',1,'x','x');
             INSERT INTO projects (id,team_id,name,created_at,updated_at) VALUES ('p1','t1','P','x','x');",
        )
        .unwrap();
        let file = |id: i64, name: &str| AttachmentDto {
            id,
            blob_id: Some(id + 100),
            filename: name.into(),
            content_type: Some("image/png".into()),
            byte_size: 5,
            checksum: None,
            created_at: format!("2026-01-0{id}T00:00:00.000Z"),
            download_url: None,
        };
        let mk = |files: Option<Vec<AttachmentDto>>, replies: Option<Vec<CommentDto>>| CommentDto {
            id: 10,
            body: "b".into(),
            parent_id: None,
            user: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
            replies,
            issue_id: None,
            project_id: Some(80),
            files,
        };
        let owner = CommentOwner::Project("p1".into());
        let names = |conn: &Connection| -> Vec<String> {
            let c = crate::db::queries::find_comment_by_remote_id(conn, 10).unwrap().unwrap();
            list_for_owner(conn, OwnerKind::Comment, &c.id)
                .unwrap()
                .into_iter()
                .map(|a| a.filename)
                .collect()
        };
        merge_comment_tree(&conn, &owner, vec![mk(Some(vec![file(1, "a"), file(2, "b")]), None)], None);
        assert_eq!(names(&conn), ["a", "b"]);
        // A refetch with one file left replaces the set.
        merge_comment_tree(&conn, &owner, vec![mk(Some(vec![file(2, "b")]), None)], None);
        assert_eq!(names(&conn), ["b"]);
        // No `files` key leaves local attachments alone.
        merge_comment_tree(&conn, &owner, vec![mk(None, None)], None);
        assert_eq!(names(&conn), ["b"]);
        // Empty list clears.
        merge_comment_tree(&conn, &owner, vec![mk(Some(vec![]), None)], None);
        assert!(names(&conn).is_empty());
    }

    #[test]
    fn merge_comment_tree_stores_a_project_thread_under_the_project() {
        let conn = fresh_conn();
        conn.execute_batch(
            "INSERT INTO teams (id,name,identifier,remote_id,created_at,updated_at) VALUES ('t1','T','TT',1,'x','x');
             INSERT INTO projects (id,team_id,name,created_at,updated_at) VALUES ('p1','t1','P','x','x');",
        )
        .unwrap();
        let mk = |id: i64, parent: Option<i64>, replies: Option<Vec<CommentDto>>| CommentDto {
            id,
            body: format!("c{id}"),
            parent_id: parent,
            user: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
            replies,
            issue_id: None,
            project_id: Some(80),
            files: None,
        };
        let tree = vec![mk(10, None, Some(vec![mk(11, Some(10), None)]))];
        let owner = CommentOwner::Project("p1".into());
        assert_eq!(merge_comment_tree(&conn, &owner, tree, None), 2);
        let all = crate::db::queries::comments_for_project(&conn, "p1").unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|c| c.owner() == owner));
        let top = all.iter().find(|c| c.remote_id == Some(10)).unwrap();
        let reply = all.iter().find(|c| c.remote_id == Some(11)).unwrap();
        assert_eq!(reply.parent_id.as_deref(), Some(top.id.as_str()));
    }

    fn now() -> String {
        "2026-04-27T12:00:00.000Z".to_string()
    }

    #[test]
    fn project_status_serializes_lowercase() {
        for (status, expected) in [
            (ProjectStatus::Backlog, "backlog"),
            (ProjectStatus::Started, "started"),
            (ProjectStatus::Paused, "paused"),
            (ProjectStatus::Completed, "completed"),
            (ProjectStatus::Cancelled, "cancelled"),
        ] {
            let s = serde_json::to_string(&status).unwrap();
            assert_eq!(s, format!("\"{expected}\""));
            let parsed: ProjectStatus = serde_json::from_str(&s).unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn roadmap_commitment_serializes_lowercase() {
        for (rc, expected) in [
            (RoadmapCommitment::Now, "now"),
            (RoadmapCommitment::Next, "next"),
            (RoadmapCommitment::Later, "later"),
        ] {
            let s = serde_json::to_string(&rc).unwrap();
            assert_eq!(s, format!("\"{expected}\""));
        }
    }

    #[test]
    fn flat_team_dto_roundtrip() {
        let conn = fresh_conn();
        let team = Team {
            id: Uuid::new_v4().to_string(),
            name: "Apollo".into(),
            identifier: "APO".into(),
            remote_id: Some(99),
            archived_at: None,
            created_at: now(),
            updated_at: now(),
        };
        let dto = TeamDto::try_from(&team).unwrap();
        let json = serde_json::to_string(&dto).unwrap();
        let parsed: TeamDto = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, dto);
        let model: Team = parsed.into();
        assert_eq!(model.name, team.name);
        assert_eq!(model.identifier, team.identifier);
        assert_eq!(model.remote_id, Some(99));
        // id is regenerated on the From path; just confirm it's a UUID
        assert_eq!(model.id.len(), 36);
        let _ = conn; // silence unused
    }

    #[test]
    fn lane_dto_decodes_optional_category() {
        let with: LaneDto = serde_json::from_str(
            r##"{"id":1,"name":"Done","color":"#fff","position":2,"category":"canceled"}"##,
        )
        .unwrap();
        assert_eq!(with.category.as_deref(), Some("canceled"));
        let without: LaneDto =
            serde_json::from_str(r##"{"id":1,"name":"Done","color":null,"position":2}"##).unwrap();
        assert_eq!(without.category, None);
    }

    #[test]
    fn flat_dto_roundtrip_lane_label_member_dependency() {
        let conn = fresh_conn();
        // seed a team so foreign keys resolve when we round-trip through the DB
        let team_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![team_uuid, "Apollo", "APO", 99i64, now(), now()],
        )
        .unwrap();

        // Lane
        let lane = Lane {
            id: Uuid::new_v4().to_string(),
            name: "Backlog".into(),
            color: Some("#aaa".into()),
            position: 0,
            team_id: team_uuid.clone(),
            remote_id: Some(11),
            category: Some("canceled".into()),
            created_at: now(),
            updated_at: now(),
        };
        conn.execute(
            "INSERT INTO lanes (id, name, color, position, team_id, remote_id, category, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![lane.id, lane.name, lane.color, lane.position, lane.team_id, lane.remote_id, lane.category, lane.created_at, lane.updated_at],
        ).unwrap();

        let lane_dto = LaneDto::from_model(&conn, &lane).unwrap();
        assert_eq!(lane_dto.id, 11);
        assert_eq!(lane_dto.team_id, 99);
        let lane_back = lane_dto
            .clone()
            .into_model(&conn, Some(lane.id.clone()))
            .unwrap();
        assert_eq!(lane_back.id, lane.id);
        assert_eq!(lane_back.team_id, lane.team_id);
        assert_eq!(lane_back.position, lane.position);
        assert_eq!(lane_back.category.as_deref(), Some("canceled"));

        // Label
        let label = Label {
            id: Uuid::new_v4().to_string(),
            remote_id: Some(22),
            team_id: team_uuid.clone(),
            name: "bug".into(),
            color: Some("#f00".into()),
            created_at: now(),
            updated_at: now(),
        };
        conn.execute(
            "INSERT INTO labels (id, remote_id, team_id, name, color, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                label.id,
                label.remote_id,
                label.team_id,
                label.name,
                label.color,
                label.created_at,
                label.updated_at
            ],
        )
        .unwrap();
        let label_dto = LabelDto::from_model(&conn, &label).unwrap();
        assert_eq!(label_dto.id, 22);
        assert_eq!(label_dto.team_id, 99);
        let label_back = label_dto.into_model(&conn, Some(label.id.clone())).unwrap();
        assert_eq!(label_back.id, label.id);
        assert_eq!(label_back.name, "bug");

        // Member
        let member = Member {
            id: Uuid::new_v4().to_string(),
            remote_id: Some(33),
            team_id: team_uuid.clone(),
            name: "Alice".into(),
            email: "alice@example.com".into(),
            created_at: now(),
            updated_at: now(),
        };
        conn.execute(
            "INSERT INTO members (id, remote_id, team_id, name, email, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                member.id,
                member.remote_id,
                member.team_id,
                member.name,
                member.email,
                member.created_at,
                member.updated_at
            ],
        )
        .unwrap();
        let member_dto = MemberDto::from_model(&conn, &member).unwrap();
        assert_eq!(member_dto.email, "alice@example.com");
        let member_back = member_dto
            .into_model(&conn, Some(member.id.clone()))
            .unwrap();
        assert_eq!(member_back.id, member.id);

        // IssueDependency requires two issues
        let issue_a = insert_minimal_issue(&conn, &team_uuid, &lane.id, 101, 1);
        let issue_b = insert_minimal_issue(&conn, &team_uuid, &lane.id, 102, 2);
        let dep = IssueDependency {
            id: Uuid::new_v4().to_string(),
            remote_id: Some(44),
            blocking_issue_id: issue_a.clone(),
            blocked_issue_id: issue_b.clone(),
            kind: "blocks".into(),
            direction: "blocking".into(),
            created_at: now(),
            updated_at: now(),
        };
        conn.execute(
            "INSERT INTO issue_dependencies (id, remote_id, blocking_issue_id, blocked_issue_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![dep.id, dep.remote_id, dep.blocking_issue_id, dep.blocked_issue_id, dep.created_at, dep.updated_at],
        ).unwrap();
        let dep_dto = IssueDependencyDto::from_model(&conn, &dep).unwrap();
        assert_eq!(dep_dto.id, 44);
        assert_eq!(dep_dto.blocking_issue_id, 101);
        assert_eq!(dep_dto.blocked_issue_id, 102);
        let dep_back = dep_dto.into_model(&conn, Some(dep.id.clone())).unwrap();
        assert_eq!(dep_back.blocking_issue_id, issue_a);
        assert_eq!(dep_back.blocked_issue_id, issue_b);
    }

    fn insert_minimal_issue(
        conn: &Connection,
        team_uuid: &str,
        lane_uuid: &str,
        remote_id: i64,
        team_number: i32,
    ) -> String {
        let id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO issues (id, title, priority, team_number, lane_id, team_id, remote_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, "minimal", 4, team_number, lane_uuid, team_uuid, remote_id, now(), now()],
        ).unwrap();
        id
    }

    #[test]
    fn issue_identifier_computed_from_team_and_team_number() {
        let conn = fresh_conn();
        let team_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![team_uuid, "Project", "PROJ", 1i64, now(), now()],
        )
        .unwrap();
        let lane_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO lanes (id, name, position, team_id, remote_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![lane_uuid, "Backlog", 0, team_uuid, 10i64, now(), now()],
        )
        .unwrap();
        let issue_uuid = insert_minimal_issue(&conn, &team_uuid, &lane_uuid, 555, 42);
        let issue = lookup_issue_by_local_id(&conn, &issue_uuid).unwrap();
        let dto = IssueDto::from_model(&conn, &issue).unwrap();
        assert_eq!(dto.identifier, "PROJ-42");
    }

    const ISSUE_FIXTURE: &str = r#"{
        "id": 7001,
        "identifier": "APO-42",
        "title": "Wire up auth",
        "priority": "high",
        "estimate": 3,
        "due_date": "2026-05-01",
        "lane": {"id": 11, "name": "In Progress"},
        "assignee": {"id": 33, "name": "Alice"},
        "creator": {"id": 34, "name": "Bob"},
        "project": {"id": 55, "name": "Q2 Roadmap"},
        "labels": [{"id": 22, "name": "bug"}],
        "created_at": "2026-04-20T10:00:00.000Z",
        "updated_at": "2026-04-26T12:00:00.000Z",
        "description": "Implement OAuth flow",
        "parent_issue": null,
        "blocking_issues": [],
        "blocked_issues": [],
        "started_at": "2026-04-21T09:00:00.000Z",
        "completed_at": null,
        "canceled_at": null
    }"#;

    /// Detail dependency entries as emitted by `Api::V1::IssueSerializer`
    /// (`serialize_dependency_issue` / `serialize_links`) for issue 7001.
    fn detail_fixture_with_links() -> IssueDto {
        let mut v: serde_json::Value = serde_json::from_str(ISSUE_FIXTURE).unwrap();
        v["blocking_issues"] = serde_json::json!([
            {"id": 7002, "identifier": "APO-43", "title": "B", "dependency_id": 11}
        ]);
        v["blocked_issues"] = serde_json::json!([
            {"id": 7003, "identifier": "APO-44", "title": "C", "dependency_id": 12}
        ]);
        v["related_issues"] = serde_json::json!([
            {"id": 7004, "identifier": "APO-45", "title": "D", "dependency_id": 13, "direction": "relates"}
        ]);
        v["duplicates"] = serde_json::json!([
            {"id": 7005, "identifier": "APO-46", "title": "E", "dependency_id": 14, "direction": "duplicates"},
            {"id": 7006, "identifier": "APO-47", "title": "F", "dependency_id": 15, "direction": "duplicated_by"}
        ]);
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn detail_parses_dependency_ids_and_link_directions() {
        let dto = detail_fixture_with_links();
        assert_eq!(
            dto.blocking_issues.as_ref().unwrap()[0].dependency_id,
            Some(11)
        );
        let rel = &dto.related_issues.as_ref().unwrap()[0];
        assert_eq!(rel.dependency_id, Some(13));
        assert_eq!(rel.direction.as_deref(), Some("relates"));
        let dups = dto.duplicates.as_ref().unwrap();
        assert_eq!(dups[1].direction.as_deref(), Some("duplicated_by"));
    }

    #[test]
    fn detail_without_dependency_fields_still_decodes() {
        let dto: IssueDto = serde_json::from_str(ISSUE_FIXTURE).unwrap();
        assert!(dto.related_issues.is_none() && dto.duplicates.is_none());
        assert!(dto.dependency_links().is_empty());
    }

    #[test]
    fn dependency_links_are_oriented_source_to_target() {
        let l = detail_fixture_with_links().dependency_links();
        let by_id = |id| l.iter().find(|d| d.dependency_id == id).unwrap();
        // 7002 blocks 7001
        assert_eq!(
            (by_id(11).source_remote_id, by_id(11).target_remote_id),
            (7002, 7001)
        );
        assert_eq!(by_id(11).kind, "blocks");
        // 7001 blocks 7003
        assert_eq!(
            (by_id(12).source_remote_id, by_id(12).target_remote_id),
            (7001, 7003)
        );
        assert_eq!(
            (by_id(13).kind, by_id(13).direction),
            ("relates", "relates")
        );
        // 7001 duplicates 7005; 7006 duplicates 7001
        assert_eq!(
            (by_id(14).source_remote_id, by_id(14).target_remote_id),
            (7001, 7005)
        );
        assert_eq!(
            (by_id(15).source_remote_id, by_id(15).target_remote_id),
            (7006, 7001)
        );
        assert_eq!(by_id(15).direction, "duplicates");
    }

    fn seed_issue_fixture_db(conn: &Connection) -> (String, String, String, String, String) {
        let team_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![team_uuid, "Apollo", "APO", 99i64, now(), now()],
        )
        .unwrap();
        let lane_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO lanes (id, name, color, position, team_id, remote_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![lane_uuid, "In Progress", "#3B82F6", 1, team_uuid, 11i64, now(), now()],
        ).unwrap();
        let assignee_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO members (id, remote_id, team_id, name, email, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                assignee_uuid,
                33i64,
                team_uuid,
                "Alice",
                "a@x",
                now(),
                now()
            ],
        )
        .unwrap();
        let creator_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO members (id, remote_id, team_id, name, email, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![creator_uuid, 34i64, team_uuid, "Bob", "b@x", now(), now()],
        )
        .unwrap();
        let project_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO projects (id, remote_id, team_id, name, status, priority, velocity_score,
                                   completed_issues_count, total_issues_count, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'backlog', 2, 0, 0, 0, ?5, ?6)",
            params![project_uuid, 55i64, team_uuid, "Q2 Roadmap", now(), now()],
        )
        .unwrap();
        let label_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO labels (id, remote_id, team_id, name, color, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![label_uuid, 22i64, team_uuid, "bug", "#f00", now(), now()],
        )
        .unwrap();
        (
            team_uuid,
            lane_uuid,
            assignee_uuid,
            project_uuid,
            label_uuid,
        )
    }

    #[test]
    fn issue_dto_roundtrip_via_fixture() {
        let conn = fresh_conn();
        let (team_uuid, _lane_uuid, _assignee_uuid, _project_uuid, label_uuid) =
            seed_issue_fixture_db(&conn);

        let original: IssueDto = serde_json::from_str(ISSUE_FIXTURE).unwrap();
        assert_eq!(original.identifier, "APO-42");
        assert_eq!(original.priority, "high");
        assert_eq!(original.labels.len(), 1);

        // DTO → model
        let issue = original
            .clone()
            .into_model(&conn, team_uuid.clone(), None)
            .unwrap();
        assert_eq!(issue.title, "Wire up auth");
        assert_eq!(issue.priority, 1);
        assert_eq!(issue.estimate, Some(3.0));
        assert_eq!(issue.team_number, Some(42));
        assert_eq!(issue.team_id, team_uuid);
        assert_eq!(issue.remote_id, Some(7001));
        // lane/assignee/creator/project resolved to local UUIDs:
        assert!(!issue.lane_id.is_empty());
        assert!(issue.assignee_id.is_some());
        assert!(issue.creator_id.is_some());
        assert!(issue.project_id.is_some());

        // Insert the issue and its label join row, then go model → DTO.
        conn.execute(
            "INSERT INTO issues (id, title, description, priority, team_number, due_date, estimate,
                                  started_at, completed_at, canceled_at, lane_id, team_id,
                                  project_id, parent_issue_id, assignee_id, creator_id, archived_at,
                                  remote_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                issue.id, issue.title, issue.description, issue.priority, issue.team_number,
                issue.due_date, issue.estimate, issue.started_at, issue.completed_at, issue.canceled_at,
                issue.lane_id, issue.team_id, issue.project_id, issue.parent_issue_id,
                issue.assignee_id, issue.creator_id, issue.archived_at, issue.remote_id,
                issue.created_at, issue.updated_at,
            ],
        ).unwrap();
        conn.execute(
            "INSERT INTO issue_labels (issue_id, label_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![issue.id, label_uuid, now(), now()],
        )
        .unwrap();

        let final_dto = IssueDto::from_model(&conn, &issue).unwrap();
        assert_eq!(final_dto.id, original.id);
        assert_eq!(final_dto.identifier, original.identifier);
        assert_eq!(final_dto.title, original.title);
        assert_eq!(final_dto.priority, original.priority);
        assert_eq!(final_dto.estimate, original.estimate);
        assert_eq!(final_dto.due_date, original.due_date);
        assert_eq!(final_dto.lane, original.lane);
        assert_eq!(final_dto.assignee, original.assignee);
        assert_eq!(final_dto.creator, original.creator);
        assert_eq!(final_dto.project, original.project);
        assert_eq!(final_dto.labels, original.labels);
        assert_eq!(final_dto.description, original.description);
        assert_eq!(final_dto.started_at, original.started_at);
        assert_eq!(final_dto.completed_at, original.completed_at);
        assert_eq!(final_dto.canceled_at, original.canceled_at);
        assert_eq!(final_dto.created_at, original.created_at);
        assert_eq!(final_dto.updated_at, original.updated_at);
    }

    const PROJECT_FIXTURE: &str = r#"{
        "id": 8001,
        "name": "Q2 Roadmap",
        "description": "Big features for Q2",
        "status": "started",
        "priority": "medium",
        "lead": {"id": 33, "name": "Alice"},
        "start_date": "2026-04-01",
        "due_date": "2026-06-30",
        "velocity_score": 7,
        "completed_issues_count": 4,
        "total_issues_count": 10,
        "progress_percentage": 40,
        "behind_schedule": false,
        "roadmap_commitment": "now",
        "created_at": "2026-03-25T08:00:00.000Z",
        "updated_at": "2026-04-26T12:00:00.000Z",
        "labels": [{"id": 22, "name": "bug"}]
    }"#;

    #[test]
    fn project_dto_roundtrip_via_fixture() {
        let conn = fresh_conn();
        let team_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![team_uuid, "Apollo", "APO", 99i64, now(), now()],
        )
        .unwrap();
        let lead_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO members (id, remote_id, team_id, name, email, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![lead_uuid, 33i64, team_uuid, "Alice", "a@x", now(), now()],
        )
        .unwrap();
        let label_uuid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO labels (id, remote_id, team_id, name, color, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![label_uuid, 22i64, team_uuid, "bug", "#f00", now(), now()],
        )
        .unwrap();

        let original: ProjectDto = serde_json::from_str(PROJECT_FIXTURE).unwrap();
        assert_eq!(original.status, Some(ProjectStatus::Started));
        assert_eq!(original.priority, "medium");
        assert_eq!(original.roadmap_commitment, Some(RoadmapCommitment::Now));

        let project = original
            .clone()
            .into_model(&conn, team_uuid.clone(), None)
            .unwrap();
        assert_eq!(project.priority, 2);
        assert_eq!(project.team_id, team_uuid);
        assert_eq!(project.remote_id, Some(8001));
        assert!(project.lead_id.is_some());

        conn.execute(
            "INSERT INTO projects (id, remote_id, team_id, name, description, status, priority,
                                    lead_id, start_date, due_date, roadmap_commitment,
                                    velocity_score, completed_issues_count, total_issues_count,
                                    created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                project.id,
                project.remote_id,
                project.team_id,
                project.name,
                project.description,
                project.status,
                project.priority,
                project.lead_id,
                project.start_date,
                project.due_date,
                project.roadmap_commitment,
                project.velocity_score,
                project.completed_issues_count,
                project.total_issues_count,
                project.created_at,
                project.updated_at,
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project_labels (project_id, label_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
            params![project.id, label_uuid, now(), now()],
        )
        .unwrap();

        let final_dto = ProjectDto::from_model(&conn, &project).unwrap();
        assert_eq!(final_dto.id, original.id);
        assert_eq!(final_dto.name, original.name);
        assert_eq!(final_dto.description, original.description);
        assert_eq!(final_dto.status, original.status);
        assert_eq!(final_dto.priority, original.priority);
        assert_eq!(final_dto.lead, original.lead);
        assert_eq!(final_dto.start_date, original.start_date);
        assert_eq!(final_dto.due_date, original.due_date);
        assert_eq!(final_dto.velocity_score, original.velocity_score);
        assert_eq!(
            final_dto.completed_issues_count,
            original.completed_issues_count
        );
        assert_eq!(final_dto.total_issues_count, original.total_issues_count);
        assert_eq!(final_dto.progress_percentage, original.progress_percentage);
        assert_eq!(final_dto.roadmap_commitment, original.roadmap_commitment);
        assert_eq!(final_dto.labels, original.labels);
        // behind_schedule is intentionally a conservative `false` on
        // model→DTO; the fixture also has `false`, so they happen to match.
        assert_eq!(final_dto.behind_schedule, original.behind_schedule);
    }
}
