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

fn create_body(name: &str, description: &str) -> serde_json::Value {
    serde_json::json!({ "dashboard": { "name": name, "description": description } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_body_wraps_name_and_description() {
        assert_eq!(
            create_body("Ops", "d"),
            serde_json::json!({"dashboard": {"name": "Ops", "description": "d"}})
        );
    }
}
