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

### Views

**Saved view**:
A named snapshot of one team board's display options, filters and view search, owned by one user and shared with the web app. Opening it never changes the user's own board settings, and it changes only when the user explicitly updates it.
_Avoid_: Filter preset, bookmark, view (alone)

**Modified view**:
An open Saved view whose display or filters the user has changed but not yet written back. It can be updated or reverted.
_Avoid_: Dirty view, unsaved view

**Home page**:
The one team board, Saved view or dashboard the user lands on at launch, shared with the web app.
_Avoid_: Default view, start page

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
