//! `GET /api/v1/dashboards`: the user's Dashboards with their groups, whole
//! (no paging, no `updated_since`).

use super::{ApiClient, ApiError};

pub use crate::db::dashboards::DashboardIn as DashboardDto;

/// The full list.
pub async fn list(client: &ApiClient) -> Result<Vec<DashboardDto>, ApiError> {
    let page = client.get_list_page::<DashboardDto>("api/v1/dashboards").await?;
    Ok(page.rows)
}
