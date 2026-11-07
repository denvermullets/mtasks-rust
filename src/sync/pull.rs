//! Pull half of the sync engine: merges Rails data into local SQLite.

use std::collections::HashSet;

use rusqlite::Connection;

use crate::api::{
    ApiClient, ApiError, Conditional, MeDto, comments, issues, labels, lanes, members, projects,
    teams,
};
use crate::api::dto::{CommentDto, IssueDto, ProjectDto, PulledDependency};
use crate::db::{queries, sync_queue, team_purge};
use crate::db::models::{CommentOwner, Issue, Project};
use crate::sync::cursor;
use crate::sync::push::entry_scope;
use crate::sync::engine::{
    CycleCtx, SyncEngine, SyncErrorKind, SyncFailure, SyncReport, open, phase_scope, sync_log,
};
use crate::sync::tombstones::{
    drop_deleted_comments, pull_saved_view_tombstones_step, pull_tombstones_step,
};

pub(super) fn pull_mode(since: &Option<String>) -> &'static str {
    if since.is_some() {
        "incremental"
    } else {
        "full"
    }
}

/// Record an auth failure from a stringified pull error, if present. Pull is
/// read-only (all GETs), so the only user-actionable error we route is a 401 —
/// `ApiError::Unauthorized`'s Display always contains `"unauthorized:"`, which
/// pull helpers embed verbatim (e.g. `"/me: unauthorized: …"`). A revoked token
/// trips the first call (`/me`), but scanning every pull error also catches a
/// mid-sync revoke.
fn note_pull_failure(report: &mut SyncReport, msg: &str) {
    if msg.contains("unauthorized:") {
        report.failures.push(SyncFailure {
            entity_type: "auth".into(),
            entity_id: String::new(),
            kind: SyncErrorKind::Auth,
            message: msg.to_string(),
        });
    }
}

// ── Pull side ─────────────────────────────────────────────────────────────
//
// Walks resources in FK-dependency order and merges each row into local SQLite
// using a last-write-wins policy keyed on `updated_at`.
//
// Order: teams → lanes / labels / members → projects → issues. Issue
// dependencies come from each issue-detail payload. Rails also has
// `GET .../issues/:id/issue_dependencies` (see docs/rails-api.md), but the
// detail payload already carries the same links, so pull does not call it.
//
// The merge rule per row (see `decide`):
// - No local row with this `remote_id` → INSERT.
// - Local row exists, no pending sync_queue entry → OVERWRITE from remote.
// - Local row exists, pending entry, local.updated_at > remote.updated_at →
//   keep local; the next push will overwrite Rails.
// - Otherwise → OVERWRITE from remote.

#[derive(Debug, PartialEq, Eq)]
enum MergeDecision {
    Overwrite,
    KeepLocal,
}

fn decide(local_updated: &str, remote_updated: &str, has_pending: bool) -> MergeDecision {
    if !has_pending {
        return MergeDecision::Overwrite;
    }
    let l = chrono::DateTime::parse_from_rfc3339(local_updated);
    let r = chrono::DateTime::parse_from_rfc3339(remote_updated);
    match (l, r) {
        (Ok(l), Ok(r)) if l > r => MergeDecision::KeepLocal,
        // If either timestamp doesn't parse, treat remote as authoritative —
        // the data store is the source of truth when local clocks are off.
        _ => MergeDecision::Overwrite,
    }
}

/// Result of a pull that includes optional bookkeeping outside the row counts.
pub struct PullOutcome {
    pub report: SyncReport,
    pub deleted_teams: Vec<String>,
    /// Signed-in user's `(rails_id, name)` from `/me`, when the call succeeded.
    /// The app persists this to config to stamp `creator_id` on new issues.
    pub current_user: Option<(i64, String)>,
    /// Whether `/me` reported `write` scope (`None` if `/me` failed).
    pub can_write: Option<bool>,
}

impl SyncEngine {
    /// Pull all resources from Rails into the local DB. The set of teams to
    /// fully sync is determined by the token's scope:
    ///
    /// - **Unscoped token** (`token.team_ids` null/absent, no `team_id`) — pull
    ///   every local non-archived team, then for each team pull lanes/labels/
    ///   members/projects/issues.
    /// - **Scoped token** (`token.team_ids` list, or legacy `team_id`) — pull
    ///   the intersection of that list with the local teams.
    ///
    /// Returns a [`PullOutcome`] summarizing row counts plus the local UUID of
    /// the locally-seeded "My Project" team if pull determined it was redundant
    /// (the caller uses this to repoint `selected_team_id`).
    pub async fn pull(client: &ApiClient, ctx: &CycleCtx) -> PullOutcome {
        let mut report = SyncReport::default();
        let mut deleted_teams = Vec::new();

        // Phase 1: the token's scope, from the cycle's single `/me` call.
        let me = match &ctx.me {
            Ok(m) => m,
            Err(e) => {
                let msg = format!("/me: {e}");
                sync_log("phase=pull", "fatal", &msg);
                report.failed += 1;
                note_pull_failure(&mut report, &msg);
                report.last_error = Some(msg);
                return PullOutcome {
                    report,
                    deleted_teams,
                    current_user: None,
                    can_write: None,
                };
            }
        };
        let current_user = Some((me.id, me.name.clone()));
        let can_write = Some(me.token.has_write_scope());

        // The Home page rides on the same `/me` call; a failure here must not
        // block the rest of the pull.
        if let Err(e) = open().and_then(|c| store_home_page(&c, me).map_err(|e| e.to_string())) {
            sync_log("phase=pull", "home_page", &e);
        }
        if let Err(e) = open().and_then(|c| {
            crate::db::sidebar_layout::apply_me(&c, me.sidebar.as_ref()).map_err(|e| e.to_string())
        }) {
            sync_log("phase=pull", "sidebar_layout", &e);
        }

        // Phase 2: refresh teams (and discover any new ones the token can see).
        match pull_teams(client).await {
            Ok(pulled) => {
                deleted_teams = pulled.deleted;
                report.notices = pulled.notices;
                report.processed += 1;
            }
            Err(msg) => {
                report.failed += 1;
                note_pull_failure(&mut report, &msg);
                report.last_error = Some(msg);
                return PullOutcome {
                    report,
                    deleted_teams,
                    current_user,
                    can_write,
                };
            }
        }

        // Phase 2b: the user's Saved views (cross-team, own cursor). Needs the
        // teams just refreshed; a failure doesn't block the per-team pulls.
        match pull_saved_views_step(client).await {
            Ok(()) => report.processed += 1,
            Err(msg) => {
                report.failed += 1;
                note_pull_failure(&mut report, &msg);
                report.last_error = Some(msg);
            }
        }

        // Phase 2c: Saved views deleted on the server (user-level feed).
        match pull_saved_view_tombstones_step(client).await {
            Ok(()) => report.processed += 1,
            Err(msg) => {
                report.failed += 1;
                note_pull_failure(&mut report, &msg);
                report.last_error = Some(msg);
            }
        }

        // Phase 2d: the user's Dashboards (full snapshot every cycle).
        match pull_dashboards_step(client).await {
            Ok(()) => report.processed += 1,
            Err(msg) => {
                report.failed += 1;
                note_pull_failure(&mut report, &msg);
                report.last_error = Some(msg);
            }
        }

        // Phase 3: figure out which teams to deeply sync.
        let targets = match collect_pull_targets(me.token.scoped_team_ids().as_deref()) {
            Ok(t) => t,
            Err(msg) => {
                report.last_error = Some(msg);
                report.failed += 1;
                return PullOutcome {
                    report,
                    deleted_teams,
                    current_user,
                    can_write,
                };
            }
        };
        if targets.is_empty() {
            // Token has access but no teams visible (or team-scoped to a team
            // we couldn't find locally after pull_teams — shouldn't happen).
            return PullOutcome {
                report,
                deleted_teams,
                current_user,
                can_write,
            };
        }

        // Phase 4: per-team resources. Each helper logs and updates the report
        // independently so one team's failure doesn't block the others.
        let mut issue_pulls: Vec<IssueTeamPull> = Vec::new();
        let mut all_issue_pulls_ok = true;
        for target in &targets {
            let mut steps = vec![
                pull_lanes_step(client, target.remote_id, &target.local_id).await,
                pull_labels_step(client, target.remote_id, &target.local_id).await,
                pull_members_step(client, target.remote_id, &target.local_id).await,
                pull_projects_step(client, target.remote_id, &target.local_id).await,
                // Before issues, so a spawned issue can resolve its recurring issue.
                pull_recurring_issues_step(client, target.remote_id, &target.local_id).await,
            ];
            match pull_issues_step(client, target.remote_id, &target.local_id).await {
                Ok(pull) => {
                    issue_pulls.push(pull);
                    steps.push(Ok(()));
                }
                Err(e) => {
                    all_issue_pulls_ok = false;
                    steps.push(Err(e));
                }
            }
            // Tombstones run before comments; the comment ids they removed
            // are handed on so a feed row for one cannot resurrect it.
            let mut deleted_comments = HashSet::new();
            match pull_tombstones_step(client, target.remote_id, &target.local_id).await {
                Ok(ids) => {
                    deleted_comments = ids;
                    steps.push(Ok(()));
                }
                Err(e) => steps.push(Err(e)),
            }
            steps.push(
                pull_comments_step(
                    client,
                    target.remote_id,
                    &target.local_id,
                    &deleted_comments,
                )
                .await,
            );
            for step in steps {
                match step {
                    Ok(()) => report.processed += 1,
                    Err(msg) => {
                        report.failed += 1;
                        note_pull_failure(&mut report, &msg);
                        report.last_error = Some(msg);
                    }
                }
            }
        }

        // A targeted pull (some teams not covered) can't tell "moved to an
        // unpulled team" from "gone", so purging needs every team pulled.
        let covers_all_teams = collect_pull_targets(None)
            .map(|all| all.len() == targets.len())
            .unwrap_or(false);
        if let Ok(conn) = open() {
            purge_unlisted_issues(&conn, &issue_pulls, all_issue_pulls_ok && covers_all_teams);
        }

        PullOutcome {
            report,
            deleted_teams,
            current_user,
            can_write,
        }
    }
}

/// Persist the Home page `/me` reported (null clears it).
fn store_home_page(conn: &Connection, me: &MeDto) -> rusqlite::Result<()> {
    crate::db::home_page::apply_me(conn, me.home_page.as_ref())
}

struct PullTarget {
    local_id: String,
    remote_id: i64,
}

fn collect_pull_targets(scoped_ids: Option<&[i64]>) -> Result<Vec<PullTarget>, String> {
    let conn = open()?;
    select_pull_targets(&conn, scoped_ids)
}

/// Choose the teams to deep-sync. `None` (unscoped token) covers every local
/// non-archived Rails-backed team; `Some(ids)` covers the intersection of those
/// remote ids with the local non-archived teams (empty list means none).
fn select_pull_targets(
    conn: &Connection,
    scoped_ids: Option<&[i64]>,
) -> Result<Vec<PullTarget>, String> {
    let teams = queries::all_teams(conn).map_err(|e| format!("db (all_teams): {e}"))?;
    Ok(teams
        .into_iter()
        .filter(|t| t.archived_at.is_none())
        .filter_map(|t| {
            let rid = t.remote_id?;
            if let Some(ids) = scoped_ids {
                if !ids.contains(&rid) {
                    return None;
                }
            }
            Some(PullTarget {
                local_id: t.id,
                remote_id: rid,
            })
        })
        .collect())
}

/// What refreshing the team list changed locally.
#[derive(Default)]
struct TeamsPulled {
    /// Local UUIDs of the teams this pull removed.
    deleted: Vec<String>,
    /// What the user needs to hear about it.
    notices: Vec<String>,
}

async fn pull_teams(client: &ApiClient) -> Result<TeamsPulled, String> {
    let dtos = teams::list(client)
        .await
        .map_err(|e| format!("teams: {e}"))?;

    let active_ids: Vec<i64> = dtos.iter().map(|d| d.id).collect();
    let inserted_any_remote_team = !dtos.is_empty();

    let conn = open()?;
    for dto in dtos {
        let existing = queries::find_team_by_remote_id(&conn, dto.id)
            .map_err(|e| format!("db (find team): {e}"))?;
        let has_pending = match &existing {
            Some(t) => sync_queue::has_pending_entry(&conn, "team", &t.id)
                .map_err(|e| format!("db: {e}"))?,
            None => false,
        };

        let remote_updated = dto.updated_at.clone().unwrap_or_default();

        if let Some(local) = &existing {
            if decide(&local.updated_at, &remote_updated, has_pending) == MergeDecision::KeepLocal {
                continue;
            }
        }

        let newly_mirrored = existing.is_none();
        let team = crate::db::models::Team::from(dto.clone());
        let team = if let Some(local) = existing {
            // Reuse the local UUID so foreign keys (lanes/issues/etc.) stay valid.
            crate::db::models::Team {
                id: local.id,
                ..team
            }
        } else {
            team
        };
        queries::upsert_team(&conn, &team).map_err(|e| format!("db (upsert team): {e}"))?;
        if newly_mirrored {
            // Saved views pulled for it earlier were skipped; refetch them all.
            crate::db::saved_views::on_team_mirrored(&conn)
                .map_err(|e| format!("db (saved views cursor): {e}"))?;
        }
    }
    drop(conn);

    let conn = open()?;
    let mut pulled = reconcile_teams(&conn, &active_ids);

    // Once we've confirmed Rails has at least one team for this token, the
    // locally-seeded "My Project" placeholder is redundant. Cleanup is gated
    // on "no issues attached" so we never destroy real local-only work.
    if inserted_any_remote_team {
        match queries::cleanup_orphan_seed_team(&conn) {
            Ok(orphan) => pulled.deleted.extend(orphan),
            Err(e) => {
                sync_log("phase=pull_teams", "cleanup_orphan_failed", &format!("db: {e}"));
            }
        }
    }

    Ok(pulled)
}

/// Apply a successful `/teams` response: restore listed teams, purge unlisted
/// ones and clear the files they leave behind.
fn reconcile_teams(conn: &Connection, listed: &[i64]) -> TeamsPulled {
    let reconciled = match team_purge::reconcile_listed_teams(conn, listed) {
        Ok(r) => r,
        Err(e) => {
            sync_log("phase=pull_teams", "reconcile_failed", &format!("db: {e}"));
            return TeamsPulled::default();
        }
    };
    for (team_id, reason) in &reconciled.failed {
        sync_log("phase=pull_teams", "purge_failed", &format!("team={team_id} db: {reason}"));
    }
    for edit in reconciled.purged.iter().flat_map(|team| &team.dropped_edits) {
        if let Some(entry_id) = edit.id {
            sync_log(&entry_scope(entry_id, edit), "dropped", "team gone");
        }
    }
    let notices = reconciled.notices();
    TeamsPulled { deleted: remove_purged_files(conn, reconciled.purged), notices }
}

/// Delete what a purge leaves on disk: staged uploads, and the file cache so
/// cached bytes don't stay reachable. The cache isn't indexed by team, so all of
/// it goes; anything still needed downloads again. Returns the purged local ids.
fn remove_purged_files(conn: &Connection, purged: Vec<team_purge::PurgedTeam>) -> Vec<String> {
    if purged.is_empty() {
        return Vec::new();
    }
    if let Err(e) = crate::files::cache::FileCache::at_default_root().clear(conn) {
        sync_log("phase=pull_teams", "cache_clear_failed", &e.to_string());
    }
    purged
        .into_iter()
        .map(|team| {
            for path in &team.outbox_paths {
                if let Err(e) = crate::files::outbox::remove(path) {
                    sync_log("phase=pull_teams", "outbox_remove_failed", &e.to_string());
                }
            }
            team.team_id
        })
        .collect()
}

/// Conditional (ETag) pull shared by the lane, label and member lists: send
/// the stored ETag, skip on 304, merge each row by the usual pending-edit rule
/// (`decide`), then store the new ETag only after every row was applied.
///
/// - `entity` is the sync-queue entity type (`"lane"`), `list` the list and
///   ETag name (`"lanes"`).
/// - `fetch` performs the conditional GET with the stored ETag.
/// - `find` returns the local row for a DTO as `(local id, local updated_at)`.
/// - `store` converts the DTO (reusing the local id when there is one) and
///   upserts it under `local_team_id`.
#[allow(clippy::too_many_arguments)]
async fn pull_etag_list<D, Fut>(
    entity: &str,
    list: &str,
    team_remote_id: i64,
    local_team_id: &str,
    fetch: impl FnOnce(Option<String>) -> Fut,
    set_team: impl Fn(&mut D, i64),
    remote_updated: impl Fn(&D) -> String,
    find: impl Fn(&Connection, &D) -> Result<Option<(String, String)>, String>,
    store: impl Fn(&Connection, D, Option<String>, &str) -> Result<(), String>,
) -> Result<(), String>
where
    Fut: std::future::Future<Output = Result<Conditional<Vec<D>>, ApiError>>,
{
    let stored = {
        let conn = open()?;
        queries::get_list_etag(&conn, local_team_id, list).unwrap_or(None)
    };
    let (dtos, new_etag) = match fetch(stored)
        .await
        .map_err(|e| format!("{list}: {e}"))?
    {
        Conditional::NotModified => {
            sync_log(
                &phase_scope(&format!("pull_{list}"), Some(team_remote_id)),
                "not_modified",
                "304",
            );
            return Ok(());
        }
        Conditional::Modified { data, etag } => (data, etag),
    };

    let conn = open()?;
    for mut dto in dtos {
        // Rails omits team_id on the wire; inject from the URL path.
        set_team(&mut dto, team_remote_id);

        let existing = find(&conn, &dto)?;
        let has_pending = match &existing {
            Some((local_id, _)) => sync_queue::has_pending_entry(&conn, entity, local_id)
                .map_err(|e| format!("db: {e}"))?,
            None => false,
        };

        if let Some((_, local_updated)) = &existing {
            if decide(local_updated, &remote_updated(&dto), has_pending)
                == MergeDecision::KeepLocal
            {
                continue;
            }
        }
        store(&conn, dto, existing.map(|(id, _)| id), local_team_id)?;
    }
    store_list_etag(&conn, local_team_id, list, new_etag.as_deref());
    Ok(())
}

async fn pull_lanes_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    // Lanes table has no archived_at column, so missing lanes are left in
    // place. Hard-deleting would risk cascading FK violations on issues that
    // still reference the lane locally. Revisit when the schema gains a
    // soft-delete column.
    pull_etag_list(
        "lane",
        "lanes",
        team_remote_id,
        local_team_id,
        |etag| async move { lanes::list_conditional(client, team_remote_id, etag.as_deref()).await },
        |dto, team| dto.team_id = team,
        |dto| dto.updated_at.clone().unwrap_or_default(),
        |conn, dto| {
            let found = queries::find_lane_by_remote_id(conn, dto.id)
                .map_err(|e| format!("db (find lane): {e}"))?;
            Ok(found.map(|l| (l.id, l.updated_at)))
        },
        |conn, dto, existing, local_team_id| {
            let mut lane = dto
                .into_model(conn, existing)
                .map_err(|e| format!("convert lane: {e}"))?;
            lane.team_id = local_team_id.to_string();
            queries::upsert_lane(conn, &lane).map_err(|e| format!("db (upsert lane): {e}"))
        },
    )
    .await
}

async fn pull_labels_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    pull_etag_list(
        "label",
        "labels",
        team_remote_id,
        local_team_id,
        |etag| async move { labels::list_conditional(client, team_remote_id, etag.as_deref()).await },
        |dto, team| dto.team_id = team,
        |dto| dto.updated_at.clone().unwrap_or_default(),
        |conn, dto| {
            let found = queries::find_label_by_remote_id(conn, dto.id)
                .map_err(|e| format!("db (find label): {e}"))?;
            Ok(found.map(|l| (l.id, l.updated_at)))
        },
        |conn, dto, existing, local_team_id| {
            let mut label = dto
                .into_model(conn, existing)
                .map_err(|e| format!("convert label: {e}"))?;
            label.team_id = local_team_id.to_string();
            queries::upsert_label(conn, &label).map_err(|e| format!("db (upsert label): {e}"))
        },
    )
    .await
}

async fn pull_members_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    pull_etag_list(
        "member",
        "members",
        team_remote_id,
        local_team_id,
        |etag| async move { members::list_conditional(client, team_remote_id, etag.as_deref()).await },
        |dto, team| dto.team_id = team,
        |dto| dto.updated_at.clone().unwrap_or_default(),
        |conn, dto| {
            let found = queries::find_member_by_remote_id(conn, dto.id)
                .map_err(|e| format!("db (find member): {e}"))?;
            Ok(found.map(|m| (m.id, m.updated_at)))
        },
        |conn, dto, existing, local_team_id| {
            let mut member = dto
                .into_model(conn, existing)
                .map_err(|e| format!("convert member: {e}"))?;
            member.team_id = local_team_id.to_string();
            queries::upsert_member(conn, &member).map_err(|e| format!("db (upsert member): {e}"))
        },
    )
    .await
}

/// Persist the ETag only after the list was fully applied, so a failed apply
/// is retried with a full fetch. A response without an ETag clears the stored one.
fn store_list_etag(conn: &Connection, team_id: &str, list: &str, etag: Option<&str>) {
    let res = match etag {
        Some(tag) => queries::set_list_etag(conn, team_id, list, tag),
        None => queries::clear_list_etag(conn, team_id, list),
    };
    if let Err(e) = res {
        sync_log(
            &format!("phase=pull_{list}"),
            "etag_store_failed",
            &e.to_string(),
        );
    }
}

/// Pull the user's Saved views (cross-team). Incremental from the Saved-views
/// cursor minus the overlap; the cursor advances to `X-Server-Time` only when
/// every row was applied.
async fn pull_saved_views_step(client: &ApiClient) -> Result<(), String> {
    let since = {
        let conn = open()?;
        crate::db::saved_views::cursor(&conn)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };
    let page = crate::api::saved_views::list_page(client, since.as_deref())
        .await
        .map_err(|e| format!("saved_views: {e}"))?;
    let conn = open()?;
    let applied = crate::db::saved_views::apply_pulled(&conn, &page.rows, page.server_time.as_deref())
        .map_err(|e| format!("db (saved views): {e}"))?;
    sync_log(
        "phase=pull_saved_views",
        "ok",
        &format!(
            "mode={} rows={} upserted={} skipped_unmirrored_team={} skipped_pending={}",
            pull_mode(&since),
            page.rows.len(),
            applied.upserted,
            applied.skipped_unmirrored,
            applied.skipped_pending
        ),
    );
    Ok(())
}

/// Pull the user's Dashboards. The endpoint has no paging or `updated_since`, so
/// every cycle fetches the whole list and the apply reconciles removals (which
/// is why `Dashboard` / `DashboardGroup` tombstones are not consumed).
async fn pull_dashboards_step(client: &ApiClient) -> Result<(), String> {
    let rows = crate::api::dashboards::list(client)
        .await
        .map_err(|e| format!("dashboards: {e}"))?;
    let conn = open()?;
    let applied = crate::db::dashboards::apply_snapshot(&conn, &rows)
        .map_err(|e| format!("db (dashboards): {e}"))?;
    sync_log(
        "phase=pull_dashboards",
        "ok",
        &format!(
            "rows={} upserted={} removed={} groups_upserted={} groups_removed={} skipped_pending={}",
            rows.len(),
            applied.dashboards_upserted,
            applied.dashboards_removed,
            applied.groups_upserted,
            applied.groups_removed,
            applied.skipped_pending
        ),
    );
    Ok(())
}

/// Pull a team's recurring issues. `updated_since` = stored cursor minus the
/// overlap (the server filter is strict `>`); the first pull omits it. The
/// cursor advances to `X-Server-Time` only when the batch was applied.
async fn pull_recurring_issues_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    let since = {
        let conn = open()?;
        crate::db::recurring_issues::cursor(&conn, local_team_id)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };
    let page = crate::api::recurring_issues::list_page(client, team_remote_id, since.as_deref())
        .await
        .map_err(|e| format!("recurring_issues: {e}"))?;
    let conn = open()?;
    let applied = crate::db::recurring_issues::apply_pulled(
        &conn,
        local_team_id,
        &page.rows,
        page.server_time.as_deref(),
    )
    .map_err(|e| format!("db (recurring issues): {e}"))?;
    sync_log(
        &phase_scope("pull_recurring_issues", Some(team_remote_id)),
        "ok",
        &format!(
            "mode={} rows={} upserted={} skipped_pending={}",
            pull_mode(&since),
            page.rows.len(),
            applied.upserted,
            applied.skipped_pending
        ),
    );
    Ok(())
}

/// Cursor resource name for the project index in `pull_cursors`.
const PROJECTS_CURSOR: &str = "projects";

/// Pull a team's projects. With a stored cursor only rows changed since
/// (cursor minus a small overlap) are fetched; the first sync of a team has
/// no cursor and fetches every project. The cursor advances to the response's
/// `X-Server-Time`, and only when every row was applied.
async fn pull_projects_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<(), String> {
    let since = {
        let conn = open()?;
        queries::get_pull_cursor(&conn, local_team_id, PROJECTS_CURSOR)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };
    let mut page = projects::list_page(client, team_remote_id, since.as_deref())
        .await
        .map_err(|e| format!("projects: {e}"))?;
    attach_project_details(client, team_remote_id, &mut page.rows).await?;
    sync_log(
        &phase_scope("pull_projects", Some(team_remote_id)),
        "ok",
        &format!("mode={} rows={}", pull_mode(&since), page.rows.len()),
    );

    let conn = open()?;
    apply_pulled_projects(&conn, local_team_id, page.rows, page.server_time.as_deref())
}

/// The project index omits `files`; only the detail payload carries them. An
/// error fails the step before the cursor moves, so the rows are retried.
async fn attach_project_details(
    client: &ApiClient,
    team_remote_id: i64,
    rows: &mut [ProjectDto],
) -> Result<(), String> {
    for dto in rows.iter_mut() {
        match projects::get(client, team_remote_id, dto.id).await {
            Ok(detail) => dto.files = Some(detail.files.unwrap_or_default()),
            // Deleted since the listing: nothing to store.
            Err(ApiError::NotFound(_)) => {}
            Err(e) => return Err(format!("project {} detail: {e}", dto.id)),
        }
    }
    Ok(())
}

/// Write pulled project rows (idempotent, so overlap re-delivery is safe),
/// then advance the cursor. An error leaves the cursor so rows are retried.
fn apply_pulled_projects(
    conn: &Connection,
    local_team_id: &str,
    dtos: Vec<ProjectDto>,
    server_time: Option<&str>,
) -> Result<(), String> {
    for dto in dedupe_by_id(dtos, |d| d.id) {
        let existing = queries::find_project_by_remote_id(conn, dto.id)
            .map_err(|e| format!("db (find project): {e}"))?;
        let has_pending = match &existing {
            Some(p) => sync_queue::has_pending_entry(conn, "project", &p.id)
                .map_err(|e| format!("db: {e}"))?,
            None => false,
        };

        if let Some(local) = &existing {
            if decide(&local.updated_at, &dto.updated_at, has_pending) == MergeDecision::KeepLocal {
                continue;
            }
        }

        let existing_uuid = existing.as_ref().map(|p| p.id.clone());
        // `None` (index row, no detail) leaves stored attachments alone.
        let pulled_files = dto.files.clone();
        let project: Project = dto
            .into_model(conn, local_team_id.to_string(), existing_uuid)
            .map_err(|e| format!("convert project: {e}"))?;
        queries::upsert_project(conn, &project)
            .map_err(|e| format!("db (upsert project): {e}"))?;
        if let Some(files) = pulled_files {
            crate::db::attachments::replace_synced(
                conn,
                crate::db::attachments::OwnerKind::Project,
                &project.id,
                &files,
            )
            .map_err(|e| format!("db (project attachments): {e}"))?;
        }
    }
    // project_labels join not populated in this ticket; documented as TODO.
    if let Some(t) = server_time {
        queries::set_pull_cursor(conn, local_team_id, PROJECTS_CURSOR, t)
            .map_err(|e| format!("db (pull cursor): {e}"))?;
    }
    Ok(())
}

/// Cursor resource name for the issue index in `pull_cursors`.
const ISSUES_CURSOR: &str = "issues";

/// The request for one page of the issue pull. A full pull (no cursor) is the
/// same request as an incremental one minus `updated_since`: detail rows.
fn issue_pull_query(since: Option<&str>, after_id: Option<i64>) -> issues::IssuePageQuery {
    issues::IssuePageQuery {
        updated_since: since.map(str::to_string),
        include_detail: true,
        limit: Some(cursor::PAGE_LIMIT),
        after_id,
    }
}

/// Pull a team's issues with full detail in paged requests. With a stored
/// cursor only rows changed since (cursor minus a small overlap) are fetched;
/// without one the whole team is fetched the same way. The cursor advances to
/// the first page's `X-Server-Time`, and only when every row was applied.
async fn pull_issues_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
) -> Result<IssueTeamPull, String> {
    let since = {
        let conn = open()?;
        queries::get_pull_cursor(&conn, local_team_id, ISSUES_CURSOR)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };

    let collected = cursor::collect_pages(|after_id| {
        let query = issue_pull_query(since.as_deref(), after_id);
        async move { issues::list_page(client, team_remote_id, &query).await }
    })
    .await
    .map_err(|e| format!("issues: {e}"))?;
    sync_log(
        &phase_scope("pull_issues", Some(team_remote_id)),
        "ok",
        &format!(
            "mode={} requests={} rows={}",
            pull_mode(&since),
            collected.requests,
            collected.rows.len()
        ),
    );

    let dtos = dedupe_by_id(collected.rows, |d| d.id);
    let total = dtos.len();
    let conn = open()?;
    let skipped = apply_pulled_issues(&conn, local_team_id, dtos.clone());

    if skipped > 0 {
        // Leave the cursor where it was so the failed rows are retried.
        return Err(format!(
            "issues: {skipped}/{total} skipped (see [sync] log)"
        ));
    }
    if let Some(t) = &collected.server_time {
        queries::set_pull_cursor(&conn, local_team_id, ISSUES_CURSOR, t)
            .map_err(|e| format!("db (pull cursor): {e}"))?;
    }
    Ok(IssueTeamPull {
        local_team_id: local_team_id.to_string(),
        full_listing: since.is_none(),
        seen: dtos.iter().map(|d| d.id).collect(),
    })
}

/// What one team's issue pull saw in this cycle.
struct IssueTeamPull {
    local_team_id: String,
    /// True when the whole team was listed (no cursor), so absence means gone.
    full_listing: bool,
    seen: Vec<i64>,
}

/// Hard-delete local issues a full listing no longer returns (deleted or out
/// of reach), along with their queued edits. An
/// issue moved to another team on the web vanishes from its source listing, so
/// absence is judged against every issue seen this cycle, and only when every
/// team's issue pull succeeded (`complete`): otherwise the target team may
/// simply not have been pulled yet.
fn purge_unlisted_issues(conn: &Connection, pulls: &[IssueTeamPull], complete: bool) {
    if !complete {
        return;
    }
    let seen: Vec<i64> = pulls.iter().flat_map(|p| p.seen.iter().copied()).collect();
    for p in pulls.iter().filter(|p| p.full_listing) {
        if let Err(e) = queries::purge_issues_missing_from_listing(conn, &p.local_team_id, &seen) {
            sync_log("purge_unlisted_issues", "error", &format!("{}: {e}", p.local_team_id));
        }
    }
}

/// Cursor resource name for the team comments feed in `pull_cursors`.
const COMMENTS_CURSOR: &str = "comments";

/// Pull a team's comments from the flat team feed. Same cursor discipline as
/// the issue pull: overlap on the stored cursor, dedupe, idempotent apply, and
/// the cursor moves to the response's `X-Server-Time` only if nothing failed.
/// Issues are pulled first so comments find their issue.
async fn pull_comments_step(
    client: &ApiClient,
    team_remote_id: i64,
    local_team_id: &str,
    deleted_comments: &HashSet<i64>,
) -> Result<(), String> {
    let since = {
        let conn = open()?;
        queries::get_pull_cursor(&conn, local_team_id, COMMENTS_CURSOR)
            .map_err(|e| format!("db (pull cursor): {e}"))?
            .and_then(|c| cursor::since_with_overlap(&c))
    };
    let page = comments::list_team_updated_since(client, team_remote_id, since.as_deref())
        .await
        .map_err(|e| format!("comments: {e}"))?;
    sync_log(
        &phase_scope("pull_comments", Some(team_remote_id)),
        "ok",
        &format!("mode={} rows={}", pull_mode(&since), page.rows.len()),
    );
    let dtos = drop_deleted_comments(dedupe_by_id(page.rows, |d| d.id), deleted_comments);
    let total = dtos.len();
    let conn = open()?;
    let failed = apply_pulled_comments(&conn, dtos);
    if failed > 0 {
        return Err(format!("comments: {failed}/{total} failed (see [sync] log)"));
    }
    if let Some(t) = &page.server_time {
        queries::set_pull_cursor(&conn, local_team_id, COMMENTS_CURSOR, t)
            .map_err(|e| format!("db (pull cursor): {e}"))?;
    }
    Ok(())
}

/// Write feed comments locally (idempotent). Comments on issues or projects we
/// do not have (or issues we have tombstoned) are ignored. Replies are applied
/// after their parents, however the feed ordered them; a parent that cannot be
/// found locally leaves the reply top-level rather than blocking the cursor.
/// Returns how many rows hit a DB error.
pub(super) fn apply_pulled_comments(conn: &Connection, dtos: Vec<CommentDto>) -> usize {
    let mut failed = 0;
    let mut remaining: Vec<CommentDto> = dtos.into_iter().filter(|d| d.issue_id.is_some() || d.project_id.is_some()).collect();
    loop {
        let before = remaining.len();
        let mut deferred = Vec::new();
        for dto in remaining {
            // Wait for a parent that is in this batch but not applied yet.
            let parent_local = match dto.parent_id {
                None => None,
                Some(pid) => match queries::find_comment_by_remote_id(conn, pid) {
                    Ok(Some(p)) => Some(p.id),
                    Ok(None) => {
                        deferred.push(dto);
                        continue;
                    }
                    Err(_) => None,
                },
            };
            if apply_one_comment(conn, dto, parent_local).is_err() {
                failed += 1;
            }
        }
        remaining = deferred;
        if remaining.is_empty() || remaining.len() == before {
            break;
        }
    }
    // Whatever is left has a parent we never saw: store as top-level.
    for dto in remaining {
        sync_log(
            &format!("phase=pull_comments comment_remote_id={}", dto.id),
            "stored_top_level",
            &format!("parent {:?} not found locally", dto.parent_id),
        );
        if apply_one_comment(conn, dto, None).is_err() {
            failed += 1;
        }
    }
    failed
}

fn apply_one_comment(
    conn: &Connection,
    mut dto: CommentDto,
    parent_local: Option<String>,
) -> Result<(), String> {
    let owner = if let Some(issue_remote) = dto.issue_id {
        let issue = match queries::find_issue_by_remote_id(conn, issue_remote) {
            Ok(Some(i)) => i,
            Ok(None) => return Ok(()),
            Err(e) => return Err(format!("db (find issue): {e}")),
        };
        if queries::is_issue_tombstoned_by_remote_id(conn, issue_remote)
            .map_err(|e| format!("db (tombstone): {e}"))?
        {
            return Ok(());
        }
        CommentOwner::Issue(issue.id)
    } else if let Some(project_remote) = dto.project_id {
        match queries::find_project_by_remote_id(conn, project_remote) {
            Ok(Some(p)) => CommentOwner::Project(p.id),
            Ok(None) => return Ok(()),
            Err(e) => return Err(format!("db (find project): {e}")),
        }
    } else {
        return Ok(());
    };
    let existing = queries::find_comment_by_remote_id(conn, dto.id)
        .map_err(|e| format!("db (find comment): {e}"))?
        .map(|c| c.id);
    let files = dto.files.take();
    let comment = dto.into_model(conn, owner, existing, parent_local);
    queries::upsert_comment(conn, &comment).map_err(|e| format!("db (upsert comment): {e}"))?;
    if let Some(files) = files {
        crate::db::attachments::replace_synced(
            conn,
            crate::db::attachments::OwnerKind::Comment,
            &comment.id,
            &files,
        )
        .map_err(|e| format!("db (comment attachments): {e}"))?;
    }
    Ok(())
}

/// Keep one row per key (the last delivered wins), in first-seen order. The
/// cursor overlap re-delivers rows, and paging can repeat one across pages.
fn dedupe_by_id<T>(rows: Vec<T>, key: impl Fn(&T) -> i64) -> Vec<T> {
    let mut index: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    let mut out: Vec<T> = Vec::with_capacity(rows.len());
    for row in rows {
        match index.get(&key(&row)) {
            Some(&i) => out[i] = row,
            None => {
                index.insert(key(&row), out.len());
                out.push(row);
            }
        }
    }
    out
}

/// Write detailed issue rows to the local DB (idempotent, so overlap
/// re-delivery is safe). Returns how many rows failed to apply.
fn apply_pulled_issues(conn: &Connection, local_team_id: &str, dtos: Vec<IssueDto>) -> usize {
    let mut skipped = 0;
    // Dependencies are persisted after every issue in this batch exists
    // locally, since an edge may point at an issue later in the batch.
    let mut pending_links: Vec<(String, Option<Vec<PulledDependency>>)> = Vec::new();
    for dto in dtos {
        let issue_remote_id = dto.id;
        let result = (|| -> Result<bool, String> {
            if queries::is_issue_tombstoned_by_remote_id(conn, issue_remote_id)
                .map_err(|e| format!("db (tombstone): {e}"))?
            {
                return Ok(false);
            }
            let existing = queries::find_issue_by_remote_id(conn, issue_remote_id)
                .map_err(|e| format!("db (find issue): {e}"))?;
            if let Some(local) = &existing {
                let has_pending = sync_queue::has_pending_entry(conn, "issue", &local.id)
                    .map_err(|e| format!("db: {e}"))?;
                if decide(&local.updated_at, &dto.updated_at, has_pending)
                    == MergeDecision::KeepLocal
                {
                    return Ok(false);
                }
            }
            let links = dto.dependency_links();
            // Only a detail payload carries the full edge set; a row with no
            // dependency lists at all must not prune anything.
            let is_detail = dto.blocking_issues.is_some()
                || dto.blocked_issues.is_some()
                || dto.related_issues.is_some()
                || dto.duplicates.is_some();
            let dto = relax_issue_fks(conn, dto);
            // A detail payload with no `files` key means no files.
            let pulled_files = if is_detail {
                Some(dto.files.clone().unwrap_or_default())
            } else {
                None
            };
            let existing_uuid = existing.map(|i| i.id);
            let recurring_remote_id = dto.recurring_issue_id;
            // Capture the remote label ids before `into_model` consumes dto.
            let label_remote_ids: Vec<i64> = dto.labels.iter().map(|l| l.id).collect();
            let issue: Issue = dto
                .into_model(conn, local_team_id.to_string(), existing_uuid)
                .map_err(|e| format!("convert: {e}"))?;
            queries::upsert_issue(conn, &issue).map_err(|e| format!("db (upsert): {e}"))?;
            // Recurring issues are pulled before issues, so the origin
            // resolves locally; an unresolved id keeps the stored value.
            crate::db::recurring_issues::set_issue_origin(conn, &issue.id, recurring_remote_id)
                .map_err(|e| format!("db (recurring origin): {e}"))?;
            // Reflect the remote label set into the issue_labels join. Labels
            // are pulled before issues, so their local rows already exist; any
            // remote id we can't resolve locally is skipped defensively.
            let local_label_ids = resolve_local_label_ids(conn, &label_remote_ids)?;
            queries::set_issue_labels(conn, &issue.id, &local_label_ids)
                .map_err(|e| format!("db (set issue labels): {e}"))?;
            if let Some(files) = pulled_files {
                crate::db::attachments::replace_synced(
                    conn,
                    crate::db::attachments::OwnerKind::Issue,
                    &issue.id,
                    &files,
                )
                .map_err(|e| format!("db (attachments): {e}"))?;
            }
            pending_links.push((issue.id.clone(), is_detail.then_some(links)));
            Ok(true)
        })();
        if let Err(e) = result {
            sync_log(
                &format!("phase=pull_issues issue_remote_id={issue_remote_id}"),
                "skipped",
                &e.to_string(),
            );
            skipped += 1;
        }
    }
    for (issue_id, links) in &pending_links {
        let Some(links) = links else { continue };
        persist_pulled_dependencies(conn, links);
        // The server's detail set is authoritative: drop synced edges it no
        // longer lists (Rails deletes blocking edges on completion).
        let keep: Vec<i64> = links.iter().map(|l| l.dependency_id).collect();
        if let Err(e) = queries::prune_issue_dependencies(conn, issue_id, &keep) {
            sync_log(
                &format!("phase=pull_issues issue_id={issue_id}"),
                "prune_dependencies_failed",
                &e.to_string(),
            );
        }
    }
    skipped
}

/// Apply one freshly fetched issue (detail row) through the same path the
/// pull uses, so local tombstones and pending local edits are respected
/// (`decide`). A row the pull would skip as unresolvable is an error.
fn apply_fetched_issue(conn: &Connection, local_team_id: &str, dto: IssueDto) -> Result<(), String> {
    match apply_pulled_issues(conn, local_team_id, vec![dto]) {
        0 => Ok(()),
        _ => Err("issue could not be applied locally (see [sync] log)".into()),
    }
}

impl SyncEngine {
    /// Opening an issue does one fresh fetch of that issue (with detail) and
    /// merges it exactly like a pulled row. Fetch errors are returned for the
    /// caller to ignore or surface; local state is untouched on error.
    pub async fn fetch_issue(
        client: &ApiClient,
        team_remote_id: i64,
        local_team_id: &str,
        issue_remote_id: i64,
    ) -> Result<(), String> {
        let dto = issues::get(client, team_remote_id, issue_remote_id)
            .await
            .map_err(|e| format!("issue: {e}"))?;
        let conn = open()?;
        apply_fetched_issue(&conn, local_team_id, dto)
    }
}

/// Store pulled dependency edges. An edge whose endpoint issue isn't present
/// locally (e.g. an issue in another team) is skipped and logged.
fn persist_pulled_dependencies(conn: &Connection, links: &[PulledDependency]) {
    for l in links {
        let find = |remote| queries::find_issue_by_remote_id(conn, remote);
        let (source, target) = match (find(l.source_remote_id), find(l.target_remote_id)) {
            (Ok(Some(s)), Ok(Some(t))) => (s, t),
            _ => {
                sync_log(
                    &format!("phase=pull_issues dependency_remote_id={}", l.dependency_id),
                    "skipped",
                    "endpoint not found locally",
                );
                continue;
            }
        };
        if let Err(e) = queries::upsert_pulled_dependency(
            conn,
            l.dependency_id,
            &source.id,
            &target.id,
            l.kind,
            l.direction,
        ) {
            sync_log(
                &format!("phase=pull_issues dependency_remote_id={}", l.dependency_id),
                "write_failed",
                &e.to_string(),
            );
        }
    }
}

/// Null out optional FKs on an `IssueDto` whose referenced rows aren't present
/// locally. Lane is intentionally NOT relaxed — an issue without a lane has
/// nowhere to render, so the caller treats unresolvable lanes as a hard error.
///
/// This is needed because Rails returns issue creators / assignees that
/// reference users who may not be members of this team (e.g. an admin who
/// created the issue but isn't on the team's roster). Our local schema only
/// stores team members, so resolving those FKs strictly would fail.
/// Map a list of remote label ids to their local UUIDs, skipping any that have
/// no local row yet (labels are pulled before issues, so this should be rare).
fn resolve_local_label_ids(conn: &Connection, remote_ids: &[i64]) -> Result<Vec<String>, String> {
    let mut local = Vec::with_capacity(remote_ids.len());
    for remote_id in remote_ids {
        match queries::find_label_by_remote_id(conn, *remote_id) {
            Ok(Some(label)) => local.push(label.id),
            Ok(None) => {
                sync_log(
                    &format!("phase=pull_issues label_remote_id={remote_id}"),
                    "skipped",
                    "not found locally",
                );
            }
            Err(e) => return Err(format!("db (find label): {e}")),
        }
    }
    Ok(local)
}

fn relax_issue_fks(conn: &Connection, mut dto: IssueDto) -> IssueDto {
    if let Some(r) = &dto.assignee {
        if matches!(queries::find_member_by_remote_id(conn, r.id), Ok(None)) {
            dto.assignee = None;
        }
    }
    if let Some(r) = &dto.creator {
        if matches!(queries::find_member_by_remote_id(conn, r.id), Ok(None)) {
            dto.creator = None;
        }
    }
    if let Some(r) = &dto.project {
        if matches!(queries::find_project_by_remote_id(conn, r.id), Ok(None)) {
            dto.project = None;
        }
    }
    if let Some(r) = &dto.parent_issue {
        if matches!(queries::find_issue_by_remote_id(conn, r.id), Ok(None)) {
            dto.parent_issue = None;
        }
    }
    dto
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::test_support::{
        count, dep_count, detail_dto, feed_comment, feed_project_comment, insert_dep,
        insert_test_project, moved_dto, project_dto, setup_dependency_db, setup_labels_db,
        setup_move_db,
    };

    #[test]
    fn decide_no_pending_overwrites() {
        // Without a pending push, remote is always authoritative — even if
        // local is newer (which means a previous push already drained).
        assert_eq!(
            decide("2026-04-27T12:00:00Z", "2026-04-27T11:00:00Z", false),
            MergeDecision::Overwrite
        );
    }

    #[test]
    fn decide_pending_and_local_newer_keeps_local() {
        assert_eq!(
            decide("2026-04-27T12:00:00Z", "2026-04-27T11:00:00Z", true),
            MergeDecision::KeepLocal
        );
    }

    #[test]
    fn decide_pending_but_remote_newer_overwrites() {
        // Concurrent edit: someone updated remote AFTER our local edit. LWW
        // means remote wins; the queued push will then overwrite again.
        assert_eq!(
            decide("2026-04-27T11:00:00Z", "2026-04-27T12:00:00Z", true),
            MergeDecision::Overwrite
        );
    }

    #[test]
    fn decide_unparseable_timestamps_overwrites() {
        // Defensive: if either timestamp is malformed, prefer remote rather
        // than risk dropping a real update because a clock string was off.
        assert_eq!(
            decide("not-a-date", "2026-04-27T12:00:00Z", true),
            MergeDecision::Overwrite
        );
        assert_eq!(
            decide("2026-04-27T12:00:00Z", "not-a-date", true),
            MergeDecision::Overwrite
        );
    }

    #[test]
    fn pull_targets_follow_token_team_ids() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        for (id, remote, archived) in [
            ("a", Some(1), None),
            ("b", Some(2), None),
            ("c", Some(3), Some(now)),
            ("d", None, None),
        ] {
            conn.execute(
                "INSERT INTO teams (id, name, identifier, remote_id, archived_at, created_at, updated_at)
                 VALUES (?1, ?1, ?1, ?2, ?3, ?4, ?4)",
                rusqlite::params![id, remote, archived, now],
            )
            .unwrap();
        }
        let ids = |t: Vec<PullTarget>| {
            let mut v: Vec<i64> = t.into_iter().map(|t| t.remote_id).collect();
            v.sort();
            v
        };
        // Unscoped: all non-archived Rails-backed teams.
        assert_eq!(ids(select_pull_targets(&conn, None).unwrap()), vec![1, 2]);
        // List: intersection with local teams (99 unknown, 3 archived).
        assert_eq!(
            ids(select_pull_targets(&conn, Some(&[2, 3, 99])).unwrap()),
            vec![2]
        );
        // Empty list: zero teams.
        assert!(select_pull_targets(&conn, Some(&[])).unwrap().is_empty());
    }

    #[test]
    fn list_etag_is_stored_per_team_and_list() {
        let conn = setup_labels_db();
        assert_eq!(queries::get_list_etag(&conn, "t1", "labels").unwrap(), None);

        store_list_etag(&conn, "t1", "labels", Some("W/\"a\""));
        store_list_etag(&conn, "t1", "lanes", Some("W/\"b\""));
        assert_eq!(
            queries::get_list_etag(&conn, "t1", "labels").unwrap().as_deref(),
            Some("W/\"a\"")
        );
        assert_eq!(
            queries::get_list_etag(&conn, "t1", "lanes").unwrap().as_deref(),
            Some("W/\"b\"")
        );
        assert_eq!(queries::get_list_etag(&conn, "t2", "labels").unwrap(), None);

        // Overwrite, then clear when the server stops sending an ETag.
        store_list_etag(&conn, "t1", "labels", Some("W/\"c\""));
        assert_eq!(
            queries::get_list_etag(&conn, "t1", "labels").unwrap().as_deref(),
            Some("W/\"c\"")
        );
        store_list_etag(&conn, "t1", "labels", None);
        assert_eq!(queries::get_list_etag(&conn, "t1", "labels").unwrap(), None);
    }

    fn link(dep: i64, kind: &'static str, s: i64, t: i64) -> PulledDependency {
        PulledDependency {
            dependency_id: dep,
            kind,
            direction: if kind == "blocks" { "blocking" } else { kind },
            source_remote_id: s,
            target_remote_id: t,
        }
    }

    fn dep_row(conn: &Connection, remote: i64) -> (String, String, String, String) {
        conn.query_row(
            "SELECT blocking_issue_id, blocked_issue_id, kind, direction
             FROM issue_dependencies WHERE remote_id = ?1",
            [remote],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap()
    }

    #[test]
    fn project_pull_dedupes_and_advances_cursor() {
        let conn = setup_labels_db();
        let a = project_dto(1, "A", "2026-10-30T10:00:00.000Z");
        let a2 = project_dto(1, "A renamed", "2026-10-30T10:00:01.000Z");
        apply_pulled_projects(&conn, "t1", vec![a, a2], Some("2026-10-30T12:00:00.000000Z"))
            .unwrap();
        let p = queries::find_project_by_remote_id(&conn, 1).unwrap().unwrap();
        assert_eq!(p.name, "A renamed");
        assert_eq!(
            queries::get_pull_cursor(&conn, "t1", "projects").unwrap().as_deref(),
            Some("2026-10-30T12:00:00.000000Z")
        );
        // The issue cursor is independent.
        assert_eq!(queries::get_pull_cursor(&conn, "t1", "issues").unwrap(), None);
    }

    #[test]
    fn project_pull_redelivery_is_idempotent_and_no_header_keeps_cursor() {
        let conn = setup_labels_db();
        queries::set_pull_cursor(&conn, "t1", "projects", "OLD").unwrap();
        for _ in 0..2 {
            apply_pulled_projects(
                &conn,
                "t1",
                vec![project_dto(1, "A", "2026-10-30T10:00:00.000Z")],
                None,
            )
            .unwrap();
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            queries::get_pull_cursor(&conn, "t1", "projects").unwrap().as_deref(),
            Some("OLD")
        );
    }

    #[test]
    fn pull_persists_dependencies_with_remote_id_kind_direction() {
        let conn = setup_dependency_db();
        persist_pulled_dependencies(
            &conn,
            &[link(11, "blocks", 2, 1), link(12, "relates", 1, 3)],
        );
        assert_eq!(
            dep_row(&conn, 11),
            ("i2".into(), "i1".into(), "blocks".into(), "blocking".into())
        );
        assert_eq!(
            dep_row(&conn, 12),
            ("i1".into(), "i3".into(), "relates".into(), "relates".into())
        );
    }

    #[test]
    fn pull_is_idempotent_and_does_not_duplicate_across_both_issues() {
        let conn = setup_dependency_db();
        persist_pulled_dependencies(&conn, &[link(12, "relates", 1, 3)]);
        // The other issue's detail reports the same edge with flipped orientation.
        persist_pulled_dependencies(&conn, &[link(12, "relates", 3, 1)]);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM issue_dependencies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn pull_adopts_remote_id_for_locally_created_edge() {
        let conn = setup_dependency_db();
        conn.execute(
            "INSERT INTO issue_dependencies (id, blocking_issue_id, blocked_issue_id, created_at, updated_at)
             VALUES ('d1', 'i1', 'i2', 'x', 'x')",
            [],
        )
        .unwrap();
        persist_pulled_dependencies(&conn, &[link(40, "blocks", 1, 2)]);
        let remote: Option<i64> = conn
            .query_row(
                "SELECT remote_id FROM issue_dependencies WHERE id = 'd1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remote, Some(40));
    }

    #[test]
    fn pull_skips_edges_to_unknown_issues() {
        let conn = setup_dependency_db();
        persist_pulled_dependencies(&conn, &[link(50, "blocks", 1, 999)]);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM issue_dependencies", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    fn detail_with_files(id: i64, files: serde_json::Value) -> IssueDto {
        let mut dto = detail_dto(id, "two", "2026-05-01T00:00:00.000Z");
        dto.files = Some(serde_json::from_value(files).unwrap());
        dto
    }

    fn file_json(id: i64, name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "blob_id": id + 100, "filename": name, "content_type": "image/png",
            "byte_size": 5, "checksum": "x==", "created_at": "2026-01-01T00:00:00.000Z",
            "download_url": format!("/api/v1/attachments/{id}/download")
        })
    }

    #[test]
    fn pulled_detail_replaces_issue_attachments_and_keeps_pending() {
        use crate::db::attachments::{list_for_owner, OwnerKind};
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        let names = |conn: &Connection| -> Vec<String> {
            let issue = queries::find_issue_by_remote_id(conn, 2).unwrap().unwrap();
            list_for_owner(conn, OwnerKind::Issue, &issue.id)
                .unwrap()
                .into_iter()
                .map(|a| a.filename)
                .collect()
        };
        let first = detail_with_files(2, serde_json::json!([file_json(2, "b"), file_json(1, "a")]));
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![first]), 0);
        assert_eq!(names(&conn), ["a", "b"]);

        let issue = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        conn.execute(
            "INSERT INTO attachments (id, owner_kind, owner_id, filename, created_at, upload_state)
             VALUES ('p1', 'issue', ?1, 'pend', '2026-02-01T00:00:00.000Z', 'pending')",
            rusqlite::params![issue.id],
        )
        .unwrap();
        let second = detail_with_files(2, serde_json::json!([file_json(1, "a"), file_json(3, "c")]));
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![second]), 0);
        assert_eq!(names(&conn), ["a", "c", "pend"]);

        // `files` missing from a detail payload means no files.
        let third = detail_dto(2, "two", "2026-05-01T00:00:00.000Z");
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![third]), 0);
        assert_eq!(names(&conn), ["pend"]);
    }

    #[test]
    fn pulled_project_replaces_attachments_and_keeps_pending() {
        use crate::db::attachments::{list_for_owner, OwnerKind};
        let conn = setup_dependency_db();
        let names = |conn: &Connection| -> Vec<String> {
            let p = queries::find_project_by_remote_id(conn, 9).unwrap().unwrap();
            list_for_owner(conn, OwnerKind::Project, &p.id)
                .unwrap()
                .into_iter()
                .map(|a| a.filename)
                .collect()
        };
        let with = |files: serde_json::Value, ts: &str| {
            let mut d = project_dto(9, "P", ts);
            d.files = Some(serde_json::from_value(files).unwrap());
            d
        };
        let first = with(serde_json::json!([file_json(2, "b"), file_json(1, "a")]), "2026-01-01T00:00:00.000Z");
        apply_pulled_projects(&conn, "t1", vec![first], None).unwrap();
        assert_eq!(names(&conn), ["a", "b"]);

        let p = queries::find_project_by_remote_id(&conn, 9).unwrap().unwrap();
        conn.execute(
            "INSERT INTO attachments (id, owner_kind, owner_id, filename, created_at, upload_state)
             VALUES ('pp1', 'project', ?1, 'pend', '2026-02-01T00:00:00.000Z', 'pending')",
            rusqlite::params![p.id],
        )
        .unwrap();
        let second = with(serde_json::json!([file_json(1, "a"), file_json(3, "c")]), "2026-02-01T00:00:00.000Z");
        apply_pulled_projects(&conn, "t1", vec![second], None).unwrap();
        assert_eq!(names(&conn), ["a", "c", "pend"]);

        // An index row (no `files`) leaves attachments alone.
        let index_row = project_dto(9, "P", "2026-03-01T00:00:00.000Z");
        apply_pulled_projects(&conn, "t1", vec![index_row], None).unwrap();
        assert_eq!(names(&conn), ["a", "c", "pend"]);

        // A detail payload with an empty list clears synced rows only.
        let empty = with(serde_json::json!([]), "2026-04-01T00:00:00.000Z");
        apply_pulled_projects(&conn, "t1", vec![empty], None).unwrap();
        assert_eq!(names(&conn), ["pend"]);
    }

    #[test]
    fn feed_comments_link_issue_and_parent_regardless_of_order() {
        let conn = setup_labels_db();
        conn.execute("UPDATE issues SET remote_id = 50 WHERE id = 'i1'", []).unwrap();
        // Reply delivered before its parent.
        let dtos = vec![
            feed_comment(2, Some(1), Some(50), "reply"),
            feed_comment(1, None, Some(50), "top"),
        ];
        assert_eq!(apply_pulled_comments(&conn, dtos), 0);
        let all = queries::comments_for_issue(&conn, "i1").unwrap();
        assert_eq!(all.len(), 2);
        let top = all.iter().find(|c| c.remote_id == Some(1)).unwrap();
        let reply = all.iter().find(|c| c.remote_id == Some(2)).unwrap();
        assert_eq!(reply.parent_id.as_deref(), Some(top.id.as_str()));
    }

    #[test]
    fn feed_comments_store_files_per_comment() {
        use crate::db::attachments::{list_for_owner, OwnerKind};
        let conn = setup_labels_db();
        conn.execute("UPDATE issues SET remote_id = 50 WHERE id = 'i1'", []).unwrap();
        let mut dto = feed_comment(1, None, Some(50), "with file");
        dto.files = Some(serde_json::from_value(serde_json::json!([file_json(7, "pic.png")])).unwrap());
        assert_eq!(apply_pulled_comments(&conn, vec![dto]), 0);
        let c = queries::find_comment_by_remote_id(&conn, 1).unwrap().unwrap();
        let files = list_for_owner(&conn, OwnerKind::Comment, &c.id).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "pic.png");
        assert_eq!(files[0].remote_id, Some(7));
    }

    #[test]
    fn feed_comments_are_idempotent_and_update_edits() {
        let conn = setup_labels_db();
        conn.execute("UPDATE issues SET remote_id = 50 WHERE id = 'i1'", []).unwrap();
        let dtos = vec![feed_comment(1, None, Some(50), "v1")];
        assert_eq!(apply_pulled_comments(&conn, dtos.clone()), 0);
        assert_eq!(apply_pulled_comments(&conn, dtos), 0);
        assert_eq!(apply_pulled_comments(&conn, vec![feed_comment(1, None, Some(50), "v2")]), 0);
        let all = queries::comments_for_issue(&conn, "i1").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].body, "v2");
    }

    #[test]
    fn feed_comments_skip_ownerless_and_unknown_issue_without_failing() {
        let conn = setup_labels_db();
        conn.execute("UPDATE issues SET remote_id = 50 WHERE id = 'i1'", []).unwrap();
        let dtos = vec![
            feed_comment(1, None, None, "ownerless"),
            feed_comment(2, None, Some(999), "unknown issue"),
            // Parent never seen: stored top-level, not a failure.
            feed_comment(3, Some(77), Some(50), "orphan reply"),
        ];
        assert_eq!(apply_pulled_comments(&conn, dtos), 0);
        let all = queries::comments_for_issue(&conn, "i1").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].parent_id, None);
    }

    #[test]
    fn feed_applies_project_comments_parents_first_and_skips_unknown_project() {
        let conn = setup_labels_db();
        insert_test_project(&conn, "p1", 80);
        // Reply delivered before its parent; one row for a project we lack.
        let dtos = vec![
            feed_project_comment(2, Some(1), 80, "reply"),
            feed_project_comment(1, None, 80, "top"),
            feed_project_comment(3, None, 999, "unknown project"),
        ];
        assert_eq!(apply_pulled_comments(&conn, dtos), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM comments"), 1 + 1);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM comments WHERE project_id='p1' AND issue_id IS NULL"),
            2
        );
        let top = queries::find_comment_by_remote_id(&conn, 1).unwrap().unwrap();
        let reply = queries::find_comment_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(top.parent_id, None);
        assert_eq!(reply.parent_id.as_deref(), Some(top.id.as_str()));
        assert_eq!(top.owner(), crate::db::models::CommentOwner::Project("p1".into()));
    }

    #[test]
    fn pull_cursor_is_stored_per_team_and_resource() {
        let conn = setup_labels_db();
        assert_eq!(queries::get_pull_cursor(&conn, "t1", "issues").unwrap(), None);
        queries::set_pull_cursor(&conn, "t1", "issues", "A").unwrap();
        queries::set_pull_cursor(&conn, "t1", "issues", "B").unwrap();
        queries::set_pull_cursor(&conn, "t1", "comments", "C").unwrap();
        assert_eq!(
            queries::get_pull_cursor(&conn, "t1", "issues").unwrap().as_deref(),
            Some("B")
        );
        assert_eq!(
            queries::get_pull_cursor(&conn, "t2", "issues").unwrap(),
            None
        );
        queries::clear_pull_cursor(&conn, "t1", "issues").unwrap();
        assert_eq!(queries::get_pull_cursor(&conn, "t1", "issues").unwrap(), None);
        assert!(queries::get_pull_cursor(&conn, "t1", "comments").unwrap().is_some());
    }

    #[test]
    fn full_issue_pull_is_the_incremental_request_without_updated_since() {
        let full = issue_pull_query(None, Some(9));
        let incremental = issue_pull_query(Some("2026-10-30T12:00:00Z"), Some(9));
        assert!(full.include_detail);
        assert_eq!(full.updated_since, None);
        assert_eq!(
            issues::IssuePageQuery {
                updated_since: None,
                ..incremental
            }
            .query_string(),
            full.query_string()
        );
    }

    #[test]
    fn dedupe_keeps_last_delivery_in_first_seen_order() {
        let rows = vec![(1, "a"), (2, "b"), (1, "c")];
        assert_eq!(dedupe_by_id(rows, |r| r.0), vec![(1, "c"), (2, "b")]);
    }

    #[test]
    fn detail_rows_set_description_parent_dependencies_and_lifecycle() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        let dtos = vec![
            detail_dto(2, "two", "2026-05-01T00:00:00.000Z"),
            // Overlap re-delivery of the same row is idempotent.
            detail_dto(2, "two", "2026-05-01T00:00:00.000Z"),
        ];
        let dtos = dedupe_by_id(dtos, |d| d.id);
        assert_eq!(apply_pulled_issues(&conn, "t1", dtos.clone()), 0);
        assert_eq!(apply_pulled_issues(&conn, "t1", dtos), 0);
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(i.description.as_deref(), Some("desc 2"));
        assert_eq!(i.parent_issue_id.as_deref(), Some("i1"));
        assert_eq!(i.started_at.as_deref(), Some("2026-02-01T00:00:00.000Z"));
        assert_eq!(i.completed_at.as_deref(), Some("2026-03-01T00:00:00.000Z"));
        assert_eq!(i.canceled_at, None);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issues WHERE remote_id = 2"), 1);
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM issue_dependencies WHERE remote_id = 55"),
            1
        );
    }

    fn detail_without_links(id: i64, updated_at: &str) -> IssueDto {
        let mut dto = detail_dto(id, "two", updated_at);
        dto.blocking_issues = Some(vec![]);
        dto
    }

    #[test]
    fn pulled_detail_missing_an_edge_removes_it_on_either_side() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        // i2 (remote 2) is pulled. Stale edges on both sides, mixed kinds.
        insert_dep(&conn, "out", Some(61), "i2", "i3");
        insert_dep(&conn, "in", Some(62), "i1", "i2");
        conn.execute(
            "UPDATE issue_dependencies SET kind='relates', direction='relates' WHERE id='in'",
            [],
        )
        .unwrap();
        // An edge not touching i2 must survive.
        insert_dep(&conn, "other", Some(63), "i1", "i3");
        let dto = detail_without_links(2, "2026-05-01T00:00:00.000Z");
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![dto]), 0);
        assert_eq!(dep_count(&conn), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issue_dependencies WHERE id='other'"), 1);
    }

    #[test]
    fn pulled_detail_keeps_pending_local_edges_without_remote_id() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        insert_dep(&conn, "pending", None, "i2", "i3");
        let dto = detail_without_links(2, "2026-05-01T00:00:00.000Z");
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![dto]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issue_dependencies WHERE id='pending'"), 1);
    }

    #[test]
    fn pulled_detail_keeps_edges_the_server_still_has() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        // detail_dto(2) reports remote issue 3 blocking it via dependency 55.
        insert_dep(&conn, "kept", Some(55), "i3", "i2");
        insert_dep(&conn, "stale", Some(56), "i2", "i1");
        let dto = detail_dto(2, "two", "2026-05-01T00:00:00.000Z");
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![dto]), 0);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issue_dependencies WHERE id='kept'"), 1);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issue_dependencies WHERE id='stale'"), 0);
    }

    fn team_pull(team: &str, full_listing: bool, seen: &[i64]) -> IssueTeamPull {
        IssueTeamPull {
            local_team_id: team.into(),
            full_listing,
            seen: seen.to_vec(),
        }
    }

    fn assert_moved_not_purged_or_duplicated(conn: &Connection) {
        let i = queries::find_issue_by_remote_id(conn, 2).unwrap().unwrap();
        assert_eq!((i.id.as_str(), i.team_id.as_str(), i.lane_id.as_str()), ("i2", "t2", "l2"));
        assert_eq!(i.team_number, Some(9));
        assert_eq!(count(conn, "SELECT COUNT(*) FROM issues WHERE remote_id = 2"), 1);
        assert_eq!(count(conn, "SELECT COUNT(*) FROM issues WHERE team_id='t1' AND id='i2'"), 0);
    }

    #[test]
    fn moved_issue_follows_when_source_is_pulled_before_target() {
        let conn = setup_move_db();
        let pulls = [team_pull("t1", true, &[1, 3]), team_pull("t2", true, &[2])];
        assert_eq!(apply_pulled_issues(&conn, "t2", vec![moved_dto()]), 0);
        purge_unlisted_issues(&conn, &pulls, true);
        assert_moved_not_purged_or_duplicated(&conn);
    }

    #[test]
    fn moved_issue_follows_when_target_is_pulled_before_source() {
        let conn = setup_move_db();
        let pulls = [team_pull("t2", true, &[2]), team_pull("t1", true, &[1, 3])];
        assert_eq!(apply_pulled_issues(&conn, "t2", vec![moved_dto()]), 0);
        purge_unlisted_issues(&conn, &pulls, true);
        assert_moved_not_purged_or_duplicated(&conn);
    }

    #[test]
    fn issue_missing_from_only_the_source_listing_is_not_purged() {
        let conn = setup_move_db();
        // Only the source team was pulled (target skipped or failed).
        let pulls = [team_pull("t1", true, &[1, 3])];
        purge_unlisted_issues(&conn, &pulls, false);
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(i.team_id, "t1");
    }

    #[test]
    fn issue_gone_from_every_listing_is_purged_with_its_queued_edits() {
        let conn = setup_move_db();
        crate::db::sync_queue::enqueue(&conn, "issue", "i2", "update", None).unwrap();
        let pulls = [team_pull("t1", true, &[1, 3]), team_pull("t2", true, &[])];
        purge_unlisted_issues(&conn, &pulls, true);
        assert!(queries::find_issue_by_remote_id(&conn, 2).unwrap().is_none());
        assert!(queries::find_issue_by_remote_id(&conn, 3).unwrap().is_some());
        assert_eq!(
            count(&conn, "SELECT COUNT(*) FROM sync_queue WHERE entity_id = 'i2' AND synced_at IS NULL"),
            0
        );
    }

    #[test]
    fn incremental_listing_purges_nothing() {
        let conn = setup_move_db();
        let pulls = [team_pull("t1", false, &[1, 3]), team_pull("t2", true, &[])];
        purge_unlisted_issues(&conn, &pulls, true);
        assert!(queries::find_issue_by_remote_id(&conn, 2).unwrap().is_some());
    }

    #[test]
    fn incomplete_pull_purges_nothing() {
        let conn = setup_move_db();
        let pulls = [team_pull("t1", true, &[]), team_pull("t2", true, &[])];
        purge_unlisted_issues(&conn, &pulls, false);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM issues WHERE remote_id IS NOT NULL"), 3);
    }

    #[test]
    fn pulled_issue_records_the_recurring_issue_that_spawned_it() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        crate::db::recurring_issues::apply_pulled(
            &conn,
            "t1",
            &[serde_json::from_value(serde_json::json!({
                "id": 5, "title": "Weekly", "description": null, "priority": "no_priority",
                "frequency": "weekly", "interval": 1, "weekday": 1, "day_of_month": null,
                "time_zone": null, "active": true, "next_run_on": null, "last_run_at": null,
                "assignee": null, "project": null, "lane": null, "label_ids": [],
                "creator": null, "created_at": "2026-01-01T00:00:00.000Z",
                "updated_at": "2026-01-01T00:00:00.000Z",
            }))
            .unwrap()],
            Some("s"),
        )
        .unwrap();
        let mut dto = detail_dto(2, "two", "2026-05-01T00:00:00.000Z");
        dto.recurring_issue_id = Some(5);
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![dto]), 0);
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        let origin = crate::db::recurring_issues::origin_of_issue(&conn, &i.id).unwrap().unwrap();
        assert_eq!(origin.title, "Weekly");

        // Server detached it: cleared.
        let back = detail_dto(2, "two", "2026-06-01T00:00:00.000Z");
        assert_eq!(apply_pulled_issues(&conn, "t1", vec![back]), 0);
        assert_eq!(crate::db::recurring_issues::origin_id(&conn, &i.id).unwrap(), None);
    }

    #[test]
    fn pending_local_edit_survives_incremental_apply() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        conn.execute(
            "UPDATE issues SET title = 'local', updated_at = '2026-09-01T00:00:00.000Z' WHERE id = 'i2'",
            [],
        )
        .unwrap();
        sync_queue::enqueue(&conn, "issue", "i2", "update", None).unwrap();
        let skipped = apply_pulled_issues(
            &conn,
            "t1",
            vec![detail_dto(2, "remote", "2026-05-01T00:00:00.000Z")],
        );
        assert_eq!(skipped, 0);
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(i.title, "local");
    }

    #[test]
    fn fetched_issue_updates_a_clean_local_row() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        apply_fetched_issue(&conn, "t1", detail_dto(2, "fresh", "2026-05-01T00:00:00.000Z"))
            .unwrap();
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(i.title, "fresh");
    }

    #[test]
    fn fetched_issue_keeps_pending_local_edit() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        conn.execute(
            "UPDATE issues SET title = 'local', updated_at = '2026-09-01T00:00:00.000Z' WHERE id = 'i2'",
            [],
        )
        .unwrap();
        sync_queue::enqueue(&conn, "issue", "i2", "update", None).unwrap();
        apply_fetched_issue(&conn, "t1", detail_dto(2, "remote", "2026-05-01T00:00:00.000Z"))
            .unwrap();
        let i = queries::find_issue_by_remote_id(&conn, 2).unwrap().unwrap();
        assert_eq!(i.title, "local");
    }

    #[test]
    fn fetched_issue_does_not_resurrect_a_local_tombstone() {
        let conn = setup_dependency_db();
        conn.execute("UPDATE lanes SET remote_id = 10", []).unwrap();
        queries::delete_issue(&conn, "i2").unwrap();
        apply_fetched_issue(&conn, "t1", detail_dto(2, "remote", "2026-12-01T00:00:00.000Z"))
            .unwrap();
        let (title, deleted): (String, Option<String>) = conn
            .query_row("SELECT title, deleted_at FROM issues WHERE id = 'i2'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_ne!(title, "remote");
        assert!(deleted.is_some());
    }

    #[test]
    fn fetched_issue_that_cannot_be_applied_is_an_error() {
        let conn = setup_dependency_db(); // lane has no remote id
        assert!(apply_fetched_issue(&conn, "t1", detail_dto(2, "x", "2026-05-01T00:00:00.000Z"))
            .is_err());
    }

    #[test]
    fn unresolvable_row_counts_as_skipped() {
        let conn = setup_dependency_db(); // lane has no remote id -> lane lookup fails
        let skipped = apply_pulled_issues(
            &conn,
            "t1",
            vec![detail_dto(2, "x", "2026-05-01T00:00:00.000Z")],
        );
        assert_eq!(skipped, 1);
    }
}

#[cfg(test)]
mod issue_pull_live {
    use super::*;

    /// Live check of the issue pull against local Rails (:3005), run with
    /// `--ignored` and MTASKS_TEST_DATA_DIR pointed at an empty scratch dir.
    /// Pulls only, so nothing is written to Rails. A full pull mirrors the
    /// listing with no duplicates, an incremental pull keeps it stable, and a
    /// synced issue the server doesn't list is purged on the next full pull.
    #[tokio::test]
    #[ignore]
    async fn full_and_incremental_pulls_match_the_server() {
        let (cfg, _) = crate::config::load();
        let client = ApiClient::new(cfg.api_base_url.trim(), cfg.api_token.unwrap_or_default()).unwrap();
        let count = |sql: &str| -> i64 {
            open().unwrap().query_row(sql, [], |r| r.get(0)).unwrap()
        };
        const DUPLICATES: &str = "SELECT COUNT(*) FROM (SELECT remote_id FROM issues
            WHERE remote_id IS NOT NULL GROUP BY remote_id HAVING COUNT(*) > 1)";

        let report = SyncEngine::full_cycle(&client).await;
        assert_eq!(report.pull.last_error, None);
        let full = count("SELECT COUNT(*) FROM issues");
        assert!(full > 0, "the local server should have issues to pull");
        assert_eq!(count(DUPLICATES), 0);
        eprintln!("full pull: {full} issues");

        let report = SyncEngine::full_cycle(&client).await;
        assert_eq!(report.pull.last_error, None);
        assert_eq!(count("SELECT COUNT(*) FROM issues"), full);
        assert_eq!(count(DUPLICATES), 0);

        let conn = open().unwrap();
        let now = crate::db::now_iso();
        conn.execute(
            "INSERT INTO issues (id, title, priority, lane_id, team_id, remote_id, created_at, updated_at)
             SELECT 'ghost', 'ghost', 0, lane_id, team_id, 2147483647, ?1, ?1 FROM issues LIMIT 1",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute("DELETE FROM pull_cursors WHERE resource = 'issues'", []).unwrap();
        let report = SyncEngine::full_cycle(&client).await;
        assert_eq!(report.pull.last_error, None);
        assert_eq!(count("SELECT COUNT(*) FROM issues WHERE id = 'ghost'"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM issues"), full);
    }
}
