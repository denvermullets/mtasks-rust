# Tabs park their state and reload data on switch

`App` keeps one set of live fields, which always belong to the active Tab. The other Tabs hold only parked session state in a `Vec<Tab>`, and switching stashes one Tab, restores the other, and reloads its data from the local database. We chose this because switching is already fast enough without caching (7–32 ms for team Tabs, and about 190–200 ms for cross-team Tabs before the `issues(team_id, deleted_at)` index, whose reload then took about 34 ms instead of about 185 ms), because moving these fields behind a per-Tab accessor would mean a wide rewrite of the 7,633-line `app.rs` with no change in behaviour, and because the local database stays the single source of truth for every Tab. The figures come from an M4 Max and exclude rasterising and GPU time.

## Considered Options

- **Move every per-Tab field behind `self.tab().x`.** It's cleaner to read, but the churn is large and it changes no behaviour. Worth revisiting if parked and live state start to drift apart.
- **Keep each Tab's data loaded in memory.** It avoids the reload, but every sync would have to refresh or invalidate each Tab's copy. It also wouldn't fix the slowest frames, which come from drawing cards rather than loading them.

## Consequences

- Every new per-Tab field must be added to the stash and restore path, or it will leak between Tabs.
- Widgets share scrollable ids across Tabs, so scroll position has to be restored explicitly on switch.
- A Tab can't assume its data is loaded while it's parked. Background results that arrive for a parked Tab must go to the local database and show up on the next reload, never into the live fields.
