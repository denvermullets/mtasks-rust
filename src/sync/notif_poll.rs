//! Notification poll: the driver-side hook for the Notifications cadence.
//!
//! [`poll`] fetches the feed from Rails and applies it to the local mirror
//! (`db::notifications::apply_poll`). Request planning and response
//! application are plain functions over a `Connection` so they unit-test
//! without HTTP.

use rusqlite::Connection;

use crate::api::notifications::{self as notif_api, NotificationsEnvelope};
use crate::api::{ApiClient, ApiError, Conditional};
use crate::db::notifications::{self as mirror, Notification};
use crate::db::{connection, TS_FORMAT};
use crate::sync::cursor::since_with_overlap;

/// What a completed poll hands to `App`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PollReport {
    /// Rows new to the mirror and unread (envelope order). Banner input.
    pub new_unread: Vec<Notification>,
    /// True when this was the cursor-less backfill poll (first poll; banners
    /// must not fire for these rows).
    pub backfill: bool,
    /// Unread count after the poll (server's on a 200, stored on a 304).
    pub unread_count: i64,
    /// False on a 304 (mirror untouched).
    pub changed: bool,
}

/// How a poll went. The driver maps this onto `CycleFinished` / auth pause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Polled(PollReport),
    /// 401: goes down the same auth-pause path as a Data sync 401.
    AuthFailed,
    Failed(String),
}

impl PollOutcome {
    pub fn succeeded(&self) -> bool {
        matches!(self, PollOutcome::Polled(_))
    }

    pub fn auth_failed(&self) -> bool {
        matches!(self, PollOutcome::AuthFailed)
    }

    /// Classify an API error: 401 pauses both cadences, anything else backs off.
    pub fn from_error(e: &ApiError) -> Self {
        match e {
            ApiError::Unauthorized(_) => PollOutcome::AuthFailed,
            other => PollOutcome::Failed(other.to_string()),
        }
    }
}

/// Query for the next poll, derived from the mirror's bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollRequest {
    pub updated_since: Option<String>,
    pub etag: Option<String>,
    pub backfill: bool,
}

/// No usable cursor: backfill (the server's default 50 newest, no ETag).
/// Otherwise `updated_since` = cursor minus the overlap, plus the stored ETag.
pub fn plan(conn: &Connection) -> rusqlite::Result<PollRequest> {
    let since = mirror::cursor(conn)?.and_then(|c| since_with_overlap(&c));
    if since.is_none() {
        return Ok(PollRequest {
            updated_since: None,
            etag: None,
            backfill: true,
        });
    }
    Ok(PollRequest {
        updated_since: since,
        etag: mirror::etag(conn)?,
        backfill: false,
    })
}

/// Apply a response to the mirror. 304 leaves it untouched; 200 goes through
/// `apply_poll` and stores the new ETag.
pub fn apply_response(
    conn: &Connection,
    resp: Conditional<NotificationsEnvelope>,
    backfill: bool,
    now: &str,
) -> rusqlite::Result<PollReport> {
    match resp {
        Conditional::NotModified => Ok(PollReport {
            new_unread: Vec::new(),
            backfill,
            unread_count: mirror::unread_count(conn)?,
            changed: false,
        }),
        Conditional::Modified { data, etag } => {
            let new_unread = mirror::apply_poll(conn, &data, now)?;
            mirror::prune(conn, now)?;
            if let Some(tag) = etag {
                mirror::set_etag(conn, &tag)?;
            }
            Ok(PollReport {
                new_unread,
                backfill,
                unread_count: mirror::unread_count(conn)?,
                changed: true,
            })
        }
    }
}

fn db_err(e: impl std::fmt::Display) -> PollOutcome {
    PollOutcome::Failed(format!("notification mirror: {e}"))
}

/// Run one Notification poll against the real DB and Rails.
pub async fn poll(client: &ApiClient) -> PollOutcome {
    let req = match connection::open()
        .map_err(db_err)
        .and_then(|c| plan(&c).map_err(db_err))
    {
        Ok(r) => r,
        Err(o) => return o,
    };
    let resp = match notif_api::list(client, req.updated_since.as_deref(), req.etag.as_deref())
        .await
    {
        Ok(r) => r,
        Err(e) => return PollOutcome::from_error(&e),
    };
    let now = chrono::Utc::now().format(TS_FORMAT).to_string();
    match connection::open()
        .map_err(db_err)
        .and_then(|c| apply_response(&c, resp, req.backfill, &now).map_err(db_err))
    {
        Ok(report) => PollOutcome::Polled(report),
        Err(o) => o,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    fn envelope(ids: &[(i64, Option<&str>)], unread: i64, server_time: &str) -> NotificationsEnvelope {
        let rows: Vec<String> = ids
            .iter()
            .map(|(id, read)| {
                let read = read.map(|r| format!("\"{r}\"")).unwrap_or("null".into());
                format!(
                    r#"{{"id":{id},"action":"mention","message":"m","read_at":{read},
                    "created_at":"2026-10-30T10:00:00Z","updated_at":"2026-10-30T10:00:00Z",
                    "actor":{{"id":1,"name":"A"}},
                    "issue":{{"id":5,"identifier":"X-1","title":"t","team_id":2}},
                    "comment_id":null}}"#
                )
            })
            .collect();
        serde_json::from_str(&format!(
            r#"{{"notifications":[{}],"meta":{{"unread_count":{unread},"server_time":"{server_time}"}}}}"#,
            rows.join(",")
        ))
        .unwrap()
    }

    #[test]
    fn unauthorized_pauses_other_errors_back_off() {
        let auth = PollOutcome::from_error(&ApiError::Unauthorized("x".into()));
        assert!(auth.auth_failed() && !auth.succeeded());
        let other = PollOutcome::from_error(&ApiError::NotFound("x".into()));
        assert!(!other.auth_failed() && !other.succeeded());
        assert!(PollOutcome::Polled(PollReport::default()).succeeded());
    }

    #[test]
    fn first_poll_is_backfill_with_no_cursor_or_etag() {
        let conn = mem();
        let req = plan(&conn).unwrap();
        assert!(req.backfill && req.updated_since.is_none() && req.etag.is_none());
    }

    #[test]
    fn backfill_apply_stores_rows_etag_and_reports_unread() {
        let conn = mem();
        let env = envelope(&[(2, None), (1, Some("2026-10-30T11:00:00Z"))], 1, "2026-10-30T12:00:00.000Z");
        let r = apply_response(
            &conn,
            Conditional::Modified { data: env, etag: Some("W/\"abc\"".into()) },
            true,
            "2026-10-30T12:00:01.000Z",
        )
        .unwrap();
        assert!(r.backfill && r.changed);
        assert_eq!(r.unread_count, 1);
        assert_eq!(r.new_unread.len(), 1);
        assert_eq!(r.new_unread[0].remote_id, 2);
        assert_eq!(mirror::etag(&conn).unwrap().as_deref(), Some("W/\"abc\""));

        // Next poll: cursor minus 5s overlap, ETag sent, not a backfill.
        let next = plan(&conn).unwrap();
        assert!(!next.backfill);
        assert_eq!(next.etag.as_deref(), Some("W/\"abc\""));
        assert_eq!(next.updated_since.as_deref(), Some("2026-10-30T11:59:55.000000Z"));
    }

    #[test]
    fn not_modified_leaves_mirror_untouched() {
        let conn = mem();
        let env = envelope(&[(2, None)], 1, "2026-10-30T12:00:00.000Z");
        apply_response(&conn, Conditional::Modified { data: env, etag: None }, true, "2026-10-30T12:00:01.000Z").unwrap();
        let before = mirror::cursor(&conn).unwrap();
        let r = apply_response(&conn, Conditional::NotModified, false, "2026-10-30T12:00:02.000Z").unwrap();
        assert!(!r.changed && r.new_unread.is_empty() && !r.backfill);
        assert_eq!(r.unread_count, 1);
        assert_eq!(mirror::cursor(&conn).unwrap(), before);
    }

    #[test]
    fn redelivered_and_read_rows_update_without_reporting_new() {
        let conn = mem();
        let first = envelope(&[(2, None)], 1, "2026-10-30T12:00:00.000Z");
        apply_response(&conn, Conditional::Modified { data: first, etag: None }, true, "2026-10-30T12:00:01.000Z").unwrap();
        // Read on the web: same row, read_at set, count drops.
        let second = envelope(&[(2, Some("2026-10-30T12:05:00Z"))], 0, "2026-10-30T12:05:01.000Z");
        let r = apply_response(&conn, Conditional::Modified { data: second, etag: None }, false, "2026-10-30T12:05:02.000Z").unwrap();
        assert!(r.new_unread.is_empty() && !r.backfill);
        assert_eq!(r.unread_count, 0);
        let rows = mirror::inbox_list(&conn, mirror::Filter::All).unwrap();
        assert_eq!(rows[0].read_at.as_deref(), Some("2026-10-30T12:05:00Z"));
    }
}
