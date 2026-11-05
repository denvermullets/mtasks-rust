# JAIT Desktop

The macOS desktop client for the JAIT issue tracker: an offline-first mirror of a user's teams that syncs with the JAIT Rails app.

## Language

### Finding things

**Command palette**:
The Cmd+K overlay for jumping to an issue, or to a view, team or project. It never filters a view.
_Avoid_: Global search, quick open, Cmd+K search

**View search**:
The Cmd+F text that narrows the current view's list in place.
_Avoid_: Header search, global search

**Hover card**:
The small panel that appears under an issue reference or mention in a body when the pointer rests on it, showing a summary of the thing referenced. It appears after a short delay and goes away shortly after the pointer leaves.
_Avoid_: Tooltip, popover, preview

### Views

**Saved view**:
A named snapshot of one team board's display options, filters and view search, owned by one user and shared with the web app. Opening it never changes the user's own board settings, and it changes only when the user explicitly updates it.
_Avoid_: Filter preset, bookmark, view (alone)

**Modified view**:
An open Saved view whose display or filters the user has changed but not yet written back. It can be updated or reverted.
_Avoid_: Dirty view, unsaved view

**Tab**:
An open place in the main area: a team, a project, or a cross-team page such as My Issues, Inbox or a Dashboard. Each team, project or page has at most one tab, and moving around inside a team stays in its tab. At least one tab is always open.
_Avoid_: Workspace, window, session

**Home page**:
The one team board, Saved view or dashboard the user lands on at launch, shared with the web app.
_Avoid_: Default view, start page

### Dashboards

**Dashboard**:
A user-owned, cross-team page made of Dashboard groups, shared with the web app. It can be the user's Home page.
_Avoid_: Board (a single team's Lane columns), view (see Saved view)

**Dashboard group**:
A named, colored section of a Dashboard with its own Sources and a position among the Dashboard's groups.
_Avoid_: Widget, panel, section

**Source**:
A team, project or label that a Dashboard group pulls issues from. A team or project Source is either "all issues" or "assigned to me". Label Sources narrow the group's issues; if the group has only label Sources, it pulls in every issue on those labels' teams.
_Avoid_: Filter, feed, query

### Staying current

**Notification**:
A per-user record that someone acted on an issue the user cares about (a mention, a comment, an assignment). It is either unread or read; once read it never becomes unread again.
_Avoid_: Alert, activity (activity is an issue's history, not a user's feed)

**Inbox**:
The cross-team view listing the user's Notifications, newest first.
_Avoid_: Notification center, feed

**Notification poll**:
The frequent, cheap check for new or changed Notifications and the unread count.

**Data sync**:
The slower cycle that pushes queued local edits and pulls changed team data (issues, projects, labels, members).
_Avoid_: Sync lane (a Lane is a board column)

### History

**Activity**:
An issue's timeline of field changes, dependency changes, comments and incoming references, as the server recorded them.
_Avoid_: Event log, history

**Pending change**:
A desktop edit shown in an issue's Activity before the server has recorded it.
_Avoid_: Local event

### Files

**Attachment**:
A file listed on an issue, comment or project, separate from its text. It can be deleted on its own.
_Avoid_: File (too vague), upload

**Inline image**:
An image embedded in the text of a description or comment, appearing where it was pasted. It is not an Attachment and is not listed with them; it goes away only by removing it from the text.
_Avoid_: Attachment, embedded file

**Pending upload**:
An Attachment or Inline image the user has added on the desktop that has not reached the server yet. It shows as pending until it lands, or as failed if the server refuses it.
_Avoid_: Draft attachment, queued file

### Scheduling

**Recurring issue**:
A schedule template, owned by a team, from which the server creates a new issue on each run (daily, weekly or monthly). It is not itself an issue and never appears on a board; the issues it creates keep a "From recurring issue" link back to it, and survive its deletion.
_Avoid_: Recurring task, scheduled issue, issue template

**Next run**:
The date, in the Recurring issue's time zone, on which the server will next create an issue from it. The server calculates it, so the desktop shows "pending" instead of a date until a new schedule or start date has synced.
_Avoid_: Due date (an issue's deadline), next occurrence

**Starts on**:
The date the user picks for a Recurring issue's first run, or the date to resume from when rescheduling. It is sent only with a schedule change, and defaults to today; editing it counts as a schedule change even when it is set back to today.
_Avoid_: Start date (a project's date), first run

**Paused**:
The state of a Recurring issue that creates no issues until it is resumed. Pausing keeps the schedule; the opposite state is Active.
_Avoid_: Disabled, inactive, stopped

### Terminals

**Terminal**:
An interactive shell session that belongs to a team and runs only on this device. Any tab on that team can show it, and its contents never reach the server.
_Avoid_: Console, shell, agent session

**Open**:
The state of a Terminal whose shell is still alive, even if it's sitting at a prompt. A team's sidebar row counts its open Terminals.
_Avoid_: Running, active

**Busy**:
The state of an open Terminal with a program other than the shell in the foreground. Closing a busy Terminal or quitting the app asks first.
_Avoid_: Running, active
