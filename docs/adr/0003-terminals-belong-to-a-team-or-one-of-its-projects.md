# Terminals belong to a team or one of its projects

A Terminal started on a project belongs to that project: it lists in the project's strip, not the team's, and a project's strip lists only its own. A project Tab shows its project's strip on every page (an issue opened in it included). A team Tab shows the team's own strip, or a project's while that project's page is open in it. Before this, every Terminal belonged to its team alone, and every team and project Tab on that team showed the same strip. We changed it because a project is where most of the issues worked in one session live, so its shells (an agent, a dev server, a checkout) belong with it rather than mixed in with every other project on the team.

The owner is a `Scope` (team, optional project). A stored record carries a nullable `project_id`. A project can have its own starting folder, and without one its new shells start in the team's.

## Considered Options

- **Per Tab.** Every open Tab gets its own Terminals, and closing a Tab closes them. It's simpler to reason about on screen, but stored Terminals would hang off Tabs, which come and go, while a project lasts.
- **Keep them per team.** It's the least work, but two project Tabs on one team show the same shells, which is what prompted the change.

## Consequences

- A project's Terminals still belong to its team. The team's sidebar count includes them, a team purge removes them, and a removed team's pinned panel lists them too, since the project Tabs that reached them closed with the team.
- `terminal_records.project_id` and `project_terminal_dirs.project_id` have no foreign key. Data sync rewrites project rows, and a cascade there could drop a running Terminal's record, so the team purge clears them itself.
- On a team Tab, moving between a project's page and another page switches strips. Panes holding the other strip's Terminals come back empty, because a Tab's panel state is per Tab, not per strip.
- Terminals stored before this change have no project and stay in their team's own strip.
