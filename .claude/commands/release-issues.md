---
description: After a published release, move the JRST issues it shipped to Released, label them with the version, and tell their reporters
argument-hint: "[vX.Y.Z]  (default: the newest published tag)"
---

You are marking the JAIT issues that shipped in a published JAIT Desktop release. Issues live in JAIT prod, team **47** (JRST), via the `jait` MCP tools. Never use the `mtasks` MCP. See `docs/agents/issue-tracker.md` for ids and conventions.

Run this **after** `scripts/release.sh` / `scripts/publish-release.sh` succeeds. The `vX.Y.Z` tag exists only once a version is published, and it marks the release boundary.

Lane ids: Done **212**, Released **254**, Cancelled **213**.

## Steps

1. **Pick the release.**
   - If `$ARGUMENTS` names a tag (`v0.13.3` or `0.13.3`), use it. Otherwise take the newest tag: `git tag --list 'v*' --sort=-v:refname | head -1`.
   - Verify it exists: `git rev-parse -q --verify refs/tags/<tag>`. If it doesn't, **stop**. The version isn't published, so nothing has shipped.
   - The previous release is the next tag down in that same sorted list. If there is none, stop and ask me what range to use.

2. **Collect the shipped tickets.**
   - `git log <prev>..<tag> --format='%h %s%n%b'`. Include merge commits, because a ticket can be named only in a PR's merge message or body.
   - Extract the unique `JRST-<n>` identifiers, case-insensitively, and normalise them to upper case.
   - Also list the commits in the range that name no ticket. Skip commits that only touch `CHANGELOG.md`, `Cargo.toml` or `Cargo.lock` version lines. Report these at the end so I can match them to tickets by hand. Don't guess a ticket for them.
   - If no identifiers are found, report the untagged commits and **stop**.

3. **Find or create the version label.** The label name is the tag itself, e.g. `v0.13.3`. Look it up with `list_labels` (team 47). If it's missing, `create_label` with that name and color `#64748b`.

4. **Classify each ticket.** Fetch it with `get_issue_by_identifier`. Then sort it into one of these:
   - **Ship:** it's in Done, or it's in Released under an older version label (that's a re-fix shipping again).
   - **Already done:** it's in Released with this release's label. Skip it so reruns are safe.
   - **Ask:** it's in Backlog or In Progress. The commit may be partial work, so don't move it without my say-so.
   - **Skip:** it's in Cancelled. Report it, don't move it.
   - **Not found:** the identifier doesn't resolve. Report it.

5. **Show the plan.** Print a table of identifier, title, current lane, current labels and action. If anything is in **Ask**, ask me about those tickets now. Apply the confirmed set after I answer. If nothing needs asking, go straight on.

6. **Apply, for each ticket being shipped:**
   - `update_issue` (team 47) with `lane_id: 254` and `label_ids: [<version label id>]`. This **replaces** all of its other labels (triage, wayfinder, older version labels, design and so on) with the one release label. That's intended.
   - `create_comment` (team 47), with the body: `Shipped in <tag>. Update JAIT (Settings → Check for updates) to get it.` The comment notifies the reporter so they know to retest on the new build.
   - If a call fails, keep going with the rest and collect the failures.

7. **Report.** List what moved, what was skipped or left for me and why, any failures, and the untagged commits from step 2.

## Hard rules

- Only touch issues whose identifier appears in the release's commit range.
- Never move an issue out of Cancelled, and never move a Backlog or In Progress issue without asking.
- Don't edit files, commit, push or create tags. This command only writes to JAIT.
