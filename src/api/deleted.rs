//! Tombstone feed: `GET /teams/:t/deleted?since=` returns a bare array of
//! `{record_type, record_id, deleted_at}` (no paging). The cursor for the next
//! pull is the response's `X-Server-Time`.

use serde::Deserialize;

use super::{ApiClient, ApiError, Page};

/// One deleted record. `record_type` is kept as a string so entries for types
/// the desktop does not handle yet (Comment, Label, ...) still parse.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TombstoneDto {
    pub record_type: String,
    pub record_id: i64,
    pub deleted_at: Option<String>,
}

pub async fn list(
    client: &ApiClient,
    team_id: i64,
    since: Option<&str>,
) -> Result<Page<TombstoneDto>, ApiError> {
    let mut path = format!("api/v1/teams/{team_id}/deleted");
    if let Some(s) = since {
        path.push_str(&format!("?since={s}"));
    }
    client.get_list_page(&path).await
}

/// `GET /deleted?since=`: the user-level feed (Saved views, dashboards).
pub async fn list_user(
    client: &ApiClient,
    since: Option<&str>,
) -> Result<Page<TombstoneDto>, ApiError> {
    let mut path = "api/v1/deleted".to_string();
    if let Some(s) = since {
        path.push_str(&format!("?since={s}"));
    }
    client.get_list_page(&path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_feed_entries_of_any_record_type() {
        let v = serde_json::json!([
            {"record_type":"Issue","record_id":42,"deleted_at":"2026-10-30T12:00:00.123456Z"},
            {"record_type":"Label","record_id":7,"deleted_at":"2026-10-30T12:00:01.000000Z"}
        ]);
        let rows: Vec<TombstoneDto> = serde_json::from_value(v).unwrap();
        assert_eq!(rows[0].record_type, "Issue");
        assert_eq!(rows[0].record_id, 42);
        assert_eq!(rows[1].record_type, "Label");
    }
}
