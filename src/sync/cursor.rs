//! Reusable pieces of cursor-based incremental pulls: the overlap applied to a
//! stored cursor, and a driver that follows `X-Next-After-Id` paging to the end.

use std::future::Future;

use crate::api::{ApiError, Page};

/// Seconds subtracted from a stored cursor so rows written at the boundary
/// (or by a slightly lagging replica) are re-delivered rather than missed.
/// Callers must apply re-delivered rows idempotently.
pub const CURSOR_OVERLAP_SECS: i64 = 5;

/// Rows per request; Rails' maximum.
pub const PAGE_LIMIT: u32 = 500;

/// Cursor minus the overlap, as an ISO8601 string. An unparseable cursor
/// yields `None`, which callers treat as "no cursor" (full pull).
pub fn since_with_overlap(cursor: &str) -> Option<String> {
    let t = chrono::DateTime::parse_from_rfc3339(cursor).ok()?;
    let t = t.with_timezone(&chrono::Utc) - chrono::Duration::seconds(CURSOR_OVERLAP_SECS);
    Some(t.to_rfc3339_opts(chrono::SecondsFormat::Micros, true))
}

/// All rows of a paged list plus the `X-Server-Time` of the first page (the
/// earliest clock reading, so it is the safe next cursor).
#[derive(Debug)]
pub struct Collected<T> {
    pub rows: Vec<T>,
    pub server_time: Option<String>,
    pub requests: usize,
}

/// Follow keyset pages until `X-Next-After-Id` is absent. `fetch` gets the
/// `after_id` for the page (None for the first). A server that repeats an
/// `after_id` would loop forever, so that is reported as a decode error.
pub async fn collect_pages<T, F, Fut>(mut fetch: F) -> Result<Collected<T>, ApiError>
where
    F: FnMut(Option<i64>) -> Fut,
    Fut: Future<Output = Result<Page<T>, ApiError>>,
{
    let mut rows = Vec::new();
    let mut server_time = None;
    let mut after = None;
    let mut requests = 0;
    loop {
        let page = fetch(after).await?;
        requests += 1;
        if requests == 1 {
            server_time = page.server_time;
        }
        rows.extend(page.rows);
        match page.next_after_id {
            None => break,
            Some(next) if Some(next) == after => {
                return Err(ApiError::Decode(format!(
                    "paging did not advance (after_id={next})"
                )))
            }
            Some(next) => after = Some(next),
        }
    }
    Ok(Collected {
        rows,
        server_time,
        requests,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(rows: Vec<i64>, next: Option<i64>, t: &str) -> Result<Page<i64>, ApiError> {
        Ok(Page {
            rows,
            next_after_id: next,
            server_time: Some(t.to_string()),
        })
    }

    #[test]
    fn overlap_subtracts_seconds() {
        assert_eq!(
            since_with_overlap("2026-10-30T12:00:10.500000Z").unwrap(),
            "2026-10-30T12:00:05.500000Z"
        );
        assert_eq!(since_with_overlap("garbage"), None);
    }

    #[tokio::test]
    async fn follows_pages_to_the_end_and_keeps_first_server_time() {
        let mut seen = Vec::new();
        let got = collect_pages(|after| {
            seen.push(after);
            async move {
                match after {
                    None => page(vec![1, 2], Some(2), "T1"),
                    Some(2) => page(vec![3, 4], Some(4), "T2"),
                    _ => page(vec![5], None, "T3"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(got.rows, vec![1, 2, 3, 4, 5]);
        assert_eq!(got.server_time.as_deref(), Some("T1"));
        assert_eq!(got.requests, 3);
        assert_eq!(seen, vec![None, Some(2), Some(4)]);
    }

    #[tokio::test]
    async fn single_page_without_header_is_one_request() {
        let got = collect_pages(|_| async { page(vec![], None, "T") })
            .await
            .unwrap();
        assert_eq!(got.requests, 1);
    }

    #[tokio::test]
    async fn stuck_cursor_errors_instead_of_looping() {
        let r = collect_pages(|_| async { page(vec![1], Some(7), "T") }).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn page_error_propagates() {
        let r: Result<Collected<i64>, _> =
            collect_pages(|_| async { Err(ApiError::Decode("x".into())) }).await;
        assert!(r.is_err());
    }
}
