use super::{ApiClient, ApiError, LaneDto};

pub async fn list(client: &ApiClient, team_id: i64) -> Result<Vec<LaneDto>, ApiError> {
    let path = format!("api/v1/teams/{team_id}/lanes");
    client.get(&path).await
}
