---
description: Review recent commits and prepend a new user-facing changelog entry to CHANGELOG.md
---

You are updating `CHANGELOG.md` at the repo root with a new entry summarizing recent shipped work for end users of JAIT Desktop, the macOS companion app for the JAIT issue tracker (a Rust/Iced app built into a `.app` and `.dmg` by `scripts/build-release.sh`).

## Steps

1. **Read the SHA marker.** Read the first line of `CHANGELOG.md`. It should match `<!-- last-sha: <hash> -->`.
   - If the file or marker exists, capture the hash and verify it's reachable: `git cat-file -e <hash>`. If it's not (e.g. a rebase rewrote history and orphaned the SHA), **stop** and ask me how to proceed — don't silently re-summarize everything.
   - **First run** (file or marker missing): treat this as a full-history pass — no limit. The repo is small (a few months old); cover everything.

2. **Find new commits.**
   - Normal run: `git log <hash>..HEAD --no-merges --oneline`.
   - First run: `git log --no-merges --oneline --reverse` (entire history, oldest-first).
   - If the output is empty, print `no new commits since last changelog entry` and **stop**. Do not modify the file.

3. **Group commits into user-visible themes.**
   - Read the commit messages (and `git show <sha> --stat` if a message is opaque) to understand each change. A `feat(...)` commit is the author declaring a feature — treat it as user-visible by default. If its message *sounds* internal ("notes", "tweak", "cleanup"), that's a contradiction worth a `git show --stat` before you decide, not a reason to drop it.
   - Drop only pure-internal commits: dependency bumps, tooling/CI/test-only, refactors with no user-visible effect. Don't drop something just because its message reads like a dev note — check what it actually touched first.
   - Fold related commits into a single bullet (e.g., several filter fixes → one "Better filtering" bullet).
   - **On a first run with lots of history**, produce multiple dated sections grouped by week or by themed batches rather than one giant list. Keep each section under ~6 bullets. Use the commit dates (`git log --format='%ci %h %s'`) to inform grouping.

4. **Write 2–5 plain-speak bullets.** Tone:
   - Speak to the user, not to engineers. "Issue comments now support @mentions" — not "Refactored CommentsController to add user mention parsing."
   - No commit hashes, no PR numbers, no internal ticket IDs (e.g. JAIT-124) in the bullet text. Those live in git history, not in front of users.
   - Optional: lead each bullet with a short bold lead-in (`**Comments on issues.**`) followed by the plain-language description, matching the style of existing entries.

5. **Prepend a new section.** Insert directly under the SHA-marker line and the `# What's new` heading (or after the last existing `## YYYY-MM-DD` section's preceding marker — match the file's current shape). Format:
   ```
   ## YYYY-MM-DD
   - **<lead-in>** <plain-language description>
   - …
   ```
   Use today's date (`date +%Y-%m-%d`). Only one section per date: if the top section already has today's date, add the new bullets to the top of that section instead of creating a second heading, and fold or reword any new bullet that overlaps an existing one there. Otherwise, do not modify any prior dated entries.

6. **Update the SHA marker.** Replace the value in `<!-- last-sha: ... -->` on line 1 with `git rev-parse HEAD`.

7. **Bump the version number.** The version is semver (`MAJOR.MINOR.PATCH`) and is shown on line 2 of `CHANGELOG.md` as `**Version: vX.Y.Z**`. Read the current version from `[package] version` in `Cargo.toml` and bump it based on the impact of these changes:
   - **patch** (`0.2.0` → `0.2.1`): only fixes and small polish.
   - **minor** (`0.2.1` → `0.3.0`): any new feature or noticeable behaviour change.
   - **major**: only when I ask for it.

   Then write the new version, without the `v`, to all three places. They must match or `scripts/build-release.sh` refuses to build:
   - `Cargo.toml`: `version = "..."` under `[package]`
   - `Cargo.toml`: `version = "..."` under `[package.metadata.bundle]` (this is the version the macOS `.app` reports)
   - `Cargo.lock`: the `version = "..."` line right after `name = "mtasks-rusty"`

   Edit only those version lines. Don't run `cargo update`, which would bump dependencies too.

8. **Show the diff and stop.** Run `git diff CHANGELOG.md Cargo.toml Cargo.lock` so I can review. Do not commit. Do not push.

## Hard rules

- Only edit `CHANGELOG.md` and the three version lines listed in step 7 (two in `Cargo.toml`, one in `Cargo.lock`). Don't touch other files.
- Never invent changes that aren't in the commit log.
- Skip commits that only modify `CHANGELOG.md` itself — those are the doc-update commits from prior runs of this command and should never be summarized.
- When in doubt, check before dropping. Run `git show <sha> --stat` and lean toward including borderline commits rather than silently omitting them — under-claiming hides real shipped work. Only drop a commit once you've confirmed it has no user-visible effect.
