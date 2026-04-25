use reqwest::{header, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::error::ApiError;

#[derive(Debug, Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

#[derive(Debug, Deserialize)]
struct GenericErrorBody {
    error: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ValidationErrorBody {
    #[allow(dead_code)]
    error: Option<String>,
    errors: Vec<String>,
}

impl ApiClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Result<Self, ApiError> {
        let token = token.into();
        if token.trim().is_empty() {
            return Err(ApiError::MissingToken);
        }
        let http = reqwest::Client::builder().build()?;
        let base_url = base_url
            .into()
            .trim_end_matches('/')
            .to_string();
        Ok(Self { http, base_url, token })
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let req = self.http.get(self.url(path));
        self.send_json(req).await
    }

    pub async fn post<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let req = self.http.post(self.url(path)).json(body);
        self.send_json(req).await
    }

    pub async fn patch<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let req = self.http.patch(self.url(path)).json(body);
        self.send_json(req).await
    }

    pub async fn delete(&self, path: &str) -> Result<(), ApiError> {
        let req = self.http.delete(self.url(path));
        let resp = self.dispatch(req).await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(map_error(resp).await)
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    async fn dispatch(&self, req: RequestBuilder) -> Result<Response, ApiError> {
        let resp = req
            .bearer_auth(&self.token)
            .header(header::ACCEPT, "application/json")
            .send()
            .await?;
        Ok(resp)
    }

    async fn send_json<T: DeserializeOwned>(&self, req: RequestBuilder) -> Result<T, ApiError> {
        let resp = self.dispatch(req).await?;
        if resp.status().is_success() {
            resp.json::<T>()
                .await
                .map_err(|e| ApiError::Decode(e.to_string()))
        } else {
            Err(map_error(resp).await)
        }
    }
}

async fn map_error(resp: Response) -> ApiError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();

    match status {
        StatusCode::UNPROCESSABLE_ENTITY => {
            match serde_json::from_str::<ValidationErrorBody>(&body) {
                Ok(v) => ApiError::Validation { errors: v.errors },
                Err(_) => ApiError::Server {
                    status: status.as_u16(),
                    message: if body.is_empty() {
                        "unprocessable entity".to_string()
                    } else {
                        body
                    },
                },
            }
        }
        StatusCode::UNAUTHORIZED => ApiError::Unauthorized(extract_message(&body, "unauthorized")),
        StatusCode::FORBIDDEN => ApiError::Forbidden(extract_message(&body, "forbidden")),
        StatusCode::NOT_FOUND => ApiError::NotFound(extract_message(&body, "not found")),
        s => ApiError::Server {
            status: s.as_u16(),
            message: extract_message(&body, s.canonical_reason().unwrap_or("error")),
        },
    }
}

fn extract_message(body: &str, fallback: &str) -> String {
    if let Ok(g) = serde_json::from_str::<GenericErrorBody>(body) {
        if let Some(m) = g.message {
            return m;
        }
        if let Some(e) = g.error {
            return e;
        }
    }
    if body.is_empty() {
        fallback.to_string()
    } else {
        body.to_string()
    }
}
