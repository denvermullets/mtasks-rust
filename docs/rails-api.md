# Rails JSON API reference

Upstream source: **`/Users/denvermullets/Documents/dev/pers/mtasks`** (sibling repo). Routes: `config/routes.rb`. Controllers: `app/controllers/api/v1/`.

## Conventions

- **Base URL**: configured by user; the desktop client appends `/api/v1/...`.
- **Auth**: `Authorization: Bearer <token>`. Tokens carry scopes — `read` for GET, `write` for POST/PATCH/DELETE.
- **Pagination**: none. Index endpoints return the full collection.
- **Response envelope**: none. Index actions return raw JSON arrays; show actions return raw objects.
- **Reference shapes** embedded inside other resources:
  - `RecordRef`: `{ id, name }`
  - `LabelRef`: `{ id, name }`
  - `IssueRef`: `{ id, identifier, title }`
  - `ParentIssueRef`: `{ id, identifier }`

## Endpoints

### `GET /api/v1/me`

Identifies the authenticated user and reveals the token's scope. The desktop client calls this at the start of every sync to decide whether to pull just one team or all visible teams.

```json
{
  "id": 42,
  "name": "...",
  "email": "...",
  "token": {
    "name": "CLI Token",
    "scopes": ["read", "write"],
    "team_id": null
  }
}
```

`token.team_id`: `null` means the token is user-scoped (full access to all teams the user is on); a number means it's restricted to that team.
`token.scopes`: typically `["read"]` or `["read", "write"]`. Mutating endpoints require `write`.

### `GET /api/v1/teams`

List teams. No filters. Ordered by name.

```json
[{ "id": 1, "name": "...", "identifier": "..." }]
```

### `GET /api/v1/teams/:team_id/lanes`

List lanes for a team. Ordered by `position`.

```json
[{ "id": 1, "name": "...", "position": 0, "color": "#hex" }]
```

### `GET /api/v1/teams/:team_id/labels`

List labels. Ordered by name.

```json
[{ "id": 1, "name": "bug", "color": "#hex" }]
```

### `GET /api/v1/teams/:team_id/members`

List members. Ordered by name.

```json
[{ "id": 1, "name": "...", "email": "..." }]
```

### `GET /api/v1/teams/:team_id/issues`

List issues. Ordered by `created_at DESC`.

**Filter query params** (all optional): `lane_id`, `assignee_id`, `project_id`, `priority`.

`priority` values: `urgent | high | medium | low | no_priority`.

```json
[{
  "id": 1, "identifier": "TEAM-1", "title": "...",
  "priority": "high", "estimate": 5, "due_date": "2026-05-15",
  "lane": { "id": 1, "name": "..." },
  "assignee": { "id": 2, "name": "..." },
  "creator":  { "id": 1, "name": "..." },
  "project":  { "id": 3, "name": "..." },
  "labels":   [{ "id": 10, "name": "bug" }],
  "created_at": "...", "updated_at": "..."
}]
```

### `GET /api/v1/teams/:team_id/issues/:id`

Show a single issue. Adds these detail-only fields on top of the list shape:

```json
{
  "description": "...",
  "parent_issue":     { "id": 5, "identifier": "TEAM-5" },
  "blocking_issues":  [{ "id": 6, "identifier": "TEAM-6", "title": "..." }],
  "blocked_issues":   [{ "id": 7, "identifier": "TEAM-7", "title": "..." }],
  "started_at": "...", "completed_at": null, "canceled_at": null
}
```

### `GET /api/v1/teams/:team_id/projects`

List projects. Ordered by name.

```json
[{
  "id": 1, "name": "...", "description": "...",
  "status": "started",
  "priority": "high",
  "lead": { "id": 2, "name": "..." },
  "start_date": "2026-04-01", "due_date": "2026-06-30",
  "velocity_score": 42,
  "completed_issues_count": 5, "total_issues_count": 12,
  "progress_percentage": 41.67,
  "behind_schedule": false,
  "roadmap_commitment": "now",
  "created_at": "...", "updated_at": "..."
}]
```

`status` values: `backlog | started | paused | completed | cancelled`.
`roadmap_commitment` values: `now | next | later`.

### `GET /api/v1/teams/:team_id/projects/:id`

Show a single project. Adds:

```json
{ "labels": [{ "id": 10, "name": "..." }], "issues_count": 12 }
```

## Write endpoints

All create/update bodies are wrapped under the resource name (Rails strong params convention). All return the freshly-serialized resource (same shape as `show`) on success and `{"error": "...", "errors": [...strings...]}` on 422. `DELETE` returns 204 No Content.

### `POST /api/v1/teams/:team_id/issues` / `PATCH /api/v1/teams/:team_id/issues/:id`

Body: `{ "issue": { ...permitted fields... } }`

Permitted: `title` (required on create), `description`, `lane_id`, `priority`, `estimate`, `due_date`, `assignee_id`, `project_id`, `parent_issue_id`, `label_ids: []`.

Server-side behavior:
- `identifier` auto-generated (`{team.identifier}-{team_number}`) on create
- `creator` set from token's user; immutable
- Lane transitions auto-set `completed_at` (done lane) or `canceled_at` (cancelled lane)

### `POST /api/v1/teams/:team_id/projects` / `PATCH /api/v1/teams/:team_id/projects/:id` / `DELETE /api/v1/teams/:team_id/projects/:id`

Body: `{ "project": { ...permitted fields... } }`

Permitted: `name` (required on create), `description`, `priority`, `status`, `lead_id`, `start_date`, `due_date`, `roadmap_commitment`, `label_ids: []`.

### `POST /api/v1/teams/:team_id/labels`

Body: `{ "label": { "name": "...", "color": "#hex" } }`

`color` optional — server picks a random color if omitted.

### `POST /api/v1/teams/:team_id/issues/:issue_id/dependencies` / `DELETE .../dependencies/:id`

**Body is flat (no wrapper):**

```json
{ "target_issue_id": 42, "direction": "blocking" }
```

`direction`: `"blocking"` (current issue blocks target) or `"blocked_by"` (current issue blocked by target).

Success response shape (note: embedded refs, not flat ids):

```json
{
  "id": 9,
  "blocking_issue": { "id": 1, "identifier": "TEAM-1", "title": "..." },
  "blocked_issue":  { "id": 2, "identifier": "TEAM-2", "title": "..." },
  "created_at": "..."
}
```

## Rust mapping

DTOs in `src/api/dto.rs` mirror response shapes 1:1. Param structs (`IssueParams`, `ProjectParams`, `LabelParams`) live in their respective `src/api/{resource}.rs` modules and serialize with the Rails wrapper. Per-resource modules: `src/api/{teams,lanes,issues,projects,labels,members,dependencies}.rs` calling `ApiClient::{get,post,patch,delete}` directly.

**Known DTO mismatch:** `IssueDependencyDto` (in `dto.rs`) has flat `blocking_issue_id`/`blocked_issue_id` fields and does NOT match the Rails wire format above. The `dependencies` module uses a separate `IssueDependencyResponse` type that matches Rails. Reconciling the local DTO is left to the sync layer (DESK-11) since the local DB representation is independent from the wire shape.
