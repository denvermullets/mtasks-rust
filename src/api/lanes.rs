use super::{ApiClient, ApiError, Conditional, LaneDto};

/// Conditional list: pass the stored ETag; a 304 yields `NotModified`.
pub async fn list_conditional(
    client: &ApiClient,
    team_id: i64,
    etag: Option<&str>,
) -> Result<Conditional<Vec<LaneDto>>, ApiError> {
    let path = format!("api/v1/teams/{team_id}/lanes");
    client.get_conditional(&path, etag).await
}
