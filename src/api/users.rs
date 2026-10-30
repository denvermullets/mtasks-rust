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

/// Metadata about the API token making the request. `team_ids` is the
/// authoritative team set: `None` (null or absent) means unscoped (every
/// non-archived team the user is on); `Some(ids)` means exactly those teams,
/// possibly none. `team_id` is the legacy single-team fallback, used only when
/// `team_ids` is absent.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MeTokenDto {
    pub name: String,
    pub scopes: Vec<String>,
    #[serde(default)]
    pub team_ids: Option<Vec<i64>>,
    #[serde(default)]
    pub team_id: Option<i64>,
}

impl MeTokenDto {
    pub fn has_write_scope(&self) -> bool {
        self.scopes.iter().any(|s| s == "write")
    }

    /// Remote team ids the token is restricted to, or `None` when unscoped.
    pub fn scoped_team_ids(&self) -> Option<Vec<i64>> {
        match (&self.team_ids, self.team_id) {
            (Some(ids), _) => Some(ids.clone()),
            (None, Some(id)) => Some(vec![id]),
            (None, None) => None,
        }
    }
}

pub async fn me(client: &ApiClient) -> Result<MeDto, ApiError> {
    client.get("api/v1/me").await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(json: &str) -> MeTokenDto {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn decodes_team_ids_list() {
        let t = token(r#"{"name":"n","scopes":["read"],"team_ids":[3,5],"team_id":null}"#);
        assert_eq!(t.team_ids, Some(vec![3, 5]));
        assert_eq!(t.scoped_team_ids(), Some(vec![3, 5]));
    }

    #[test]
    fn decodes_empty_team_ids_as_zero_teams() {
        let t = token(r#"{"name":"n","scopes":["read"],"team_ids":[]}"#);
        assert_eq!(t.scoped_team_ids(), Some(vec![]));
    }

    #[test]
    fn decodes_null_team_ids_as_unscoped() {
        let t = token(r#"{"name":"n","scopes":["read"],"team_ids":null,"team_id":null}"#);
        assert_eq!(t.scoped_team_ids(), None);
    }

    #[test]
    fn decodes_absent_team_ids_and_falls_back_to_team_id() {
        let t = token(r#"{"name":"n","scopes":["read"],"team_id":9}"#);
        assert_eq!(t.team_ids, None);
        assert_eq!(t.scoped_team_ids(), Some(vec![9]));
        let t = token(r#"{"name":"n","scopes":["read"]}"#);
        assert_eq!(t.scoped_team_ids(), None);
    }
}
