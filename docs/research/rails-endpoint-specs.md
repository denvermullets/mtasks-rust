# Rails `api/v1` endpoint specs for desktop parity gaps

Ticket: JRST-3 (issue 5345). Source of truth: the read-only Rails repo at `/Users/denvermullets/Documents/dev/pers/mtasks` (HEAD `942035e`). Paths below are relative to that repo. Hourglass is out of scope.

## 0. Conventions every new endpoint should follow

Derived from `app/controllers/api/v1/base_controller.rb` and the existing controllers.

- Controllers go in `app/controllers/api/v1/`, inherit `Api::V1::BaseController`, and serialize through plain-Ruby classes in `app/serializers/api/v1/` (`IssueSerializer.new(x, detailed:).as_json`).
- Auth is `Authorization: Bearer <token>`. `ApiToken.authenticate` hashes the token; the user is `Current.user`. Scope is `read` for GET/HEAD and `write` for everything else; a missing scope returns 403.
- Team scoping goes through `accessible_teams` (`token.filter_teams(current_user.teams.not_archived)`). `set_current_team` returns 404 `{error:'Not Found', message}` for a team outside the token's set. Tokens with no `api_token_teams` rows are unscoped. Ids the client sends for related records must pass `foreign_team_references` (extend `team_reference_scopes` when a new key is accepted).
- Errors: 404 `{error:'Not Found', message}`; 422 via `render_validation_errors` gives `{error:'Unprocessable Entity', errors:[...]}`; 401/403 as above.
- Lists are bare JSON arrays today (no envelope, no pagination). Ids are integers, timestamps ISO8601. Where a spec needs metadata (unread count, server time, cursors) see cross-cutting question 1.
- Writes call `track_api_feature(...)` (VektisApiTracking). New write endpoints should too; the HTML controllers show the slug to reuse.
- `configure_paper_trail_whodunnit` already runs for API writes, so version attribution works for free.
- Already in use by the desktop: `GET /me`, teams, issues index/show/create/update, `by_identifier`, lanes, labels index/create, members, projects CRUD, comments index/create, dependencies, decisions.

Serializer gaps several groups depend on:

- `IssueSerializer` base JSON has no `team_id`, `team_number`, `archived_at`, or `recurring_issue_id`, and `parent_issue` is detail-only. Add `team_id`, `team_number`, `archived_at`, `recurring_issue_id` to the base JSON (additive, safe).
- Issue `updated_at` is not bumped by label, dependency, comment or attachment changes (see group A).

---

## Group A. Sync primitive (incremental pull, replaces the N+1 detail pull)

Size: **M** (L if tombstones are done properly).

### A1. `GET /api/v1/teams/:team_id/issues` (extend existing)

Existing: `Api::V1::IssuesController#index` returns `current_team.issues.not_archived`, ordered `created_at desc`, base shape only. The base shape has no description or dependencies, which is why the desktop calls `show` per issue. No `updated_since` exists today.

New optional params (backward compatible):

| param | meaning |
|---|---|
| `updated_since` | ISO8601. `where('issues.updated_at > ?', t)`, ordered `updated_at asc, id asc` when present. |
| `include` | `detail` renders `IssueSerializer(detailed: true)`, removing the N+1. Preload with `Issue.with_links` plus `blocked_dependencies/blocking_dependencies` to avoid a server-side N+1. |
| `include_archived` | `1` drops the `not_archived` filter so archive transitions are delivered (rows carry `archived_at`). |
| `limit`, `after_id` | Keyset pagination on `(updated_at, id)`. Default 200, max 500. |

Response: array of issue JSON plus the new base fields. Response header `X-Server-Time: <ISO8601>` so the client uses the server clock as the next cursor (avoids skew); the client overlaps a few seconds and dedupes by id. Pagination continuation via a `Link` header or `X-Next-After-Id`.

Auth/scoping: `set_current_team` as today; read scope.
Reuse: `Issue`, `IssueSerializer`, `filter_issues`.

Rails work and design questions:

1. **Does `updated_at` move when it should?** Label joins (`IssueLabel`), `IssueDependency`, `Comment`, attachments, and `IssueTeamMover` side effects do not touch the issue unless `touch: true` is set on the `belongs_to`. Options: (a) add `touch: true` on `IssueLabel#issue`, `IssueDependency` (both issues), `Comment#issue`; or (b) a separate `changes` feed. Recommend (a). Touching does not create paper_trail versions, because `has_paper_trail only:` restricts tracked attributes.
2. **Index** `issues(team_id, updated_at)`; none is evident for it.
3. **Deletions.** `Issue#destroy` is a hard delete, so an incremental pull cannot see it. Options: a `deleted_records` table (`record_type, record_id, team_id, deleted_at`) written from `after_destroy`, exposed as `GET /teams/:team_id/deleted?since=`; or reuse paper_trail destroy versions for `Issue`/`IssueDependency` (they already have `has_paper_trail`). Needed for Issue, Comment, Label, Project, SavedView, Dashboard/Group, RecurringIssue and attachments. Needs a Rails-side decision.
4. **Team move** (group H4) looks like delete-from-source plus create-in-target. The id is unchanged, so the desktop can reconcile by `id` across teams, but the source team's feed should also surface it.
5. Cap and pagination for large teams.

### A2. `GET /api/v1/teams/:team_id/comments?updated_since=` (optional)

One call for all team comments instead of `issues/:id/comments` per issue. `Comment` has exactly one owner (`issue_id` xor `project_id`), `parent_id`, `body`, timestamps. Response: flat list with `issue_id`/`project_id`, `parent_id`, `user`, `files` (group B). Scope via `current_team.issues`/`projects`. Size S. Tombstones as in A1.

### A3. Small lists (`projects`, `labels`, `lanes`, `members`)

`projects` already emits `updated_at`; add `updated_since`. For labels, lanes and members, which are small, add ETag/`If-None-Match` (`fresh_when`) rather than a cursor. Size S.

---

## Group B. Attachments and images

Size: **L**. Needs a transport decision.

### Facts

- `Issue`, `Comment`, `Project` each have `has_many_attached :files` (`issue.rb:32`, `comment.rb:10`, `project.rb:20`). Production storage is S3 (`config/storage.yml`, `config.active_storage.service = :amazon`).
- `ImageUploadsController < ActiveStorage::DirectUploadsController` (`POST /image_uploads`) includes `Authentication` (session cookie) and only accepts `image/*`, `byte_size` 1..10 MB. Tokens cannot reach it.
- HTML attaches files through form params (`comment_params` permits `files: []`). Only projects have an HTML purge (`DELETE /teams/:team_id/projects/:id/purge_file?file_id=`). No issue-file or comment-file purge route exists in `routes.rb`.
- The API `issue_params`, `comment_params` and project params do not permit `files`, and serializers do not emit attachments.
- How pasted markdown images reference the blob is not established here (see `app/services/markdown_renderer.rb`). Open question.

### Proposed endpoints

Two-step direct upload (works with S3, keeps bytes out of Rails):

1. `POST /api/v1/direct_uploads`, body `{blob:{filename, byte_size, checksum, content_type}}`, response `{signed_id, key, filename, content_type, byte_size, direct_upload:{url, headers}}`. Implement as `Api::V1::DirectUploadsController` with BaseController token auth (the stock class inherits `ActiveStorage::BaseController`, so auth must be mixed in), reusing the `require_image` style guard but with a broader allowlist and a larger cap (e.g. 25 MB). Write scope.
2. Attach by `signed_id`:
   - `POST /api/v1/teams/:team_id/issues/:issue_id/attachments` `{signed_ids:[...]}` -> 201 with attachment JSON.
   - `POST /api/v1/teams/:team_id/projects/:project_id/attachments`, same.
   - Comments: accept `files:[signed_id,...]` on the existing issue and project comment `create` (`@comment.files.attach`). `Comment#body_or_files_present` already permits body-less comments with files.
   - `DELETE .../issues/:issue_id/attachments/:id` and `.../projects/:project_id/attachments/:id` (`:id` is the `ActiveStorage::Attachment` id); reuse `ProjectsController#purge_file` logic (`files.find(id).purge`). Comment attachment delete: `.../comments/:comment_id/attachments/:id`, author or team admin only.

Alternative: multipart `files[]` on the same endpoints (double hop through Rails; acceptable fallback for small files). Rails side chooses.

Attachment JSON:

```json
{ "id": 12, "blob_id": 9, "filename": "a.png", "content_type": "image/png",
  "byte_size": 1234, "checksum": "...", "created_at": "...", "download_url": "/api/v1/attachments/12/download" }
```

Download: `GET /api/v1/attachments/:id/download` checks that the attachment's record (Issue/Comment/Project) belongs to an accessible team, then `redirect_to attachment.url(expires_in: 5.minutes)` (S3 presigned). Fallback is the signed `rails_blob_url`, which is unauthenticated but unguessable. Recommend the authenticated variant so revoked tokens lose access.

Serializers: add `files: [...]` to `IssueSerializer` (detail), `CommentSerializer`, `ProjectSerializer` (detail); preload `with_attached_files`. Touch the owner on attach/detach so incremental sync sees it (A1).

Open questions: direct upload vs multipart; allowed types and size cap; image variants (skip v1); markdown-embedded image references and whether the desktop can resolve them without a session cookie; `host` config for URL generation inside `ActionController::API`.

---

## Group C. Notifications (inbox)

Size: **S/M**.

### Facts

- `Notification` (`app/models/notification.rb`): `user`, `actor`, `issue`, optional `comment`; columns `action`, `message`, `read_at`, `version_id`, timestamps. Indexes `(user_id, created_at)` and `(user_id, read_at)`.
- HTML `index` = `Current.user.notifications.recent` (limit 50, newest first). `mark_as_read` uses `Notification#mark_as_read!` (idempotent). `mark_all_as_read` is `unread.update_all(read_at:)`, **which does not bump `updated_at`**, so an `updated_since` feed would miss it. Set `updated_at` in that `update_all`, preferably by extracting a model method both controllers use.
- Notifications are user-owned; the scoping unit is `notification.issue.team_id`.

### Endpoints

`GET /api/v1/notifications`
- Params: `updated_since` (filters `updated_at > t`, order `updated_at asc`), `unread=1`, `limit` (default 50, max 200), `before_id` (newest-first backfill when no `updated_since`).
- Response (an envelope is justified here, see cross-cutting question 1):

```json
{ "notifications": [
    { "id": 1, "action": "commented", "message": "...", "read_at": null,
      "created_at": "...", "updated_at": "...",
      "actor": {"id": 3, "name": "..."},
      "issue": {"id": 10, "identifier": "ABC-12", "title": "...", "team_id": 2},
      "comment_id": 55, "version_id": null } ],
  "meta": { "unread_count": 4, "server_time": "..." } }
```

- Polling: `unread_count` and `server_time` ride along on every list. Emit an `ETag` over `[unread_count, max(updated_at)]` and honour `If-None-Match` with 304.

`GET /api/v1/notifications/unread_count` -> `{unread_count: n}` (cheap poll; uses the `(user_id, read_at)` index; ETag-able).

`PATCH /api/v1/notifications/:id/read` -> 200 with the notification JSON; 404 if not the user's; idempotent. Track `notification`/`read` (attributed to the issue's team, as HTML does).

`PATCH /api/v1/notifications/read_all` -> `{marked: n, unread_count: 0}`. Optional `team_id` and optional `before` timestamp so notifications that arrived after the desktop last looked are not cleared. Track `notification`/`read_all`.

Auth/scoping: `Current.user.notifications.joins(:issue).where(issues: {team_id: accessible_teams.select(:id)})`, so a team-scoped token never sees, or clears, other teams' notifications. `read` for GET, `write` for PATCH.

Reuse: `Notification`, `NotificationService`, `MentionNotifying`.

Open questions: retention (the web shows only the latest 50; the API would expose all); issue deletion destroys notifications, so the client needs tombstones or must drop notifications whose issue is gone (A1).

---

## Group D. Saved views, home page and dashboards

Sizes: saved views + home page **S/M**; dashboards **M/L**.

### D1. Saved views

Facts (`saved_view.rb`, `saved_views_controller.rb`): per user and team; `name` (<=80); `query` is a hash restricted to `SavedView::QUERY_KEYS` (view_mode, group_by, sub_group_by, order_by, completed_filter, visible_properties, show_* flags, lane_ids, assignee_ids, creator_ids, label_ids, project_ids, priority, q), values stored as strings (`query_from` keeps the last value per key); `position`. HTML routes are `POST/PATCH/DELETE /teams/:team_id/saved_views`, every lookup through `current_user.saved_views.where(team:)`.

Endpoints:
- `GET /api/v1/saved_views` (cross-team, for the sidebar), optional `team_id`, `updated_since`; limited to `accessible_teams`. Response `[{id, team_id, name, query:{...}, position, created_at, updated_at}]`.
- `POST /api/v1/teams/:team_id/saved_views` `{saved_view:{name, query}}`. `query` as a JSON object or a query string, normalised via `SavedView.query_from` (`to_query` first for an object). `position = max+1`. 201.
- `PATCH /api/v1/saved_views/:id` `{saved_view:{name?, query?}}`, resolved through `current_user.saved_views.where(team_id: accessible_teams)`.
- `DELETE /api/v1/saved_views/:id` -> 204 (`User#home_view` already tolerates a missing view).
- Reorder: the web has no reorder UI, only the `position` column; skip unless wanted.

The `query` is web board filter state; the desktop maps it to its own filter model. Size S.

### D2. Home page

Facts: stored in `users.settings` as exactly one of `home_team_id`, `home_view_id`, `home_dashboard_id` (`Settings::HomePageController`, `User#home_path`). Validation: team in `user_teams`, view in the user's saved views on those teams, dashboard in the user's dashboards. Blank = default.

Endpoints:
- Read: add `home_page: {kind, id}` to the existing `GET /me` (and optionally `GET /me/home_page`).
- `PUT /api/v1/me/home_page` `{home: "team:3"}` (the form's format) or `{kind, id}`; blank clears. Extract `valid_target?`/`save_home` into `User#set_home_page(kind, id)` or a service so both controllers share it. Write scope; team and view targets must be in `accessible_teams`.

Size S. Question: does the desktop need to write this, or is read-only enough?

### D3. Dashboards (user-owned, not team-scoped)

Facts: `Dashboard` (`name`<=80, `description`, `position`), `DashboardGroup` (`name`, `description`, `color` matching `/\A#\h{6}\z/` from `DashboardGroup::COLORS`, `position`), `DashboardGroupSource` (`source_type` Team/Project/Label, `source_id`, `include_all`). `DashboardGroupsController#permitted_sources` intersects submitted ids with the user's non-archived teams and their projects/labels, and `all_*` ids only count if also sources; `DashboardGroup#replace_sources!` syncs them. Issue resolution lives in `DashboardIssuesQuery` (filters `today|week|hot|open`, project due-date fallback `COALESCE(issues.due_date, projects.due_date)`, `DashboardRefinements`; recent commits hide cancelled and done issues).

Endpoints (all through `current_user.dashboards`, foreign ids 404):
- `GET /api/v1/dashboards` -> `[{id, name, description, position, updated_at, groups:[{id, name, description, color, position, team_ids, project_ids, label_ids, all_team_ids, all_project_ids}]}]`. Preload `groups: :sources` (the readers use the loaded association).
- `GET /api/v1/dashboards/:id`, `POST /api/v1/dashboards` `{dashboard:{name, description}}` (position max+1), `PATCH`, `DELETE` (204, groups cascade).
- `POST /api/v1/dashboards/:dashboard_id/groups` `{dashboard_group:{name, description, color, team_ids, project_ids, label_ids, all_team_ids, all_project_ids}}`. Extract `permitted_sources` from the web controller into a shared object (e.g. `DashboardGroups::SourceSanitizer.new(user)`); save in a transaction `group.save && group.replace_sources!(...)`. 201.
- `PATCH .../groups/:id`, `DELETE .../groups/:id` (204).
- `PATCH .../groups/:id/move` `{direction: "up"|"down"}` via `DashboardGroup#move!`; 400 otherwise (as HTML).
- `PATCH .../groups/reorder` `{ids:[...]}` via `Dashboard#reorder_groups!`; return the ordered group list (HTML returns 204).
- Optional evaluation: `GET /api/v1/dashboards/:id/issues?filter=&mine=&search=&team_id=&sort=&assignee=&label=&status=` returning `{groups:[{group_id, inaccessible, issues:[...]}], due_today_count, overdue_count}` via `DashboardIssuesQuery.call`. Recommend shipping config endpoints first and evaluating locally on the desktop, adding this (size M) if the rules drift.

Auth/scoping: user-level, but sources reference teams. For team-scoped tokens, intersect sources with `accessible_teams` on write and drop inaccessible sources on read. Open question: should team-scoped tokens see dashboards at all?

Reorder and move use `update_column(:position)` so `updated_at` does not move; touch the dashboard in `move!`/`reorder_groups!` or use an ETag on the full (small) config list.

---

## Group E. Activity history

Size: **M**.

### Facts

- `Issue` has `has_paper_trail only: %i[title description lane_id priority estimate due_date assignee_id project_id parent_issue_id team_id]`. `IssueDependency` has `has_paper_trail`. **`Project` has no paper_trail**: `app/views/projects/_activity.html.erb` is a placeholder, so project activity needs `has_paper_trail` added to `Project` first (existing projects will have no history).
- The issue timeline is `VersionDescriptionService.timeline(issue)`: issue versions where `event != 'create'`, dependency versions touching the issue (matched on `blocking_issue_id`/`blocked_issue_id` in `object_changes`/`object`), top-level comments, and incoming references, sorted by `created_at`. Descriptions are server-generated English strings. A team move is described only as the move.
- API writes set `whodunnit` to the user id string. Notifications carry `version_id`.

### Endpoints

`GET /api/v1/teams/:team_id/issues/:issue_id/activity`
- Params: `since` (`created_at >`), `limit`, `before_id`, `types` (any of `versions,dependencies,comments,references`).
- Response, oldest first:

```json
[ {"type":"version","id":801,"created_at":"...","actor":{"id":3,"name":"..."},"event":"update",
   "changes":[{"field":"lane_id","from":{"id":1,"name":"Backlog"},"to":{"id":2,"name":"Doing"}},
              {"field":"priority","from":"low","to":"high"}],
   "description":"moved to Doing"},
  {"type":"dependency","id":..,"event":"create","kind":"blocks","other_issue":{"id":..,"identifier":".."},"actor":..},
  {"type":"comment","id":55,"comment":{...CommentSerializer...}},
  {"type":"reference","id":..,"source_issue":{"id":..,"identifier":".."},"user":..} ]
```

Return structured `changes` (resolve lane/assignee/project/parent/team ids to `{id, name|identifier}`) alongside the server `description`. Verify whether `object_changes` stores priority as enum integer or string. Extract an entry serializer from `VersionDescriptionService`. Read scope; issue resolved through `current_team.issues`.

`GET /api/v1/teams/:team_id/projects/:project_id/activity`: only after `has_paper_trail` is added to `Project` (decide tracked columns, e.g. name, status, priority, lead_id, dates, roadmap_commitment), optionally merging comments and decisions. Same entry shape. Size S after the model change.

Optional team-wide feed `GET /teams/:team_id/activity?since=`: `versions` has no `team_id` column, so it needs a denormalised column or an `item_id IN (team issues)` join. Skip unless the desktop has a team-wide activity view.

Open questions: retention and version volume; `versions.object_changes` column type (the timeline query uses jsonb operators, so jsonb is assumed).

---

## Group F. Global search

Size: **S**.

### Facts

- `IssuesController#search` (`GET /teams/:team_id/issues/search?q=&exclude_id=`) returns `[{id, identifier, title}]`, limit 20, over `current_team.issues.not_archived` via `Issue.matching_search(q)`.
- `matching_search`: a blank term returns all. A term matching `IDENTIFIER_SEARCH` (`/\A(?:([A-Za-z0-9]{3,4})-)?(\d+)\z/`, e.g. `VEK-529` or `529`) filters by `team_number`, plus `teams.identifier = PREFIX.upcase` through a join when a prefix is given. Otherwise `ILIKE %term%` on `issues.title` (and `description` when `include_description: true`, unused by the endpoint). Postgres-specific.
- The API has `GET /issues/by_identifier/:identifier` (exact, `[A-Z]+-\d+`) but no search.

### Endpoint

`GET /api/v1/issues/search`
- Params: `q` (required), `team_id` (optional; omitted = all `accessible_teams`), `exclude_id`, `include_description=1`, `limit` (default 20, max 50), `include_archived=1`.
- Response `[{id, identifier, title, team_id, lane:{id,name}, priority, updated_at}]` (core fields compatible with the web picker).
- Scope: `Issue.where(team_id: accessible_teams.select(:id)).matching_search(q)`; with `team_id`, use `set_current_team` semantics. Add an explicit order (e.g. `updated_at desc`). A bare number such as `529` can match one issue per team, which is intended. Optionally mirror as `GET /teams/:team_id/issues/search`; implement once in a shared concern. The same endpoint with `exclude_id` serves the dependency picker (the web has a team-scoped `issue_dependencies#search`).
- Reuse: `Issue.matching_search`, `IDENTIFIER_SEARCH`, `Issue#identifier`.

Open questions: whether the desktop needs server search given local data (useful for unsynced issues); title vs description (description search wants a trigram index).

---

## Group G. Recurring issues

Size: **M**.

### Facts

`RecurringIssue` (`recurring_issue.rb`): `team`, `creator`, optional `assignee`/`project`/`lane`; `title`, `description`, `priority` (enum matching Issue), `frequency` (daily 0, weekly 1, monthly 2), `interval` 1..365, `weekday` 0..6 (weekly), `day_of_month` 1..31 (monthly), `time_zone` (must be an `ActiveSupport::TimeZone` name), `active`, `next_run_on`, `last_run_at`, `label_ids`. `schedule_from(start_date)` computes the first `next_run_on`; `spawn!` creates the issue (background job; the desktop never spawns). HTML controller is member-level, `except: show`.

### Endpoints (under teams)

- `GET /api/v1/teams/:team_id/recurring_issues` (supports `updated_since`):

```json
{ "id":1, "title":"...", "description":"...", "priority":"medium",
  "frequency":"weekly", "interval":1, "weekday":3, "day_of_month":null,
  "time_zone":"Mountain Time (US & Canada)", "active":true,
  "next_run_on":"2026-11-05", "last_run_at":null,
  "assignee":{"id":1,"name":"..."}, "project":{...}, "lane":{...},
  "label_ids":[1,2], "schedule_summary":"Every week on Wednesday",
  "creator":{...}, "created_at":"...", "updated_at":"..." }
```
- `POST` body `{recurring_issue:{title, description, priority, assignee_id, project_id, lane_id, frequency, interval, weekday, day_of_month, time_zone, active, label_ids:[], starts_on:"YYYY-MM-DD"}}`. Mirror the controller: `new(time_zone: user.time_zone)`, `assign_attributes`, `creator = user`, `schedule_from(starts_on)`; 422 on validation (a missing or invalid `starts_on` leaves `next_run_on` blank and fails validation).
- `PATCH /:id` same body. Only call `schedule_from` when `starts_on` is present (the HTML form always sends it, but an API toggle of `active` must not reset the schedule).
- `DELETE /:id` -> 204 (`has_many :issues, dependent: :nullify`; spawned issues remain).
- Scoping: `set_current_team` and `foreign_team_references` for `lane_id`, `project_id`, `assignee_id`, `label_ids`. Authorization is member-level.
- Reuse: `RecurringIssue`, `schedule_from`, `schedule_summary`.

Questions: defaults for `weekday`/`day_of_month` (HTML defaults to today) belong in the desktop form; schedules are dates in the schedule's time zone with no time of day.

---

## Group H. Mutations and team move

Sizes: all **S** except move **S/M**. These can ship first as one `needs rails work` issue.

### H1. Issue delete

`DELETE /api/v1/teams/:team_id/issues/:id` -> 204.
- Mirror `IssuesController#destroy`: `@issue.destroy`, then `ProjectVelocityJob.perform_later(project_id)` when the issue had a project; track via the VektisApiIssueTracking pattern (`track_issue_deleted` exists on the web side).
- Authorization discrepancy: `authorize_issue_modification!` returns early for any team member, so over HTML any member can delete despite the comment saying creator or admin only. Decide whether the API enforces creator-or-admin (`team.admin?(user)`, recommended) or matches the web. Flag the web behaviour as a probable bug.
- Cascades (`dependent: :destroy`): comments, labels, dependencies, references, notifications, PR links; sub-issues are nullified. The desktop must drop those locally. Needs a tombstone (A1).

### H2. Comment delete

`DELETE /api/v1/teams/:team_id/issues/:issue_id/comments/:id` -> 204.
- Allowed if `comment.user == current_user` or `team.admin?(current_user)`, else 403. `replies` are `dependent: :destroy`, so the whole subtree goes; document it. Track `comment`/`delete` with `depth`.
- Project comments: the web has no destroy route for them, so no API delete unless wanted.

### H3. Label update and delete

- `PATCH /api/v1/teams/:team_id/labels/:id` `{label:{name, color}}` -> 200 `{id, name, color}`; 422 on blank or a duplicate name (case-insensitive unique per team).
- `DELETE /api/v1/teams/:team_id/labels/:id` -> 204. `issue_labels` are `dependent: :destroy`, so the label detaches from all issues; touch those issues so sync sees it (A1). Check the `project_labels` cascade.
- Track `label-management`/`update|delete`. The web has no admin gate on labels; keep parity unless the Rails side wants stricter.
- Add `updated_at` to the label JSON.

### H4. Move issue to another team (`IssueTeamMover`)

`PATCH /api/v1/teams/:team_id/issues/:id/move` body `{target_team_id}` -> 200 with `IssueSerializer(detailed: true)` (new `identifier` and `team_id`). The HTML route is `PATCH /teams/:team_id/issues/:issue_id/move` (`issue_moves#update`).
- Call `IssueTeamMover.call(issue:, target_team:, user:)`; on failure 422 `{error, errors:[mover.error]}` (already in that team; not a member of both).
- Scoping: the token must include both teams. Resolve the source with `set_current_team`, the target with `accessible_teams.find_by(id:)` (404 otherwise). The mover also checks real membership.
- Side effects the desktop must reconcile: `team_number` renumbered from `target_team.next_issue_number` (identifier changes); lane remapped by case-insensitive name else the target's first lane; assignee cleared if not a target member; project cleared; parent cleared if in another team; **all labels removed**; **all sub-issues detached** via `update_all`, which skips `updated_at` (use `update_all(parent_issue_id: nil, updated_at: Time.current)` so the sync cursor sees it); velocity recalculation enqueued (the old project is not recalculated). The paper_trail version records the `team_id` change, and `VersionDescriptionService` describes only the move.
- Track `issue-transfer`/`move`. Reuse `IssueTeamMover` unchanged.

---

## 6. Grouping into `needs rails work` issues

| # | Issue | Endpoints | Size | Depends on |
|---|---|---|---|---|
| 1 | Sync primitive | A1 (`updated_since`, `include=detail`, `include_archived`, pagination, `X-Server-Time`), serializer fields, `touch: true` on children, `issues(team_id, updated_at)` index, tombstone feed, A2 comments feed, A3 | M-L | none; others rely on it |
| 2 | Notifications inbox API | C: list, unread_count, read, read_all (plus the `update_all` `updated_at` fix) | S-M | none |
| 3 | Mutations: delete issue/comment, label update/delete, move issue | H1-H4 | S each | tombstones (1) preferred |
| 4 | Attachments API | B: direct upload, attach/detach on issues/projects/comments, serializer `files`, download | L | transport decision |
| 5 | Saved views + home page | D1, D2 | S-M | none |
| 6 | Dashboards API | D3 config CRUD, groups, move, reorder (optional evaluated issues) | M-L | none |
| 7 | Activity history API | E issue activity; project activity needs `has_paper_trail` on `Project` | M | none |
| 8 | Global search | F | S | none |
| 9 | Recurring issues API | G | M | none |

Suggested desktop order: 1, 3, 2 first (sync correctness and everyday actions), then 8, 5, 9, 7, 6, and 4 last because of the transport decision.

## 7. Cross-cutting open design questions

1. **List envelope**: existing lists are bare arrays. Metadata (unread count, server time, cursors) wants an envelope or headers. Recommend headers (`X-Server-Time`, `Link`) on existing endpoints (non-breaking) and an envelope only on brand-new endpoints that need it (notifications).
2. **Clock and cursors**: always return server time; clients overlap and dedupe by id.
3. **Tombstones**: one mechanism for all deletions (Issue, Comment, Label, Project, Dashboard/Group, SavedView, RecurringIssue, attachments, notification cascade). Use paper_trail destroy versions where models already track, else a `deleted_records` table.
4. **`updated_at` hygiene**: `update_all`/`update_column` paths skip timestamps (`mark_all_as_read`, `sever_cross_team_hierarchy`, `Dashboard#renumber_groups`, sub-issue nullify). Audit each resource that gets an `updated_since` feed.
5. **Authorization parity vs strictness**: the web lets any member delete issues (likely a bug), edit labels and manage recurring issues; comments are creator/admin. Decide per endpoint.
6. **Team-scoped tokens and user-level resources** (notifications, dashboards, saved views, home page): always intersect with `accessible_teams`.
7. **Track slugs**: reuse the web slugs through `track_api_feature`; saved views and dashboards showed no tracking in the code read.

## Sources read

All in `/Users/denvermullets/Documents/dev/pers/mtasks`: `config/routes.rb`; `app/controllers/api/v1/{base,issues,comments,labels,issue_dependencies,projects}_controller.rb`; `app/serializers/api/v1/{issue,comment,project}_serializer.rb`; `app/controllers/{notifications,saved_views,dashboards,dashboard_groups,image_uploads,recurring_issues,issue_moves,labels,comments,issues,projects}_controller.rb`; `app/controllers/settings/home_page_controller.rb`; `app/controllers/concerns/team_scoped.rb`; `app/models/{notification,saved_view,dashboard,dashboard_group,dashboard_group_source,recurring_issue,issue,comment,project,label,issue_dependency,api_token,user}.rb`; `app/services/{issue_team_mover,version_description_service,dashboard_issues_query}.rb`; `db/schema.rb` (notifications); `config/storage.yml`.

Not read in full, so verify: `DashboardIssuesQuery` internals, `MarkdownRenderer`, the event taxonomy, HTML issue/project file params, and whether an `issues(team_id, updated_at)` index exists.
