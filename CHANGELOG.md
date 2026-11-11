<!-- last-sha: bb325378ef33e074bd65cf26b6cd3b40f03be738 -->
**Version: v0.11.0**

# What's new

## 2026-11-11
- **Send an issue to Claude from the terminal.** While a terminal panel is open, a small robot button sits next to the issue's ID on its page and on each issue row in a project. Click it to type that issue's prompt into the terminal, ready for Claude Code.
- **Properties fit beside the terminal.** On issue and project pages, when a terminal panel is docked on the right, Properties move into a compact card under the description instead of squeezing the page. The card shows the main fields first, with "More" for the rest.
- **Hide issue properties.** Press `/` on an issue page, or use the command palette, to show or hide its Properties, just like on project pages. Your choice is remembered.
- **Long issue titles wrap.** Editing an issue's title on its page now wraps long titles onto several lines instead of cutting them off.
- **Command palette improvements.** The Cmd+K palette now scrolls through long result lists, and you can open Settings from it.

## 2026-11-10
- **Calendar for date fields.** Date fields on the issue, project and recurring issue forms, and on a project's page, now have a calendar you can open to pick a day. You can still type the date if you prefer.
- **Tidier issue form.** The issue form's fields are now chips with icons, like on the web, and each one opens its own dropdown of choices.
- **Clearer terminal folders.** Saving a terminal folder now confirms where new terminals will start, and terminals you restart or reopen later follow the new folder. The panel shows whether it's using the project's or the team's folder. If a folder can't be opened, you're told the terminal started in your home folder and how to give JAIT access in System Settings.
- **Inbox dot on the tab.** The Inbox tab shows a dot while you have unread notifications.
- **Pop-ups scroll.** Dialogs taller than the window, such as Settings or the command palette, now scroll instead of getting cut off.

## 2026-11-09
- **Terminal alerts.** When a program in a terminal needs you, for example Claude Code waiting on a permission prompt, finishing, or hitting an error, a dot appears on its tab, in the terminal panel and next to the team in the sidebar. If the app is in the background you also get a macOS notification; click it to jump straight to that terminal.
- **Forms open as full pages.** Creating or editing an issue, project or recurring issue now opens a full page with Back and a breadcrumb trail, like on the web, instead of a pop-up. Leaving the page discards the draft. Labels on the issue form are now picked from a dropdown.
- **More fields on the issue form.** The issue form now has Project, Assignee, Estimate and Due date fields, and you can attach files by picking or dropping them on the page; they upload once the issue is saved. A "Create more" box opens a fresh form after you create an issue so you can add several in a row.
- **Sidebar shows where you are.** The team's Board or Projects row stays highlighted while you have one of its issues or projects open. Team sections also have more space between them, and long team names now cut off neatly on one line.
- **Fixes.** Images in issues load again, and hovering over a card no longer jumps the page back to the top.

## 2026-11-08
- **Nervosa theme.** A new dark teal-and-orange theme, available only in the desktop app. Pick it in Settings.
- **See-through window.** New Window opacity and Blur sliders in Settings let you make the app's background translucent and blur what's behind it. Changes show up as you drag. Text, cards and menus stay solid.
- **Close tabs with Cmd+W.** Cmd+W now closes the tab you're on, and closing a tab also closes its terminals. If a program is still running in one of them, you're asked before the tab closes.
- **Status changes show up right away.** Moving an issue to Done, Canceled or an in-progress column now updates the board straight away, instead of waiting for the next sync to show or hide it.
- **Sub-issues on every issue.** Each issue page now has a Sub-issues section with a done count and progress bar. You can set, change or remove an issue's parent, add an existing issue as a sub-issue, and detach a sub-issue from its parent.
- **Dependency maps.** Click "Dependencies" on an issue or project, or use the command palette, to open a map of what's blocking what. An issue's map has a Dependencies tab with a graph, a depth selector and lists of blockers, plus a Hierarchy tab that shows the issue's parent and sub-issues as an outline (also reachable from "View tree →" on the issue). A project's map shows the blockers across all of its issues.
- **Dependency lines on the board.** Turn on "Dependencies" in the board's Display menu to draw lines between linked cards, with a legend bar along the top. Hover a card to highlight its lines and the cards it's linked to, and click a linked card to open its dependency map. Back takes you to the board.
- **More detail in project burndown charts.** Hover a project's burndown chart to see a card with the date and its Scope, Started, Completed and Expected counts.
- **Clicking a tab just focuses it.** A quick click on a tab no longer sometimes starts dragging it.
- **Clickable breadcrumbs.** The team name at the top of a page now takes you to that team's board, "Projects" on a project page goes back to the project list, and "My Issues", "Inbox" or a dashboard name takes you back to where you opened an issue from. Project pages also show the project's ID once it has synced, and issue pages no longer show the Search, Filter, Display and "+ New" controls.
- **Hover cards for projects.** Hover the Project field on an issue to see a card with the project's status, progress, lead, due date and the start of its description. The card gets out of the way when you click to open the dropdown.
- **Issue page shortcuts open the sidebar.** On an issue page, `s`, `p`, `j` and `l` now open the matching dropdown in the issue's sidebar, like on the web, instead of a picker that covered the sidebar. Clicking the field again closes it.
- **Icons display properly.** Arrows, the terminal-count badge, the "⋯" menu and other small symbols in the sidebar, board and list no longer show up as empty boxes.
- **Smoother terminal cursor.** Moving the cursor in a terminal without typing, for example jumping by word with Option+Arrow, now shows the cursor's new position straight away.

## 2026-11-07
- **Board rows.** Pick a "Rows" sub-grouping in the Display menu to split the board into rows that run across every column, lined up so you can scan them side by side. Empty rows start collapsed (click a row to expand or collapse it), a new "Show empty rows" option keeps them visible, and arrow-key navigation skips collapsed rows.
- **Display menu matches the web.** The Display menu now uses the same names and order as JAIT on the web: "Columns" on the board, "Grouping" in the list, and "No grouping" and "Parent issue" as options.
- **Sort a project's issues.** The Issues card on a project page has Sort chips for Newest, ID, Status, Updated and Priority. It sorts by ID unless you pick another.
- **Your edits stick through sync.** An open issue or project no longer overwrites newer changes from the web with stale text, and edits you're making aren't lost when a sync arrives.

## 2026-11-06
- **Drag to reorder tabs.** Drag a tab to a new spot in the tab bar; a dashed outline marks where it will land.
- **Sidebar matches the web.** Your team groups from JAIT on the web now show in the sidebar, with teams you own and teams you've joined in their own sections, in the same order as on the web. Teams you administer carry an ADMIN badge, and collapsing a group is saved back to the web.
- **Paste images into terminals.** Pasting while an image is on the clipboard now reaches programs that can read it, so tools like Claude Code can pull the image in.

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
