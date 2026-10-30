# Issue tracker: JAIT (prod)

Issues and specs for this repo live in JAIT production, team **JAIT App (JRST), team_id 47**,
via the `jait` MCP server (`mcp__jait__*` tools). The `mtasks` MCP server points at local dev:
never use it for this repo's tracking.

Team members: Ryan Vaznis — user id 3.
Lanes: Backlog 210 · In Progress 211 · Done 212 · Cancelled 213.
Label ids: needs-triage 291 · needs-info 292 · ready-for-agent 293 · ready-for-human 294 ·
wontfix 295 · wayfinder:map 296 · wayfinder:research 297 · wayfinder:prototype 298 ·
wayfinder:grilling 299 · wayfinder:task 300.

## Conventions

- **Create an issue**: `create_issue` (team_id 47, title, description). Defaults to Backlog.
- **Read an issue**: `get_issue` (team_id + issue_id) or `get_issue_by_identifier` (`JRST-<n>`).
  Comments: `list_comments`.
- **List issues**: `list_issues` (team_id 47; filter by lane_id / project_id / assignee_id).
- **Sub-issue of a parent**: pass `parent_issue_id` on `create_issue`.
- **Comment**: `create_comment`.
- **Labels**: `list_labels` / `create_label`; set with `update_issue` `label_ids`
  (replaces the whole set — include existing ids).
- **Close**: `update_issue` lane_id 212 (Done). Won't-do / out-of-scope: lane_id 213 (Cancelled).
- Refer to issues by title (+ `JRST-<n>` identifier), never a bare numeric id.

## When a skill says "publish to the issue tracker"

Create a JAIT issue on team 47.

## When a skill says "fetch the relevant ticket"

Read it as in **Read an issue** above.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets.

- **Map project**: every map gets its own JAIT project, named `Wayfinder: <effort>` (lead user 3,
  status `started`), created **before** the map issue. The map and every child ticket carry its
  `project_id` so the effort reads as one board. It holds planning only — the roadmap/implementation
  projects a map produces are separate projects.
- **Map**: one issue labelled `wayfinder:map`, in the map project, holding Destination / Notes /
  Decisions-so-far / Not yet specified / Out of scope.
- **Child ticket**: `create_issue` with `parent_issue_id` = the map and `project_id` = the map
  project. Label `wayfinder:<type>` (`research` / `prototype` / `grilling` / `task`). No triage
  labels on map tickets. Tickets graduated from fog later get the same `project_id`.
- **Blocking**: native dependencies. `create_issue_dependency` with
  `issue_id=<child>, target_issue_id=<blocker>, direction=blocked_by`.
  Inspect with `list_issue_dependencies`; remove with `remove_issue_dependency`.
  A ticket is unblocked when every blocker is in Done or Cancelled.
- **Frontier query**: `list_issues` team 47, keep children of the map not in Done/Cancelled,
  drop any with an assignee or an open blocker; first in map order wins.
- **Claim**: `update_issue` assignee_id 3 (and lane 211 In Progress) — the session's first write.
- **Resolve**: `create_comment` with the answer, `update_issue` lane_id 212, then append a
  context pointer (gist + link) to the map's Decisions-so-far.
- **Out of scope**: `update_issue` lane_id 213, plus one line in the map's Out of scope section.
