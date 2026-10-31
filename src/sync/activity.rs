//! Issue Activity fetch: pulls an issue's server Activity into the local cache
//! (`db::activity`).
//!
//! Shape, so the follow-up tickets slot in without reshaping it:
//!
//! 1. [`resolve`] reads the target (remote ids, team in the URL, stored
//!    [`FetchState`]) from the DB. Local-only issues resolve to `None`.
//! 2. [`plan`] (pure) turns the stored state into a [`Plan`]. Only
//!    [`Plan::FullHistory`] exists so far; JRST-148 adds an incremental variant.
//! 3. [`execute`] runs the plan over the network and returns a [`Fetched`]
//!    batch. It touches no DB, so an `Err` leaves the cache untouched
//!    (JRST-147). JRST-146's restart-once wraps the `FullHistory` arm.
//! 4. [`commit`] writes the batch via `db::activity::store_batch` in one
//!    transaction.
//!
//! The paging decisions ([`first_cursor`], [`advance`]) are pure; the network
//! loop ([`collect_history`]) only feeds them pages.

// Unused until the detail view triggers a fetch (JRST-149); drop then.
#![allow(dead_code)]

use std::future::Future;

use rusqlite::{params, Connection, OptionalExtension};

use crate::api::activity::{self as api, ActivityEntry, ActivityPage, ActivityParams, MAX_LIMIT};
use crate::api::{ApiClient, ApiError};
use crate::db::activity::{self as store, FetchState};
use crate::db::connection;
use crate::sync::cursor::since_with_overlap;

/// Everything needed to fetch one issue, resolved from the local DB.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// Local issue id (key of the cache tables).
    pub issue_id: String,
    pub issue_remote_id: i64,
    /// Remote id of the issue's *current* team (what the URL needs).
    pub team_remote_id: i64,
    pub state: FetchState,
}

/// What a fetch should do for a target, decided from its stored state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// No full-history flag: page back from the newest entry to the start.
    FullHistory,
    /// History is complete: ask only for entries created after the stored
    /// cursor minus the overlap (`since` is that ISO8601 string). Re-delivered
    /// entries upsert by entry cursor, so the overlap is harmless.
    Incremental { since: String },
}

/// A fetched batch, ready for [`commit`]. Nothing is stored until it exists.
#[derive(Debug, Clone, PartialEq)]
pub struct Fetched {
    /// Oldest first.
    pub entries: Vec<ActivityEntry>,
    /// `X-Server-Time` to store as the issue's cursor.
    pub cursor: String,
    /// Whether the batch completes the issue's history.
    pub fully_fetched: bool,
}

/// How a fetch ended. The cache is only changed on `Fetched`.
#[derive(Debug)]
pub enum FetchOutcome {
    /// Stored this many entries.
    Fetched { stored: usize },
    /// Local-only issue/team (no remote id): nothing to ask the server.
    Skipped,
    Failed(String),
}

// ---- pure decisions -------------------------------------------------------

/// Pick the plan for a target's stored state.
/// Incremental needs the full-history flag *and* a usable cursor; anything
/// else (never fetched, interrupted first fetch, unparseable cursor) pages the
/// whole history again.
pub fn plan(state: &FetchState) -> Plan {
    if state.fully_fetched {
        if let Some(since) = state.cursor.as_deref().and_then(since_with_overlap) {
            return Plan::Incremental { since };
        }
    }
    Plan::FullHistory
}

/// The cursor to store after a paged fetch: the *first* page's server time (the
/// earliest clock reading, so nothing written during paging can fall in a gap).
/// A first page without `X-Server-Time` is an error: a full-history flag with no
/// cursor could not resume.
pub fn first_cursor(first_page: &ActivityPage) -> Result<String, ApiError> {
    first_page
        .server_time
        .clone()
        .ok_or_else(|| ApiError::Decode("activity response had no X-Server-Time".into()))
}

/// What to do after a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Request the next older page with this `before_id`.
    Next(String),
    /// No older entries remain.
    Done,
}

/// Decide the next step from a page's `X-Next-Before-Id`. `requested` is the
/// `before_id` this page was fetched with. A server that hands back the same
/// cursor would loop forever, so that is an error.
pub fn advance(requested: Option<&str>, page: &ActivityPage) -> Result<Step, ApiError> {
    match page.next_before_id.as_deref() {
        None => Ok(Step::Done),
        Some(next) if Some(next) == requested => Err(ApiError::Decode(format!(
            "activity paging did not advance (before_id={next})"
        ))),
        Some(next) => Ok(Step::Next(next.to_string())),
    }
}

/// Join pages (collected newest page first, each oldest-first) into one
/// oldest-first list.
pub fn join_pages(pages_newest_first: Vec<Vec<ActivityEntry>>) -> Vec<ActivityEntry> {
    pages_newest_first.into_iter().rev().flatten().collect()
}

// ---- network --------------------------------------------------------------

/// Page backwards from the newest entry until `X-Next-Before-Id` is absent.
/// `fetch_page` gets the `before_id` (None for the first page). All-or-nothing:
/// any page error is returned and the pages so far are dropped.
pub async fn collect_history<F, Fut>(fetch_page: F) -> Result<Fetched, ApiError>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<ActivityPage, ApiError>>,
{
    collect_paged(fetch_page, true).await
}

/// Page backwards like [`collect_history`] for an incremental fetch (the
/// request carries `since`, so "no older entries" means "caught up", not
/// "start of history"). The batch does not claim to complete the history.
pub async fn collect_since<F, Fut>(fetch_page: F) -> Result<Fetched, ApiError>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<ActivityPage, ApiError>>,
{
    collect_paged(fetch_page, false).await
}

/// The shared paging loop behind [`collect_history`] and [`collect_since`].
async fn collect_paged<F, Fut>(mut fetch_page: F, fully_fetched: bool) -> Result<Fetched, ApiError>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<ActivityPage, ApiError>>,
{
    let mut pages = Vec::new();
    let mut cursor = None;
    let mut before: Option<String> = None;
    loop {
        let page = fetch_page(before.clone()).await?;
        if cursor.is_none() {
            cursor = Some(first_cursor(&page)?);
        }
        let step = advance(before.as_deref(), &page)?;
        pages.push(page.entries);
        match step {
            Step::Done => break,
            Step::Next(b) => before = Some(b),
        }
    }
    Ok(Fetched {
        entries: join_pages(pages),
        cursor: cursor.expect("set on the first page"),
        fully_fetched,
    })
}

/// [`collect_history`], restarted from scratch once if a `before_id` request
/// 404s (the cursor entry vanished mid-walk). A 404 on the first page (no
/// `before_id`) is the issue itself missing and is not retried; a second
/// failure of any kind is returned as-is.
pub async fn collect_history_restarting<F, Fut>(mut fetch_page: F) -> Result<Fetched, ApiError>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<ActivityPage, ApiError>>,
{
    let paged = std::cell::Cell::new(false);
    let first = collect_history(|before| {
        paged.set(before.is_some());
        fetch_page(before)
    })
    .await;
    match first {
        Err(ApiError::NotFound(_)) if paged.get() => collect_history(fetch_page).await,
        other => other,
    }
}

/// Run a plan against the server. No DB access.
pub async fn execute(
    client: &ApiClient,
    target: &Target,
    plan: &Plan,
    limit: u32,
) -> Result<Fetched, ApiError> {
    match plan {
        Plan::FullHistory => {
            collect_history_restarting(|before| async move {
                let params = ActivityParams {
                    before_id: before.as_deref(),
                    since: None,
                    limit: Some(limit),
                };
                api::list(client, target.team_remote_id, target.issue_remote_id, &params).await
            })
            .await
        }
        Plan::Incremental { since } => {
            collect_since(|before| async move {
                let params = ActivityParams {
                    before_id: before.as_deref(),
                    since: Some(since.as_str()),
                    limit: Some(limit),
                };
                api::list(client, target.team_remote_id, target.issue_remote_id, &params).await
            })
            .await
        }
    }
}

// ---- db -------------------------------------------------------------------

/// Resolve the fetch target. `None` when the issue is missing/deleted or it or
/// its current team has no remote id.
pub fn resolve(conn: &Connection, issue_id: &str) -> rusqlite::Result<Option<Target>> {
    let ids: Option<(Option<i64>, Option<i64>)> = conn
        .query_row(
            "SELECT i.remote_id, t.remote_id
             FROM issues i JOIN teams t ON t.id = i.team_id
             WHERE i.id = ?1 AND i.deleted_at IS NULL",
            params![issue_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((Some(issue_remote_id), Some(team_remote_id))) = ids else {
        return Ok(None);
    };
    Ok(Some(Target {
        issue_id: issue_id.to_string(),
        issue_remote_id,
        team_remote_id,
        state: store::fetch_state(conn, issue_id)?,
    }))
}

/// Store a fetched batch and its fetch state in one transaction.
/// Settled Pending changes are pruned with it, treating the fetch as started now.
pub fn commit(conn: &Connection, issue_id: &str, fetched: &Fetched) -> rusqlite::Result<()> {
    commit_at(conn, issue_id, fetched, &crate::db::now_iso())
}

/// [`commit`] given the local time the fetch began (`db::now_iso()` format), so a
/// push that landed while the request was in flight is not pruned.
pub fn commit_at(
    conn: &Connection,
    issue_id: &str,
    fetched: &Fetched,
    fetch_started_at: &str,
) -> rusqlite::Result<()> {
    store::store_batch_at(
        conn,
        issue_id,
        &fetched.entries,
        Some(&fetched.cursor),
        fetched.fully_fetched,
        fetch_started_at,
    )
}

/// Fetch one issue's Activity into the local cache (production entry point).
pub async fn fetch(client: &ApiClient, issue_id: &str) -> FetchOutcome {
    fetch_with(client, issue_id, MAX_LIMIT, connection::open).await
}

/// [`fetch`] with an explicit page size (tests force paging with a small one)
/// and DB opener. Connections are opened per step, not held across awaits.
pub async fn fetch_with<O>(client: &ApiClient, issue_id: &str, limit: u32, open: O) -> FetchOutcome
where
    O: Fn() -> rusqlite::Result<Connection>,
{
    let target = match open().and_then(|c| resolve(&c, issue_id)) {
        Ok(Some(t)) => t,
        Ok(None) => return FetchOutcome::Skipped,
        Err(e) => return FetchOutcome::Failed(e.to_string()),
    };
    let started_at = crate::db::now_iso();
    let fetched = match execute(client, &target, &plan(&target.state), limit).await {
        Ok(f) => f,
        Err(e) => return FetchOutcome::Failed(e.to_string()),
    };
    match open().and_then(|c| commit_at(&c, issue_id, &fetched, &started_at)) {
        Ok(()) => FetchOutcome::Fetched {
            stored: fetched.entries.len(),
        },
        Err(e) => FetchOutcome::Failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use std::cell::RefCell;

    fn entry(id: i64) -> ActivityEntry {
        serde_json::from_str(&format!(
            r#"{{"type":"version","id":{id},"cursor":"version:{id}","created_at":"2026-07-06T00:00:{id:02}Z",
                "actor":null,"event":"update","changes":[],"description":"d"}}"#
        ))
        .unwrap()
    }

    fn page(ids: &[i64], next: Option<&str>, time: Option<&str>) -> ActivityPage {
        ActivityPage {
            entries: ids.iter().map(|i| entry(*i)).collect(),
            next_before_id: next.map(String::from),
            server_time: time.map(String::from),
        }
    }

    fn cursors(entries: &[ActivityEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| match e {
                ActivityEntry::Version(v) => v.cursor.clone(),
                ActivityEntry::Reference(r) => r.cursor.clone(),
                ActivityEntry::Dependency(d) => d.cursor.clone(),
            })
            .collect()
    }

    #[test]
    fn plan_without_flag_is_full_history() {
        assert_eq!(plan(&FetchState::default()), Plan::FullHistory);
    }

    fn state(cursor: Option<&str>, fully: bool) -> FetchState {
        FetchState { cursor: cursor.map(String::from), fully_fetched: fully }
    }

    #[test]
    fn plan_with_flag_and_cursor_is_incremental_with_overlap() {
        assert_eq!(
            plan(&state(Some("2026-07-06T00:00:10.000000Z"), true)),
            Plan::Incremental { since: "2026-07-06T00:00:05.000000Z".into() }
        );
    }

    #[test]
    fn plan_without_flag_or_usable_cursor_is_full_history() {
        assert_eq!(plan(&state(Some("2026-07-06T00:00:10Z"), false)), Plan::FullHistory);
        assert_eq!(plan(&state(None, true)), Plan::FullHistory);
        assert_eq!(plan(&state(Some("garbage"), true)), Plan::FullHistory);
    }

    #[tokio::test]
    async fn collect_since_pages_but_does_not_claim_full_history() {
        let f = collect_since(|before| {
            let p = match before.as_deref() {
                None => page(&[5, 6], Some("version:5"), Some("T-first")),
                _ => page(&[3, 4], None, Some("T-second")),
            };
            async move { Ok(p) }
        })
        .await
        .unwrap();
        assert_eq!((f.entries.len(), f.cursor.as_str(), f.fully_fetched), (4, "T-first", false));
    }

    #[test]
    fn incremental_commit_keeps_the_flag_advances_the_cursor_and_dedupes_overlap() {
        let conn = mem();
        let full = Fetched { entries: vec![entry(1), entry(2)], cursor: "2026-07-06T00:00:10.000000Z".into(), fully_fetched: true };
        commit(&conn, "synced", &full).unwrap();
        // Overlap re-delivers entry 2; entry 3 is new.
        let inc = Fetched { entries: vec![entry(2), entry(3)], cursor: "2026-07-06T00:00:20.000000Z".into(), fully_fetched: false };
        commit(&conn, "synced", &inc).unwrap();
        assert_eq!(store::entries_for_issue(&conn, "synced").unwrap().len(), 3);
        let st = store::fetch_state(&conn, "synced").unwrap();
        assert_eq!(st.cursor.as_deref(), Some("2026-07-06T00:00:20.000000Z"));
        assert!(st.fully_fetched);
    }

    #[test]
    fn advance_follows_header_and_stops_when_absent() {
        let p = page(&[1], Some("version:1"), None);
        assert_eq!(advance(None, &p).unwrap(), Step::Next("version:1".into()));
        assert_eq!(advance(Some("version:1"), &page(&[], None, None)).unwrap(), Step::Done);
    }

    #[test]
    fn advance_rejects_a_repeated_cursor() {
        let p = page(&[1], Some("version:1"), None);
        assert!(advance(Some("version:1"), &p).is_err());
    }

    #[test]
    fn first_cursor_needs_server_time() {
        assert_eq!(first_cursor(&page(&[], None, Some("T1"))).unwrap(), "T1");
        assert!(first_cursor(&page(&[], None, None)).is_err());
    }

    #[test]
    fn join_pages_is_oldest_first() {
        let joined = join_pages(vec![vec![entry(5), entry(6)], vec![entry(3), entry(4)], vec![entry(1)]]);
        assert_eq!(
            cursors(&joined),
            ["version:1", "version:3", "version:4", "version:5", "version:6"]
        );
    }

    #[tokio::test]
    async fn collect_pages_back_to_the_start_with_first_server_time() {
        let seen = RefCell::new(Vec::new());
        let f = collect_history(|before| {
            seen.borrow_mut().push(before.clone());
            let p = match before.as_deref() {
                None => page(&[5, 6], Some("version:5"), Some("T-first")),
                Some("version:5") => page(&[3, 4], Some("version:3"), Some("T-second")),
                _ => page(&[1, 2], None, Some("T-third")),
            };
            async move { Ok(p) }
        })
        .await
        .unwrap();
        assert_eq!(f.cursor, "T-first");
        assert!(f.fully_fetched);
        assert_eq!(f.entries.len(), 6);
        assert_eq!(
            *seen.borrow(),
            [None, Some("version:5".to_string()), Some("version:3".to_string())]
        );
    }

    #[tokio::test]
    async fn single_page_history_is_one_request() {
        let n = RefCell::new(0);
        let f = collect_history(|_| {
            *n.borrow_mut() += 1;
            async { Ok(page(&[1], None, Some("T"))) }
        })
        .await
        .unwrap();
        assert_eq!((*n.borrow(), f.entries.len()), (1, 1));
    }

    fn nf() -> ApiError {
        ApiError::NotFound("gone".into())
    }

    #[tokio::test]
    async fn before_id_404_restarts_the_full_fetch_once() {
        let seen = RefCell::new(Vec::new());
        let dead = RefCell::new(true);
        let f = collect_history_restarting(|before| {
            seen.borrow_mut().push(before.clone());
            let r = match before.as_deref() {
                None => Ok(page(&[5, 6], Some("version:5"), Some("T"))),
                Some(_) if *dead.borrow() => {
                    *dead.borrow_mut() = false;
                    Err(nf())
                }
                Some(_) => Ok(page(&[3, 4], None, Some("T2"))),
            };
            async move { r }
        })
        .await
        .unwrap();
        assert_eq!(f.entries.len(), 4);
        assert_eq!(seen.borrow().iter().filter(|b| b.is_none()).count(), 2);
    }

    #[tokio::test]
    async fn second_404_fails_without_a_third_attempt() {
        let firsts = RefCell::new(0);
        let r = collect_history_restarting(|before| {
            if before.is_none() {
                *firsts.borrow_mut() += 1;
            }
            let r = match before {
                None => Ok(page(&[2], Some("version:2"), Some("T"))),
                Some(_) => Err(nf()),
            };
            async move { r }
        })
        .await;
        assert!(matches!(r, Err(ApiError::NotFound(_))));
        assert_eq!(*firsts.borrow(), 2);
    }

    #[tokio::test]
    async fn first_page_404_is_not_retried() {
        let n = RefCell::new(0);
        let r = collect_history_restarting(|_| {
            *n.borrow_mut() += 1;
            async { Err(nf()) }
        })
        .await;
        assert!(matches!(r, Err(ApiError::NotFound(_))));
        assert_eq!(*n.borrow(), 1);
    }

    #[tokio::test]
    async fn non_404_errors_are_not_retried() {
        let n = RefCell::new(0);
        let r = collect_history_restarting(|before| {
            *n.borrow_mut() += 1;
            let r = match before {
                None => Ok(page(&[2], Some("version:2"), Some("T"))),
                Some(_) => Err(ApiError::Server { status: 500, message: "x".into() }),
            };
            async move { r }
        })
        .await;
        assert!(matches!(r, Err(ApiError::Server { .. })));
        assert_eq!(*n.borrow(), 2);
    }

    #[tokio::test]
    async fn a_failing_page_returns_the_error_and_no_batch() {
        let r = collect_history(|before| async move {
            match before {
                None => Ok(page(&[2], Some("version:2"), Some("T"))),
                Some(_) => Err(ApiError::Server { status: 500, message: "boom".into() }),
            }
        })
        .await;
        assert!(matches!(r, Err(ApiError::Server { .. })));
    }

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at) VALUES ('t1','T','T',7,?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO teams (id, name, identifier, created_at, updated_at) VALUES ('t2','L','L',?1,?1)", params![now]).unwrap();
        for team in ["t1", "t2"] {
            conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES (?3,'B',0,?2,?1,?1)", params![now, team, format!("l-{team}")]).unwrap();
        }
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, remote_id, created_at, updated_at) VALUES ('synced','i',4,'l-t1','t1',42,?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, created_at, updated_at) VALUES ('local','i',4,'l-t1','t1',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, remote_id, created_at, updated_at) VALUES ('noteam','i',4,'l-t2','t2',43,?1,?1)", params![now]).unwrap();
        conn
    }

    #[test]
    fn resolve_uses_the_issues_current_team_remote_id() {
        let conn = mem();
        let t = resolve(&conn, "synced").unwrap().unwrap();
        assert_eq!((t.issue_remote_id, t.team_remote_id), (42, 7));
        assert_eq!(t.state, FetchState::default());
        // Moving the issue to another (synced) team changes the URL's team.
        conn.execute("UPDATE teams SET remote_id = 9 WHERE id = 't2'", []).unwrap();
        conn.execute("UPDATE issues SET team_id = 't2' WHERE id = 'synced'", []).unwrap();
        assert_eq!(resolve(&conn, "synced").unwrap().unwrap().team_remote_id, 9);
    }

    #[test]
    fn resolve_skips_issues_without_remote_ids() {
        let conn = mem();
        assert!(resolve(&conn, "local").unwrap().is_none()); // issue not synced
        assert!(resolve(&conn, "noteam").unwrap().is_none()); // team not synced
        assert!(resolve(&conn, "missing").unwrap().is_none());
    }

    #[test]
    fn commit_stores_entries_cursor_and_flag_together() {
        let conn = mem();
        let fetched = Fetched {
            entries: vec![entry(1), entry(2)],
            cursor: "2026-07-06T00:00:10.000000Z".into(),
            fully_fetched: true,
        };
        commit(&conn, "synced", &fetched).unwrap();
        assert_eq!(store::entries_for_issue(&conn, "synced").unwrap().len(), 2);
        let st = store::fetch_state(&conn, "synced").unwrap();
        assert_eq!(st.cursor.as_deref(), Some("2026-07-06T00:00:10.000000Z"));
        assert!(st.fully_fetched);
    }

    /// Live check against local Rails on :3005 (run with `--ignored`).
    /// Env: ACT_TEAM, ACT_ISSUE (remote ids), ACT_LIMIT (small, to force paging).
    /// Config (token) comes from the app's config.toml; never printed.
    #[tokio::test]
    #[ignore]
    async fn live_paging_against_local_rails() {
        let env = |k: &str| std::env::var(k).unwrap().parse::<i64>().unwrap();
        let (cfg, _) = crate::config::load();
        let client = ApiClient::new(cfg.api_base_url.trim(), cfg.api_token.unwrap_or_default()).unwrap();
        let conn = mem();
        conn.execute("UPDATE teams SET remote_id = ?1 WHERE id = 't1'", params![env("ACT_TEAM")]).unwrap();
        conn.execute("UPDATE issues SET remote_id = ?1 WHERE id = 'synced'", params![env("ACT_ISSUE")]).unwrap();
        let target = resolve(&conn, "synced").unwrap().unwrap();
        let limit = env("ACT_LIMIT") as u32;
        let f = execute(&client, &target, &plan(&target.state), limit).await.unwrap();
        println!("entries={} cursor={}", f.entries.len(), f.cursor);
        let ids = cursors(&f.entries);
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "no duplicates across pages");
        // Compare with one big request.
        let all = execute(&client, &target, &plan(&target.state), MAX_LIMIT).await.unwrap();
        assert_eq!(ids, cursors(&all.entries));
        commit(&conn, "synced", &f).unwrap();
        assert_eq!(store::entries_for_issue(&conn, "synced").unwrap().len(), ids.len());
    }

    /// Live incremental check (run with `--ignored`; same env as the paging test).
    /// After a full fetch, the cursor is "now", so an incremental fetch must
    /// return only the overlap window (a subset of the full history).
    #[tokio::test]
    #[ignore]
    async fn live_incremental_against_local_rails() {
        let env = |k: &str| std::env::var(k).unwrap().parse::<i64>().unwrap();
        let (cfg, _) = crate::config::load();
        let client = ApiClient::new(cfg.api_base_url.trim(), cfg.api_token.unwrap_or_default()).unwrap();
        let conn = mem();
        conn.execute("UPDATE teams SET remote_id = ?1 WHERE id = 't1'", params![env("ACT_TEAM")]).unwrap();
        conn.execute("UPDATE issues SET remote_id = ?1 WHERE id = 'synced'", params![env("ACT_ISSUE")]).unwrap();
        let limit = env("ACT_LIMIT") as u32;
        let target = resolve(&conn, "synced").unwrap().unwrap();
        let full = execute(&client, &target, &plan(&target.state), limit).await.unwrap();
        commit(&conn, "synced", &full).unwrap();
        let target = resolve(&conn, "synced").unwrap().unwrap();
        let p = plan(&target.state);
        assert!(matches!(p, Plan::Incremental { .. }), "{p:?}");
        let inc = execute(&client, &target, &p, limit).await.unwrap();
        println!("full={} incremental={} plan={p:?}", full.entries.len(), inc.entries.len());
        let all = cursors(&full.entries);
        assert!(cursors(&inc.entries).iter().all(|c| all.contains(c)));
        assert!(inc.entries.len() < full.entries.len() || full.entries.len() <= 1);
        commit(&conn, "synced", &inc).unwrap();
        assert_eq!(store::entries_for_issue(&conn, "synced").unwrap().len(), all.len());
    }
}

/// JRST-147: a failed fetch keeps the cache, cursor and full-history flag.
#[cfg(test)]
mod failure_tests {
    use super::*;
    use crate::db::migrations;

    fn entry(id: i64) -> ActivityEntry {
        serde_json::from_str(&format!(
            r#"{{"type":"version","id":{id},"cursor":"version:{id}","created_at":"2026-07-06T00:00:{id:02}Z",
                "actor":null,"event":"update","changes":[],"description":"d"}}"#
        ))
        .unwrap()
    }

    /// File-backed DB (so `fetch_with` can reopen it per step) with a synced
    /// issue that already has two cached entries, a cursor and the flag.
    fn seeded(path: &std::path::Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute("INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at) VALUES ('t1','T','T',7,?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO lanes (id, name, position, team_id, created_at, updated_at) VALUES ('l1','B',0,'t1',?1,?1)", params![now]).unwrap();
        conn.execute("INSERT INTO issues (id, title, priority, lane_id, team_id, remote_id, created_at, updated_at) VALUES ('i1','i',4,'l1','t1',42,?1,?1)", params![now]).unwrap();
        commit(
            &conn,
            "i1",
            &Fetched {
                entries: vec![entry(1), entry(2)],
                cursor: "CURSOR-0".into(),
                fully_fetched: true,
            },
        )
        .unwrap();
        conn
    }

    fn snapshot(conn: &Connection) -> (usize, FetchState) {
        (
            store::entries_for_issue(conn, "i1").unwrap().len(),
            store::fetch_state(conn, "i1").unwrap(),
        )
    }

    fn temp_db() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("jrst147-{}.db", uuid::Uuid::new_v4()))
    }

    #[tokio::test]
    async fn offline_fetch_leaves_cache_cursor_and_flag_untouched() {
        let path = temp_db();
        let conn = seeded(&path);
        let before = snapshot(&conn);
        // Nothing listens on port 1: a connection error, like being offline.
        let client = ApiClient::new("http://127.0.0.1:1", "token").unwrap();
        let out = fetch_with(&client, "i1", 2, || Connection::open(&path)).await;
        assert!(matches!(out, FetchOutcome::Failed(_)), "got {out:?}");
        assert_eq!(snapshot(&conn), before);
        assert_eq!(before.1.cursor.as_deref(), Some("CURSOR-0"));
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    /// Server errors and mid-paging failures take the same path: `execute`
    /// returns `Err`, so `commit` never runs and nothing is stored.
    #[tokio::test]
    async fn server_error_partway_through_paging_stores_nothing() {
        let path = temp_db();
        let conn = seeded(&path);
        let before = snapshot(&conn);
        let res = collect_history(|before| {
            let p = ActivityPage {
                entries: vec![entry(8), entry(9)],
                next_before_id: Some("version:8".into()),
                server_time: Some("CURSOR-NEW".into()),
            };
            async move {
                match before {
                    None => Ok(p),
                    Some(_) => Err(ApiError::Server { status: 500, message: "boom".into() }),
                }
            }
        })
        .await;
        // The same Err -> Failed mapping `fetch_with` applies; no commit.
        let outcome = match res {
            Ok(f) => {
                commit(&conn, "i1", &f).unwrap();
                FetchOutcome::Fetched { stored: f.entries.len() }
            }
            Err(e) => FetchOutcome::Failed(e.to_string()),
        };
        assert!(matches!(&outcome, FetchOutcome::Failed(m) if m.contains("500")), "got {outcome:?}");
        assert_eq!(snapshot(&conn), before);
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }
}
