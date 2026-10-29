# Desktop vs Rails `api/v1` contract diff

Ticket: JRST-2 (issue 5344). Scope: the **existing** contract only; new endpoint specs live on `research/rails-endpoint-specs`.

Sources (primary): Rails `config/routes.rb` (namespace `api/v1`, lines 127-150), `app/controllers/api/v1/*.rb`, `app/serializers/api/v1/*.rb`, `app/models/{issue,project,lane,issue_dependency}.rb`, `app/services/issue_dependencies/link.rb` in `/Users/denvermullets/Documents/dev/pers/mtasks`; desktop `src/api/*.rs`, `src/sync/engine.rs`, `src/config.rs`, `docs/rails-api.md` (branched from `dev` at 4b8f410).

## 1. Global contract

| Topic | Rails (truth) | Desktop | Verdict |
|---|---|---|---|
| Pagination | None. Every index returns the full array (`issues#index` is `not_archived`, `order(created_at: :desc)`, no limit). | Assumes none. | Match. Scaling risk only (full list plus one GET per changed issue). |
| Auth | `Authorization: Bearer`; `read` scope for GET/HEAD, `write` otherwise. 401 `{error:"Unauthorized",message}`; 403 `{error:"Forbidden",message:"Token lacks required scope"}` (`base_controller.rb`). | `map_error` reads `message` then `error`. 401 -> `LeavePending` (good). 403 -> `DropAndLog`, which calls `mark_synced`, so the queued edit is **discarded**. | MISMATCH: a read-only token silently loses every local write. `MeTokenDto::has_write_scope()` exists but has no caller. |
| Team visibility | An inaccessible, archived, or token-excluded team returns **404** `{error:"Not Found",message:"Team not found or access denied"}`, never 403. | 404 on create/update is `DropAndLog` (edit lost); on delete it counts as success. | MISMATCH (data loss if a team is archived or removed from the token between edit and drain). |
| 422 shape | `{error:"Unprocessable Entity", errors:[strings]}` (also foreign-team refs: `"<key> does not belong to this team"`). | `ValidationErrorBody` matches; entry dropped, not retried. | Match. |
| Other errors | 404 `{error:"Not Found", message}`; missing wrapper (`params.require`) gives Rails default 400 (not the JSON shape); an unknown `issue[priority]` raises `ArgumentError` -> 500 (Issue enum has no `validate: true`; Lane's does). | 400 -> `Server{400}` drop; 500 -> retried forever. | Low risk: desktop sends only valid strings. |
| Timestamps | ActiveSupport JSON: ISO-8601 UTC with ms (`2026-04-27T10:00:00.123Z`); `due_date`/`start_date` are plain `YYYY-MM-DD`. | Strings; local writes use `%Y-%m-%dT%H:%M:%S%.3fZ`; `decide()` does LWW on `updated_at` (unparseable -> remote wins). | Match. `docs/rails-api.md` shows `"..."`; document the format. |
| Priority enum | `urgent 0, high 1, medium 2, low 3, no_priority 4`; serialized as the **string** name for issues and projects. | `Priority::as_rails_str/from_rails_str` use identical spellings; `IssueDto.priority: String`. | Match. |
| Project status | Strings `backlog started paused completed cancelled` (US "cancelled"); column is **nullable** (`default 'backlog'`, validation `allow_nil`). | `ProjectDto.status: ProjectStatus` (non-Option), same spellings. | Latent: one project with `status: null` fails decode of the **whole** `GET /projects` list. Make it `Option`. |
| Lane category | `lanes#index` returns `category` in `backlog unstarted started completed canceled` ("canceled", one L). | `LaneDto` has no `category`. | IGNORED (see 3). |
| Idempotency | Only comments (`Idempotency-Key` -> `hourglass_message_id`) and decisions honour it. Issue/project/label/dependency creates do not. | Sends the key for comments only. | A crash between POST and `mark_synced` duplicates issues/projects/labels. Duplicate dependencies hit a uniqueness 422, which the desktop drops. |
| Numeric types | `progress_percentage` is an integer (`.round`); `velocity_score` integer. | `i64`. | Match. `docs/rails-api.md` example `41.67` is wrong. |

## 2. Per-endpoint table

"-" = desktop does not use it.

| Rails route | Desktop | Params | Response fields desktop ignores | Behavior mismatch |
|---|---|---|---|---|
| `GET /me` | CALLED (`users::me`, start of every pull) | - | **`token.team_ids`** (new), `token.name` | `team_id` is non-null only for **one-team** tokens. A multi-team scoped token returns `team_id:null` and the desktop treats it as user-scoped. Targets still come out right today because `pull_teams` uses the filtered `/teams`, but the model is wrong. Read `team_ids`. |
| `GET /users/by_email` | - | `email` | all | Unused (could resolve non-roster assignees). |
| `GET /issues/by_identifier/:identifier` | - | `TEAM-12` | all | Unused (mention/link resolution). |
| `POST /integrations/handshake` | - | - | - | Hourglass bootstrap; out of scope. |
| `GET /teams` | CALLED | - | none (`id,name,identifier` only) | No timestamps, so team LWW always sees an empty `remote_updated` (already `Option`). `archive_missing_teams` relies on the list being token-filtered. |
| `GET /teams/:t/lanes` | CALLED | - | **`category`** | Lanes cannot be mapped to workflow state (completed/canceled lanes drive `completed_at`/`canceled_at` server-side). Index omits `team_id` (patched in code). |
| `GET /teams/:t/labels` | CALLED | - | none | OK; no timestamps. |
| `POST /teams/:t/labels` | CALLED (`label_create`) | `{label:{name,color?}}` | body ignored except `id` | OK. Validation failures (422) dropped. |
| `GET /teams/:t/members` | CALLED | - | none | OK. Roster is team users only; issue creator/assignee may be non-members (handled by `relax_issue_fks`). |
| `GET /teams/:t/issues` | CALLED | filters `lane_id assignee_id project_id priority` (wired in `IssueFilters`; pull sends none) | list omits description/parent/dependencies/lifecycle stamps by design | Excludes archived issues; `archive_missing_issues` mirrors that locally (OK). No pagination. |
| `GET /teams/:t/issues/:id` | CALLED once per changed issue (**N+1**, section 4) | - | **`related_issues`, `duplicates`** (each with `direction`); **`dependency_id`** on every `blocking_issues`/`blocked_issues` entry (desktop `IssueRef` has no such field) | Dependencies are never persisted by pull, so the desktop never learns remote dependency ids. |
| `POST /teams/:t/issues` | CALLED | `{issue:{title,description,lane_id,priority,estimate,due_date,assignee_id,project_id,parent_issue_id,label_ids[]}}` | returns list shape; only `id` kept | No `Idempotency-Key` support. Foreign-team refs -> 422 -> entry dropped. |
| `PATCH /teams/:t/issues/:id` | CALLED | same as create | response ignored | Lane moves stamp lifecycle fields server-side; local gets them only on the next detail GET. |
| `DELETE /teams/:t/issues/:id` | CALLED (`issue_delete`, `engine.rs` ~399-423) | - | - | **No route** (`only: index show create update`). Always 404 -> `Err(NotFound)` -> `Outcome::Success` (~line 420). Local row is hard-deleted (`queries::delete_issue`), so the next pull sees the remote issue as new and **resurrects it**. |
| `GET /teams/:t/issues/:id/issue_dependencies` | - | - | all | Exists (`id, kind, direction, blocking_issue, blocked_issue, created_at`), but the engine header says Rails has no index endpoint for dependencies. Wrong. |
| `POST /teams/:t/issues/:id/issue_dependencies` | CALLED at the **wrong path** `.../dependencies` (`src/api/dependencies.rs`) | flat `{target_issue_id, direction}`; direction one of `blocking blocked_by relates duplicates duplicated_by` (unknown -> `blocking`) | response `kind`, `direction` (`IssueDependencyResponse` has neither) | 404 on every create -> `classify` -> `DropAndLog`: the dependency is never created and the queue entry is marked synced. Desktop only sends `Direction::Blocking`. Fixing the path fixes the 404; request body and 201 shape already match. |
| `DELETE /teams/:t/issues/:id/issue_dependencies/:dep` | CALLED at the **wrong path** (`dependency_delete`, `engine.rs` ~939-974) | - | body `{ok:true,id}` (discarded) | 404 -> treated as `Success` (~972): deletes silently do nothing. Real success is **200** with a JSON body, not 204. `:dep` may be an outgoing or incoming link of the issue. |
| `GET /teams/:t/issues/:id/comments` | CALLED on demand (`app.rs` ~1059 when an issue opens; not in `pull`) | - | `updated_at`, nested `replies` (flat thread kept), `user.email` | Top-level only with nested replies. Undocumented in `rails-api.md`. |
| `POST /teams/:t/issues/:id/comments` | CALLED (`comment_create`) | `{comment:{body,parent_id?}}` + `Idempotency-Key` | body ignored except `id` | Replay returns **200** with the existing comment (new = 201); desktop treats both as success. Undocumented. |
| `POST/DELETE /teams/:t/issues/:id/decisions` | - | `hourglass_message_id, body_snapshot, ...` | all | Hourglass pins; out of scope. |
| `GET /teams/:t/projects` | CALLED | - | none | Nullable `status` risk above. No labels in the list shape. |
| `GET /teams/:t/projects/:id` | **not called** (`projects::get` has no caller) | - | `labels`, `issues_count` | Project labels are pushed (`label_ids`) but never pulled. |
| `POST/PATCH /teams/:t/projects` | CALLED | `{project:{name,description,priority,status,lead_id,start_date,due_date,roadmap_commitment,label_ids[]}}` | responses ignored | `status: "completed"` makes the server null `roadmap_commitment` silently; local and remote diverge. |
| `DELETE /teams/:t/projects/:id` | CALLED | - | - | 204; route exists, so 404-as-success is fine. Server nullifies issues' `project_id`; desktop does not refresh until their `updated_at` moves. |
| `GET/POST /teams/:t/projects/:id/comments` | - | `{comment:{body,parent_id}}` + `Idempotency-Key` | all | Same serializer as issue comments; undocumented; project discussion not synced. |
| `POST/DELETE /teams/:t/projects/:id/decisions` | - | - | all | Hourglass; out of scope. |

## 3. Ignored response fields (consolidated)

- `/me`: `token.team_ids`.
- Lanes: `category`.
- Issue detail: `related_issues[]`, `duplicates[]` (each `{id,identifier,title,dependency_id,direction}`); `dependency_id` inside `blocking_issues[]`/`blocked_issues[]`.
- Dependency create/index: `kind` (`blocks|relates|duplicates`), `direction`.
- Comments: `updated_at`, `replies[]` (modelled, unused).
- Project detail: `labels`, `issues_count` (never fetched).
- Create/update bodies for issues, projects, labels, comments (only `id` kept).

## 4. N+1 issue-detail pull

`pull_issues_step` (`engine.rs` ~1417): one `GET /issues` per team, then one `GET /issues/:id` for every issue that is new locally or whose `updated_at` is newer. First sync of N issues = 1 + N requests, issued serially (plain `await` loop). The detail serializer also runs unpreloaded association queries per issue (dependencies, outgoing/incoming links for relates and duplicates). Steady state is cheap thanks to the `updated_at` gate, but that gate can miss changes that do not bump `issues.updated_at` (dependency or label edits made on the web), and the dependency data is not read by the desktop anyway. Rails has no `detailed=1`, `updated_since` or batch endpoint, so removing the N+1 itself needs Rails work (sibling ticket); desktop-side we can only bound concurrency.

## 5. Rails endpoints the desktop does not use

`GET /users/by_email`, `GET /issues/by_identifier/:identifier`, `POST /integrations/handshake`, `issue_dependencies#index`, project `show`, project comments (`index`, `create`), issue and project `decisions`.

The API has **no** issue destroy, no lane write endpoints, and no label update/destroy. The desktop's delete-issue has no remote equivalent.

## 6. Prioritized repair list

### Desktop fixes

1. **P0 Dependency paths**: use `.../issue_dependencies` in `src/api/dependencies.rs` for create and delete. Drop the `NotFound => Success` arm in `dependency_delete` (~972) once the path is right. Add a test pinning the paths.
2. **P0 Issue delete**: stop calling a nonexistent route. Until Rails has destroy/archive: keep a local tombstone (soft-archive) so pull does not resurrect the issue, or block remote-synced issues from being deleted. Remove the `NotFound => Success` arm at `engine.rs` ~420.
3. **P0 Read-only / forbidden tokens**: check `me.token.has_write_scope()` and pause pushes with a visible banner; never let a 403 reach `DropAndLog`/`mark_synced` (keep pending or park as failed).
4. **P1 `/me` team scope**: add `team_ids: Option<Vec<i64>>` to `MeTokenDto`; make `collect_pull_targets` use it (`None` = all local teams, `Some(ids)` = intersection). Keep `team_id` as a `#[serde(default)]` fallback.
5. **P1 Null-safety**: `Option<ProjectStatus>` in `ProjectDto`; make unknown enum strings degrade per-row instead of failing the list decode.
6. **P1 Read dependencies**: add `dependency_id` to the issue dependency ref type, parse `related_issues`/`duplicates`, add `kind` (+ `direction`) to the response type and local schema, or call `issue_dependencies#index`. Without this, remote links cannot be deleted from the desktop and relates/duplicates are invisible.
7. **P1 Lane `category`**: add `category: Option<String>` to `LaneDto` plus a migration.
8. **P2 404 semantics**: separate team-404 ("Team not found or access denied") from resource-404; do not drop edits on team-404, flag the team inaccessible.
9. **P2 Duplicate creates**: persist `remote_id` atomically or reconcile on retry (server idempotency for non-comment creates needs Rails work).
10. **P2 N+1**: bound concurrency (`buffer_unordered` 4-8) for detail fetches; fetch project detail when labels are needed.
11. **P3 Comments**: pull `updated_at`/`replies`; wire project comments if the project view shows discussion.
12. **P3 Dependency direction**: expose `blocked_by` and kinds in UI; client always sends `blocking` today.

### Fixes to `docs/rails-api.md`

1. `/me`: add `token.team_ids` (sorted array or null) and state that `team_id` is non-null only for one-team tokens.
2. Lanes: add `category` and its five values (`canceled`, one L).
3. Dependencies: replace every `/dependencies` with `/issue_dependencies`; add the `GET` index and its shape (`id, kind, direction, blocking_issue, blocked_issue, created_at`); document `kind` and the five `direction` values; `DELETE` returns 200 `{ok:true,id}`, create returns 201.
4. Issue detail: document `related_issues`, `duplicates`, and `dependency_id` on `blocking_issues`/`blocked_issues`.
5. Add issue and project comments: paths, `{comment:{body,parent_id}}`, `Idempotency-Key` (200 on replay, 201 on create), response shape (`user`, `replies`, `updated_at`).
6. State explicitly that there is no issue destroy, lane write, or label update/destroy; fix the engine header comment claiming Rails has no dependency index.
7. Fix the `progress_percentage` example (integer); document timestamp formats and that project `status` may be null.
8. Document error shapes (401/403/404 bodies, team-scope 404, 422 `{error,errors}`) and list `by_email`, `by_identifier`, `decisions`, `handshake` as present but unused by desktop.
9. Delete the "Known DTO mismatch" paragraph once repair item 6 lands.
