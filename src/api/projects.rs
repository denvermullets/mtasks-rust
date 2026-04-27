use super::{ApiClient, ApiError, ProjectDto};

pub async fn list(client: &ApiClient, team_id: i64) -> Result<Vec<ProjectDto>, ApiError> {
    let path = format!("api/v1/teams/{team_id}/projects");
    client.get(&path).await
}

pub async fn get(client: &ApiClient, team_id: i64, id: i64) -> Result<ProjectDto, ApiError> {
    let path = format!("api/v1/teams/{team_id}/projects/{id}");
    client.get(&path).await
}
