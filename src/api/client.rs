use reqwest::{header, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::error::ApiError;

/// Result of a conditional GET (`If-None-Match`).
#[derive(Debug, PartialEq)]
pub enum Conditional<T> {
    /// 304: the server's copy matches the ETag we sent.
    NotModified,
    /// 200: fresh body plus the response's `ETag` header, if any.
    Modified { data: T, etag: Option<String> },
}

/// One page of a keyset-paged list plus the paging/clock response headers.
#[derive(Debug, PartialEq)]
pub struct Page<T> {
    pub rows: Vec<T>,
    /// `X-Next-After-Id`; absent on the last page.
    pub next_after_id: Option<i64>,
    /// `X-Server-Time`, the server clock for use as the next sync cursor.
    pub server_time: Option<String>,
}

/// Read `X-Next-After-Id` / `X-Server-Time` from response headers.
pub(crate) fn page_meta(headers: &header::HeaderMap) -> (Option<i64>, Option<String>) {
    let get = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    (
        get("x-next-after-id").and_then(|v| v.parse().ok()),
        get("x-server-time"),
    )
}

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
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Ok(Self {
            http,
            base_url,
            token,
        })
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let req = self.http.get(self.url(path));
        self.send_json(req).await
    }

    /// GET a list endpoint. A row that fails to decode (e.g. an unknown enum
    /// value) is skipped with a log line instead of failing the whole list.
    pub async fn get_list<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>, ApiError> {
        let value: serde_json::Value = self.get(path).await?;
        decode_list_lenient(value, path)
    }

    /// GET one page of a list endpoint, with the paging/clock headers
    /// (`X-Next-After-Id`, `X-Server-Time`). Same row-skipping decode.
    pub async fn get_list_page<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<Page<T>, ApiError> {
        let resp = self.dispatch(self.http.get(self.url(path))).await?;
        if !resp.status().is_success() {
            return Err(map_error(resp).await);
        }
        let (next_after_id, server_time) = page_meta(resp.headers());
        let value = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))?;
        Ok(Page {
            rows: decode_list_lenient(value, path)?,
            next_after_id,
            server_time,
        })
    }

    /// GET a list endpoint and also return the raw response headers, for
    /// endpoints whose paging header isn't `X-Next-After-Id` (Activity). Same
    /// row-skipping decode.
    pub async fn get_list_with_headers<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<(Vec<T>, header::HeaderMap), ApiError> {
        let resp = self.dispatch(self.http.get(self.url(path))).await?;
        if !resp.status().is_success() {
            return Err(map_error(resp).await);
        }
        let headers = resp.headers().clone();
        let value = resp
            .json::<serde_json::Value>()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))?;
        Ok((decode_list_lenient(value, path)?, headers))
    }

    /// Conditional variant of [`Self::get_list`]: same row-skipping decode.
    pub async fn get_list_conditional<T: DeserializeOwned>(
        &self,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<Vec<T>>, ApiError> {
        match self
            .get_conditional::<serde_json::Value>(path, etag)
            .await?
        {
            Conditional::NotModified => Ok(Conditional::NotModified),
            Conditional::Modified { data, etag } => Ok(Conditional::Modified {
                data: decode_list_lenient(data, path)?,
                etag,
            }),
        }
    }

    /// GET with `If-None-Match` when `etag` is known. A 304 is "not modified",
    /// not an error; a 200 returns the body and the new `ETag` header.
    pub async fn get_conditional<T: DeserializeOwned>(
        &self,
        path: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<T>, ApiError> {
        let mut req = self.http.get(self.url(path));
        if let Some(tag) = etag {
            req = req.header(header::IF_NONE_MATCH, tag);
        }
        let resp = self.dispatch(req).await?;
        if resp.status() == StatusCode::NOT_MODIFIED {
            return Ok(Conditional::NotModified);
        }
        if !resp.status().is_success() {
            return Err(map_error(resp).await);
        }
        let new_etag = resp
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let data = resp
            .json::<T>()
            .await
            .map_err(|e| ApiError::Decode(e.to_string()))?;
        Ok(Conditional::Modified {
            data,
            etag: new_etag,
        })
    }

    pub async fn post<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let req = self.http.post(self.url(path)).json(body);
        self.send_json(req).await
    }

    /// POST with an extra request header. Used to attach an `Idempotency-Key`
    /// so a re-drained create doesn't produce a duplicate on the server.
    pub async fn post_with_header<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        header_name: &'static str,
        header_value: &str,
    ) -> Result<T, ApiError> {
        let req = self
            .http
            .post(self.url(path))
            .header(header_name, header_value)
            .json(body);
        self.send_json(req).await
    }

    pub async fn put<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let req = self.http.put(self.url(path)).json(body);
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

    /// PUT raw bytes to a storage target (a direct-upload URL). The target is
    /// S3 or a Rails disk URL, not the API: no bearer token and no `Accept`
    /// header, only exactly the headers the server returned. A relative URL
    /// (Rails disk service behind a proxy) is resolved against the base URL.
    /// Any non-2xx is `ApiError::Server` with the status, so callers can tell
    /// an expired target (403) from a bad one.
    pub async fn put_raw(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: Vec<u8>,
    ) -> Result<(), ApiError> {
        let url = if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            self.url(url)
        };
        let mut req = self.http.put(url).body(body);
        for (name, value) in headers {
            req = req.header(name.as_str(), value.as_str());
        }
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let text = resp.text().await.unwrap_or_default();
        Err(ApiError::Server {
            status: status.as_u16(),
            message: if text.is_empty() {
                status.canonical_reason().unwrap_or("error").to_string()
            } else {
                text
            },
        })
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

/// Decode a JSON array row by row. A non-array body is a hard decode error; a
/// row that doesn't decode is logged and skipped so one bad row can't take the
/// whole list down.
pub(crate) fn decode_list_lenient<T: DeserializeOwned>(
    value: serde_json::Value,
    what: &str,
) -> Result<Vec<T>, ApiError> {
    let rows = match value {
        serde_json::Value::Array(rows) => rows,
        other => {
            return Err(ApiError::Decode(format!(
                "{what}: expected a JSON array, got {}",
                match other {
                    serde_json::Value::Object(_) => "an object",
                    serde_json::Value::Null => "null",
                    _ => "a scalar",
                }
            )))
        }
    };
    let mut out = Vec::with_capacity(rows.len());
    for (i, row) in rows.into_iter().enumerate() {
        match serde_json::from_value::<T>(row) {
            Ok(item) => out.push(item),
            Err(e) => eprintln!("skipping undecodable row {i} from {what}: {e}"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_meta_reads_headers_case_insensitively() {
        let mut h = header::HeaderMap::new();
        h.insert("X-Next-After-Id", "42".parse().unwrap());
        h.insert(
            "x-server-time",
            "2026-10-30T12:00:00.123456Z".parse().unwrap(),
        );
        assert_eq!(
            page_meta(&h),
            (Some(42), Some("2026-10-30T12:00:00.123456Z".to_string()))
        );
    }

    #[test]
    fn page_meta_absent_headers_mean_last_page() {
        assert_eq!(page_meta(&header::HeaderMap::new()), (None, None));
        let mut h = header::HeaderMap::new();
        h.insert("x-next-after-id", "junk".parse().unwrap());
        assert_eq!(page_meta(&h).0, None);
    }
    use crate::api::ProjectDto;
    use crate::db::models::ProjectStatus;

    fn project_json(id: i64, status: &str) -> String {
        format!(
            r#"{{"id":{id},"name":"P{id}","description":null,"status":{status},
            "priority":"high","lead":null,"start_date":null,"due_date":null,
            "velocity_score":0,"completed_issues_count":0,"total_issues_count":0,
            "progress_percentage":0,"behind_schedule":false,"roadmap_commitment":null,
            "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}}"#
        )
    }

    #[test]
    fn project_with_null_status_decodes_as_none() {
        let dto: ProjectDto = serde_json::from_str(&project_json(1, "null")).unwrap();
        assert_eq!(dto.status, None);
    }

    #[test]
    fn list_skips_row_with_unknown_enum_value() {
        let body = format!(
            "[{},{},{}]",
            project_json(1, r#""started""#),
            project_json(2, r#""bogus""#),
            project_json(3, "null"),
        );
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        let rows: Vec<ProjectDto> = decode_list_lenient(value, "projects").unwrap();
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        assert_eq!(ids, [1, 3]);
        assert_eq!(rows[0].status, Some(ProjectStatus::Started));
        assert_eq!(rows[1].status, None);
    }

    #[test]
    fn non_array_body_is_a_decode_error() {
        let value = serde_json::json!({"error": "x"});
        let r: Result<Vec<ProjectDto>, _> = decode_list_lenient(value, "projects");
        assert!(matches!(r, Err(ApiError::Decode(_))));
    }
}
