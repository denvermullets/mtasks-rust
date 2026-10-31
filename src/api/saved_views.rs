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

/// `POST /api/v1/teams/:team_id/saved_views` (201). `query` is the Rails query
/// map (string values).
pub async fn create(
    client: &ApiClient,
    team_id: i64,
    name: &str,
    query: &serde_json::Value,
) -> Result<SavedViewDto, ApiError> {
    client
        .post(&create_path(team_id), &create_body(name, query))
        .await
}

fn create_path(team_id: i64) -> String {
    format!("api/v1/teams/{team_id}/saved_views")
}

fn create_body(name: &str, query: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "saved_view": { "name": name, "query": query } })
}

/// `DELETE /api/v1/saved_views/:id` (204).
pub async fn delete(client: &ApiClient, id: i64) -> Result<(), ApiError> {
    client.delete(&delete_path(id)).await
}

/// `PATCH /api/v1/saved_views/:id {saved_view:{query}}`: returns the view as
/// the server now has it. `query` goes out as a JSON object.
pub async fn update_query(
    client: &ApiClient,
    id: i64,
    query: &serde_json::Value,
) -> Result<SavedViewDto, ApiError> {
    client.patch(&delete_path(id), &update_body(query)).await
}

/// `PATCH /api/v1/saved_views/:id {saved_view:{name}}`: renames only; the
/// query is not sent.
pub async fn update_name(
    client: &ApiClient,
    id: i64,
    name: &str,
) -> Result<SavedViewDto, ApiError> {
    client.patch(&delete_path(id), &rename_body(name)).await
}

fn update_body(query: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "saved_view": { "query": query } })
}

fn rename_body(name: &str) -> serde_json::Value {
    serde_json::json!({ "saved_view": { "name": name } })
}

fn delete_path(id: i64) -> String {
    format!("api/v1/saved_views/{id}")
}

fn list_path(updated_since: Option<&str>) -> String {
    format!("api/v1/saved_views{}", updated_since_query(updated_since))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_body_sends_the_query_as_an_object() {
        let q = serde_json::json!({"view_mode":"list","lane_ids":"1,2"});
        assert_eq!(
            update_body(&q),
            serde_json::json!({"saved_view":{"query":{"view_mode":"list","lane_ids":"1,2"}}})
        );
    }

    #[test]
    fn rename_body_sends_only_the_name() {
        assert_eq!(
            rename_body("New"),
            serde_json::json!({"saved_view":{"name":"New"}})
        );
    }

    #[test]
    fn delete_path_names_the_view() {
        assert_eq!(create_path(7), "api/v1/teams/7/saved_views");
        assert_eq!(
            create_body("A", &serde_json::json!({"q": "x"})),
            serde_json::json!({"saved_view": {"name": "A", "query": {"q": "x"}}})
        );
        assert_eq!(delete_path(5), "api/v1/saved_views/5");
    }

    #[test]
    fn list_path_is_cross_team_with_optional_cursor() {
        assert_eq!(list_path(None), "api/v1/saved_views");
        assert_eq!(
            list_path(Some("2026-10-31T10:00:00.000Z")),
            "api/v1/saved_views?updated_since=2026-10-31T10%3A00%3A00.000Z"
        );
    }
}
