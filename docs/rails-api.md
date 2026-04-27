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

## Rust mapping

DTOs in `src/api/dto.rs` mirror these shapes 1:1. Per-resource read modules live in `src/api/{teams,lanes,issues,projects,labels,members}.rs` and call `ApiClient::get` / `ApiClient::post` / etc. directly.
