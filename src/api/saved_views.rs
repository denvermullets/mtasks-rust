//! `GET /api/v1/saved_views`: the user's Saved views, cross-team (the server
//! already limits it to the token's teams). Not paged.

use super::{updated_since_query, ApiClient, ApiError, Page};

pub use crate::db::saved_views::SavedViewIn as SavedViewDto;

/// One response of the index with `X-Server-Time`. `updated_since` (ISO8601)
/// limits it to rows changed after that time.
pub async fn list_page(
    client: &ApiClient,
    updated_since: Option<&str>,
) -> Result<Page<SavedViewDto>, ApiError> {
    client.get_list_page(&list_path(updated_since)).await
}

fn list_path(updated_since: Option<&str>) -> String {
    format!("api/v1/saved_views{}", updated_since_query(updated_since))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_path_is_cross_team_with_optional_cursor() {
        assert_eq!(list_path(None), "api/v1/saved_views");
        assert_eq!(
            list_path(Some("2026-10-31T10:00:00.000Z")),
            "api/v1/saved_views?updated_since=2026-10-31T10%3A00%3A00.000Z"
        );
    }
}
