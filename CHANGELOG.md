<!-- last-sha: 10f06e2a46b38714a7ef06399406e5ac1559f765 -->
**Version: v0.3.0**

# What's new

## 2026-11-06
- **Quick property pickers.** Hover a card or list row (or open an issue) and press `s` for status, `p` for priority, `j` for project or `l` for labels to open a searchable picker right beside it. Use ↑/↓ and Enter to choose, Escape to close; labels can be toggled several at a time, and you can create a new label straight from the search box.
- **Shortcuts match the web app.** Board shortcuts now line up with JAIT on the web: `c` creates an issue, Shift+D deletes, Shift+C collapses a section, and Shift+P / Shift+S create a project and sync. The old `h`/`j`/`k`/`l` board navigation keys are gone; use the arrow keys instead.
- **Send an issue to Claude.** With a terminal open on a project page, each issue row gets a "▶ Claude" button that types a ready-made prompt for that issue into your focused terminal.
- **Project page and terminal polish.** Project pages stack their cards neatly when the terminal panel is open and hide the search, filter and display controls that didn't apply there. A click in a terminal that jitters slightly no longer selects a stray character.

## 2026-11-05
- **Tabs.** Keep teams, projects, My Issues, the Inbox and dashboards open side by side in tabs. Switch with Cmd+1–9 or the Command palette. Each tab remembers its scroll position, search, focus, open issue, Back history and unsent comment drafts, and your tabs come back when you relaunch.
- **Built-in terminals.** Each team (and each project) gets its own terminals in a panel docked on the right. You can split the panel into two columns or a 2×2 grid, rename terminals, drag to resize, hide the panel without stopping anything, and set a working directory per team. Terminals follow your app theme and font, support mouse, selection, Cmd+C, Cmd+click links and Mac editing keys, and are restored on relaunch.
- **Safer quitting.** Closing the window or pressing Cmd+Q now asks first if a terminal is still running something, and closes every terminal cleanly.
- **Per-team display and filters.** Display options and filters are now saved per team, and My Issues gets its own.
- **Refreshed project page.** Project pages have new Burndown and Issues cards with properties you can toggle on and off.
- **Archiving removed, sync fixes.** Issue archiving is gone. Being removed from a team is now handled cleanly, and large teams load faster.

## 2026-11-01
- **Dashboards.** Build dashboards out of groups of issues drawn from teams, projects and labels. Filter by Today, Week, Hot or Open, sort them, narrow to your own issues, use the keyboard to move around, and set a dashboard as your Home page. Changes sync with the web.
- **Full Markdown rendering.** Descriptions and comments now render headings, lists (nested lists and task lists too), quotes, tables, links, inline code and syntax-highlighted code blocks. Click the text or press `e` to edit, and Escape to go back.
- **Hover cards.** Hover over an issue reference or an @mention to see a preview: status, priority, assignee, project, due date, a short excerpt, or a member's contact details and open issue count.
- **Visual polish.** The board, issue detail, projects, Inbox and dashboards have been restyled, bold and italic now work in every font, and the title bar matches your theme.
- **Access changes respected.** If your access to a team is revoked, its local copies are removed.

## 2026-10-31
- **Command palette.** Press Cmd+K to jump to any view, team, project, saved view or issue, in any team, from the keyboard.
- **Inbox and notifications.** A new Inbox with an unread badge, Unread/All filters, and keys to mark items read (`r`, `R`) or open them. Shift+I opens it from anywhere. Native macOS banners for new notifications can be turned on or off in Settings.
- **Saved views and Home page.** Your saved views from the web appear in the sidebar. You can open, create, rename, update, revert and delete them, and choose a team or view as the Home page that opens at launch.
- **Recurring issues.** See, create, edit, pause, resume and delete recurring issues, with schedules, time zones and next-run times. Issues show which recurring issue created them.
- **Attachments and images.** Attach files to issues, projects and comments by file picker, drag-and-drop, the `a` key or pasting. Images show as thumbnails and inline, with a lightbox you can step through. Failed uploads can be retried.
- **Accurate activity history.** An issue's activity now comes from the server and shows who changed what, with your unsynced edits marked as pending. The app also ships as a proper macOS `.app`.

## 2026-10-30
- **Themes and fonts.** Choose a color theme and a font in Settings. The whole app follows your choice.
- **More link types.** Dependencies can now be "relates to" or "duplicates", and issue detail groups them by kind.
- **Move issues between teams.** Move an issue to another team from issue detail, or press `t`.
- **Project discussions and comment control.** Post and reply to comments on a project, and delete comments you wrote.
- **Label and lane touches.** Rename, recolor and delete labels. Lane category icons now show on the board, in lists and in pickers.
- **Faster, sturdier sync.** Sync now only fetches what changed and adjusts how often it runs. Deletes reach the server, and edits you make while offline or after losing access stay safe and pending. A banner appears if your token is read-only.

## 2026-10-29
- **List view and My Issues.** Switch any board to a list with aligned columns and collapsible sections. A new My Issues view gathers everything assigned to you across teams.
- **Display options and filters.** Choose which properties show and filter issues by many fields. A new header bar holds Search, Display and +New.
- **@mentions and issue references.** Mention teammates and link issues with `#` (with autocomplete). Issues show where they're referenced.
- **Threaded comments and activity.** Comments support nested replies, and each issue has an activity timeline.
- **Project burndown charts, roadmap and sub-issues.** Projects get a burndown chart, the roadmap is organized into Now / Next / Later, and issues can have sub-issues.
- **Team and lane management, CSV import/export.** Create and edit teams and lanes, and import or export a team's issues as CSV.

## 2026-07-06
- **Issue detail view.** Open an issue to see and edit everything about it, including assignee and creator.
- **Labels.** Create and manage labels and add several to an issue.
- **Dependencies and comments.** Mark issues as blocking or blocked by each other, and comment on issues.
- **Projects and roadmap.** A new projects table with a detail page, plus a first roadmap view.
- **New sidebar and error messages.** A reorganized sidebar, and clear error messages, including when your sign-in stops working.

## 2026-05-01
- **Connect to JAIT.** Add your API token and server in Settings, and test the connection with one click.
- **Two-way sync.** Teams, issues and projects sync with your JAIT account. Changes you make offline go up when you reconnect.
- **Projects in the sidebar, sync status.** Projects are listed in the sidebar, and the status bar shows when you last synced.

## 2026-02-28
- **First release.** A keyboard-driven board for your issues that works offline.
