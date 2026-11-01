//! `GET /api/v1/dashboards`: the user's Dashboards with their groups, whole
//! (no paging, no `updated_since`).

use super::{ApiClient, ApiError};

use crate::db::dashboards::{GroupSource, SourceType};

pub use crate::db::dashboards::DashboardIn as DashboardDto;
pub use crate::db::dashboards::GroupIn as GroupDto;

/// The full list.
pub async fn list(client: &ApiClient) -> Result<Vec<DashboardDto>, ApiError> {
    let page = client.get_list_page::<DashboardDto>("api/v1/dashboards").await?;
    Ok(page.rows)
}

/// `POST /api/v1/dashboards` (201): the created dashboard, groups empty.
pub async fn create(
    client: &ApiClient,
    name: &str,
    description: &str,
) -> Result<DashboardDto, ApiError> {
    client.post("api/v1/dashboards", &create_body(name, description)).await
}

/// `PATCH /api/v1/dashboards/:id`: name and description.
pub async fn update(
    client: &ApiClient,
    id: i64,
    name: &str,
    description: &str,
) -> Result<DashboardDto, ApiError> {
    client.patch(&format!("api/v1/dashboards/{id}"), &create_body(name, description)).await
}

/// `DELETE /api/v1/dashboards/:id` (cascades to its groups on the server).
pub async fn delete(client: &ApiClient, id: i64) -> Result<(), ApiError> {
    client.delete(&format!("api/v1/dashboards/{id}")).await
}

/// `DELETE /api/v1/dashboards/:dashboard_id/groups/:id`.
pub async fn delete_group(client: &ApiClient, dashboard_remote_id: i64, id: i64) -> Result<(), ApiError> {
    client.delete(&format!("api/v1/dashboards/{dashboard_remote_id}/groups/{id}")).await
}


/// `PATCH /api/v1/dashboards/:dashboard_id/groups/reorder`: every group id in
/// display order. Returns the dashboard's groups in the order the server applied.
pub async fn reorder_groups(
    client: &ApiClient,
    dashboard_id: i64,
    group_ids: &[i64],
) -> Result<Vec<crate::db::dashboards::GroupIn>, ApiError> {
    client
        .patch(
            &format!("api/v1/dashboards/{dashboard_id}/groups/reorder"),
            &reorder_body(group_ids),
        )
        .await
}

fn reorder_body(group_ids: &[i64]) -> serde_json::Value {
    serde_json::json!({ "ids": group_ids })
}

fn create_body(name: &str, description: &str) -> serde_json::Value {
    serde_json::json!({ "dashboard": { "name": name, "description": description } })
}

/// `POST /api/v1/dashboards/:dashboard_id/groups` (201): the created group in
/// list shape. The server silently drops source ids the token can't reach, so
/// the caller adopts the returned sources.
pub async fn create_group(
    client: &ApiClient,
    dashboard_remote_id: i64,
    name: &str,
    description: &str,
    color: &str,
    sources: &[GroupSource],
) -> Result<GroupDto, ApiError> {
    client
        .post(
            &format!("api/v1/dashboards/{dashboard_remote_id}/groups"),
            &group_body(name, description, color, sources),
        )
        .await
}

/// `PATCH /api/v1/dashboards/:dashboard_id/groups/:id`: every field and all
/// source lists (a full replace). Returns the group in list shape; the caller
/// adopts the returned sources.
pub async fn update_group(
    client: &ApiClient,
    dashboard_remote_id: i64,
    id: i64,
    name: &str,
    description: &str,
    color: &str,
    sources: &[GroupSource],
) -> Result<GroupDto, ApiError> {
    client
        .patch(
            &format!("api/v1/dashboards/{dashboard_remote_id}/groups/{id}"),
            &group_body(name, description, color, sources),
        )
        .await
}

/// `{dashboard_group:{..}}` with the five id lists, as remote ids. Shared by
/// create and edit. `all_*` lists carry only ids that are also sources.
pub fn group_body(
    name: &str,
    description: &str,
    color: &str,
    sources: &[GroupSource],
) -> serde_json::Value {
    let ids = |t: SourceType| -> Vec<i64> {
        sources.iter().filter(|s| s.source_type == t).map(|s| s.remote_id).collect()
    };
    let all = |t: SourceType| -> Vec<i64> {
        sources
            .iter()
            .filter(|s| s.source_type == t && s.include_all)
            .map(|s| s.remote_id)
            .collect()
    };
    serde_json::json!({ "dashboard_group": {
        "name": name,
        "description": description,
        "color": color,
        "team_ids": ids(SourceType::Team),
        "project_ids": ids(SourceType::Project),
        "label_ids": ids(SourceType::Label),
        "all_team_ids": all(SourceType::Team),
        "all_project_ids": all(SourceType::Project),
    }})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_body_is_a_bare_ids_list() {
        assert_eq!(reorder_body(&[3, 1, 2]), serde_json::json!({"ids": [3, 1, 2]}));
    }

    #[test]
    fn group_body_sends_remote_ids_and_all_lists() {
        let src = |t, id, all| GroupSource { source_type: t, remote_id: id, include_all: all };
        let body = group_body(
            "G",
            "d",
            "#ec4899",
            &[
                src(SourceType::Team, 1, true),
                src(SourceType::Team, 2, false),
                src(SourceType::Project, 5, true),
                src(SourceType::Label, 7, false),
            ],
        );
        assert_eq!(
            body,
            serde_json::json!({"dashboard_group": {
                "name": "G", "description": "d", "color": "#ec4899",
                "team_ids": [1, 2], "project_ids": [5], "label_ids": [7],
                "all_team_ids": [1], "all_project_ids": [5]
            }})
        );
    }

    #[test]
    fn create_body_wraps_name_and_description() {
        assert_eq!(
            create_body("Ops", "d"),
            serde_json::json!({"dashboard": {"name": "Ops", "description": "d"}})
        );
    }
}
