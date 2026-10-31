use serde::Serialize;

use super::{ApiClient, ApiError, Conditional, LabelDto};

#[derive(Debug, Default, Clone, Serialize)]
pub struct LabelParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Debug, Serialize)]
struct LabelWrap<'a> {
    label: &'a LabelParams,
}

/// Conditional list: pass the stored ETag; a 304 yields `NotModified`.
pub async fn list_conditional(
    client: &ApiClient,
    team_id: i64,
    etag: Option<&str>,
) -> Result<Conditional<Vec<LabelDto>>, ApiError> {
    let path = format!("api/v1/teams/{team_id}/labels");
    client.get_list_conditional(&path, etag).await
}

pub async fn create(
    client: &ApiClient,
    team_id: i64,
    params: &LabelParams,
) -> Result<LabelDto, ApiError> {
    let path = format!("api/v1/teams/{team_id}/labels");
    client.post(&path, &LabelWrap { label: params }).await
}

pub async fn update(
    client: &ApiClient,
    team_id: i64,
    id: i64,
    params: &LabelParams,
) -> Result<LabelDto, ApiError> {
    let path = format!("api/v1/teams/{team_id}/labels/{id}");
    client.patch(&path, &LabelWrap { label: params }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_body_carries_name_and_color() {
        let params = LabelParams {
            name: "defect".into(),
            color: Some("#3b82f6".into()),
        };
        let body = serde_json::to_value(LabelWrap { label: &params }).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "label": { "name": "defect", "color": "#3b82f6" } })
        );
    }

    #[test]
    fn label_params_wrap_under_label_key() {
        let params = LabelParams {
            name: "bug".into(),
            color: Some("#ff0000".into()),
        };
        let body = serde_json::to_value(LabelWrap { label: &params }).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "label": { "name": "bug", "color": "#ff0000" } })
        );
    }

    #[test]
    fn label_params_skip_color_when_none() {
        let params = LabelParams {
            name: "bug".into(),
            color: None,
        };
        let body = serde_json::to_value(LabelWrap { label: &params }).unwrap();
        assert_eq!(body, serde_json::json!({ "label": { "name": "bug" } }));
    }
}
