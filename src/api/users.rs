use serde::Deserialize;

use super::{ApiClient, ApiError};

/// Response shape from `GET /api/v1/me`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MeDto {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub token: MeTokenDto,
    /// The user's Home page; null or absent when none is set (or its target is
    /// hidden from this token).
    #[serde(default)]
    pub home_page: Option<HomePageDto>,
}

/// `{kind, id}` of the Home page target. `kind` is `team`, `view` or
/// `dashboard`; `id` is the target's remote id.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HomePageDto {
    pub kind: String,
    pub id: i64,
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

/// `PUT /api/v1/me/home_page {kind, id}`. The response is not needed; the
/// next `/me` read reports the stored value.
pub async fn set_home_page(client: &ApiClient, kind: &str, id: i64) -> Result<(), ApiError> {
    let _: serde_json::Value = client
        .put("api/v1/me/home_page", &serde_json::json!({ "kind": kind, "id": id }))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(json: &str) -> MeTokenDto {
        serde_json::from_str(json).unwrap()
    }

    fn me(extra: &str) -> MeDto {
        serde_json::from_str(&format!(
            r#"{{"id":1,"name":"n","email":"e","token":{{"name":"t","scopes":[]}}{extra}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn decodes_home_page_kind_and_id() {
        for kind in ["team", "view", "dashboard"] {
            let m = me(&format!(r#","home_page":{{"kind":"{kind}","id":42}}"#));
            assert_eq!(m.home_page, Some(HomePageDto { kind: kind.into(), id: 42 }));
        }
    }

    #[test]
    fn decodes_null_or_absent_home_page_as_none() {
        assert_eq!(me(r#","home_page":null"#).home_page, None);
        assert_eq!(me("").home_page, None);
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
