# Rails JSON API reference

Upstream source: **`/Users/denvermullets/Documents/dev/pers/mtasks`** (sibling repo). Routes: `config/routes.rb` (namespace `api/v1`). Controllers: `app/controllers/api/v1/`. Serializers: `app/serializers/api/v1/`.

## Conventions

- **Base URL**: configured by user; the desktop client appends `/api/v1/...`.
- **Auth**: `Authorization: Bearer <token>`. Tokens carry scopes. The check is by HTTP method only: `GET`/`HEAD` need `read`, every other verb (`POST`/`PATCH`/`PUT`/`DELETE`) needs `write`.
- **Ids**: integers.
- **Timestamps**: model times are ISO 8601 UTC with milliseconds (`2026-04-27T10:00:00.123Z`). Sync-feed times (`deleted_at`, `X-Server-Time`) use microseconds (`2026-10-30T12:00:00.123456Z`). `due_date` / `start_date` are plain `YYYY-MM-DD`. Request `updated_since` / `since` must be ISO 8601.
- **Pagination**: only `GET /teams/:team_id/issues` paginates, and only when `limit` or `after_id` is sent (see below). Every other index returns the full collection.
- **Response envelope**: none. Index actions return raw JSON arrays; show actions return raw objects. Pagination state travels in headers.
- **Reference shapes** embedded inside other resources:
  - `RecordRef`: `{ id, name }`
  - `LabelRef`: `{ id, name }`
  - `IssueRef`: `{ id, identifier, title }`
  - `ParentIssueRef`: `{ id, identifier }`

### Errors

| Status | Body | When |
|---|---|---|
| 401 | `{ "error": "Unauthorized", "message": "Invalid or missing API token" }` | Bad or missing token. |
| 403 | `{ "error": "Forbidden", "message": "Token lacks required scope" }` | A read-only token on any non-GET verb. Also `message: "Only the author or a team admin can delete this"` (or `...remove this`) when deleting someone else's comment or comment attachment without being a team admin. Those are the only 403 sources. |
| 404 (team) | `{ "error": "Not Found", "message": "Team not found or access denied" }` | Unknown team, **archived** team, or a team outside the token's team set. Never 403. Tell it apart from a resource 404 by the exact message. |
| 404 (resource) | `{ "error": "Not Found", "message": "<X> not found" }` | e.g. `Issue not found`, `Project not found`, `Label not found`, `Dependency not found`, `Target issue not found`, `Comment not found`. A resource in another team is also 404. |
| 422 | `{ "error": "Unprocessable Entity", "errors": ["...strings..."] }` | Validation failures. Foreign-team refs: `"lane_id does not belong to this team"`. Bad `updated_since`: `["updated_since must be an ISO8601 timestamp"]`. Bad `after_id`: `["after_id must be an issue id in this team"]`. |
| 400 | Rails default (not the JSON shape) | Missing body wrapper (`params.require`). |

An unknown `issue[priority]` string raises in the Rails enum and returns 500 (Issue's enum does not use `validate: true`); the desktop only sends valid values.

### Sync primitives (JAIT-280)

Used by incremental pull. All verified against the Rails controllers.

- **`updated_since`** (`GET .../issues`, `GET .../projects`, `GET .../comments`): ISO 8601; returns rows with `updated_at >= since` (inclusive), ordered `updated_at ASC, id ASC`. Keep an overlap when advancing the cursor.
- **`X-Server-Time`** response header (`Time.current.utc.iso8601(6)`): use it as the next cursor. Set on issues index, projects index, team comments feed and the deleted feed, including 422 responses for a bad time param. **Not** sent by labels, lanes or members.
- **ETag / 304**: only labels, lanes and members indexes. The response carries a weak `ETag: W/"..."`; send it back verbatim as `If-None-Match` and get `304` with an empty body when nothing changed. Issues and projects have no ETag (use `updated_since`). Labels have no `updated_since`.
- **`GET /teams/:team_id/deleted`**: tombstone feed (below).
- **`DELETE /teams/:team_id/issues/:id`**: exists (below).

## Endpoints

### `GET /api/v1/me`

Identifies the authenticated user and reveals the token's scope. The desktop client calls this at the start of every sync to decide which teams to pull.

```json
{
  "id": 42,
  "name": "...",
  "email": "...",
  "home_page": "...",
  "token": {
    "name": "CLI Token",
    "scopes": ["read", "write"],
    "team_ids": null,
    "team_id": null
  }
}
```

- `token.team_ids`: `null` = unscoped token (all the user's non-archived teams). An array of sorted integers = exactly those teams. It may be **empty** (`[]`, a token scoped to zero teams).
- `token.team_id`: the single id **only if the token's team set has exactly one team**, otherwise `null`. A multi-team scoped token therefore also returns `team_id: null`; read `team_ids` instead.
- `token.scopes`: non-empty subset of `read`, `write`. There is no writable boolean; check `"write"` is in the array. `/me` is a GET, so it works with read-only tokens.
- `home_page` is ignorable.

### `GET /api/v1/teams`

List teams. No filters. Ordered by name. Token-filtered.

```json
[{ "id": 1, "name": "...", "identifier": "..." }]
```

### `GET /api/v1/teams/:team_id/lanes`

List lanes for a team. Ordered by `position`. Supports ETag/304. No write endpoints for lanes.

```json
[{ "id": 1, "name": "...", "position": 0, "color": "#hex", "category": "started" }]
```

`category` values: `backlog | unstarted | started | completed | canceled` (note `canceled`, one L). Treat as optional. Completed and canceled lanes drive `completed_at` / `canceled_at` on issues server-side.

### `GET /api/v1/teams/:team_id/labels`

List labels. Ordered by name. Supports ETag/304.

```json
[{ "id": 1, "name": "bug", "color": "#hex", "updated_at": "..." }]
```

### `GET /api/v1/teams/:team_id/members`

List members (team users only). Ordered by name. Supports ETag/304.

```json
[{ "id": 1, "name": "...", "email": "..." }]
```

### `GET /api/v1/teams/:team_id/issues`

List issues.

**Filter query params** (all optional): `lane_id`, `assignee_id`, `project_id`, `priority`. There are no creator, created_at, label or title filters.

`priority` values: `urgent | high | medium | low | no_priority`.

**Sync query params:**

| Param | Effect |
|---|---|
| `updated_since` | ISO 8601; `updated_at >= since`, ordered `updated_at ASC, id ASC`. Without it the order is `created_at DESC`. |
| `include=detail` | Every row uses the detailed serializer (the detail fields of `GET .../issues/:id`). Any other value gives the base shape. |
| `include_archived` | Truthy (`true`, `1`) includes archived issues. **Default excludes them**, so archivals are invisible unless this is sent. Archiving bumps `updated_at`. |
| `limit` | Page size. Default 200, max 500; non-positive falls back to 200. |
| `after_id` | Keyset cursor: an issue id in this team (archived allowed). Unknown id gives 422. |

Pagination is active iff `limit` or `after_id` is present, and then the order is forced to `updated_at ASC, id ASC`. If more rows remain the response carries `X-Next-After-Id: <id of last row>`; **absence of that header means last page**. The body is always a bare array. Next request: same params plus `after_id=<header value>` (keep the same `updated_since`). Without `limit`/`after_id` the whole result is one array and no header is sent. `X-Server-Time` is always set.

Base shape (note `team_id`, `archived_at` and `recurring_issue_id` are present):

```json
[{
  "id": 1, "identifier": "TEAM-1", "team_id": 3, "team_number": 1, "title": "...",
  "priority": "high", "estimate": 5, "due_date": "2026-05-15",
  "lane": { "id": 1, "name": "..." },
  "assignee": { "id": 2, "name": "..." },
  "creator":  { "id": 1, "name": "..." },
  "project":  { "id": 3, "name": "..." },
  "labels":   [{ "id": 10, "name": "bug" }],
  "created_at": "...", "updated_at": "...",
  "archived_at": null, "recurring_issue_id": null
}]
```

Nullable: `estimate`, `due_date`, `lane`, `assignee`, `creator`, `project`, `archived_at`, `recurring_issue_id`.

### `GET /api/v1/teams/:team_id/issues/:id`

Show a single issue (detailed). Also returned by `include=detail` on the index and by `GET /api/v1/issues/by_identifier/:identifier`. Adds these fields on top of the list shape:

```json
{
  "description": "...",
  "parent_issue":    { "id": 5, "identifier": "TEAM-5" },
  "blocking_issues": [{ "id": 6, "identifier": "TEAM-6", "title": "...", "dependency_id": 11 }],
  "blocked_issues":  [{ "id": 7, "identifier": "TEAM-7", "title": "...", "dependency_id": 12 }],
  "related_issues":  [{ "id": 8, "identifier": "TEAM-8", "title": "...", "dependency_id": 13, "direction": "relates" }],
  "duplicates":      [{ "id": 9, "identifier": "TEAM-9", "title": "...", "dependency_id": 14, "direction": "duplicates" }],
  "started_at": "...", "completed_at": null, "canceled_at": null,
  "files": [ /* attachment objects */ ]
}
```

- `blocking_issues` = issues that block THIS issue. `blocked_issues` = issues THIS issue blocks.
- `duplicates[].direction` is `duplicates` or `duplicated_by`.
- `dependency_id` is the id to use for `DELETE .../issue_dependencies/:id`.
- `parent_issue` carries `identifier`, not title, and may be `null`.

### `DELETE /api/v1/teams/:team_id/issues/:id`

Deletes an issue. **`204 No Content`**, empty body. Any team member may delete (no ownership check). Errors: 403 for a read-only token, 404 `Issue not found` (including an issue in another team), 422 `{error, errors}` if destroy fails.

Side effects: the issue's comments, dependencies, decisions and references are destroyed (each producing its own tombstone, see below); sub-issues have `parent_issue_id` nullified (their `updated_at`/parent change, so a pull sees it); the project's velocity is recalculated.

### `GET /api/v1/teams/:team_id/deleted?since=<ISO8601>`

Tombstone feed for incremental pull. Returns a bare array, oldest first, no pagination or limit:

```json
[{ "record_type": "Issue", "record_id": 42, "deleted_at": "2026-10-30T12:00:00.123456Z" }]
```

- `since` is optional and inclusive (`>=`). **Omitted = all history**, unbounded. Use the response's `X-Server-Time` as the next cursor.
- `record_type`: `Issue`, `Project`, `Comment` (issue OR project comment; no discriminator, look it up locally), `IssueDependency`, `Label`, `RecurringIssue`. Sourced from paper_trail destroy versions.
- **Not tombstoned**: archivals (use `archived_at` via `include_archived`), lanes, members, notifications, issue-label join rows.
- Requires `read`; team-access 404 as above. `GET /api/v1/deleted` (no team) is the per-user feed (saved views, dashboards); the desktop does not use it.

### `GET /api/v1/teams/:team_id/comments?updated_since=<ISO8601>`

Team-wide comment feed (issue and project comments together). Flat array, replies as separate rows (`parent_id`), no nested `replies`, ordered `updated_at ASC, id ASC`, `updated_at >= since`. No pagination. `X-Server-Time` is set.

```json
{
  "id": 1, "body": "...", "parent_id": null,
  "user": { "id": 2, "name": "...", "email": "..." },
  "files": [],
  "created_at": "...", "updated_at": "...",
  "issue_id": 5, "project_id": null, "owner_type": "Issue", "owner_id": 5
}
```

### `GET /api/v1/teams/:team_id/projects`

List projects. Ordered by name; with `updated_since` ordered `updated_at ASC, id ASC` and `updated_at >= since`. No pagination, no ETag. `X-Server-Time` is set.

```json
[{
  "id": 1, "name": "...", "description": "...",
  "status": "started",
  "priority": "high",
  "lead": { "id": 2, "name": "..." },
  "start_date": "2026-04-01", "due_date": "2026-06-30",
  "velocity_score": 42,
  "completed_issues_count": 5, "total_issues_count": 12,
  "progress_percentage": 42,
  "behind_schedule": false,
  "roadmap_commitment": "now",
  "created_at": "...", "updated_at": "..."
}]
```

`status` values: `backlog | started | paused | completed | cancelled` (US spelling, two L). **`status` may be `null`** (the column is nullable). `velocity_score` and `progress_percentage` are integers (`progress_percentage` is rounded). `roadmap_commitment` values: `now | next | later`; setting `status: "completed"` makes the server clear `roadmap_commitment`. Project rows have no creator field.

### `GET /api/v1/teams/:team_id/projects/:id`

Show a single project. Adds:

```json
{ "labels": [{ "id": 10, "name": "..." }], "issues_count": 12, "files": [] }
```

### Comments: `GET|POST /api/v1/teams/:team_id/issues/:issue_id/comments`, `GET|POST /api/v1/teams/:team_id/projects/:project_id/comments`

- `GET` returns top-level comments ordered by `created_at`, each with nested `replies` (one level, replies carry no `replies` key):

```json
{
  "id": 1, "body": "...", "parent_id": null,
  "user": { "id": 2, "name": "...", "email": "..." },
  "files": [],
  "created_at": "...", "updated_at": "...",
  "replies": [{ "id": 2, "body": "...", "parent_id": 1, "user": { }, "files": [], "created_at": "...", "updated_at": "..." }]
}
```

- `POST` body: `{ "comment": { "body": "...", "parent_id": null } }`. Send an `Idempotency-Key` header: a replay with the same key returns the existing comment with **200**; a new comment returns **201**. Creating a comment touches the parent issue/project (`updated_at` moves).
- `DELETE .../issues/:issue_id/comments/:id` returns 204; 403 `Only the author or a team admin can delete this` otherwise. Comment attachments: `DELETE .../comments/:comment_id/attachments/:id`. Project comments have no delete route (attachment delete only).

## Write endpoints

All create/update bodies are wrapped under the resource name (Rails strong params convention), except dependencies (flat). Create/update return the freshly-serialized resource on success (create `201`, update `200`) and `{"error": "Unprocessable Entity", "errors": [...strings...]}` on 422. Issue, project, label and dependency creates are **not idempotent** (only comments honour `Idempotency-Key`), so a retried POST can duplicate.

**Endpoints that do not exist**: lane writes (create/update/delete), and any issue archive endpoint. Label update and destroy and issue destroy DO exist (below).

### `POST /api/v1/teams/:team_id/issues` / `PATCH /api/v1/teams/:team_id/issues/:id`

Body: `{ "issue": { ...permitted fields... } }`

Permitted: `title` (required on create), `description`, `lane_id`, `priority`, `estimate`, `due_date`, `assignee_id`, `project_id`, `parent_issue_id`, `label_ids: []`.

Server-side behavior:
- `identifier` auto-generated (`{team.identifier}-{team_number}`) on create
- `creator` set from token's user; immutable
- Lane transitions auto-set `completed_at` (completed lane) or `canceled_at` (canceled lane)

`DELETE` is documented above. There is also `PATCH .../issues/:id/move` (move to another team); unused by the desktop.

### `POST /api/v1/teams/:team_id/projects` / `PATCH /api/v1/teams/:team_id/projects/:id` / `DELETE /api/v1/teams/:team_id/projects/:id`

Body: `{ "project": { ...permitted fields... } }`

Permitted: `name` (required on create), `description`, `priority`, `status`, `lead_id`, `start_date`, `due_date`, `roadmap_commitment`, `label_ids: []`.

`DELETE` returns 204 and nullifies `project_id` on the project's issues.

### `POST /api/v1/teams/:team_id/labels` / `PATCH .../labels/:id` / `DELETE .../labels/:id`

Body: `{ "label": { "name": "...", "color": "#hex" } }`

`color` optional on create: server picks a random color if omitted. Names are unique per team, case-insensitively (duplicate gives 422). Create returns 201 and update 200 with `{ id, name, color, updated_at }`. `DELETE` returns 204 and touches every carrying issue so it shows up in the next `updated_since` pull.

### Issue dependencies: `/api/v1/teams/:team_id/issues/:issue_id/issue_dependencies`

The URL segment is `issue_dependencies` (NOT `dependencies`).

- `GET .../issue_dependencies` lists links where the issue is on either side, ordered by id.
- `POST .../issue_dependencies` creates a link. **Body is flat (no wrapper):**

```json
{ "target_issue_id": 42, "direction": "blocking" }
```

`direction`: `blocking` (current issue blocks target), `blocked_by` (current issue blocked by target), `relates`, `duplicates` (current issue is the duplicate), `duplicated_by`. A missing or unknown direction is treated as `blocking`. Returns **201**; 404 `Target issue not found` if the target is not in the team; 422 on validation (duplicate or self link).

- `DELETE .../issue_dependencies/:id` returns **200** `{ "ok": true, "id": 9 }` (NOT 204). `:id` may be an outgoing or an incoming link of the issue. 404 `Dependency not found` otherwise.

Response object (index and create):

```json
{
  "id": 9,
  "kind": "blocks",
  "direction": "blocking",
  "blocking_issue": { "id": 1, "identifier": "TEAM-1", "title": "..." },
  "blocked_issue":  { "id": 2, "identifier": "TEAM-2", "title": "..." },
  "created_at": "..."
}
```

`kind`: `blocks | relates | duplicates`. `direction` is relative to `:issue_id`. For `blocks`, `blocking_issue` is the source; for `blocked_by`, the target is `blocking_issue`; for `duplicates`, the source is the duplicate.

## Present but unused by the desktop

`GET /api/v1/users/by_email`, `GET /api/v1/issues/by_identifier/:identifier`, `POST /api/v1/integrations/handshake`, issue and project `decisions` (Hourglass pins), `activity` feeds, attachments / direct uploads, notifications, dashboards, saved views, recurring issues, `PATCH .../issues/:id/move`, and `GET /api/v1/deleted` (user feed).

## Rust mapping

DTOs in `src/api/dto.rs` mirror response shapes. Param structs (`IssueParams`, `ProjectParams`, `LabelParams`) live in their respective `src/api/{resource}.rs` modules and serialize with the Rails wrapper. Per-resource modules: `src/api/{teams,lanes,issues,projects,labels,members,dependencies}.rs` calling `ApiClient::{get,post,patch,delete}` directly.

**Local vs wire dependency types:** `IssueDependencyDto` (in `dto.rs`) is the local-row view with flat `blocking_issue_id`/`blocked_issue_id` fields and does NOT match the Rails wire format above. The `dependencies` module uses a separate `IssueDependencyResponse` type for the wire shape. The two are reconciled in the sync layer.
