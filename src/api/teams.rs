use super::{ApiClient, ApiError, TeamDto};

pub async fn list(client: &ApiClient) -> Result<Vec<TeamDto>, ApiError> {
    client.get("api/v1/teams").await
}
