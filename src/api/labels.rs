use super::{ApiClient, ApiError, LabelDto};

pub async fn list(client: &ApiClient, team_id: i64) -> Result<Vec<LabelDto>, ApiError> {
    let path = format!("api/v1/teams/{team_id}/labels");
    client.get(&path).await
}
