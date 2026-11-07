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
    /// The user's sidebar groups and team order; absent when the server does
    /// not report them (the sidebar keeps its ungrouped layout).
    #[serde(default)]
    pub sidebar: Option<SidebarDto>,
}

/// `sidebar` on `GET /me`. Team ids are remote ids.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct SidebarDto {
    #[serde(default)]
    pub groups: Vec<SidebarGroupDto>,
    #[serde(default)]
    pub team_order: SidebarTeamOrderDto,
}

/// One user-defined sidebar group, in display order within `groups`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SidebarGroupDto {
    /// Opaque string id minted by Rails.
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub team_ids: Vec<i64>,
    #[serde(default)]
    pub collapsed: bool,
}

/// Stored team order per scope; teams missing from a list sort after it.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct SidebarTeamOrderDto {
    #[serde(default)]
    pub owned: Vec<i64>,
    #[serde(default)]
    pub joined: Vec<i64>,
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

/// `PATCH /api/v1/me/sidebar_groups/:id {collapsed}`. Needs a write token; a
/// group deleted on the web is a 404. The updated group in the response is not
/// needed: the next `/me` read reports it.
pub async fn set_sidebar_group_collapsed(
    client: &ApiClient,
    group_id: &str,
    collapsed: bool,
) -> Result<(), ApiError> {
    let path = format!("api/v1/me/sidebar_groups/{}", super::encode_query_value(group_id));
    let _: serde_json::Value =
        client.patch(&path, &serde_json::json!({ "collapsed": collapsed })).await?;
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
    fn decodes_the_sidebar_when_present() {
        let m = me(
            r#","sidebar":{"groups":[{"id":"a1b2","name":"Bozo","team_ids":[12,3],"collapsed":true},{"id":"c3"}],"team_order":{"owned":[3,12],"joined":[8,9]}}"#,
        );
        let s = m.sidebar.unwrap();
        assert_eq!(s.groups.len(), 2);
        assert_eq!(s.groups[0].id, "a1b2");
        assert_eq!(s.groups[0].name, "Bozo");
        assert_eq!(s.groups[0].team_ids, vec![12, 3]);
        assert!(s.groups[0].collapsed);
        // Sparse groups default rather than failing the whole /me decode.
        assert_eq!(s.groups[1].team_ids, Vec::<i64>::new());
        assert!(!s.groups[1].collapsed);
        assert_eq!(s.team_order.owned, vec![3, 12]);
        assert_eq!(s.team_order.joined, vec![8, 9]);
    }

    #[test]
    fn decodes_an_absent_sidebar_as_none_and_an_empty_one_as_empty() {
        assert_eq!(me("").sidebar, None);
        assert_eq!(me(r#","sidebar":null"#).sidebar, None);
        let empty = me(r#","sidebar":{"groups":[],"team_order":{"owned":[],"joined":[]}}"#);
        assert_eq!(empty.sidebar, Some(SidebarDto::default()));
        assert_eq!(me(r#","sidebar":{}"#).sidebar, Some(SidebarDto::default()));
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
