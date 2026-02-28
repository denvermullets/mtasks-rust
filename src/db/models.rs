use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub identifier: String,
    pub remote_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
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

    pub fn label(&self) -> &'static str {
        match self {
            Priority::Urgent => "urgent",
            Priority::High => "high",
            Priority::Medium => "medium",
            Priority::Low => "low",
            Priority::None => "none",
        }
    }
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
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
    pub remote_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

impl Issue {
    pub fn priority_enum(&self) -> Priority {
        Priority::from_i32(self.priority)
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
