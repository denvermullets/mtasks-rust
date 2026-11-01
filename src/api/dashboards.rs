//! `GET /api/v1/dashboards`: the user's Dashboards with their groups, whole
//! (no paging, no `updated_since`).

use super::{ApiClient, ApiError};

pub use crate::db::dashboards::DashboardIn as DashboardDto;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_body_is_a_bare_ids_list() {
        assert_eq!(reorder_body(&[3, 1, 2]), serde_json::json!({"ids": [3, 1, 2]}));
    }

    #[test]
    fn create_body_wraps_name_and_description() {
        assert_eq!(
            create_body("Ops", "d"),
            serde_json::json!({"dashboard": {"name": "Ops", "description": "d"}})
        );
    }
}
