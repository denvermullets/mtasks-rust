use serde::Deserialize;

use super::{ApiClient, ApiError};

/// Response shape from `GET /api/v1/me`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MeDto {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub token: MeTokenDto,
}

/// Metadata about the API token making the request. `team_id = None` means
/// the token is user-scoped (full access to every team the user is on);
/// `Some(t)` means the token is restricted to a single team.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MeTokenDto {
    pub name: String,
    pub scopes: Vec<String>,
    pub team_id: Option<i64>,
}

impl MeTokenDto {
    pub fn has_write_scope(&self) -> bool {
        self.scopes.iter().any(|s| s == "write")
    }
}

pub async fn me(client: &ApiClient) -> Result<MeDto, ApiError> {
    client.get("api/v1/me").await
}
