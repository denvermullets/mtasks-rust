use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Result as SqlResult, Row};
use serde::{Deserialize, Serialize};

use crate::api::error::ApiError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub identifier: String,
    pub remote_id: Option<i64>,
    /// Set when Rails stopped returning the team (archived upstream). Archived
    /// teams are hidden from the sidebar unless "Show archived" is on.
    #[serde(default)]
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Team {
    pub fn from_row(row: &Row<'_>) -> SqlResult<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            identifier: row.get("identifier")?,
            remote_id: row.get("remote_id")?,
            archived_at: row.get("archived_at")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lane {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
    pub position: i32,
    pub team_id: String,
    pub remote_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

impl Lane {
    pub fn from_row(row: &Row<'_>) -> SqlResult<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            color: row.get("color")?,
            position: row.get("position")?,
            team_id: row.get("team_id")?,
            remote_id: row.get("remote_id")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    Urgent = 0,
    High = 1,
    Medium = 2,
    Low = 3,
    None = 4,
}

impl Priority {
    pub fn from_i32(v: i32) -> Self {
        match v {
            0 => Priority::Urgent,
            1 => Priority::High,
            2 => Priority::Medium,
            3 => Priority::Low,
            _ => Priority::None,
        }
    }

    pub fn to_i32(self) -> i32 {
        self as i32
    }

    pub fn as_rails_str(&self) -> &'static str {
        match self {
            Priority::Urgent => "urgent",
            Priority::High => "high",
            Priority::Medium => "medium",
            Priority::Low => "low",
            Priority::None => "no_priority",
        }
    }

    pub fn from_rails_str(s: &str) -> Result<Self, ApiError> {
        match s {
            "urgent" => Ok(Priority::Urgent),
            "high" => Ok(Priority::High),
            "medium" => Ok(Priority::Medium),
            "low" => Ok(Priority::Low),
            "no_priority" => Ok(Priority::None),
            other => Err(ApiError::EnumMapping(format!("unknown priority: {other}"))),
        }
    }
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_rails_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    Backlog,
    Started,
    Paused,
    Completed,
    Cancelled,
}

impl ProjectStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProjectStatus::Backlog => "backlog",
            ProjectStatus::Started => "started",
            ProjectStatus::Paused => "paused",
            ProjectStatus::Completed => "completed",
            ProjectStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "backlog" => Some(ProjectStatus::Backlog),
            "started" => Some(ProjectStatus::Started),
            "paused" => Some(ProjectStatus::Paused),
            "completed" => Some(ProjectStatus::Completed),
            "cancelled" => Some(ProjectStatus::Cancelled),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProjectStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromSql for ProjectStatus {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        ProjectStatus::parse(s)
            .ok_or_else(|| FromSqlError::Other(format!("invalid project status: {s}").into()))
    }
}

impl ToSql for ProjectStatus {
    fn to_sql(&self) -> SqlResult<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoadmapCommitment {
    Now,
    Next,
    Later,
}

impl RoadmapCommitment {
    pub fn as_str(&self) -> &'static str {
        match self {
            RoadmapCommitment::Now => "now",
            RoadmapCommitment::Next => "next",
            RoadmapCommitment::Later => "later",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "now" => Some(RoadmapCommitment::Now),
            "next" => Some(RoadmapCommitment::Next),
            "later" => Some(RoadmapCommitment::Later),
            _ => None,
        }
    }
}

impl std::fmt::Display for RoadmapCommitment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromSql for RoadmapCommitment {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        RoadmapCommitment::parse(s)
            .ok_or_else(|| FromSqlError::Other(format!("invalid roadmap commitment: {s}").into()))
    }
}

impl ToSql for RoadmapCommitment {
    fn to_sql(&self) -> SqlResult<ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: i32,
    pub team_number: Option<i32>,
    pub due_date: Option<String>,
    pub estimate: Option<f64>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub canceled_at: Option<String>,
    pub lane_id: String,
    pub team_id: String,
    pub project_id: Option<String>,
    pub parent_issue_id: Option<String>,
    pub assignee_id: Option<String>,
    pub creator_id: Option<String>,
    pub archived_at: Option<String>,
    pub remote_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

impl Issue {
    pub fn priority_enum(&self) -> Priority {
        Priority::from_i32(self.priority)
    }

    pub fn from_row(row: &Row<'_>) -> SqlResult<Self> {
        Ok(Self {
            id: row.get("id")?,
            title: row.get("title")?,
            description: row.get("description")?,
            priority: row.get("priority")?,
            team_number: row.get("team_number")?,
            due_date: row.get("due_date")?,
            estimate: row.get("estimate")?,
            started_at: row.get("started_at")?,
            completed_at: row.get("completed_at")?,
            canceled_at: row.get("canceled_at")?,
            lane_id: row.get("lane_id")?,
            team_id: row.get("team_id")?,
            project_id: row.get("project_id")?,
            parent_issue_id: row.get("parent_issue_id")?,
            assignee_id: row.get("assignee_id")?,
            creator_id: row.get("creator_id")?,
            archived_at: row.get("archived_at")?,
            remote_id: row.get("remote_id")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub remote_id: Option<i64>,
    pub team_id: String,
    pub name: String,
    pub description: Option<String>,
    pub status: ProjectStatus,
    pub priority: i32,
    pub lead_id: Option<String>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub roadmap_commitment: Option<RoadmapCommitment>,
    pub velocity_score: i64,
    pub completed_issues_count: i64,
    pub total_issues_count: i64,
    /// Server-computed % complete (round(completed/total*100)). Owned by Rails;
    /// only written on pull (see `queries::upsert_project`).
    pub progress_percentage: i64,
    /// Server-computed health signal driving the "At risk" flag. Owned by Rails.
    pub behind_schedule: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl Project {
    pub fn priority_enum(&self) -> Priority {
        Priority::from_i32(self.priority)
    }

    pub fn from_row(row: &Row<'_>) -> SqlResult<Self> {
        Ok(Self {
            id: row.get("id")?,
            remote_id: row.get("remote_id")?,
            team_id: row.get("team_id")?,
            name: row.get("name")?,
            description: row.get("description")?,
            status: row.get("status")?,
            priority: row.get("priority")?,
            lead_id: row.get("lead_id")?,
            start_date: row.get("start_date")?,
            due_date: row.get("due_date")?,
            roadmap_commitment: row.get("roadmap_commitment")?,
            velocity_score: row.get("velocity_score")?,
            completed_issues_count: row.get("completed_issues_count")?,
            total_issues_count: row.get("total_issues_count")?,
            progress_percentage: row.get("progress_percentage")?,
            behind_schedule: row.get("behind_schedule")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: String,
    pub remote_id: Option<i64>,
    pub team_id: String,
    pub name: String,
    pub color: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Label {
    pub fn from_row(row: &Row<'_>) -> SqlResult<Self> {
        Ok(Self {
            id: row.get("id")?,
            remote_id: row.get("remote_id")?,
            team_id: row.get("team_id")?,
            name: row.get("name")?,
            color: row.get("color")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub remote_id: Option<i64>,
    pub team_id: String,
    pub name: String,
    pub email: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Member {
    pub fn from_row(row: &Row<'_>) -> SqlResult<Self> {
        Ok(Self {
            id: row.get("id")?,
            remote_id: row.get("remote_id")?,
            team_id: row.get("team_id")?,
            name: row.get("name")?,
            email: row.get("email")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueDependency {
    pub id: String,
    pub remote_id: Option<i64>,
    pub blocking_issue_id: String,
    pub blocked_issue_id: String,
    /// `blocks` | `relates` | `duplicates` (Rails `IssueDependency#kind`).
    pub kind: String,
    /// Relative to `blocking_issue_id`: `blocking` | `relates` | `duplicates`.
    pub direction: String,
    pub created_at: String,
    pub updated_at: String,
}

impl IssueDependency {
    pub fn from_row(row: &Row<'_>) -> SqlResult<Self> {
        Ok(Self {
            id: row.get("id")?,
            remote_id: row.get("remote_id")?,
            blocking_issue_id: row.get("blocking_issue_id")?,
            blocked_issue_id: row.get("blocked_issue_id")?,
            kind: row.get("kind")?,
            direction: row.get("direction")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub remote_id: Option<i64>,
    pub issue_id: String,
    pub author_id: Option<String>,
    pub body: String,
    pub parent_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Max reply depth (top-level = 0). Mirrors Rails `Comment::MAX_NESTING_DEPTH`.
pub const MAX_NESTING_DEPTH: usize = 4;

impl Comment {
    /// Whether a comment at `depth` accepts replies (Rails `can_reply?`).
    pub fn can_reply_at(depth: usize) -> bool {
        depth < MAX_NESTING_DEPTH
    }

    /// Order a flat comment list (oldest first) as a depth-first thread: each
    /// comment followed by its replies, paired with its display depth (capped at
    /// `MAX_NESTING_DEPTH`). Comments whose parent is missing locally are
    /// treated as top-level.
    pub fn thread_order(comments: &[Comment]) -> Vec<(usize, &Comment)> {
        fn walk<'a>(
            comments: &'a [Comment],
            node: &'a Comment,
            depth: usize,
            out: &mut Vec<(usize, &'a Comment)>,
        ) {
            out.push((depth.min(MAX_NESTING_DEPTH), node));
            for child in comments
                .iter()
                .filter(|c| c.parent_id.as_deref() == Some(node.id.as_str()))
            {
                walk(comments, child, depth + 1, out);
            }
        }
        let mut out = Vec::with_capacity(comments.len());
        for root in comments.iter().filter(|c| match c.parent_id.as_deref() {
            None => true,
            Some(p) => !comments.iter().any(|o| o.id == p),
        }) {
            walk(comments, root, 0, &mut out);
        }
        out
    }

    pub fn from_row(row: &Row<'_>) -> SqlResult<Self> {
        Ok(Self {
            id: row.get("id")?,
            remote_id: row.get("remote_id")?,
            issue_id: row.get("issue_id")?,
            author_id: row.get("author_id")?,
            body: row.get("body")?,
            parent_id: row.get("parent_id")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncQueueEntry {
    pub id: Option<i64>,
    pub entity_type: String,
    pub entity_id: String,
    pub action: String,
    pub payload: Option<String>,
    pub created_at: String,
    pub synced_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn cm(id: &str, parent: Option<&str>) -> Comment {
        Comment {
            id: id.into(),
            remote_id: None,
            issue_id: "i".into(),
            author_id: None,
            body: id.into(),
            parent_id: parent.map(String::from),
            created_at: "t".into(),
            updated_at: "t".into(),
        }
    }

    #[test]
    fn thread_order_nests_depth_first_and_caps_depth() {
        let list = vec![
            cm("a", None),
            cm("b", None),
            cm("a1", Some("a")),
            cm("a2", Some("a1")),
            cm("a3", Some("a2")),
            cm("a4", Some("a3")),
            cm("a5", Some("a4")),
            cm("orphan", Some("gone")),
        ];
        let order: Vec<(usize, &str)> = Comment::thread_order(&list)
            .into_iter()
            .map(|(d, c)| (d, c.id.as_str()))
            .collect();
        assert_eq!(
            order,
            vec![
                (0, "a"),
                (1, "a1"),
                (2, "a2"),
                (3, "a3"),
                (4, "a4"),
                (4, "a5"),
                (0, "b"),
                (0, "orphan"),
            ]
        );
        assert!(Comment::can_reply_at(3));
        assert!(!Comment::can_reply_at(MAX_NESTING_DEPTH));
    }

    const ALL_PRIORITIES: [Priority; 5] = [
        Priority::Urgent,
        Priority::High,
        Priority::Medium,
        Priority::Low,
        Priority::None,
    ];

    #[test]
    fn priority_rails_str_roundtrip() {
        for p in ALL_PRIORITIES {
            assert_eq!(Priority::from_rails_str(p.as_rails_str()).unwrap(), p);
        }
    }

    #[test]
    fn priority_rails_strings_exact() {
        assert_eq!(Priority::Urgent.as_rails_str(), "urgent");
        assert_eq!(Priority::High.as_rails_str(), "high");
        assert_eq!(Priority::Medium.as_rails_str(), "medium");
        assert_eq!(Priority::Low.as_rails_str(), "low");
        assert_eq!(Priority::None.as_rails_str(), "no_priority");
    }

    #[test]
    fn priority_i32_roundtrip() {
        for i in 0..=4 {
            assert_eq!(Priority::from_i32(i).to_i32(), i);
        }
    }

    #[test]
    fn priority_from_rails_str_invalid() {
        match Priority::from_rails_str("bogus") {
            Err(ApiError::EnumMapping(msg)) => assert!(msg.contains("bogus")),
            other => panic!("expected EnumMapping error, got {other:?}"),
        }
    }

    #[test]
    fn project_status_serde_roundtrip() {
        let s = serde_json::to_string(&ProjectStatus::Started).unwrap();
        assert_eq!(s, "\"started\"");
        let parsed: ProjectStatus = serde_json::from_str("\"cancelled\"").unwrap();
        assert_eq!(parsed, ProjectStatus::Cancelled);
    }

    #[test]
    fn roadmap_commitment_serde_roundtrip() {
        let s = serde_json::to_string(&RoadmapCommitment::Later).unwrap();
        assert_eq!(s, "\"later\"");
        let parsed: RoadmapCommitment = serde_json::from_str("\"now\"").unwrap();
        assert_eq!(parsed, RoadmapCommitment::Now);
    }

    #[test]
    fn project_status_sql_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (s TEXT NOT NULL)")
            .unwrap();
        conn.execute(
            "INSERT INTO t (s) VALUES (?1)",
            rusqlite::params![ProjectStatus::Paused],
        )
        .unwrap();
        let got: ProjectStatus = conn
            .query_row("SELECT s FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(got, ProjectStatus::Paused);
    }

    #[test]
    fn project_status_sql_invalid_errors() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (s TEXT NOT NULL)")
            .unwrap();
        conn.execute("INSERT INTO t (s) VALUES ('bogus')", rusqlite::params![])
            .unwrap();
        let got: rusqlite::Result<ProjectStatus> =
            conn.query_row("SELECT s FROM t", [], |row| row.get(0));
        assert!(got.is_err());
    }

    #[test]
    fn roadmap_commitment_optional_null_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (rc TEXT)").unwrap();
        conn.execute(
            "INSERT INTO t (rc) VALUES (NULL), (?1)",
            rusqlite::params![RoadmapCommitment::Next],
        )
        .unwrap();
        let mut stmt = conn.prepare("SELECT rc FROM t ORDER BY rowid").unwrap();
        let rows: Vec<Option<RoadmapCommitment>> = stmt
            .query_map([], |row| row.get::<_, Option<RoadmapCommitment>>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows, vec![None, Some(RoadmapCommitment::Next)]);
    }
}
