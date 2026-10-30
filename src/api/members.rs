use super::{ApiClient, ApiError, MemberDto};

pub async fn list(client: &ApiClient, team_id: i64) -> Result<Vec<MemberDto>, ApiError> {
    let path = format!("api/v1/teams/{team_id}/members");
    client.get_list(&path).await
}
