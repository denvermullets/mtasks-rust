use super::{ApiClient, ApiError, IssueDto};

#[derive(Debug, Default, Clone)]
pub struct IssueFilters {
    pub lane_id: Option<i64>,
    pub assignee_id: Option<i64>,
    pub project_id: Option<i64>,
    /// One of: "urgent" | "high" | "medium" | "low" | "no_priority".
    pub priority: Option<String>,
}

impl IssueFilters {
    fn query_string(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(v) = self.lane_id {
            parts.push(format!("lane_id={v}"));
        }
        if let Some(v) = self.assignee_id {
            parts.push(format!("assignee_id={v}"));
        }
        if let Some(v) = self.project_id {
            parts.push(format!("project_id={v}"));
        }
        if let Some(v) = &self.priority {
            parts.push(format!("priority={v}"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("?{}", parts.join("&"))
        }
    }
}

pub async fn list(
    client: &ApiClient,
    team_id: i64,
    filters: &IssueFilters,
) -> Result<Vec<IssueDto>, ApiError> {
    let path = format!(
        "api/v1/teams/{team_id}/issues{}",
        filters.query_string()
    );
    client.get(&path).await
}

pub async fn get(client: &ApiClient, team_id: i64, id: i64) -> Result<IssueDto, ApiError> {
    let path = format!("api/v1/teams/{team_id}/issues/{id}");
    client.get(&path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filters_produce_no_query_string() {
        assert_eq!(IssueFilters::default().query_string(), "");
    }

    #[test]
    fn populated_filters_join_with_ampersand() {
        let f = IssueFilters {
            lane_id: Some(1),
            assignee_id: Some(2),
            project_id: Some(3),
            priority: Some("high".into()),
        };
        assert_eq!(
            f.query_string(),
            "?lane_id=1&assignee_id=2&project_id=3&priority=high"
        );
    }

    #[test]
    fn partial_filters_only_emit_set_keys() {
        let f = IssueFilters {
            priority: Some("urgent".into()),
            ..Default::default()
        };
        assert_eq!(f.query_string(), "?priority=urgent");
    }
}
