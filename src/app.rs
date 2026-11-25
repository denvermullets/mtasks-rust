use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use iced::keyboard;
use iced::widget::{column, container, row, text_input};
use iced::{event, Element, Length, Subscription, Task, Theme};

use crate::api::{ApiClient, ApiError, CommentDto, TeamDto};
use crate::config::{self, Config};
use crate::csv_io;
use crate::db::models::{
    Comment, Issue, IssueDependency, Label, Lane, Member, Priority, Project, ProjectStatus,
    RoadmapCommitment, Team,
};
use crate::db::activity::{activity_for_issue, ActivityItem};
use crate::db::{connection, queries, sync_queue};
use crate::sync::activity::{self as activity_sync, FetchOutcome};
use crate::display::{
    self, CardProperty, DisplayOptions, FilterField, Filters, GroupBy, OrderBy, SubGroup, ViewMode,
};
use crate::fonts;
use crate::keys;
use crate::scroll::{self, ScrollKey};
use crate::sync::engine::{FullSyncReport, SyncEngine, SyncErrorKind};
use crate::sync::notif_poll::{self, PollOutcome, PollReport};
use crate::sync::scheduler::{CycleKind, Event as SchedEvent, Run, Scheduler};
use crate::palette_results::{PaletteData, PaletteNav, PaletteState};
use crate::theme as t;
use crate::views::toast::{Toast, ToastAction, ToastKind};
use crate::views::{
    board, command_palette, csv_import, display_panel, header, issue_detail, issue_form, issue_list,
    issue_picker, labels as labels_view, placeholder,
    project_detail, property_picker, projects as projects_view, roadmap as roadmap_view, settings, status_bar,
    tab_bar, team_list, team_members, team_picker, toast,
};

mod tab_update;
mod updater_wiring;
pub use tab_update::TabMessage;

#[derive(Debug, Clone)]
pub enum Message {
    // Navigation
    /// Toggle a team's sidebar expansion (chevron / team-header click).
    ToggleTeamExpanded(String),
    /// Click on a sidebar team row outside its chevron.
    SelectTeamRow(String),
    /// Select a team and switch the right pane to `view` (team sub-row click).
    SelectTeamView(String, View),
    /// Tab bar actions; handled in `app/tab_update.rs`.
    Tab(TabMessage),
    WindowResized(iced::Size),
    /// Terminal panel and keys; handled in `terminal/state.rs`.
    Terminal(crate::terminal::TerminalMessage),
    /// Open a Saved view (local id) on its team's board.
    OpenSavedView(String),
    /// Pause (false) or resume (true) a recurring issue (local id); local
    /// write plus queued push.
    ToggleRecurringActive(String, bool),
    /// Open the recurring issue form prefilled to edit one (local id; JRST-177).
    EditRecurringIssue(String),
    /// Open the "New recurring issue" form (JRST-172) for the selected team.
    NewRecurringIssue,
    RecurringFormTitleChanged(String),
    RecurringFormDescriptionChanged(crate::undo_editor::EditorAction),
    RecurringFormPriorityChanged(Priority),
    /// `None` = server default.
    RecurringFormTimeZoneChanged(Option<String>),
    RecurringFormFrequencyChanged(crate::views::recurring_issue_form::Frequency),
    RecurringFormIntervalChanged(String),
    /// 0 = Sunday.
    RecurringFormWeekdayChanged(i64),
    RecurringFormDayOfMonthChanged(String),
    RecurringFormStartsOnChanged(String),
    RecurringFormAssigneeChanged(crate::views::recurring_issue_form::Choice),
    RecurringFormProjectChanged(crate::views::recurring_issue_form::Choice),
    RecurringFormLaneChanged(crate::views::recurring_issue_form::Choice),
    RecurringFormLabelToggled(String),
    /// Validate and save the form: local row plus queued create.
    SaveRecurringIssue,
    /// Ask to delete a recurring issue (local id); opens the confirm modal.
    DeleteRecurring(String),
    /// Confirmed: delete the recurring issue locally and queue the push.
    DeleteRecurringConfirmed,
    /// Drop the open Saved view's unsaved changes (local only).
    RevertSavedView,
    /// Ask to delete a Saved view (local id).
    ConfirmDeleteSavedView(String),
    ConfirmDeleteDashboard(String),
    DeleteDashboardConfirmed,
    ConfirmDeleteDashboardGroup(String),
    DeleteDashboardGroupConfirmed,
    OpenRenameSavedView(String),
    DeleteSavedViewConfirmed,
    /// Make a Saved view (local id) the user's Home page.
    SetSavedViewAsHome(String),
    /// Switch the right pane to a non-team-scoped view (e.g. My Issues).
    NavigateToView(View),
    /// Open a dashboard (its local id) from the sidebar.
    OpenDashboard(String),
    /// Move a dashboard group (local id) up or down.
    MoveDashboardGroup(String, crate::db::dashboards::MoveDirection),
    /// Pick a dashboard filter tab (Today / Week / Hot / Open).
    SetDashboardFilter(crate::dashboard_eval::Filter),
    SetDashboardSort(crate::dashboard_eval::Sort),
    /// Local team id, or None for all teams.
    SetDashboardTeam(Option<String>),
    SetDashboardStatus(Option<String>),
    /// Dashboard header: pick an assignee (or Unassigned); None clears it.
    SetDashboardAssignee(Option<crate::dashboard_eval::AssigneeFilter>),
    SetDashboardLabel(Option<String>),

    // Team menu (sidebar kebab)
    /// Open/close the per-team menu on an expanded team header.
    ToggleTeamMenu(String),
    /// Make a team's board the user's Home page (local UUID).
    SetTeamAsHome(String),
    /// Make a dashboard the user's Home page (local UUID).
    SetDashboardAsHome(String),
    /// Show/hide archived teams in the sidebar.
    ToggleShowArchivedTeams,
    /// Collapse or expand a sidebar group (Rails group id).
    ToggleSidebarGroup(String),
    /// Open the read-only members panel for a team (local UUID).
    OpenTeamMembers(String),
    /// Open a team's Rails settings page in the browser. Teams and lanes have
    /// no API write endpoints, so editing them is web-only.
    OpenTeamWebSettings(String, TeamWebSection),

    // Issue CRUD
    CreateIssue,
    EditFocusedIssue,
    SaveIssue,
    ConfirmDeleteFocusedIssue,
    DeleteConfirmed,

    // Issue form fields
    IssueTitleChanged(String),
    IssueDescriptionChanged(crate::undo_editor::EditorAction),
    IssuePriorityChanged(Priority),
    IssueLaneChanged(String),
    /// Toggle a label in the issue form's multi-select (by local UUID).
    IssueLabelToggled(String),
    /// Open/close one property chip's dropdown on the issue form page.
    IssueFormMenuToggled(crate::views::issue_form::FormMenu),
    /// Dismiss the open issue form dropdown (Escape / click outside).
    IssueFormMenuClosed,
    /// Open/close a date field's calendar (field, its current text).
    DatePickerToggled(crate::views::date_picker::DateField, String),
    /// Step the open calendar by this many months.
    DatePickerMonth(i32),
    /// Dismiss the open calendar (Escape / click outside).
    DatePickerClosed,
    /// A calendar pick: close it, then run these (set the field, commit).
    DatePicked(Vec<Message>),
    /// Issue form Assignee pick (local member UUID; `None` = no assignee).
    IssueAssigneeChanged(Option<String>),
    /// Issue form Project pick (local UUID; `None` = no project).
    IssueProjectChanged(Option<String>),
    /// "New Project" at the bottom of the issue form's Project list: open the
    /// project form, then come back to the issue form with it picked.
    IssueFormNewProject,
    /// Issue form Estimate pick, in points.
    IssueEstimateChanged(Option<f64>),
    IssueDueDateChanged(String),
    /// "Attach files" on the issue form page: open the multi-file picker.
    PickIssueFormFiles,
    /// Files picked or dropped on the issue form page; held until Save.
    IssueFormFilesPicked(Vec<std::path::PathBuf>),
    /// Remove a held file (by position) from the issue form page.
    RemoveIssueFormFile(usize),
    IssueCreateMoreToggled(bool),

    // Issue detail — buffered text fields (committed on Enter / nav-away)
    /// The title editor (it wraps long titles; Enter commits, no newlines).
    IssueDetailTitleChanged(crate::undo_editor::EditorAction),
    IssueDetailDescriptionChanged(crate::undo_editor::EditorAction),
    /// Click on the rendered description: open the editor with focus.
    EditIssueDescription,
    StopEditingIssueDescription,
    EditProjectDescription,
    StopEditingProjectDescription,
    IssueDetailDueDateChanged(String),
    IssueDetailEstimateChanged(String),
    /// Flush buffered detail fields to the DB (text field `on_submit`).
    CommitIssueDetail,
    /// A click landed outside an active issue-detail field: close the
    /// description editor and commit the buffers.
    IssueDetailClickedOutside,
    // Issue detail — live-apply sidebar assignee pick_list
    /// Assignee picker changed; `None` = "Unassigned". Carries a local member UUID.
    IssueDetailAssigneeChanged(Option<String>),

    // Issue and project detail — comments (threaded)
    /// Composer text-editor action.
    CommentComposerChanged(crate::undo_editor::EditorAction),
    /// "Comment" button pressed: persist the composer body locally + enqueue push.
    SubmitComment,
    /// "Reply" pressed on a comment (local id): open the inline reply composer.
    StartReply(String),
    CancelReply,
    ReplyComposerChanged(crate::undo_editor::EditorAction),
    /// Submit the inline reply composer as a child of `reply_to`.
    SubmitReply,
    /// "Delete" pressed on one of the user's own unsynced comments (local id).
    ConfirmDeleteComment(String),
    DeleteCommentConfirmed,
    /// "Delete" pressed on a synced attachment card (local attachment id).
    ConfirmDeleteAttachment(String),
    /// Cancel a pending upload (local row id): drops the row, queue entries and outbox file.
    CancelAttachmentUpload(String),
    DeleteAttachmentConfirmed,
    /// "Retry" on a failed upload card (local attachment id).
    RetryUpload(String),
    /// "Remove" on a failed upload card (local attachment id).
    RemoveFailedUpload(String),
    /// Autocomplete row `n` chosen (click or Tab) in the open `@`/`#` suggestion list.
    PickSuggestion(usize),
    /// Enter/Tab in an editor with the suggestion list open: take the highlighted row.
    AcceptSuggestion,
    /// Up/Down (or Ctrl-p/n) in an editor with the suggestion list open.
    MoveSuggestion(i32),
    /// Comments fetched from Rails for a local issue UUID; merged into SQLite.
    CommentsFetched(String, Result<Vec<CommentDto>, String>),
    /// Activity fetch for an issue's detail finished (JRST-149); `true` when entries were stored.
    /// Issue id and how many Activity entries the fetch stored.
    ActivityFetched(String, usize),
    /// A file card was clicked: open the Attachment in its default app (JRST-186).
    OpenAttachment(crate::db::attachments::Attachment),
    /// An image Attachment card was clicked: open the lightbox on it.
    OpenLightbox(crate::db::attachments::Attachment),
    /// An inline image in a description/comment was clicked: open the lightbox on it.
    OpenInlineImage(crate::views::body::InlineImage),
    /// ←/→ in the lightbox: step by this many images.
    LightboxStep(isize),
    /// `o` in the lightbox: open the current image in the default app (Preview).
    OpenLightboxExternally,
    /// Bytes for an Attachment downloaded on a cache miss; cache them, then open.
    AttachmentDownloaded(crate::db::attachments::Attachment, Result<Vec<u8>, String>),
    /// Bytes for an image fetched into the cache for display (JRST-187), keyed by blob key.
    ImageFetched(String, Result<Vec<u8>, String>),
    ProjectCommentsFetched(String, Result<Vec<CommentDto>, String>),
    /// Result of the fresh fetch made when an issue detail is opened.
    IssueFetched(Result<(), String>),

    // Issue detail — sub-issues
    /// "+ New sub-issue" pressed: open the create modal with the parent's local
    /// UUID pre-filled as `parent_issue_id`.
    CreateSubIssue(String),

    // Issue detail — dependencies (blocking / blocked-by)
    /// Open the issue-detail view for another issue by its local UUID (e.g. a
    /// click on a linked dependency row). Rebuilds the detail buffer, unlike
    /// `NavigateToView(View::IssueDetail(_))` which only swaps the view.
    OpenIssueDetail(String),
    /// Click on a link in rendered body text, carrying its typed target.
    OpenBodyLink(crate::mentions::LinkTarget),
    /// The hovered body span changed (`None` = pointer left). JRST-299 consumes it.
    /// A body paragraph's hovered span changed. The id is the reporting
    /// widget's owner id (see `hover_rich::HoverRich::on_hover`).
    BodyHover(u64, Option<crate::views::hover_rich::Hover<crate::mentions::LinkTarget>>),
    /// The pointer entered (`Some`) or left (`None`) a row that anchors a hover
    /// card, with the row's window rectangle (`views::row_hover`). The id is
    /// the row widget's owner id.
    RowHover(u64, Option<(crate::body_hover::Anchor, iced::Rectangle)>),
    /// A hover-card timer elapsed (see `hover_card::Effect`).
    HoverTimer(u64),
    /// The pointer entered or left the hover card itself.
    HoverCardEntered,
    HoverCardLeft,
    /// The mouse wheel or trackpad scrolled somewhere.
    HoverScrolled,
    /// Leave an issue detail that was opened from My Issues and return there.
    BackToMyIssues,
    BackToDashboard,
    /// Restore the open dashboard's header and focus to the defaults.
    ResetDashboard,
    /// Open the searchable issue picker to add a dependency in `direction`.
    OpenDependencyPicker(DepDirection),
    /// Open the issue picker to set / change the open issue's parent.
    OpenParentPicker,
    /// Open the issue picker to add an existing issue as a sub-issue.
    OpenChildPicker,
    /// Remove the open issue's parent.
    RemoveParent,
    /// Detach a sub-issue (local UUID), making it top-level.
    DetachSubIssue(String),
    /// Step the open picker's link kind (Tab forward, Shift+Tab back).
    CycleDependencyKind(bool),
    /// Set the open picker's link kind (clicking a kind chip).
    SetDependencyKind(DepDirection),
    /// The picker's search box changed.
    DependencyPickerSearchChanged(String),
    /// A target issue (local UUID) was chosen in the picker → create the edge.
    DependencyPickerSelected(String),
    /// Remove a dependency edge by its local dependency-row UUID.
    RemoveDependency(String),
    /// Open the "Move to team" picker for an issue (local UUID).
    OpenMoveTeamPicker(String),
    /// `t` on the board: open the move picker for the focused issue.
    MoveFocusedIssueToTeam,
    /// A target team (local UUID) was chosen in the move picker.
    MoveTeamSelected(String),

    // Labels
    OpenLabels,
    LabelNameChanged(String),
    LabelColorChanged(String),
    CreateLabel,
    /// Open edit mode for a label (by local UUID).
    SelectLabelForEdit(String),
    EditLabelNameChanged(String),
    EditLabelColorChanged(String),
    SaveLabelEdit,
    ConfirmDeleteLabel,
    DeleteLabelConfirmed,
    /// Toggle a label on the issue targeted by the label modal (by local UUID).
    ToggleIssueLabel(String),
    SaveIssueLabels,

    // Move
    MoveIssueNextLane,
    MoveIssuePrevLane,

    // Priority quick-set
    SetFocusedPriority(i32),

    // Board navigation
    FocusNextIssue,
    FocusPrevIssue,
    FocusNextLane,
    FocusPrevLane,
    /// A click on a board card at (lane_idx, issue_idx). The handler decides
    /// whether to treat as a single-click (focus) or double-click (open edit).
    CardPressed(usize, usize),
    /// The pointer entered (`Some`: local issue id, window rectangle) or left
    /// (`None`) a board card or list row; the id is the reporting widget's
    /// owner (`views::row_hover`). Rails' card shortcuts act on the hovered
    /// card and anchor their picker beside it.
    CardHover(u64, Option<(String, iced::Rectangle)>),

    // UI
    CloseModal,
    ToggleHelp,
    /// Cmd+B: collapse or expand the left teams sidebar.
    ToggleSidebar,
    /// `/` or the palette command on the issue or project page: show or hide
    /// that page's Properties.
    ToggleProperties,
    /// The inline Properties card's "More" / "Less": show or hide its
    /// secondary fields, on every issue and project page until toggled back.
    ToggleMoreProperties,
    /// Palette, project page: always stack Burndown above Issues, or not.
    ToggleProjectCardsStacked,
    /// Project page, Terminal showing: type this issue's jait MCP prompt into
    /// the focused Terminal (no Enter).
    SendIssuePrompt(String),
    /// Palette: dock the Terminal panel at the bottom or down the right.
    ToggleTerminalDock,
    /// Open this issue's dependency map (JRST-442): Dependencies tab, default
    /// depth. From the issue detail's button or the palette.
    OpenDependencyMap(String),
    /// Switch the open dependency map between its Dependencies and Hierarchy tabs.
    SetDependencyMapTab(DependencyMapTab),
    /// Change the open dependency map's depth (clamped to 1..=MAX by the handler).
    SetDependencyMapDepth(u8),
    /// Click on a non-focus card of the open dependency map: refocus on it.
    FocusDependencyMap(String),
    /// Open this issue's dependency map on the Hierarchy tab (the issue
    /// detail's "View tree →").
    OpenHierarchyTree(String),
    /// Expand or collapse a Hierarchy outline node (not persisted).
    ToggleHierarchyNode(String),
    /// Leave the dependency map for its issue's detail (Back, Escape, the
    /// breadcrumb's issue crumb).
    CloseDependencyMap,
    /// Open a project's dependency map (JRST-445), Active scope. From the
    /// project page's Dependencies button or the palette.
    OpenProjectDependencyMap(String),
    /// Active / All switch on the project dependency map.
    SetProjectMapActiveOnly(bool),
    /// Leave the project dependency map for the project page.
    CloseProjectDependencyMap,
    /// Cmd+K: open the command palette.
    OpenCommandPalette,
    /// The palette's text field changed.
    PaletteQueryChanged(String),
    /// A palette row was chosen; closes the palette and navigates.
    PaletteChoose(crate::palette_results::PaletteNav),
    /// Up/Down in the palette: move the highlight by this many rows (wraps).
    PaletteMove(i32),
    /// Enter in the palette: choose the highlighted row, if any.
    PaletteSubmit,
    /// Rails' `s`/`p`/`j`/`l`: open the status/priority/project/labels
    /// popover on the key target (see `key_target`).
    OpenPropertyPicker(property_picker::Field),
    /// The popover's search box changed.
    PropertyPickerQuery(String),
    /// Up/Down in the property picker.
    PropertyPickerMove(i32),
    /// Enter in the property picker: apply the highlighted row.
    PropertyPickerSubmit,
    /// A property picker row was clicked (index into its items).
    PropertyPickerChoose(usize),
    /// The project popover's "New Project" footer.
    PropertyPickerNewProject,

    // Header bar
    /// Header search box changed; filters the current view's items.
    HeaderSearchChanged(String),
    /// Dashboard header: flip the Mine toggle.
    ToggleDashboardMine,
    /// A dashboard or group ⋮ button: open its menu, or close it if open.
    ToggleDashboardMenu(crate::views::dashboard::MenuTarget),
    CloseDashboardMenu,
    /// A dashboard or group menu item: closes the menu, then runs the message.
    DashboardMenuPick(Box<Message>),
    /// Focus the header search box (Cmd+F).
    FocusSearch,
    /// "Display" button: open the display-options popover.
    OpenDisplayOptions,
    /// Save the open Modified view's working copy as its new definition.
    UpdateSavedView,
    /// "Filter" button: open the filter popover.
    OpenFilterPanel,
    /// "Save view…" in the display popover: ask for a name.
    OpenSaveView,
    OpenSaveAsNewView,
    SaveViewNameChanged(String),
    /// Create the Saved view from the current board state.
    SaveViewSubmit,
    /// Sidebar "New dashboard": open the create modal.
    OpenNewDashboard,
    /// Opens the rename/description modal for the dashboard with this local id.
    OpenEditDashboard(String),
    EditDashboardSubmit,
    NewDashboardNameChanged(String),
    NewDashboardDescriptionChanged(String),
    NewDashboardSubmit,
    /// Dashboard "Add group" modal (state in `App::group_form`).
    OpenNewGroup,
    /// Open the group modal pre-filled for the group (local id).
    OpenEditGroup(String),
    GroupNameChanged(String),
    GroupDescriptionChanged(String),
    GroupColorPicked(String),
    GroupToggleSource(crate::db::dashboards::SourceType, i64),
    GroupSetIncludeAll(crate::db::dashboards::SourceType, i64, bool),
    GroupSubmit,
    /// Inert click target (swallows clicks inside a popover).
    Noop,
    SetViewMode(ViewMode),
    /// `v`: flip between Board and List.
    ToggleViewMode,
    /// List mode: collapse/expand the section with this title (header click).
    ToggleSection(String),
    /// Collapse/expand a dashboard group (its local id) on the open dashboard.
    ToggleDashboardGroup(String),
    /// Board sub-group row (swimlane) with this title: collapse/expand it
    /// (row header click).
    ToggleSwimlane(String),
    /// `C`: collapse/expand the focused list section, or the focused board row.
    ToggleFocusedSection,
    /// A board lane / the list scrolled: (key, absolute y offset, viewport height).
    Scrolled(ScrollKey, f32, f32),
    SetGroupBy(GroupBy),
    SetSubGroup(SubGroup),
    SetOrderBy(OrderBy),
    SetCompletedFilter(display::CompletedFilter),
    ToggleShowSubIssues,
    ToggleShowEmptyGroups,
    ToggleShowEmptyRows,
    ToggleShowDependencies,
    ToggleCardProperty(CardProperty),
    ToggleFilter(FilterField, String),
    ClearFilters,

    // Settings
    OpenSettings,
    SettingsBaseUrlChanged(String),
    SettingsTokenChanged(String),
    /// Theme picked in Settings: applies live and saves to config immediately.
    SettingsThemeChanged(t::ThemeChoice),
    /// Font picked in Settings: saves to config; applies after restart.
    SettingsFontChanged(fonts::FontChoice),
    SettingsTerminalFontChanged(fonts::FontChoice),
    SettingsTerminalSizeChanged(u16),
    SettingsNativeNotificationsToggled(bool),
    /// Window opacity slider moved (percent): applies live.
    SettingsWindowOpacityChanged(u8),
    /// Background blur slider moved (radius): applies live.
    SettingsWindowBlurChanged(u8),
    /// An opacity/blur slider was released: save both to config.
    SettingsWindowEffectsReleased,
    /// A window opened: apply the configured background blur to it.
    WindowOpened,
    TestConnection,
    TestConnectionResult(Result<usize, String>),
    SaveSettings,

    // Updates (project 320)
    /// "Check for updates" (Settings button; the palette entry reuses it): a
    /// manual check. Dev builds explain that updates are disabled.
    UpdateCheckClicked,
    /// Settings → Updates: open the DMG download from the manifest.
    UpdateDmgClicked,
    /// Settings → Updates: the "Check for updates automatically" toggle.
    UpdateAutoCheckToggled(bool),
    /// Periodic background check trigger (the machine decides if one is due).
    UpdaterTick,
    /// The Mac woke from sleep.
    UpdaterWoke,
    /// Any updater input: a task result or a UI action, fed to the machine by
    /// `App::apply_updater_event`.
    Updater(crate::updater::machine::Event),
    /// The status bar's "↑ X ready" chip: open its popover.
    OpenUpdatePopover,
    /// Restart now (popover): ask the updater, with the real busy-Terminal state.
    RestartNowClicked,
    /// "Restart" in the "Restart to update?" confirm.
    RestartConfirmed,

    // Sync
    /// Manual sync (`s`, toast retry): runs immediately.
    SyncNow,
    /// Scheduler timer fired.
    SyncTick,
    /// Keyboard/mouse activity, for idle tracking.
    UserInput,
    WindowFocused,
    WindowUnfocused,
    /// The one entry to quitting: the window's close button and Cmd+Q both send it.
    QuitRequested,
    /// The quit ask was confirmed: close every Terminal, then exit.
    QuitConfirmed,
    /// Every Terminal has closed (or the wait ran out): exit.
    QuitReady,
    /// Once, at startup: route system terminate requests (Cmd+Q, Dock Quit, AppleScript quit) to `QuitRequested`.
    InstallTerminateHook,
    SyncCompleted(FullSyncReport),
    /// A Notification poll finished.
    NotifPollCompleted(PollOutcome),

    // Toasts
    /// Dismiss a toast by id (× button).
    DismissToast(u64),
    /// Timer tick that expires aged-out non-error toasts.
    ToastTick,
    /// "Retry now" on a toast: dismiss it and trigger an immediate sync.
    RetrySyncFromToast(u64),
    /// A toast button (or its ×) was pressed: remove the toast, then handle the message.
    ToastActed(u64, Box<Message>),

    // Project CRUD
    CreateProject,
    SaveProject,

    // Project form fields
    ProjectNameChanged(String),
    ProjectDescriptionChanged(crate::undo_editor::EditorAction),
    ProjectStatusChanged(ProjectStatus),
    ProjectPriorityChanged(Priority),
    ProjectLeadChanged(Option<String>),
    ProjectStartDateChanged(String),
    ProjectDueDateChanged(String),
    ProjectRoadmapCommitmentChanged(Option<RoadmapCommitment>),

    // Projects table (list view)
    /// Open the project detail view for a local project UUID (row click).
    OpenProjectDetail(String),
    /// Change the table sort key; re-selecting the active key flips direction.
    ProjectSortChanged(ProjectSort),
    /// Change how a project page's Issues list is sorted.
    ProjectIssueSortChanged(display::ProjectIssueSort),
    /// Change which of a project page's issues are listed (All / Active).
    ProjectIssueFilterChanged(display::ProjectIssueFilter),
    /// Toggle the "Hide completed" filter.
    ToggleHideCompletedProjects,

    // Roadmap
    /// Open the "Add project to roadmap" picker preset to `commitment`.
    OpenRoadmapAdd(RoadmapCommitment),
    /// Toggle a roadmap card's ⋮ commitment menu (local project UUID).
    ToggleRoadmapMenu(String),
    CloseRoadmapMenu,
    /// Move a project to a lane, or off the roadmap with `None`.
    SetRoadmapCommitment(String, Option<RoadmapCommitment>),
    /// The pointer went down on a roadmap card in `from`: a click or a drag.
    RoadmapDragPress(String, RoadmapCommitment),
    RoadmapDragMove(iced::Point),
    /// The pressed card's pointer entered / left a lane.
    RoadmapDragEnter(RoadmapCommitment),
    RoadmapDragLeave(RoadmapCommitment),
    /// Released: a click opens the project, a drag over another lane moves it.
    RoadmapDragEnd,

    // Attachments
    /// "Attach file" on the issue detail: open the multi-file picker.
    PickAttachmentFiles,
    /// Files chosen for the open issue (picker, drag-and-drop, shortcut all
    /// send this). Each is validated, copied to the outbox and queued.
    FilesPicked(Vec<std::path::PathBuf>),
    /// Cmd+V with an image-only clipboard in the issue description editor.
    PasteImageIntoDescription,
    PasteImageIntoProjectDescription,
    /// Cmd+V with an image-only clipboard in the comment / inline reply composer.
    PasteImageIntoComment,
    PasteImageIntoReply,
    /// A file dropped on the window; routed by `files::drop::drop_target`.
    FileDropped(std::path::PathBuf),
    /// The batch window after the first `FileDropped` closed: route the whole
    /// buffered drop at once.
    FlushDrops,
    /// "+ Attach file" on a comment composer (issue or project detail).
    PickCommentFiles,
    /// Files chosen for the open detail view's comment composer (picker now;
    /// drag-and-drop sends this too). Staged as chips, sent with the comment.
    CommentFilesPicked(Vec<std::path::PathBuf>),
    /// Remove a staged composer file (local attachment id) before sending.
    RemoveStagedFile(String),

    // CSV import / export (sidebar footer)
    /// Export the selected team's issues via a native save dialog.
    ExportCsv,
    /// Result of the save dialog + write: Ok(Some(path)) saved, Ok(None) cancelled.
    CsvExported(Result<Option<String>, String>),
    /// Open the native file picker for an import.
    PickCsvImport,
    /// Picked file contents: Ok(Some((file name, text))), Ok(None) cancelled.
    CsvImportLoaded(Result<Option<(String, String)>, String>),
    ConfirmCsvImport,
    /// Switch which lane a picked project will land in (chip selector).
    RoadmapAddLaneChanged(RoadmapCommitment),
    /// Assign an off-roadmap project (local UUID) to the picker's current lane.
    RoadmapAssignProject(String),

    // Project detail — buffered text fields (committed on Enter / nav-away)
    ProjectDetailNameChanged(String),
    ProjectDetailDescriptionChanged(crate::undo_editor::EditorAction),
    ProjectDetailStartDateChanged(String),
    ProjectDetailDueDateChanged(String),
    /// Flush buffered detail fields to the DB (text field `on_submit`).
    CommitProjectDetail,
    /// A click landed outside an active project-detail field: close the
    /// description editor and commit the buffers.
    ProjectDetailClickedOutside,
    // Project detail — live-apply sidebar pick_lists
    ProjectDetailStatusChanged(ProjectStatus),
    ProjectDetailPriorityChanged(Priority),
    ProjectDetailLeadChanged(Option<String>),

    // Inbox
    /// Move the Inbox selection by a signed step (j/k).
    InboxMove(isize),
    /// Select the Inbox row at this index and open its issue (click).
    InboxOpenAt(usize),
    /// Mark the selected Inbox Notification read (`r`).
    InboxMarkRead,
    /// Open the selected Notification's issue and mark it read (Enter).
    InboxOpen,
    /// The one-shot fetch for a Notification's un-mirrored issue finished
    /// (issue remote id, result).
    InboxIssueFetched(i64, Result<(), String>),
    /// Leave an issue detail opened from the Inbox and return there.
    BackToInbox,
    /// A native banner was clicked: open its Notification's issue, or the Inbox.
    BannerClicked(crate::db::notifications::BannerTarget),
    InboxMarkAllRead,
    /// Switch the Inbox to the other filter (f).
    InboxToggleFilter,
    /// Set the Inbox filter (filter tabs).
    InboxSetFilter(crate::db::notifications::Filter),

    // Keyboard event
    KeyPressed(keyboard::Key, keyboard::Modifiers),

    /// A single-line input changed (see `undo_input`): record it, then
    /// handle the field's own message.
    TextInputEdited(crate::undo_input::Edit<Message>),
    /// Cmd+Z or Cmd+Shift+Z pressed in a focused text field.
    TextKey(crate::undo_input::KeyAction),
    /// Apply that key to the focused input, if it is one with a history.
    TextKeyOn { key: Option<String>, action: crate::undo_input::KeyAction },
}

/// The comment composers of the open detail view (issue or project).
struct CommentDrafts<'a> {
    comment: &'a mut crate::undo_editor::UndoableContent,
    reply_to: &'a mut Option<String>,
    reply: &'a mut crate::undo_editor::UndoableContent,
    suggest: &'a mut Option<crate::mentions::SuggestState>,
    files: &'a mut crate::views::issue_detail::ComposerFiles,
}

/// Longest the app waits for Terminals to close on quit: the 2 second
/// grace, the 5 second kill wait, and some slack.
const QUIT_WAIT: std::time::Duration = std::time::Duration::from_secs(8);

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    None,
    IssueForm,
    DeleteConfirm(String),
    /// Ask before closing a busy Terminal.
    TerminalCloseConfirm(crate::terminal::TerminalId),
    /// Ask before quitting with a busy Terminal.
    QuitConfirm,
    /// "Restart to update?": Restart now with a busy Terminal. The Quit
    /// confirm's sibling; Not now keeps the install-on-quit plan.
    RestartConfirm(semver::Version),
    /// Ask before closing a Tab whose Terminal strip has a busy Terminal.
    TabCloseConfirm(crate::tabs::TabKey),
    /// The update chip's popover (versions, Release notes, Restart now).
    UpdatePopover,
    Settings,
    ProjectForm,
    /// "New recurring issue" form; state in `App::recurring_form`.
    RecurringForm,
    /// Confirm deleting an unsynced comment (local id) and its replies.
    DeleteCommentConfirm(String),
    /// Confirm deleting a synced attachment (local attachment id).
    DeleteAttachmentConfirm(String),
    /// Full-size image viewer over an ordered list of images (see `files::lightbox`).
    Lightbox(crate::files::lightbox::LightboxState),
    Labels,
    /// Confirm deleting the label being edited (local id) from the labels panel.
    DeleteLabelConfirm(String),
    /// Confirm deleting a recurring issue (local id) from its table.
    DeleteRecurringConfirm(String),
    /// Confirm deleting a Saved view (local id) from the sidebar.
    DeleteSavedViewConfirm(String),
    DeleteDashboardConfirm(String),
    DeleteDashboardGroupConfirm(String),
    /// Name prompt for saving the current board as a new Saved view.
    SaveView,
    /// "New dashboard" form; state in `App::dashboard_form`.
    NewDashboard,
    /// Rename/description form; target id in `DashboardForm::editing`.
    EditDashboard,
    /// "Add group" form on the open dashboard; state in `App::group_form`.
    NewGroup,
    /// The same form pre-filled to edit a group (local id).
    EditGroup(String),
    /// Name prompt for renaming a Saved view (local id).
    RenameSavedView(String),
    /// Searchable picker for adding an issue dependency (see `issue_picker`).
    IssuePicker,
    /// Team picker for moving an issue (local UUID); see `team_picker`.
    MoveTeam(String),
    /// "Add project to roadmap" picker; carries the lane a chosen project lands in.
    RoadmapAdd(RoadmapCommitment),
    /// Read-only members panel for a team (local UUID).
    TeamMembers(String),
    /// Preview of a CSV import (see `csv_import`).
    CsvImport,
    /// Display-options popover (group/sort/properties), anchored under the header.
    DisplayOptions,
    /// Filter popover, anchored under the header.
    FilterPanel,
    /// Command palette (Cmd+K); see `command_palette`.
    CommandPalette(PaletteState),
    /// Status/priority/project/labels popover for one issue; see `property_picker`.
    PropertyPicker(property_picker::PickerState),
}

/// Sections of the Rails team settings page (`/teams/:id/edit?section=…`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamWebSection {
    General,
    Lanes,
    Members,
}

impl TeamWebSection {
    fn query_value(self) -> &'static str {
        match self {
            TeamWebSection::General => "general",
            TeamWebSection::Lanes => "lanes",
            TeamWebSection::Members => "members",
        }
    }
}

/// Build the Rails web URL for a team's settings section.
fn team_web_url(base_url: &str, team_remote_id: i64, section: TeamWebSection) -> String {
    format!(
        "{}/teams/{team_remote_id}/edit?section={}",
        base_url.trim().trim_end_matches('/'),
        section.query_value()
    )
}

/// The link kind a picker/add-action creates, relative to the open issue.
/// `Blocking` = the open issue blocks the target; `BlockedBy` = the target
/// blocks the open issue; the rest read the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepDirection {
    Blocking,
    BlockedBy,
    Relates,
    Duplicates,
    DuplicatedBy,
}

impl DepDirection {
    pub const ALL: [DepDirection; 5] = [
        DepDirection::Blocking,
        DepDirection::BlockedBy,
        DepDirection::Relates,
        DepDirection::Duplicates,
        DepDirection::DuplicatedBy,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DepDirection::Blocking => "Blocking",
            DepDirection::BlockedBy => "Blocked by",
            DepDirection::Relates => "Relates to",
            DepDirection::Duplicates => "Duplicates",
            DepDirection::DuplicatedBy => "Duplicated by",
        }
    }

    /// The next (or previous) kind, wrapping around.
    pub fn cycle(self, forward: bool) -> DepDirection {
        let n = Self::ALL.len();
        let start = Self::ALL.iter().position(|d| *d == self).unwrap_or(0);
        let i = if forward { start + 1 } else { start + n - 1 };
        Self::ALL[i % n]
    }

    /// The stored edge for linking `current` to `target` in this kind. The link's
    /// source goes in `blocking_issue_id`. Returns `(source, target, kind)`.
    pub fn stored_edge(self, current: &str, target: &str) -> (String, String, &'static str) {
        let (cur, tgt) = (current.to_string(), target.to_string());
        match self {
            DepDirection::Blocking => (cur, tgt, "blocks"),
            DepDirection::BlockedBy => (tgt, cur, "blocks"),
            DepDirection::Relates => (cur, tgt, "relates"),
            DepDirection::Duplicates => (cur, tgt, "duplicates"),
            DepDirection::DuplicatedBy => (tgt, cur, "duplicates"),
        }
    }
}

/// Which screen the right-hand pane renders. `MyIssues` is the board across
/// all teams, restricted to issues assigned to the current user. While it is
/// the active view, `App::issues`/`lanes`/`members`/... hold cross-team data
/// (see `reload_data`).
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    Board,
    IssueDetail(String),
    ProjectList,
    ProjectDetail(String),
    Roadmap,
    /// A team's Recurring issues table (JRST-159). Team-scoped; its rows live
    /// in `App::recurring_rows`.
    RecurringIssues,
    MyIssues,
    /// Notification Inbox. Not team-scoped, but unlike `MyIssues` it loads no
    /// cross-team issue data (the per-team data stays as is).
    Inbox,
    /// A dashboard (local id). Cross-team like `MyIssues`: the mirror data of
    /// every active team is loaded, and the selected team is left alone.
    Dashboard(String),
    /// One issue's dependency map (JRST-442): the Dependencies or Hierarchy
    /// tab, `depth` levels of blockers each way. Opened from the issue
    /// detail, and parked per Tab like `IssueDetail`.
    DependencyMap {
        issue_id: String,
        tab: DependencyMapTab,
        depth: u8,
    },
    /// A project's dependency map (JRST-445). `active_only` hides completed
    /// and canceled issues (the web's default). Parked per Tab like
    /// `ProjectDetail`.
    ProjectDependencyMap {
        project_id: String,
        active_only: bool,
    },
}

/// The two tabs of `View::DependencyMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyMapTab {
    Dependencies,
    Hierarchy,
}

impl DependencyMapTab {
    pub fn label(self) -> &'static str {
        match self {
            DependencyMapTab::Dependencies => "Dependencies",
            DependencyMapTab::Hierarchy => "Hierarchy",
        }
    }
}

/// Levels of blockers a dependency map shows until the user changes it.
pub const DEFAULT_DEPENDENCY_DEPTH: u8 = 3;
/// The deepest a dependency map can be set (spec: 1-5).
pub const MAX_DEPENDENCY_DEPTH: u8 = 5;

impl View {
    /// The issue this view is about: the open issue detail or its map.
    pub fn issue_id(&self) -> Option<&str> {
        match self {
            View::IssueDetail(id) | View::DependencyMap { issue_id: id, .. } => Some(id),
            _ => None,
        }
    }

    /// The project this view is about: the open project detail or its map.
    pub fn project_id(&self) -> Option<&str> {
        match self {
            View::ProjectDetail(id) | View::ProjectDependencyMap { project_id: id, .. } => {
                Some(id)
            }
            _ => None,
        }
    }

    /// A fresh project dependency map: Active scope.
    pub fn project_dependency_map(project_id: impl Into<String>) -> View {
        View::ProjectDependencyMap { project_id: project_id.into(), active_only: true }
    }

    /// A fresh dependency map for `issue_id`: Dependencies tab, default depth.
    pub fn dependency_map(issue_id: impl Into<String>) -> View {
        View::DependencyMap {
            issue_id: issue_id.into(),
            tab: DependencyMapTab::Dependencies,
            depth: DEFAULT_DEPENDENCY_DEPTH,
        }
    }
}

impl View {
    /// Views whose data is every active team's (see `App::load_cross_team`).
    fn is_cross_team(&self) -> bool {
        matches!(self, View::MyIssues | View::Dashboard(_))
    }

    /// An open dashboard that a pull (or a local delete) removed falls back to
    /// the default board; every other view is unchanged.
    fn without_missing_dashboard(self, dashboards: &[crate::db::dashboards::Dashboard]) -> View {
        match self {
            View::Dashboard(id) if !dashboards.iter().any(|d| d.id == id) => View::Board,
            other => other,
        }
    }
}

/// Sort key for the Projects table (mirrors the Rails index sort options).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectSort {
    Created,
    Name,
    Priority,
    DueDate,
    Velocity,
}

impl ProjectSort {
    pub const ALL: [ProjectSort; 5] = [
        ProjectSort::Created,
        ProjectSort::Name,
        ProjectSort::Priority,
        ProjectSort::DueDate,
        ProjectSort::Velocity,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ProjectSort::Created => "Created",
            ProjectSort::Name => "Name",
            ProjectSort::Priority => "Priority",
            ProjectSort::DueDate => "Due date",
            ProjectSort::Velocity => "Velocity",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    pub fn toggled(self) -> SortDir {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }

    /// The little arrow rendered next to the active sort button.
    pub fn arrow(self) -> &'static str {
        match self {
            SortDir::Asc => "\u{25B2}",  // ▲
            SortDir::Desc => "\u{25BC}", // ▼
        }
    }
}

/// Dashboard name/description modal state; the rename ticket reuses it.
#[derive(Debug, Default, Clone)]
pub struct DashboardForm {
    /// Local id of the dashboard being edited (None when creating).
    pub editing: Option<String>,
    pub name: String,
    pub description: String,
    /// Inline validation error under the name field.
    pub error: Option<&'static str>,
}

/// Dashboard "Add group" modal state: the draft plus an inline name error.
#[derive(Debug, Default, Clone)]
pub struct GroupForm {
    pub draft: crate::db::dashboards::GroupDraft,
    pub error: Option<&'static str>,
}

/// The board card or list row under the pointer, with its window rectangle
/// (the picker popover anchors to it).
#[derive(Debug, Clone)]
struct HoveredCard {
    owner: u64,
    issue_id: String,
    rect: iced::Rectangle,
}

pub struct App {
    teams: Vec<Team>,
    /// Mirrored dashboards for the sidebar (reloaded with the rest after a sync).
    dashboards: Vec<crate::db::dashboards::Dashboard>,
    /// State of the dashboard name/description modal (create now, rename later).
    dashboard_form: DashboardForm,
    /// Which dashboard or group ⋮ menu is open.
    dashboard_menu: Option<crate::views::dashboard::MenuTarget>,
    /// Roadmap card ⋮ menu + in-flight card drag.
    roadmap_ui: roadmap_view::RoadmapUi,
    /// State of the dashboard "Add group" modal.
    group_form: GroupForm,
    /// The open dashboard's groups with their sources (reloaded with the mirror).
    dashboard_groups: Vec<crate::dashboard_eval::GroupConfig>,
    /// Header state of the open dashboard. Per-dashboard memory is a later ticket.
    dashboard_header: crate::dashboard_eval::Header,
    /// Collapsed group ids per dashboard, for the session.
    collapsed_dashboard_groups: crate::views::dashboard::CollapsedGroups,
    /// Header state and focus parked per dashboard (session only).
    dashboard_sessions: crate::dashboard_session::Sessions,
    /// Open Tabs (ADR 0001). The active Tab's state is this struct's live
    /// fields; the others are in `parked_tabs`. None until a team exists.
    tab_set: Option<crate::tabs::TabSet>,
    /// The Tabs last written to the local database, so a change is saved once.
    saved_tabs: Option<(crate::tabs::TabSet, Vec<crate::tabs::Page>)>,
    /// Pages restored at launch for Tabs not focused yet; a Tab takes its
    /// entry the first time it becomes live.
    restored_pages: HashMap<crate::tabs::TabKey, crate::tabs::Page>,
    /// The panels last written to the local database, so a change is saved once.
    saved_panels: Option<Vec<(Option<crate::tabs::TabKey>, crate::db::panel_states::StoredPanel)>>,
    /// Panels stored for Tabs restored at launch, until each is first focused.
    restored_panels: HashMap<crate::tabs::TabKey, crate::db::panel_states::StoredPanel>,
    /// Window width and the tab bar's sideways scroll, for keeping the active
    /// Tab in view. Neither is per Tab.
    window_width: f32,
    window_height: f32,
    tab_bar_offset: f32,
    /// A Tab pressed in the bar, until the pointer is released.
    tab_drag: Option<tab_update::TabDrag>,
    /// Recently closed Tabs, most recent last: each key with the position it
    /// was closed from and the page it was on, for Cmd+Shift+T.
    closed_tabs: Vec<tab_update::ClosedTab>,
    parked_tabs: crate::tab_state::Parked,
    /// Project id to (name, team id) for each open project Tab, for its label.
    tab_projects: HashMap<String, (String, String)>,
    /// Terminals, their panel and its canvas. Outside every Tab.
    terminals: crate::terminal::TerminalState,
    /// Undo histories of the single-line text inputs.
    text_undo: crate::undo_input::Fields<Message>,
    /// Saved views per local team id, in sidebar order (see `db::saved_views`).
    saved_views: HashMap<String, Vec<crate::db::saved_views::SavedView>>,
    /// The open Saved view's working copy; while set it stands in for the
    /// config's display options and filters and the header search.
    open_view: Option<crate::saved_view::OpenView>,
    /// Name being typed in the "Save view" prompt, and its validation error.
    save_view_name: String,
    save_view_error: Option<&'static str>,
    /// The name prompt is saving the open view's working copy (Save as new view).
    save_view_from_open: bool,
    /// The user's Home page as last reported by the server (see `db::home_page`).
    home_page: Option<crate::db::home_page::HomePage>,
    /// Sidebar groups and team order as last reported by the server, loaded
    /// from the local store so launch works offline (see `db::sidebar_layout`).
    /// `None` until a pull has reported one.
    sidebar: Option<crate::db::sidebar_layout::StoredSidebar>,
    selected_team_id: Option<String>,
    /// Current right-pane screen.
    view: View,
    /// Teams whose sidebar sub-rows are expanded. Session-only (not persisted).
    expanded_teams: HashSet<String>,
    /// Team whose sidebar kebab menu is open, if any.
    team_menu_open: Option<String>,
    /// Sidebar toggle: include archived teams. Session-only, defaults off.
    show_archived_teams: bool,
    /// Members for the team shown in `Modal::TeamMembers` (may not be the
    /// selected team, so it's loaded separately from `members`).
    team_members: Vec<Member>,
    lanes: Vec<Lane>,
    issues: Vec<Issue>,
    projects: Vec<Project>,
    members: Vec<Member>,
    labels: Vec<Label>,
    /// Labels attached to each issue, keyed by issue local UUID. Built once per
    /// reload so board cards render chips without a query each.
    issue_labels: HashMap<String, Vec<Label>>,
    /// Comments per local issue id, for the board cards' count.
    comment_counts: HashMap<String, usize>,
    /// Blocking / blocked-by counts per local issue id, for the board cards' icon.
    block_counts: HashMap<String, queries::BlockCounts>,
    /// Dependency edges for the open detail issue, resolved for the sidebar.
    /// Repopulated each reload only while `view` is `IssueDetail`.
    detail_deps: queries::DependencyGroups,
    /// Every dependency row, for the open dependency map (`View::DependencyMap`).
    map_deps: Vec<IssueDependency>,
    /// Hierarchy outline nodes toggled away from their default open state.
    map_flipped: HashSet<String>,
    /// Every stored dependency; feeds the board's dependency lines.
    board_deps: Vec<crate::db::models::IssueDependency>,
    /// Members of the open detail's own team: what its bodies and comments
    /// resolve mentions against. Not `members`, which is the selected team's
    /// (or every team's in My Issues). Empty outside a detail view.
    detail_members: Vec<Member>,
    /// The open issue detail was reached from My Issues, so Back returns there.
    detail_from_my_issues: bool,
    /// The open issue detail was reached from the Inbox (Back returns there).
    detail_from_inbox: bool,
    /// The open issue detail was reached from a dashboard (Back returns there).
    detail_from_dashboard: Option<String>,
    /// The team-level page a team Tab's open issue was opened from (Back).
    detail_from_page: Option<View>,
    /// A dependency map opened from a board card (JRST-452): the view to
    /// return to on Back. `None` for every other entry (issue detail,
    /// palette), which return to the issue detail. Parked per Tab.
    map_return: Option<crate::tab_state::MapReturn>,
    /// Comments on the open detail issue, oldest first. Repopulated each reload
    /// only while `view` is `IssueDetail`.
    detail_comments: Vec<Comment>,
    /// Members who wrote `detail_comments`, from any team (see
    /// `queries::comment_authors`).
    detail_comment_authors: Vec<Member>,
    /// Local change history for the open issue (see `db::activity`).
    detail_activity: Vec<ActivityItem>,
    /// Files attached to the open issue (synced, then pending/failed). Repopulated
    /// each reload only while `view` is `IssueDetail`.
    detail_attachments: Vec<crate::db::attachments::Attachment>,
    /// Pending inline images (placeholder uuid -> outbox file) of the open issue.
    detail_pending_paths: crate::views::body::PendingInlineMap,
    /// The top-level comment composer was the last editor touched; a file drop
    /// then stages onto it (JRST-208). Cleared on view change.
    composer_focused: bool,
    /// Files of an in-progress multi-file drop, flushed by `Message::FlushDrops`.
    drop_buffer: crate::files::drop::DropBuffer,
    /// Fetch state per blob key for images shown in the UI (thumbnails now;
    /// inline images and the lightbox reuse it). Bytes live in the file cache.
    images: crate::files::images::ImageStore,
    /// Files attached to each comment of the open issue/project, keyed by the
    /// comment's local id (comments without files have no entry). Repopulated
    /// each reload while `view` is `IssueDetail` or `ProjectDetail`.
    detail_comment_attachments:
        std::collections::HashMap<String, Vec<crate::db::attachments::Attachment>>,
    /// Issues whose description/comments mention the open detail issue ("Referenced In").
    detail_references: Vec<crate::mentions::Reference>,
    /// Title of the recurring issue that spawned the open issue detail, if any.
    detail_origin: Option<String>,
    /// Search + direction state for the open issue-dependency picker modal.
    issue_picker: Option<issue_picker::IssuePickerState>,
    focused_lane: usize,
    focused_issue: usize,
    /// Board card or list row under the pointer.
    hovered_card: Option<HoveredCard>,
    /// List-mode sections (by title) whose rows are hidden, plus board
    /// sub-group rows flipped from their default (see `swimlane_key`).
    collapsed_sections: HashSet<String>,
    /// Last reported (offset, viewport size) per scrollable. Drives
    /// scroll-to-focus.
    scroll_state: HashMap<ScrollKey, (f32, f32)>,
    modal: Modal,
    issue_form: Option<issue_form::IssueFormState>,
    /// The one open date field calendar, if any.
    date_picker: Option<crate::views::date_picker::DatePickerState>,
    /// Buffered edit state while the right pane shows `View::IssueDetail`.
    issue_detail: Option<issue_detail::IssueDetailState>,
    /// Buffered edit state while the right pane shows `View::ProjectDetail`.
    /// Lanes and labels of the team a new issue from My Issues goes to.
    new_issue_scope: Option<(Vec<Lane>, Vec<Label>)>,
    /// The open project form came from the issue form's "New Project": saving
    /// or cancelling it returns to the issue form.
    project_form_from_issue: bool,
    project_detail: Option<project_detail::ProjectDetailState>,
    /// Cards and edges of the open `View::ProjectDependencyMap`, rebuilt by
    /// `refresh_project_map` on open, scope change and data reload.
    project_map: Option<crate::views::project_dependency_map::ProjectMapData>,
    /// Projects table sort key + direction (session-only).
    project_sort: ProjectSort,
    project_sort_dir: SortDir,
    /// Project page Issues list sort (session-only; Rails defaults to ID).
    project_issue_sort: display::ProjectIssueSort,
    /// Project page Issues list filter (session-only; Rails defaults to Active).
    project_issue_filter: display::ProjectIssueFilter,
    /// "Hide completed" filter for the Projects table; defaults on, matching Rails.
    hide_completed_projects: bool,
    /// Header search text and the view it was typed on. The filter only applies
    /// while `view` still equals `search_view`, so navigating away drops it.
    search_query: String,
    search_view: View,
    project_form: Option<projects_view::ProjectFormState>,
    settings_form: Option<settings::SettingsFormState>,
    label_form: Option<labels_view::LabelFormState>,
    csv_import: Option<csv_import::CsvImportState>,
    show_help: bool,
    /// The inline Properties card (issue and project pages) shows its "More"
    /// fields. Session-only.
    more_properties: bool,
    pending_sync_count: i32,
    config: Config,
    config_error: Option<String>,
    last_sync_error: Option<String>,
    /// Identifiers of teams flagged inaccessible (team-access 404 on push).
    inaccessible_teams: Vec<String>,
    /// Token has no write scope: pushes are paused, edits stay local.
    sync_read_only: bool,
    sync_in_progress: bool,
    /// Unread Notification count (JRST-90 badge). Loaded at startup from the
    /// mirror, refreshed by every completed poll.
    notif_unread_count: i64,
    /// Inbox rows, selection and filter (JRST-91). Reloaded on entering the
    /// Inbox and after every poll.
    inbox: crate::inbox::InboxState,
    /// Display rows of the selected team's Recurring issues (JRST-159),
    /// reloaded by `reload_data` and on entering the view.
    recurring_rows: Vec<crate::db::recurring_issues::RecurringIssueRow>,
    /// Open "New recurring issue" form (JRST-172).
    recurring_form: Option<crate::views::recurring_issue_form::RecurringFormState>,
    /// Report of the most recent successful poll (JRST-95 prune, JRST-96
    /// banners read `new_unread` + `backfill`).
    last_notif_poll: Option<PollReport>,
    scheduler: Scheduler,
    /// Delay until the scheduler's next timed check; `None` = no timer armed.
    sync_next_delay: Option<std::time::Duration>,
    /// Active toasts, newest last. Rendered as a bottom-right stack over the UI.
    toasts: Vec<Toast>,
    /// Hover card over body references (JRST-299).
    body_hover: crate::body_hover::BodyHover,
    /// The open hover card's content, read from the local database when the
    /// card opened (never on the view path). `None` content = no card.
    hover_content: Option<(crate::body_hover::Anchor, Option<crate::body_hover::CardContent>)>,
    /// Monotonic id source for toasts (so dismiss/retry target the right one).
    next_toast_id: u64,
    /// Set when sync hit a 401; pauses the sync timer until the user saves a
    /// fresh token in Settings, so we don't hammer the API with a dead token.
    sync_paused_auth: bool,
    /// Self-update state (project 320). Restored from `config.updater` at
    /// launch; saved back after every update (see `apply_updater_event`).
    updater: crate::updater::machine::UpdaterState,
    /// Where the app runs from; `Disabled` in dev builds (no `.app`).
    install: crate::updater::install::InstallContext,
    /// The updater's I/O. Tasks hold a clone of the Arc.
    update_edge: std::sync::Arc<dyn crate::updater::edge::UpdateEdge>,
    /// Restart now has swapped and is closing Terminals before it exits. A
    /// quit request in this window must not swap again.
    restart_exiting: bool,
}

/// A comment needs text or staged files (Rails: `body_or_files_present`).
fn comment_has_content(body: &str, has_staged_files: bool) -> bool {
    !body.is_empty() || has_staged_files
}

impl App {
    /// Snapshot of what the command palette searches, taken when it opens.
    fn palette_data(&self) -> PaletteData {
        let conn = self.db();
        PaletteData {
            issues: queries::searchable_issues(&conn).unwrap_or_default(),
            current_team_id: self.selected_team_id.clone(),
            teams: self
                .teams
                .iter()
                .filter(|t| t.archived_at.is_none())
                .cloned()
                .collect(),
            projects: queries::searchable_projects(&conn).unwrap_or_default(),
            saved_views: self.palette_saved_views(),
            dashboards: self
                .dashboards
                .iter()
                .map(|d| crate::palette_results::SearchableDashboard {
                    id: d.id.clone(),
                    name: d.name.clone(),
                })
                .collect(),
            on_issue_detail: matches!(self.view, View::IssueDetail(_)),
            on_project_detail: matches!(self.view, View::ProjectDetail(_)),
            terminal_focus: self.terminals.palette_focus(self.panel_scope().as_ref()),
            terminal_panel_showing: self.terminals.panel_showing(self.panel_scope().as_ref()),
            project_cards_stacked: self.config.project_cards_stacked,
            terminal_dock_right: self.terminals.dock() == crate::terminal::panel::Dock::Right,
            update_ready: self.updater.view().ready.is_some(),
            tabs: self
                .tab_labels()
                .into_iter()
                .map(|(label, active)| crate::palette_results::SearchableTab { label, active })
                .collect(),
        }
    }

    /// Saved views of unarchived teams, for the palette.
    fn palette_saved_views(&self) -> Vec<crate::palette_results::SearchableSavedView> {
        self.teams
            .iter()
            .filter(|t| t.archived_at.is_none())
            .flat_map(|t| {
                self.saved_views
                    .get(&t.id)
                    .into_iter()
                    .flatten()
                    .map(|v| crate::palette_results::SearchableSavedView {
                        id: v.id.clone(),
                        team_id: v.team_id.clone(),
                        team_name: t.name.clone(),
                        name: v.name.clone(),
                    })
            })
            .collect()
    }

    fn db(&self) -> rusqlite::Connection {
        connection::open().expect("Failed to open database")
    }

    /// Queue a toast for display. Error messages are de-duped so a repeatedly
    /// failing sync tick doesn't stack identical cards.
    fn push_toast(&mut self, message: impl Into<String>, kind: ToastKind, retry: bool) {
        let message = message.into();
        if kind == ToastKind::Error && self.toasts.iter().any(|t| t.message == message) {
            return;
        }
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        self.toasts.push(Toast {
            id,
            message,
            kind,
            retry,
            created: std::time::Instant::now(),
            actions: Vec::new(),
            on_dismiss: None,
            sticky: false,
        });
    }

    /// Queue a toast with buttons. It stays until the user acts on it.
    fn push_action_toast(
        &mut self,
        message: impl Into<String>,
        actions: Vec<ToastAction>,
        on_dismiss: Option<Message>,
    ) {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        self.toasts.push(Toast {
            id,
            message: message.into(),
            kind: ToastKind::Info,
            retry: false,
            created: std::time::Instant::now(),
            actions,
            on_dismiss,
            sticky: true,
        });
    }

    /// Stop showing anything that belonged to teams that were just removed.
    /// `reload_data` then picks a new team if the selected one went.
    fn leave_deleted_teams(&mut self, deleted: &[String]) {
        if deleted.is_empty() {
            return;
        }
        let selected_gone = self
            .selected_team_id
            .as_ref()
            .is_some_and(|id| deleted.contains(id));
        if selected_gone {
            self.selected_team_id = None;
            self.open_view = None;
            self.modal = Modal::None;
            self.focused_lane = 0;
            self.focused_issue = 0;
            if !self.view.is_cross_team() && self.view != View::Inbox {
                self.view = View::Board;
            }
        }
        // A detail view can be open on another team's row, e.g. from My Issues.
        let conn = self.db();
        let detail_gone = match (self.view.issue_id(), self.view.project_id()) {
            (Some(id), _) => matches!(queries::get_issue(&conn, id), Ok(None)),
            (_, Some(id)) => matches!(queries::get_project(&conn, id), Ok(None)),
            _ => false,
        };
        if detail_gone {
            self.view = View::Board;
        }
    }

    fn reload_data(&mut self) {
        let conn = self.db();
        self.teams = queries::all_teams(&conn).unwrap_or_default();
        self.saved_views = crate::db::saved_views::grouped_by_team(&conn).unwrap_or_default();
        self.dashboards = crate::db::dashboards::list(&conn).unwrap_or_default();
        self.home_page = crate::db::home_page::get(&conn).unwrap_or_default();
        self.sidebar = crate::db::sidebar_layout::load(&conn).unwrap_or_default();

        let kept = self.view.clone().without_missing_dashboard(&self.dashboards);
        if kept != self.view {
            self.view = kept;
            self.focused_lane = 0;
            self.focused_issue = 0;
        }

        if self.selected_team_id.is_none() {
            self.selected_team_id = default_team_id(&self.teams);
        }
        self.ensure_tab_set();
        self.reload_tab_projects(&conn);

        if self.view.is_cross_team() {
            self.load_cross_team(&conn);
        } else if let Some(ref team_id) = self.selected_team_id {
            self.lanes = queries::lanes_for_team(&conn, team_id).unwrap_or_default();
            self.issues = queries::issues_for_team(&conn, team_id).unwrap_or_default();
            self.projects = queries::projects_for_team(&conn, team_id).unwrap_or_default();
            self.members = queries::members_for_team(&conn, team_id).unwrap_or_default();
            self.labels = queries::labels_for_team(&conn, team_id).unwrap_or_default();
            self.issue_labels = build_issue_labels_map(&conn, team_id);
            self.comment_counts =
                queries::comment_counts_for_team(&conn, team_id).unwrap_or_default();
            self.block_counts = queries::block_counts_for_team(&conn, team_id).unwrap_or_default();
        } else {
            self.projects.clear();
            self.members.clear();
            self.labels.clear();
            self.issue_labels.clear();
            self.comment_counts.clear();
            self.block_counts.clear();
        }

        self.reload_dashboard_config(&conn);
        self.reload_recurring(&conn);
        self.reload_detail_origin(&conn);
        self.refresh_detail_buffers(&conn);
        self.refresh_open_view();
        self.refresh_project_map();

        // Filters hold per-team ids; drop the ones this data doesn't contain
        // from the scope's own set (My Issues loads every team, so its ids
        // all stay). An issue opened from a cross-team Tab selects the
        // issue's team, whose set is pruned against that team's data.
        if self.selected_team_id.is_some() && self.prune_board_filters() {
            if let Err(e) = config::save(&self.config) {
                self.config_error = Some(e);
            }
        }

        self.load_detail_dependencies(&conn);

        self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
        self.clamp_focus();
    }

    /// Rebuild the project dependency map's cards from the loaded mirror
    /// data. No-op (and drops the cache) on any other view.
    fn refresh_project_map(&mut self) {
        let View::ProjectDependencyMap { project_id, active_only } = &self.view else {
            self.project_map = None;
            return;
        };
        let Some(project) = self.projects.iter().find(|p| p.id == *project_id) else {
            self.project_map = None;
            return;
        };
        let deps = queries::all_issue_dependencies(&self.db()).unwrap_or_default();
        let identifier = self
            .teams
            .iter()
            .find(|t| t.id == project.team_id)
            .map_or("", |t| t.identifier.as_str());
        self.project_map = Some(crate::views::project_dependency_map::build(
            crate::views::project_dependency_map::MapInput {
                project,
                deps: &deps,
                issues: &self.issues,
                lanes: &self.lanes,
                projects: &self.projects,
                members: &self.members,
                issue_labels: &self.issue_labels,
                active_only: *active_only,
                team_identifier: identifier,
            },
        ));
    }

    /// Load every active team's lanes/issues/members/projects/labels into the
    /// shared fields, for the cross-team My Issues view.
    fn load_cross_team(&mut self, conn: &rusqlite::Connection) {
        self.lanes.clear();
        self.issues.clear();
        self.projects.clear();
        self.members.clear();
        self.labels.clear();
        self.issue_labels.clear();
        self.comment_counts.clear();
        self.block_counts.clear();
        for team in self.teams.iter().filter(|t| t.archived_at.is_none()) {
            let id = &team.id;
            self.lanes
                .extend(queries::lanes_for_team(conn, id).unwrap_or_default());
            self.issues
                .extend(queries::issues_for_team(conn, id).unwrap_or_default());
            self.projects
                .extend(queries::projects_for_team(conn, id).unwrap_or_default());
            self.members
                .extend(queries::members_for_team(conn, id).unwrap_or_default());
            self.labels
                .extend(queries::labels_for_team(conn, id).unwrap_or_default());
            self.issue_labels.extend(build_issue_labels_map(conn, id));
            self.comment_counts
                .extend(queries::comment_counts_for_team(conn, id).unwrap_or_default());
            self.block_counts
                .extend(queries::block_counts_for_team(conn, id).unwrap_or_default());
        }
    }

    /// Re-read the open dashboard's groups and sources from the mirror.
    fn reload_dashboard_config(&mut self, conn: &rusqlite::Connection) {
        use crate::db::dashboards;
        self.dashboard_groups = match &self.view {
            View::Dashboard(id) => dashboards::groups_for(conn, id)
                .unwrap_or_default()
                .into_iter()
                .map(|group| crate::dashboard_eval::GroupConfig {
                    sources: dashboards::sources_for(conn, &group.id).unwrap_or_default(),
                    group,
                })
                .collect(),
            _ => Vec::new(),
        };
    }

    /// The open dashboard evaluated over the loaded cross-team data.
    fn dashboard_evaluation(&self) -> crate::dashboard_eval::Evaluation<'_> {
        crate::dashboard_eval::evaluate(&crate::dashboard_eval::Input {
            groups: &self.dashboard_groups,
            mirror: crate::dashboard_eval::Mirror {
                teams: &self.teams,
                projects: &self.projects,
                labels: &self.labels,
                lanes: &self.lanes,
                members: &self.members,
                issues: &self.issues,
                issue_labels: &self.issue_labels,
            },
            current_user_remote_id: self.config.current_user_remote_id,
            today: chrono::Local::now().date_naive(),
            header: crate::dashboard_eval::Header {
                search: self.active_search().to_string(),
                ..self.dashboard_header.clone()
            },
        })
    }

    /// Switch to `view`, reloading data when crossing in or out of the
    /// cross-team views (their data replaces the per-team data).
    /// Remove a dashboard locally and queue its server delete if it was pushed.
    fn delete_dashboard(&mut self, id: &str) {
        match crate::db::dashboards::delete_local(&self.db(), id) {
            Ok(Some(payload)) => {
                self.enqueue_sync(
                    crate::db::dashboards::ENTITY_TYPE,
                    id,
                    "delete",
                    Some(payload.as_str()),
                );
            }
            Ok(None) => {}
            Err(e) => eprintln!("[dashboards] delete failed: {e}"),
        }
        self.reload_data();
    }

    /// Remove a dashboard group locally and queue its server delete if it was pushed.
    fn delete_dashboard_group(&mut self, id: &str) {
        match crate::db::dashboards::delete_group_local(&self.db(), id) {
            Ok(Some(payload)) => {
                self.enqueue_sync(
                    crate::db::dashboards::GROUP_ENTITY_TYPE,
                    id,
                    "delete",
                    Some(payload.as_str()),
                );
            }
            Ok(None) => {}
            Err(e) => eprintln!("[dashboards] delete group failed: {e}"),
        }
        self.focused_lane = 0;
        self.focused_issue = 0;
        self.reload_data();
    }

    fn close_form_page(&mut self) {
        self.modal = Modal::None;
        self.issue_form = None;
        self.project_form = None;
        self.recurring_form = None;
    }

    /// Put a project in a roadmap lane (or take it off with `None`), queue
    /// the change for sync and refresh. A no-op when nothing changes.
    fn set_roadmap_commitment(&mut self, project_id: &str, commitment: Option<RoadmapCommitment>) {
        let conn = self.db();
        let Ok(Some(mut project)) = queries::get_project(&conn, project_id) else {
            return;
        };
        if project.roadmap_commitment == commitment {
            return;
        }
        project.roadmap_commitment = commitment;
        project.updated_at = crate::db::now_iso();
        let _ = queries::update_project(&conn, &project);
        let payload = serde_json::to_string(&project).ok();
        self.enqueue_sync("project", &project.id, "update", payload.as_deref());
        self.reload_data();
    }

    fn switch_view(&mut self, view: View) {
        let crossing = self.view.is_cross_team() != view.is_cross_team()
            || matches!(view, View::Dashboard(_));
        self.view = view;
        self.dashboard_menu = None;
        self.roadmap_ui = roadmap_view::RoadmapUi::default();
        self.composer_focused = false;
        self.retain_open_view();
        if self.view == View::Inbox {
            self.reload_inbox();
        }
        if self.view == View::RecurringIssues {
            let conn = self.db();
            self.reload_recurring(&conn);
        }
        if crossing {
            self.focused_lane = 0;
            self.focused_issue = 0;
            self.reload_data();
        }
        self.restore_dashboard_session();
    }

    /// Park the open dashboard's header (with its search text) and focus.
    fn stash_dashboard_session(&mut self) {
        let View::Dashboard(id) = &self.view else {
            return;
        };
        let session = crate::dashboard_session::Session {
            header: crate::dashboard_eval::Header {
                search: self.active_search().to_string(),
                ..self.dashboard_header.clone()
            },
            focused_lane: self.focused_lane,
            focused_issue: self.focused_issue,
        };
        let id = id.clone();
        self.dashboard_sessions.save(&id, session);
    }

    /// Entering a dashboard: load its parked header, search text and focus.
    fn restore_dashboard_session(&mut self) {
        let View::Dashboard(id) = &self.view else {
            return;
        };
        let session = self.dashboard_sessions.restore(id);
        self.search_query = session.header.search.clone();
        self.search_view = self.view.clone();
        self.dashboard_header = session.header;
        self.focused_lane = session.focused_lane;
        self.focused_issue = session.focused_issue;
        // The parked focus may point past rows or groups that went away.
        self.clamp_focus();
    }

    /// Re-read the selected team's Recurring issue rows from the mirror.
    /// Refresh the open issue detail's "From recurring issue" title.
    /// Bring the open detail's text buffers up to date with the DB (a pull may
    /// have changed them) unless they hold unsaved edits.
    fn refresh_detail_buffers(&mut self, conn: &rusqlite::Connection) {
        if let Some(d) = self.issue_detail.as_mut() {
            if let Ok(Some(issue)) = queries::get_issue(conn, &d.issue_id) {
                d.refresh_from(&issue);
            }
        }
        if let Some(d) = self.project_detail.as_mut() {
            if let Ok(Some(project)) = queries::get_project(conn, &d.project_id) {
                d.refresh_from(&project);
            }
        }
    }

    fn reload_detail_origin(&mut self, conn: &rusqlite::Connection) {
        self.detail_origin = match &self.view {
            View::IssueDetail(id) => crate::db::recurring_issues::origin_title(conn, id),
            _ => None,
        };
    }

    fn reload_recurring(&mut self, conn: &rusqlite::Connection) {
        self.recurring_rows = match &self.selected_team_id {
            Some(team_id) if !self.view.is_cross_team() => {
                crate::db::recurring_issues::list_rows(conn, team_id).unwrap_or_default()
            }
            _ => Vec::new(),
        };
    }

    /// Re-read the Inbox rows from the mirror, keeping the selection.
    fn reload_inbox(&mut self) {
        let conn = self.db();
        let items = crate::db::notifications::inbox_list(&conn, self.inbox.filter).unwrap_or_default();
        self.inbox.set_items(items);
    }

    /// Resolve dependency edges for the open detail issue so the sidebar can
    /// render them; other views don't need this, so keep it cheap.
    fn load_detail_dependencies(&mut self, conn: &rusqlite::Connection) {
        self.board_deps = queries::all_issue_dependencies(conn).unwrap_or_default();
        self.detail_members = self
            .detail_team_id()
            .map(|team_id| queries::members_for_team(conn, team_id).unwrap_or_default())
            .unwrap_or_default();
        self.map_deps = if matches!(self.view, View::DependencyMap { .. }) {
            queries::all_issue_dependencies(conn).unwrap_or_default()
        } else {
            Vec::new()
        };
        if let View::IssueDetail(ref issue_id) = self.view {
            self.detail_deps =
                queries::dependency_groups_for_issue(conn, issue_id).unwrap_or_default();
            self.detail_comments = queries::comments_for_issue(conn, issue_id).unwrap_or_default();
            self.detail_activity =
                activity_for_issue(conn, issue_id).unwrap_or_default();
            self.detail_references = self.compute_references(conn, issue_id);
            self.detail_attachments = crate::db::attachments::list_for_owner(
                conn,
                crate::db::attachments::OwnerKind::Issue,
                issue_id,
            )
            .unwrap_or_default();
            self.detail_pending_paths =
                crate::views::issue_detail::pending_inline_paths(&self.detail_attachments);
            self.load_comment_attachments(conn);
        } else if let View::ProjectDetail(ref project_id) = self.view {
            self.detail_attachments = crate::db::attachments::list_for_owner(
                conn,
                crate::db::attachments::OwnerKind::Project,
                project_id,
            )
            .unwrap_or_default();
            self.detail_pending_paths =
                crate::views::issue_detail::pending_inline_paths(&self.detail_attachments);
            self.detail_references.clear();
            self.detail_deps = Default::default();
            self.detail_comments =
                queries::comments_for_project(conn, project_id).unwrap_or_default();
            self.detail_activity.clear();
            self.load_comment_attachments(conn);
        } else {
            self.detail_comment_attachments.clear();
            self.detail_attachments.clear();
            self.detail_references.clear();
            self.detail_deps = Default::default();
            self.detail_comments.clear();
            self.detail_activity.clear();
        }
        self.detail_comment_authors =
            queries::comment_authors(conn, &self.detail_comments).unwrap_or_default();
    }

    /// JRST-214: a failed inline image was removed; drop its placeholder from the
    /// owner's body and the open editor. Waiting body writers read the body from
    /// the DB, so they proceed on their own; an update is only enqueued when
    /// nothing is queued for the owner.
    fn strip_removed_inline(&mut self, conn: &rusqlite::Connection, row: &crate::db::attachments::Attachment) {
        use crate::db::attachments::OwnerKind;
        let Some(uuid) = row.placeholder_uuid.as_deref() else { return };
        let kind = row.owner_kind;
        let Ok(Some(body)) = crate::db::attachments::strip_inline_placeholder(conn, kind, &row.owner_id, uuid) else {
            return;
        };
        let entity = match kind {
            OwnerKind::Issue => {
                if let Some(d) = self.issue_detail.as_mut().filter(|d| d.issue_id == row.owner_id) {
                    d.description = crate::undo_editor::UndoableContent::with_text(&body);
                }
                "issue"
            }
            OwnerKind::Project => {
                if let Some(d) = self.project_detail.as_mut().filter(|d| d.project_id == row.owner_id) {
                    d.description = crate::undo_editor::UndoableContent::with_text(&body);
                }
                "project"
            }
            OwnerKind::Comment => return,
        };
        if !sync_queue::has_pending_entry(conn, entity, &row.owner_id).unwrap_or(true) {
            let _ = sync_queue::enqueue(conn, entity, &row.owner_id, "update", None);
        }
        self.reload_data();
    }

    /// Refill `detail_comment_attachments` for the comments in `detail_comments`.
    fn load_comment_attachments(&mut self, conn: &rusqlite::Connection) {
        let ids: Vec<&str> = self.detail_comments.iter().map(|c| c.id.as_str()).collect();
        self.detail_comment_attachments =
            crate::db::attachments::list_for_comments(conn, &ids).unwrap_or_default();
        // Pasted-but-unsent images in comments render from the outbox too.
        for files in self.detail_comment_attachments.values() {
            self.detail_pending_paths
                .extend(crate::views::issue_detail::pending_inline_paths(files));
        }
    }

    /// Local "Referenced In": scan the open issue's team (descriptions in
    /// `self.issues`, comments from SQLite) for tokens naming it. Rails exposes
    /// no IssueReference list endpoint, so this is derived from synced text.
    fn compute_references(
        &self,
        conn: &rusqlite::Connection,
        issue_id: &str,
    ) -> Vec<crate::mentions::Reference> {
        let Ok(Some(target)) = queries::get_issue(conn, issue_id) else {
            return Vec::new();
        };
        let Some(team) = self.teams.iter().find(|t| t.id == target.team_id) else {
            return Vec::new();
        };
        let comments = queries::comments_for_team(conn, &target.team_id).unwrap_or_default();
        crate::mentions::referenced_in(&target, &team.identifier, &self.issues, &comments)
    }

    /// Recompute the `@`/`#` suggestion list for `target` from its cursor.
    fn refresh_suggestions(&mut self, target: crate::mentions::EditorTarget) {
        use crate::mentions::{self as m, EditorTarget};
        self.composer_focused = matches!(target, EditorTarget::Comment);
        // On project detail only the comment composers exist; the id excluded
        // from `#` suggestions is the project's, which matches no issue.
        let (content, exclude_id) = match (&self.view, &self.issue_detail, &self.project_detail) {
            (View::ProjectDetail(_), _, Some(p)) => match target {
                EditorTarget::Comment => (&p.comment_composer, p.project_id.as_str()),
                EditorTarget::Reply => (&p.reply_composer, p.project_id.as_str()),
                EditorTarget::Description => return,
            },
            (_, Some(d), _) => match target {
                EditorTarget::Description => (&d.description, d.issue_id.as_str()),
                EditorTarget::Comment => (&d.comment_composer, d.issue_id.as_str()),
                EditorTarget::Reply => (&d.reply_composer, d.issue_id.as_str()),
            },
            _ => return,
        };
        // Column is a byte offset into the line (iced 0.13 / cosmic-text).
        let (line, col) = content.cursor_position();
        let state = match (content.line(line), self.selected_team()) {
            (Some(l), Some(team)) => m::build_suggest(
                target,
                &l,
                col,
                &self.members,
                &self.issues,
                &team.identifier,
                exclude_id,
            ),
            _ => None,
        };
        if let Some(slot) = self.open_suggest_mut() {
            *slot = state;
        }
    }

    /// Same for the issue form's description (new/edit modal).
    fn refresh_form_suggestions(&mut self) {
        use crate::mentions::{self as m, EditorTarget};
        let Some(form) = &self.issue_form else { return };
        let (line, col) = form.description.cursor_position();
        let team = self.selected_team();
        let state = match (form.description.line(line), team) {
            (Some(l), Some(team)) => m::build_suggest(
                EditorTarget::Description,
                &l,
                col,
                &self.members,
                &self.issues,
                &team.identifier,
                form.editing_issue_id.as_deref().unwrap_or(""),
            ),
            _ => None,
        };
        if let Some(form) = &mut self.issue_form {
            form.suggest = state;
        }
    }

    /// Replace the partial token in `content` with suggestion `idx` (or the
    /// highlighted one).
    fn insert_suggestion(
        content: &mut crate::undo_editor::UndoableContent,
        s: &crate::mentions::SuggestState,
        idx: Option<usize>,
    ) {
        use iced::widget::text_editor::{Action, Edit};
        if let Some(item) = s.items.get(idx.unwrap_or(s.selected)) {
            // One undo step: undoing a suggestion brings back the partial token.
            let backspaces = (0..s.delete_chars).map(|_| Action::Edit(Edit::Backspace));
            let paste = Action::Edit(Edit::Paste(std::sync::Arc::new(item.insert.clone())));
            content.perform_as_one(backspaces.chain(std::iter::once(paste)));
        }
    }

    /// The open suggestion list: the form's while its modal is up, else the
    /// detail view's.
    fn open_suggest_mut(&mut self) -> Option<&mut Option<crate::mentions::SuggestState>> {
        if self.modal == Modal::IssueForm {
            return self.issue_form.as_mut().map(|f| &mut f.suggest);
        }
        if matches!(self.view, View::ProjectDetail(_)) {
            return self.project_detail.as_mut().map(|d| &mut d.suggest);
        }
        self.issue_detail.as_mut().map(|d| &mut d.suggest)
    }

    /// Comment drafts do not survive leaving their detail view (the state is
    /// rebuilt on open), so cancel the unsent staged files / pasted images
    /// they own: their uploads never send. Submitted comments' rows are kept.
    fn drop_drafts(&mut self, ids: &[String]) {
        let conn = self.db();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        if matches!(crate::db::attachments::discard_unsent_for_drafts(&conn, &refs), Ok(n) if n > 0) {
            self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
        }
    }

    /// Drop the drafts of a detail state that is no longer the open view.
    fn discard_dead_composers(&mut self) {
        let mut ids = Vec::new();
        if let Some(d) = &self.issue_detail {
            if !matches!(&self.view, View::IssueDetail(id) if *id == d.issue_id) {
                ids.extend([d.composer_files.comment_id.clone(), d.composer_files.reply_id.clone()]);
                self.issue_detail = None;
            }
        }
        if let Some(d) = &self.project_detail {
            if !matches!(&self.view, View::ProjectDetail(id) if *id == d.project_id) {
                ids.extend([d.composer_files.comment_id.clone(), d.composer_files.reply_id.clone()]);
                self.project_detail = None;
            }
        }
        if !ids.is_empty() {
            self.drop_drafts(&ids);
        }
    }

    /// The comment composers of whichever detail view is open (issue or project).
    fn comment_drafts_mut(&mut self) -> Option<CommentDrafts<'_>> {
        if matches!(self.view, View::ProjectDetail(_)) {
            return self.project_detail.as_mut().map(|d| CommentDrafts {
                comment: &mut d.comment_composer,
                reply_to: &mut d.reply_to,
                reply: &mut d.reply_composer,
                suggest: &mut d.suggest,
                files: &mut d.composer_files,
            });
        }
        self.issue_detail.as_mut().map(|d| CommentDrafts {
            comment: &mut d.comment_composer,
            reply_to: &mut d.reply_to,
            reply: &mut d.reply_composer,
            suggest: &mut d.suggest,
            files: &mut d.composer_files,
        })
    }

    fn pick_suggestion(&mut self, idx: Option<usize>) {
        use crate::mentions::EditorTarget;
        if self.modal == Modal::IssueForm {
            if let Some(form) = &mut self.issue_form {
                if let Some(s) = form.suggest.take() {
                    Self::insert_suggestion(&mut form.description, &s, idx);
                }
            }
        } else if matches!(self.view, View::ProjectDetail(_)) {
            if let Some(d) = self.comment_drafts_mut() {
                if let Some(s) = d.suggest.take() {
                    let content = match s.target {
                        EditorTarget::Reply => d.reply,
                        _ => d.comment,
                    };
                    Self::insert_suggestion(content, &s, idx);
                }
            }
        } else if let Some(d) = &mut self.issue_detail {
            if let Some(s) = d.suggest.take() {
                let content = match s.target {
                    EditorTarget::Description => &mut d.description,
                    EditorTarget::Comment => &mut d.comment_composer,
                    EditorTarget::Reply => &mut d.reply_composer,
                };
                Self::insert_suggestion(content, &s, idx);
            }
        }
    }

    /// The team the open detail's issue or project belongs to. Its bodies are
    /// resolved against this team, whichever team is selected.
    fn detail_team_id(&self) -> Option<&str> {
        match (self.view.issue_id(), self.view.project_id()) {
            (Some(id), _) => self.issues.iter().find(|i| i.id == id).map(|i| i.team_id.as_str()),
            (_, Some(id)) => self.projects.iter().find(|p| p.id == id).map(|p| p.team_id.as_str()),
            _ => None,
        }
    }

    fn detail_team(&self) -> Option<&Team> {
        let id = self.detail_team_id()?;
        self.teams.iter().find(|t| t.id == id)
    }

    fn selected_team(&self) -> Option<&Team> {
        self.selected_team_id
            .as_ref()
            .and_then(|id| self.teams.iter().find(|t| t.id == *id))
    }

    // --- Active board state -------------------------------------------
    // The one accessor for the board's display options, filters and view
    // search. Everything outside this block goes through it; today it
    // resolves to the local state (the config plus the header search).

    /// Why "Save view…" is unavailable right now, if it is.
    fn save_view_block_reason(&self) -> Option<&'static str> {
        let on_team_board = self.view == View::Board && self.selected_team_id.is_some();
        crate::saved_view::save_block_reason(on_team_board, self.sync_read_only)
    }

    fn board_display(&self) -> &DisplayOptions {
        match &self.open_view {
            Some(v) => &v.state.display,
            None => self.config.display_for(&self.board_scope()),
        }
    }

    fn board_display_mut(&mut self) -> &mut DisplayOptions {
        let scope = self.board_scope();
        match &mut self.open_view {
            Some(v) => &mut v.state.display,
            None => self.config.display_for_mut(&scope),
        }
    }

    /// Whose display options and filters the active board uses: My Issues' own,
    /// the selected team's, or the global set on other cross-team pages.
    fn board_scope(&self) -> config::BoardScope {
        if self.view == View::MyIssues {
            return config::BoardScope::MyIssues;
        }
        match &self.selected_team_id {
            Some(id) if !self.view.is_cross_team() => config::BoardScope::Team(id.clone()),
            _ => config::BoardScope::Global,
        }
    }

    fn board_filters(&self) -> &Filters {
        match &self.open_view {
            Some(v) => &v.state.filters,
            None => self.config.filters_for(&self.board_scope()),
        }
    }

    fn board_filters_mut(&mut self) -> &mut Filters {
        let scope = self.board_scope();
        match &mut self.open_view {
            Some(v) => &mut v.state.filters,
            None => self.config.filters_for_mut(&scope),
        }
    }

    /// Drop the open view's working copy unless it is still on screen: its
    /// team's board (or an issue opened from it). Leaving drops unsaved edits.
    fn retain_open_view(&mut self) {
        let on_board = matches!(self.view, View::Board | View::IssueDetail(_));
        if let Some(v) = &self.open_view {
            if !v.stays_open(self.selected_team_id.as_deref(), on_board) {
                self.open_view = None;
            }
        }
    }

    /// Reconcile the open Saved view with the freshly loaded mirror: follow a
    /// pulled change (see `OpenView::refresh`), or, if the view is gone (a
    /// server tombstone), land on the team's board.
    fn refresh_open_view(&mut self) {
        let Some(v) = &mut self.open_view else {
            return;
        };
        let latest = self
            .saved_views
            .values()
            .flatten()
            .find(|sv| sv.id == v.view_id);
        match latest {
            Some(sv) => {
                let mirror = crate::saved_view::Mirror {
                    lanes: &self.lanes,
                    members: &self.members,
                    labels: &self.labels,
                    projects: &self.projects,
                };
                let scope = match &self.selected_team_id {
                    Some(id) => config::BoardScope::Team(id.clone()),
                    None => config::BoardScope::Global,
                };
                let local = crate::saved_view::BoardState {
                    display: self.config.display_for(&scope).clone(),
                    filters: self.config.filters_for(&scope).clone(),
                    search: String::new(),
                };
                v.refresh(sv, &mirror, &local);
            }
            None => {
                self.open_view = None;
                if matches!(self.view, View::IssueDetail(_)) {
                    self.commit_issue_detail();
                    self.detail_from_my_issues = false;
                    self.detail_from_inbox = false;
                    self.detail_from_dashboard = None;
                }
                self.view = View::Board;
                self.focused_lane = 0;
                self.focused_issue = 0;
            }
        }
    }

    /// Drop filter ids the loaded data doesn't contain; true if any changed.
    /// On an open view this prunes the working copy, which no config save
    /// follows (the stored query is untouched).
    fn prune_board_filters(&mut self) -> bool {
        if let Some(v) = &mut self.open_view {
            v.state
                .filters
                .prune(&self.members, &self.labels, &self.lanes, &self.projects);
            return false;
        }
        let scope = self.board_scope();
        self.config.prune_filters_for(
            &scope,
            &self.members,
            &self.labels,
            &self.lanes,
            &self.projects,
        )
    }

    /// The header search text if it applies to the current view, else "".
    fn active_search(&self) -> &str {
        if let Some(v) = &self.open_view {
            return &v.state.search;
        }
        if self.search_view == self.view {
            &self.search_query
        } else {
            ""
        }
    }

    fn set_view_search(&mut self, query: String) {
        if let Some(v) = &mut self.open_view {
            v.state.search = query;
            return;
        }
        self.search_query = query;
        self.search_view = self.view.clone();
    }

    fn clear_view_search(&mut self) {
        if let Some(v) = &mut self.open_view {
            v.state.search.clear();
            return;
        }
        self.search_query.clear();
    }
    // --- end active board state ----------------------------------------

    fn breadcrumb(&self) -> Vec<header::Crumb> {
        // An open issue/project names its own team (My Issues and dashboards
        // open other teams' issues), so its id carries the right prefix.
        let mut crumbs = breadcrumb(
            &self.view,
            self.detail_team().or_else(|| self.selected_team()),
            &self.issues,
            &self.projects,
        );
        if let View::Dashboard(id) = &self.view {
            if let Some(d) = self.dashboards.iter().find(|d| d.id == *id) {
                crumbs.push(header::Crumb::plain(d.name.clone()));
            }
        }
        if self.detail_from_my_issues && matches!(self.view, View::IssueDetail(_)) {
            crumbs.insert(0, header::Crumb::new("My Issues", Some(Message::BackToMyIssues)));
        }
        if let (Some(o), View::IssueDetail(_)) = (&self.detail_from_dashboard, &self.view) {
            if let Some(d) = self.dashboards.iter().find(|d| d.id == *o) {
                crumbs.insert(0, header::Crumb::new(d.name.clone(), Some(Message::BackToDashboard)));
            }
        }
        if self.detail_from_inbox && matches!(self.view, View::IssueDetail(_)) {
            crumbs.insert(0, header::Crumb::new("Inbox", Some(Message::BackToInbox)));
        }
        if let (Some(v), View::Board) = (&self.open_view, &self.view) {
            // The view's name replaces the "Issues" crumb.
            if let Some(last) = crumbs.last_mut() {
                let mirror = crate::saved_view::Mirror {
                    lanes: &self.lanes,
                    members: &self.members,
                    labels: &self.labels,
                    projects: &self.projects,
                };
                last.label = if v.is_modified(&mirror) {
                    format!("{} (Modified)", v.name)
                } else {
                    v.name.clone()
                };
            }
        }
        crumbs
    }

    /// The header's Back for an open issue (`tab_state::back_target`).
    /// Whether a create/edit form fills the page (issue, project, recurring
    /// issue: the forms the web serves as `new`/`edit` pages).
    fn form_page_open(&self) -> bool {
        matches!(self.modal, Modal::IssueForm | Modal::ProjectForm | Modal::RecurringForm)
    }

    /// The open form page and its header crumbs: the page it was opened from
    /// (its last crumb goes back), then "New issue" etc.
    fn form_page_view(&self) -> Option<(Vec<header::Crumb>, Element<'_, Message>)> {
        let (label, page): (String, Element<'_, Message>) = match self.modal {
            Modal::IssueForm => {
                let form = self.issue_form.as_ref()?;
                let team_ident = self
                    .selected_team()
                    .map(|t| t.identifier.clone())
                    .unwrap_or_default();
                let ident = |i: &Issue| format!("{team_ident}-{}", i.team_number.unwrap_or(0));
                let editing = form
                    .editing_issue_id
                    .as_ref()
                    .and_then(|id| self.issues.iter().find(|i| i.id == *id));
                let parent = form
                    .parent_issue_id
                    .as_ref()
                    .filter(|_| form.editing_issue_id.is_none())
                    .and_then(|id| self.issues.iter().find(|i| i.id == *id))
                    .map(|p| (ident(p), p.title.clone()));
                let label = if form.editing_issue_id.is_some() {
                    "Edit issue"
                } else if form.parent_issue_id.is_some() {
                    "New sub-issue"
                } else {
                    "New issue"
                };
                let context = issue_form::PageContext {
                    chip: editing.map(ident).unwrap_or_else(|| team_ident.clone()),
                    parent,
                };
                // Members and projects are per team; My Issues loads every
                // team's, so offer only the issue's own team's.
                let form_team = editing
                    .map(|i| i.team_id.as_str())
                    .or(self.selected_team_id.as_deref())
                    .unwrap_or_default();
                let choices = issue_form::Choices {
                    members: self.members.iter().filter(|m| m.team_id == form_team).collect(),
                    projects: self.projects.iter().filter(|p| p.team_id == form_team).collect(),
                    // A new project goes to the selected team.
                    can_create_project: self.selected_team_id.as_deref() == Some(form_team),
                };
                let page = match &self.new_issue_scope {
                    Some((lanes, labels))
                        if form.editing_issue_id.is_none() && form.parent_issue_id.is_none() =>
                    {
                        issue_form::page(form, lanes, labels, choices, context, self.date_picker.as_ref())
                    }
                    _ => issue_form::page(form, &self.lanes, &self.labels, choices, context, self.date_picker.as_ref()),
                };
                (label.to_string(), page)
            }
            Modal::ProjectForm => (
                "New project".to_string(),
                projects_view::page(self.project_form.as_ref()?, &self.members, self.date_picker.as_ref()),
            ),
            Modal::RecurringForm => {
                let form = self.recurring_form.as_ref()?;
                let label = if form.editing.is_some() { "Edit recurring issue" } else { "New recurring issue" };
                (label.to_string(), crate::views::recurring_issue_form::page(form, &self.labels, self.date_picker.as_ref()))
            }
            _ => return None,
        };
        let mut crumbs = self.breadcrumb();
        if let Some(last) = crumbs.last_mut() {
            last.on_press = Some(Message::CloseModal);
        }
        crumbs.push(header::Crumb::plain(label));
        Some((crumbs, page))
    }

    fn back_message(&self) -> Option<Message> {
        use crate::tab_state::{back_target, Back, DetailFrom};
        if matches!(self.view, View::DependencyMap { .. }) {
            return Some(Message::CloseDependencyMap);
        }
        if matches!(self.view, View::ProjectDependencyMap { .. }) {
            return Some(Message::CloseProjectDependencyMap);
        }
        if !matches!(self.view, View::IssueDetail(_)) {
            return None;
        }
        let from = DetailFrom {
            my_issues: self.detail_from_my_issues,
            inbox: self.detail_from_inbox,
            dashboard: self.detail_from_dashboard.clone(),
            page: self.detail_from_page.clone(),
        };
        Some(match back_target(self.active_tab_key()?, &from)? {
            Back::MyIssues => Message::BackToMyIssues,
            Back::Inbox => Message::BackToInbox,
            Back::Dashboard(_) => Message::BackToDashboard,
            Back::TabRoot => Message::Tab(TabMessage::BackToRoot),
            Back::Page(view) => Message::Tab(TabMessage::BackToPage(view)),
        })
    }

    /// Empty-state text for My Issues, when there's something to explain.
    fn my_issues_empty(&self) -> Option<&'static str> {
        let ids = display::my_member_ids(&self.members, self.config.current_user_remote_id);
        let visible: usize = self.board_columns().iter().map(|c| c.issues.len()).sum();
        let narrowed = self.board_filters().count() > self.board_filters().assignees.len()
            || !crate::search_match::needle(self.active_search()).is_empty();
        display::my_issues_empty_message(!ids.is_empty(), visible, narrowed)
    }

    /// Views with a list to filter; detail/placeholder views disable search.
    fn search_enabled(&self) -> bool {
        matches!(
            self.view,
            View::Board
                | View::MyIssues
                | View::Dashboard(_)
                | View::ProjectList
                | View::Roadmap
        )
    }

    /// The team a new issue goes to from My Issues: the most recently active
    /// team Tab's team, else the first team. An archived team (its Tab stays
    /// open, but its data isn't loaded on My Issues) falls back to the first.
    fn cross_team_new_issue_team(&self) -> Option<String> {
        let first = default_team_id(&self.teams);
        let team = self
            .tab_set
            .as_ref()
            .and_then(|set| set.new_issue_team(first.as_deref()))
            .or(first.as_deref())?;
        let archived = self
            .teams
            .iter()
            .any(|t| t.id == team && t.archived_at.is_some());
        if archived { first } else { Some(team.to_string()) }
    }

    /// The lanes and labels a new issue's form offers when My Issues loaded
    /// every team's: only the selected team's.
    fn new_issue_scope(&self) -> Option<(Vec<Lane>, Vec<Label>)> {
        let team = self.selected_team_id.as_deref()?;
        Some((
            self.lanes.iter().filter(|l| l.team_id == team).cloned().collect(),
            self.labels.iter().filter(|l| l.team_id == team).cloned().collect(),
        ))
    }

    /// What the header "+ New" button creates on the current view, if anything.
    fn new_action(&self) -> Option<Message> {
        self.selected_team()?;
        match self.view {
            View::Board | View::MyIssues => Some(Message::CreateIssue),
            View::ProjectList => Some(Message::CreateProject),
            View::Roadmap => Some(Message::OpenRoadmapAdd(RoadmapCommitment::Now)),
            _ => None,
        }
    }

    /// Board columns for the current display options, filters and search.
    /// Focus indices (`focused_lane`/`focused_issue`) index into these.
    fn board_columns(&self) -> Vec<display::Column<'_>> {
        self.board_layout().0
    }

    /// `board_columns` plus the empty columns left off the board (Rails'
    /// "Hidden columns" list; always empty on a dashboard).
    fn board_layout(&self) -> (Vec<display::Column<'_>>, Vec<display::HiddenColumn>) {
        if matches!(self.view, View::Dashboard(_)) {
            let columns = self
                .dashboard_evaluation()
                .groups
                .into_iter()
                .map(|g| display::Column {
                    title: g.group.name.clone(),
                    lane_icon: None,
                    issues: g.issues,
                    sections: Vec::new(),
                })
                .collect();
            return (columns, Vec::new());
        }
        let my = self.view == View::MyIssues;
        let data = display::Data {
            team: if my { None } else { self.selected_team() },
            teams: &self.teams,
            merge_lanes_by_name: my,
            lanes: &self.lanes,
            issues: &self.issues,
            members: &self.members,
            projects: &self.projects,
            labels: &self.labels,
            issue_labels: &self.issue_labels,
        };
        let needle = crate::search_match::needle(self.active_search());
        if my {
            let ids = display::my_member_ids(&self.members, self.config.current_user_remote_id);
            let filters = display::my_issues_filters(self.board_filters(), &ids, &self.lanes);
            return display::build_board(&data, self.board_display(), &filters, &needle);
        }
        display::build_board(&data, self.board_display(), self.board_filters(), &needle)
    }

    /// Point the focus at `issue_id`'s card if it is on the board. Columns
    /// aren't lanes one-to-one once empty ones are hidden, so a move must
    /// follow the card rather than the lane index.
    fn focus_issue_by_id(&mut self, issue_id: &str) {
        let found = self.board_columns().iter().enumerate().find_map(|(c, col)| {
            col.issues
                .iter()
                .position(|i| i.id == issue_id)
                .map(|p| (c, p))
        });
        if let Some((lane, issue)) = found {
            self.focused_lane = lane;
            self.focused_issue = issue;
        }
    }

    /// A project's issues for the detail page: the board's filters (minus the
    /// project field, which the page already scopes), completed/sub-issue
    /// settings, search and ordering.
    fn project_detail_issues(&self, project_id: &str, needle: &str) -> Vec<&Issue> {
        let data = display::Data {
            team: self.selected_team(),
            teams: &self.teams,
            merge_lanes_by_name: false,
            lanes: &self.lanes,
            issues: &self.issues,
            members: &self.members,
            projects: &self.projects,
            labels: &self.labels,
            issue_labels: &self.issue_labels,
        };
        // The Show chip owns completed/canceled visibility here, and the board's
        // filters have no control on this page (the header hides Filter), so
        // neither may narrow the list: "All" means every issue in the project.
        let mut display_opts = self.board_display().clone();
        display_opts.show_completed = true;
        let filters = display::Filters::default();
        let mut out = display::visible_from(
            self.issues
                .iter()
                .filter(|i| i.project_id.as_deref() == Some(project_id))
                // Rails' "Show: Active" drops completed and canceled issues.
                .filter(|i| {
                    self.project_issue_filter == display::ProjectIssueFilter::All
                        || crate::dep_graph::is_active(i, &self.lanes)
                }),
            &data,
            &display_opts,
            &filters,
            needle,
        );
        // The project page has its own sort (Rails' chips), not the board's.
        display::sort_project_issues(&mut out, self.project_issue_sort, &self.lanes);
        out
    }

    /// Issues in the focused column, in on-screen order (sub-groups flattened).
    fn issues_in_focused_lane(&self) -> Vec<&Issue> {
        let mut cols = self.board_columns();
        if self.focused_lane < cols.len() {
            cols.swap_remove(self.focused_lane).issues
        } else {
            vec![]
        }
    }

    /// Whether j/k/h/l use list-mode navigation: the list display mode, or a
    /// dashboard (always a list of groups).
    fn list_nav_active(&self) -> bool {
        matches!(self.view, View::Dashboard(_)) || self.board_display().view_mode == ViewMode::List
    }

    /// Whether the board draws sub-group rows (swimlanes): board mode on a
    /// board view with a sub-group set.
    fn swimlanes_active(&self) -> bool {
        matches!(self.view, View::Board | View::MyIssues)
            && self.board_display().view_mode == ViewMode::Board
            && self.board_display().sub_group != SubGroup::None
    }

    /// Collapsed flag per board row of `columns`. Empty rows start collapsed;
    /// a row whose title is toggled in `collapsed_sections` flips that.
    fn swimlane_collapsed(&self, columns: &[display::Column<'_>]) -> Vec<bool> {
        display::board_rows(columns)
            .iter()
            .map(|r| {
                let toggled = self.collapsed_sections.contains(&swimlane_key(&r.title));
                display::board_row_collapsed(r, toggled)
            })
            .collect()
    }

    /// Board j/k/h/l with sub-group rows: walk on-screen cards, skipping
    /// collapsed rows, and keep the row when changing column.
    fn swimlane_nav(&mut self, message: &Message) {
        let next = {
            let columns = self.board_columns();
            let collapsed = self.swimlane_collapsed(&columns);
            let Some(col) = columns.get(self.focused_lane) else {
                return;
            };
            match message {
                Message::FocusNextIssue | Message::FocusPrevIssue => {
                    let forward = matches!(message, Message::FocusNextIssue);
                    let issue = display::swimlane_step(
                        &col.sections,
                        &collapsed,
                        self.focused_issue,
                        forward,
                    );
                    (self.focused_lane, issue)
                }
                _ => {
                    let target = if matches!(message, Message::FocusNextLane) {
                        self.focused_lane + 1
                    } else {
                        self.focused_lane.wrapping_sub(1)
                    };
                    let Some(next) = columns.get(target) else {
                        return;
                    };
                    let row = display::swimlane_row_of(&col.sections, self.focused_issue);
                    (target, display::swimlane_enter(&next.sections, &collapsed, row))
                }
            }
        };
        (self.focused_lane, self.focused_issue) = next;
    }

    /// Per-section counts for list-mode navigation: collapsed sections count
    /// as empty so j/k and h/l skip their hidden rows.
    fn list_nav_counts(&self) -> Vec<usize> {
        if let View::Dashboard(did) = &self.view {
            let collapsed = self.collapsed_dashboard_groups.get(did);
            return self
                .dashboard_evaluation()
                .groups
                .iter()
                .map(|g| {
                    if collapsed.is_some_and(|c| c.contains(&g.group.id)) {
                        0
                    } else {
                        g.issues.len()
                    }
                })
                .collect();
        }
        self.board_columns()
            .iter()
            .map(|c| {
                if self.collapsed_sections.contains(&c.title) {
                    0
                } else {
                    c.issues.len()
                }
            })
            .collect()
    }

    /// Bring the focus into view: the focused board column sideways, then the
    /// focused card or row within its scrollable.
    fn scroll_to_focus(&mut self) -> Task<Message> {
        let column = self.reveal_focused_board_column();
        Task::batch([column, self.scroll_focused_item_into_view()])
    }

    /// Scroll the board sideways so the focused column is fully in view, even
    /// when it is empty. Does nothing outside board mode.
    fn reveal_focused_board_column(&mut self) -> Task<Message> {
        if !matches!(self.view, View::Board | View::MyIssues)
            || self.board_display().view_mode == ViewMode::List
        {
            return Task::none();
        }
        let (offset, viewport) = self
            .scroll_state
            .get(&ScrollKey::Board)
            .copied()
            .unwrap_or((0.0, scroll::FALLBACK_VIEWPORT_H));
        let left = scroll::board_column_left(self.focused_lane);
        let new = scroll::offset_to_reveal(left, scroll::BOARD_COLUMN_W, offset, viewport);
        if (new - offset).abs() < 0.5 {
            return Task::none();
        }
        self.scroll_state.insert(ScrollKey::Board, (new, viewport));
        ScrollKey::Board.scroll_to(new)
    }

    /// Scroll the board lane / list so the focused card or row is fully in
    /// view. Positions come from fixed element heights (see `scroll`); the
    /// current offset and viewport height come from `Message::Scrolled`.
    fn scroll_focused_item_into_view(&mut self) -> Task<Message> {
        if let View::Dashboard(did) = &self.view {
            let collapsed = self.collapsed_dashboard_groups.get(did);
            let groups: Vec<crate::views::dashboard::SpanGroup> = self
                .dashboard_evaluation()
                .groups
                .iter()
                .map(|g| crate::views::dashboard::SpanGroup {
                    len: g.issues.len(),
                    collapsed: collapsed.is_some_and(|c| c.contains(&g.group.id)),
                    has_description: !g.group.description.is_empty(),
                })
                .collect();
            if self.focused_issue >= groups.get(self.focused_lane).map_or(0, |g| g.len)
                || groups[self.focused_lane].collapsed
            {
                return Task::none();
            }
            let (top, height) =
                crate::views::dashboard::focused_span(&groups, self.focused_lane, self.focused_issue);
            let key = ScrollKey::Dashboard;
            let (offset, viewport) = self
                .scroll_state
                .get(&key)
                .copied()
                .unwrap_or((0.0, scroll::FALLBACK_VIEWPORT_H));
            let new = scroll::offset_to_reveal(top, height, offset, viewport);
            if (new - offset).abs() < 0.5 {
                return Task::none();
            }
            self.scroll_state.insert(key, (new, viewport));
            return key.scroll_to(new);
        }
        if !matches!(self.view, View::Board | View::MyIssues) {
            return Task::none();
        }
        let list = self.board_display().view_mode == ViewMode::List;
        let swimlanes = self.swimlanes_active();
        let (key, top, height) = {
            let columns = self.board_columns();
            let Some(col) = columns.get(self.focused_lane) else {
                return Task::none();
            };
            if self.focused_issue >= col.issues.len() {
                return Task::none();
            }
            if list {
                if self.collapsed_sections.contains(&col.title) {
                    return Task::none();
                }
                let (top, h) = issue_list::focused_span(
                    &columns,
                    &self.collapsed_sections,
                    self.focused_lane,
                    self.focused_issue,
                );
                (ScrollKey::IssueList, top, h)
            } else if swimlanes {
                let collapsed = self.swimlane_collapsed(&columns);
                let data = board::CardData {
                    teams: &self.teams,
                    lanes: &self.lanes,
                    members: &self.members,
                    projects: &self.projects,
                    issue_labels: &self.issue_labels,
                    comment_counts: &self.comment_counts,
                    block_counts: &self.block_counts,
                    display: self.board_display(),
                    hovered: None,
                    deps: None,
                };
                let Some((top, h)) = board::swimlane_focused_span(
                    &columns,
                    &collapsed,
                    &data,
                    self.focused_lane,
                    self.focused_issue,
                ) else {
                    return Task::none();
                };
                (ScrollKey::Swimlanes, top, h)
            } else {
                let data = board::CardData {
                    teams: &self.teams,
                    lanes: &self.lanes,
                    members: &self.members,
                    projects: &self.projects,
                    issue_labels: &self.issue_labels,
                    comment_counts: &self.comment_counts,
                    block_counts: &self.block_counts,
                    display: self.board_display(),
                    hovered: None,
                    deps: None,
                };
                let (top, h) = board::focused_span(col, &data, self.focused_issue);
                (ScrollKey::Lane(self.focused_lane), top, h)
            }
        };
        let (offset, viewport) = self
            .scroll_state
            .get(&key)
            .copied()
            .unwrap_or((0.0, scroll::FALLBACK_VIEWPORT_H));
        let new = scroll::offset_to_reveal(top, height, offset, viewport);
        if (new - offset).abs() < 0.5 {
            return Task::none();
        }
        self.scroll_state.insert(key, (new, viewport));
        key.scroll_to(new)
    }

    /// Keep the selected Inbox row in view. Rows are fixed-height and sit in
    /// their own scrollable below the filter tabs, so row `i` starts at
    /// `i * ROW_HEIGHT`.
    fn scroll_inbox_to_selection(&mut self) -> Task<Message> {
        use crate::views::inbox as inbox_view;
        let top = self.inbox.selected as f32 * inbox_view::ROW_HEIGHT;
        let (offset, viewport) = self
            .scroll_state
            .get(&ScrollKey::Inbox)
            .copied()
            .unwrap_or((0.0, scroll::FALLBACK_VIEWPORT_H));
        let new = scroll::offset_to_reveal(top, inbox_view::ROW_HEIGHT, offset, viewport);
        if (new - offset).abs() < 0.5 {
            return Task::none();
        }
        self.scroll_state.insert(ScrollKey::Inbox, (new, viewport));
        ScrollKey::Inbox.scroll_to(new)
    }

    fn focused_issue(&self) -> Option<&Issue> {
        let lane_issues = self.issues_in_focused_lane();
        lane_issues.get(self.focused_issue).copied()
    }

    fn clamp_focus(&mut self) {
        let counts: Vec<usize> = self
            .board_columns()
            .iter()
            .map(|c| c.issues.len())
            .collect();
        (self.focused_lane, self.focused_issue) =
            clamped_focus(&counts, self.focused_lane, self.focused_issue);
        // Sub-group rows: a focus left inside a collapsed row moves to a
        // card that is on screen.
        if self.swimlanes_active() {
            let columns = self.board_columns();
            let collapsed = self.swimlane_collapsed(&columns);
            if let Some(col) = columns.get(self.focused_lane) {
                self.focused_issue =
                    display::swimlane_snap(&col.sections, &collapsed, self.focused_issue);
            }
        }
    }

    /// Persist the display/filter settings and re-validate board focus.
    fn persist_display(&mut self) {
        // An open view's edits live in the working copy only.
        if self.open_view.is_none() {
            if let Err(e) = config::save(&self.config) {
                self.config_error = Some(e);
            }
        }
        self.clamp_focus();
    }

    /// Active-filter badge count: selected filter values plus a non-empty search.
    fn active_filter_count(&self) -> usize {
        // My Issues pins the assignee, so a saved assignee filter doesn't count.
        // Project detail is scoped to one project, so project filters don't count.
        let ignored = match self.view {
            View::MyIssues => self.board_filters().assignees.len(),
            View::ProjectDetail(_) => self.board_filters().projects.len(),
            _ => 0,
        };
        self.board_filters().count() - ignored
            + usize::from(!crate::search_match::needle(self.active_search()).is_empty())
    }

    /// The header's view actions on an open view on the Board: Update view, Save
    /// as new view and Revert (only when Modified), then always Rename and Delete.
    fn update_view_action(&self) -> Option<Element<'_, Message>> {
        let v = self.open_view.as_ref()?;
        if self.view != View::Board {
            return None;
        }
        let mirror = crate::saved_view::Mirror {
            lanes: &self.lanes,
            members: &self.members,
            labels: &self.labels,
            projects: &self.projects,
        };
        let mut actions = iced::widget::row![].spacing(8);
        if v.is_modified(&mirror) {
            actions = actions
                .push(header::update_view_button(self.sync_read_only))
                .push(header::save_as_new_view_button(self.sync_read_only))
                .push(header::revert_button());
        }
        Some(
            actions
                .push(header::rename_view_button(&v.view_id, self.sync_read_only))
                .push(header::delete_view_button(&v.view_id, self.sync_read_only))
                .into(),
        )
    }

    fn enqueue_sync(
        &self,
        entity_type: &str,
        entity_id: &str,
        action: &str,
        payload: Option<&str>,
    ) {
        let conn = self.db();
        let _ = sync_queue::enqueue(&conn, entity_type, entity_id, action, payload);
    }

    /// Switch the right pane to the detail view for `issue`, flushing any
    /// currently-open detail buffer first so unsaved edits aren't lost. Renders
    /// comments from the local DB immediately and returns a background task that
    /// refreshes them from Rails (no-op when offline or the issue isn't synced).
    /// Open `issue` from the board. From My Issues, first select the issue's
    /// own team and restore per-team data so the detail view (lanes, members,
    /// projects) is that team's.
    /// True when a click on `issue_id` should open its Dependency map: the
    /// board is showing dependency lines and the card has at least one.
    fn card_opens_map(&self, issue_id: &str) -> bool {
        if !matches!(self.view, View::Board | View::MyIssues)
            || self.board_display().view_mode == ViewMode::List
            || !self.board_display().show_dependencies
        {
            return false;
        }
        let columns = self.board_columns();
        crate::views::dep_overlay::DepLines::new(
            columns.iter().flat_map(|c| c.issues.iter().map(|i| i.id.as_str())),
            &self.board_deps,
        )
        .is_linked(issue_id)
    }

    fn open_focused_issue(&mut self, issue: Issue) -> Task<Message> {
        // A team or project Tab only shows its own team: another team's
        // issue opens in that team's Tab (`tabs::issue_open_target`).
        let target = self.active_tab_key().and_then(|active| {
            crate::tabs::issue_open_target(active, self.active_tab_team().as_deref(), &issue.team_id)
        });
        if let Some(key) = target {
            if let crate::tabs::TabKey::Team(id) = &key {
                self.expanded_teams.insert(id.clone());
            }
            let focus = self.focus_tab(key);
            return Task::batch([focus, self.open_issue_detail(&issue)]);
        }
        if let View::Dashboard(dashboard_id) = self.view.clone() {
            return self.open_issue_from_dashboard(dashboard_id, issue);
        }
        if self.view == View::Inbox {
            return self.open_issue_from_inbox(issue);
        }
        if self.view == View::MyIssues {
            self.selected_team_id = Some(issue.team_id.clone());
            self.view = View::Board;
            self.reload_data();
            self.detail_from_my_issues = true;
        }
        // Issue to issue inside a cross-team Tab (a link, a dependency): the
        // Tab stays, and loads the next issue's team for the detail. Back
        // keeps the origin flags, so it still returns to the page.
        let cross_team_tab = self.active_tab_key().is_some_and(crate::tabs::TabKey::is_cross_team);
        if cross_team_tab
            && matches!(self.view, View::IssueDetail(_))
            && self.selected_team_id.as_deref() != Some(issue.team_id.as_str())
        {
            self.selected_team_id = Some(issue.team_id.clone());
            self.reload_data();
        }
        self.open_issue_detail(&issue)
    }

    /// Open `issue` from a dashboard: select its own team (like My Issues) so
    /// the detail is that team's, remembering the dashboard for Back.
    fn open_issue_from_dashboard(&mut self, dashboard_id: String, issue: Issue) -> Task<Message> {
        // The view is about to stop being a dashboard, which skips the stash
        // `update` does afterwards: park the focus now (a click sets it and
        // opens the issue in the same message).
        self.stash_dashboard_session();
        self.detail_from_dashboard = Some(dashboard_id);
        self.selected_team_id = Some(issue.team_id.clone());
        self.view = View::Board;
        self.reload_data();
        self.open_issue_detail(&issue)
    }

    /// Leave an issue detail opened from a dashboard and return to it. False
    /// when the detail didn't come from one. The selected team stays the
    /// issue's: the Dashboard's Tab is cross-team, and its data and display
    /// options don't depend on it.
    fn back_to_dashboard(&mut self) -> bool {
        let Some(dashboard_id) = self.detail_from_dashboard.take() else {
            return false;
        };
        self.commit_issue_detail();
        self.issue_detail = None;
        self.switch_view(View::Dashboard(dashboard_id));
        true
    }

    /// Open `issue` from the Inbox: select its own team (like My Issues) so the
    /// detail is that team's, and remember to return to the Inbox on Back.
    fn open_issue_from_inbox(&mut self, issue: Issue) -> Task<Message> {
        self.selected_team_id = Some(issue.team_id.clone());
        self.view = View::Board;
        self.reload_data();
        self.detail_from_my_issues = false;
        self.detail_from_inbox = true;
        self.open_issue_detail(&issue)
    }

    /// Mark the selected Inbox Notification read in the mirror and refresh the
    /// badge and rows. Returns the Notification (as it was) if one is selected.
    fn mark_selected_inbox_read(&mut self) -> Option<crate::db::notifications::Notification> {
        let selected = self.inbox.selected_item().cloned()?;
        let conn = self.db();
        let now = crate::db::now_iso();
        if crate::db::notifications::mark_read(&conn, selected.remote_id, &now).unwrap_or(false) {
            self.notif_unread_count = crate::db::notifications::unread_count(&conn).unwrap_or(0);
            self.reload_inbox();
        }
        Some(selected)
    }

    fn open_issue_detail(&mut self, issue: &Issue) -> Task<Message> {
        // The page Back returns a team Tab to: set when leaving a page, kept
        // when going from issue A to issue B.
        // Returning from the dependency map isn't leaving a page.
        if self.view.issue_id().is_none() {
            self.detail_from_page = Some(self.view.clone());
        }
        self.commit_issue_detail();
        self.commit_project_detail();
        if let Some(old) = self.issue_detail.take() {
            self.drop_drafts(&[old.composer_files.comment_id, old.composer_files.reply_id]);
        }
        self.issue_detail = Some(issue_detail::IssueDetailState::from_issue(issue));
        self.view = View::IssueDetail(issue.id.clone());
        self.composer_focused = false;
        let conn = self.db();
        self.load_detail_dependencies(&conn);
        self.reload_detail_origin(&conn);
        Task::batch([
            self.fetch_comments_task(&conn, issue),
            self.fetch_activity_task(issue),
            self.fetch_issue_task(&conn, issue),
        ])
    }

    /// Activity fetch for the issue whose detail is open, or `Task::none()`
    /// when no detail is open (or the issue is gone).
    fn refetch_open_issue_activity(&self) -> Task<Message> {
        let View::IssueDetail(id) = &self.view else {
            return Task::none();
        };
        let conn = self.db();
        match queries::get_issue(&conn, id) {
            Ok(Some(issue)) => self.fetch_activity_task(&issue),
            _ => Task::none(),
        }
    }

    /// Open an Attachment in its default app: straight from the file cache on a
    /// hit, otherwise download it (authenticated, redirect followed) first.
    fn open_attachment(&mut self, att: crate::db::attachments::Attachment) -> Task<Message> {
        let (Some(key), Some(remote_id)) = (crate::files::open::blob_key(&att), att.remote_id)
        else {
            self.push_toast(
                format!("{} hasn't finished uploading yet", att.filename),
                ToastKind::Error,
                false,
            );
            return Task::none();
        };
        if !self.files_unlocked() {
            self.push_toast("Sign in to open files", ToastKind::Error, false);
            return Task::none();
        }
        let cache = crate::files::cache::FileCache::at_default_root();
        if let Ok(Some(path)) = cache.get(&self.db(), &key) {
            self.open_cached_attachment(&att, &key, &path);
            return Task::none();
        }
        let base_url = self.config.api_base_url.trim().to_string();
        let token = self.config.api_token.clone().unwrap_or_default();
        let Ok(client) = ApiClient::new(base_url, token) else {
            self.push_toast("Sign in to download files", ToastKind::Error, false);
            return Task::none();
        };
        Task::perform(
            async move {
                crate::api::attachments::download(&client, remote_id)
                    .await
                    .map_err(|e| e.to_string())
            },
            move |r| Message::AttachmentDownloaded(att.clone(), r),
        )
    }

    /// Make sure `att`'s image is on its way into the cache for display.
    /// No-op for non-images, unuploaded files, and keys already known to
    /// `images`. A cache hit becomes `Ready` at once; a miss downloads and
    /// reports `Message::ImageFetched`.
    pub(crate) fn request_image(&mut self, att: &crate::db::attachments::Attachment) -> Task<Message> {
        if !crate::files::images::is_thumbnailable(&att.content_type) {
            return Task::none();
        }
        let (Some(key), Some(remote_id)) = (crate::files::open::blob_key(att), att.remote_id)
        else {
            return Task::none();
        };
        if !self.images.begin(&key) {
            return Task::none();
        }
        if !self.files_unlocked() {
            self.images.set_failed(&key);
            return Task::none();
        }
        let cache = crate::files::cache::FileCache::at_default_root();
        if let Ok(Some(path)) = cache.get(&self.db(), &key) {
            self.images.set_ready(&key, path);
            return Task::none();
        }
        let base_url = self.config.api_base_url.trim().to_string();
        let token = self.config.api_token.clone().unwrap_or_default();
        let Ok(client) = ApiClient::new(base_url, token) else {
            self.images.set_failed(&key);
            return Task::none();
        };
        Task::perform(
            async move {
                crate::api::attachments::download(&client, remote_id)
                    .await
                    .map_err(|e| e.to_string())
            },
            move |r| Message::ImageFetched(key.clone(), r),
        )
    }

    /// Fetch one inline image (a signed blob-redirect URL, no auth) into the
    /// file cache under its `files::inline::blob_key`; reports
    /// `Message::ImageFetched`. No-op for keys already known to `images`.
    pub(crate) fn request_inline_image(&mut self, src: &str) -> Task<Message> {
        let Some(key) = crate::files::inline::blob_key(src) else {
            return Task::none();
        };
        if !self.images.begin(&key) {
            return Task::none();
        }
        if !self.files_unlocked() {
            self.images.set_failed(&key);
            return Task::none();
        }
        let cache = crate::files::cache::FileCache::at_default_root();
        if let Ok(Some(path)) = cache.get(&self.db(), &key) {
            self.images.set_ready(&key, path);
            return Task::none();
        }
        let base_url = self.config.api_base_url.trim().to_string();
        let url = crate::files::inline::fetch_url(src, &base_url);
        let token = self.config.api_token.clone().unwrap_or_default();
        let Ok(client) = ApiClient::new(base_url, token) else {
            self.images.set_failed(&key);
            return Task::none();
        };
        Task::perform(
            async move { client.get_bytes_public(&url).await.map_err(|e| e.to_string()) },
            move |r| Message::ImageFetched(key.clone(), r),
        )
    }

    /// `request_inline_image` for every blob image in the open issue's description.
    fn request_description_images(&mut self) -> Task<Message> {
        let Some(detail) = self.issue_detail.as_ref() else {
            return Task::none();
        };
        if !matches!(&self.view, View::IssueDetail(id) if *id == detail.issue_id) {
            return Task::none();
        }
        let srcs = crate::views::body::fetchable_srcs_of_bodies(
            [detail.description_text().as_str()],
            &self.config.api_base_url,
        );
        Task::batch(srcs.iter().map(|s| self.request_inline_image(s)))
    }

    /// `request_inline_image` for every blob image in the open detail's comments
    /// (issue and project detail share `detail_comments`).
    fn request_comment_images(&mut self) -> Task<Message> {
        if !matches!(self.view, View::IssueDetail(_) | View::ProjectDetail(_)) {
            return Task::none();
        }
        let srcs = crate::views::body::fetchable_srcs_of_bodies(
            self.detail_comments.iter().map(|c| c.body.as_str()),
            &self.config.api_base_url,
        );
        Task::batch(srcs.iter().map(|s| self.request_inline_image(s)))
    }

    /// `request_inline_image` for every blob image in the open project's description.
    fn request_project_description_images(&mut self) -> Task<Message> {
        let Some(detail) = self.project_detail.as_ref() else {
            return Task::none();
        };
        if !matches!(&self.view, View::ProjectDetail(id) if *id == detail.project_id) {
            return Task::none();
        }
        let srcs = crate::views::body::fetchable_srcs_of_body(
            &detail.description_text(),
            &self.config.api_base_url,
        );
        Task::batch(srcs.iter().map(|s| self.request_inline_image(s)))
    }

    /// Lightbox set for the open issue/project detail, in reading order.
    fn lightbox_images(&self) -> Vec<crate::files::lightbox::LightboxImage> {
        use crate::files::lightbox::{reading_order_images, CommentImages};
        let description = match (&self.view, &self.issue_detail, &self.project_detail) {
            (View::IssueDetail(_), Some(d), _) => d.description_text(),
            (View::ProjectDetail(_), _, Some(d)) => d.description_text(),
            _ => return Vec::new(),
        };
        let comments: Vec<CommentImages> = Comment::thread_order(&self.detail_comments)
            .into_iter()
            .map(|(_, c)| CommentImages {
                body: &c.body,
                attachments: self
                    .detail_comment_attachments
                    .get(&c.id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]),
            })
            .collect();
        reading_order_images(
            &description,
            &self.detail_attachments,
            &comments,
            &self.config.api_base_url,
        )
    }

    /// Fetch the lightbox's current image and its neighbours (Attachments and Inline).
    fn request_lightbox_images(&mut self) -> Task<Message> {
        let Modal::Lightbox(state) = &self.modal else {
            return Task::none();
        };
        let lo = state.index.saturating_sub(1);
        let wanted: Vec<_> = state.images[lo..(state.index + 2).min(state.images.len())].to_vec();
        Task::batch(wanted.iter().map(|i| match i {
            crate::files::lightbox::LightboxImage::Attachment(a) => self.request_image(a),
            crate::files::lightbox::LightboxImage::Inline { src, .. } => {
                self.request_inline_image(src)
            }
        }))
    }

    /// `request_image` for every Attachment currently rendered in the open detail view.
    fn request_visible_images(&mut self) -> Task<Message> {
        let atts: Vec<crate::db::attachments::Attachment> = self
            .detail_attachments
            .iter()
            .chain(self.detail_comment_attachments.values().flatten())
            .filter(|a| crate::files::images::is_thumbnailable(&a.content_type))
            .cloned()
            .collect();
        let inline = self.request_description_images();
        let project_inline = self.request_project_description_images();
        let comment_inline = self.request_comment_images();
        Task::batch(
            atts.iter()
                .map(|a| self.request_image(a))
                .chain([inline, project_inline, comment_inline]),
        )
    }

    /// Copy a cached blob to its real filename and hand it to the OS.
    fn open_cached_attachment(
        &mut self,
        att: &crate::db::attachments::Attachment,
        key: &str,
        cached: &std::path::Path,
    ) {
        let root = crate::files::default_root();
        let opened = crate::files::open::materialize(&root, key, &att.filename, cached)
            .and_then(|p| crate::files::open::open_in_default_app(&p));
        if let Err(e) = opened {
            self.push_toast(
                format!("Couldn't open {}: {e}", att.filename),
                ToastKind::Error,
                false,
            );
        }
    }

    /// Build the async task that fetches an issue's Activity into the local
    /// cache. `Task::none()` when no client can be built; the fetch itself skips
    /// issues that are not synced yet.
    fn fetch_activity_task(&self, issue: &Issue) -> Task<Message> {
        let base_url = self.config.api_base_url.trim().to_string();
        let token = self.config.api_token.clone().unwrap_or_default();
        let Ok(client) = ApiClient::new(base_url, token) else {
            return Task::none();
        };
        let issue_id = issue.id.clone();
        Task::perform(
            async move {
                let stored = match activity_sync::fetch(&client, &issue_id).await {
                    FetchOutcome::Fetched { stored } => stored,
                    FetchOutcome::Skipped => 0,
                    FetchOutcome::Failed(msg) => {
                        eprintln!("Activity fetch for {issue_id} failed: {msg}");
                        0
                    }
                };
                (issue_id, stored)
            },
            |(id, stored)| Message::ActivityFetched(id, stored),
        )
    }

    /// Build the async task that does the one fresh fetch of `issue` (with
    /// detail) when it is opened and merges it like a pulled row. No-op unless
    /// the issue + team are synced and a client can be built.
    fn fetch_issue_task(&self, conn: &rusqlite::Connection, issue: &Issue) -> Task<Message> {
        let Some(issue_remote_id) = issue.remote_id else {
            return Task::none();
        };
        let Ok(Some(team_remote_id)) = queries::remote_id_for(conn, "teams", &issue.team_id)
        else {
            return Task::none();
        };
        let base_url = self.config.api_base_url.trim().to_string();
        let token = self.config.api_token.clone().unwrap_or_default();
        let Ok(client) = ApiClient::new(base_url, token) else {
            return Task::none();
        };
        let local_team_id = issue.team_id.clone();
        Task::perform(
            async move {
                SyncEngine::fetch_issue(&client, team_remote_id, &local_team_id, issue_remote_id)
                    .await
            },
            Message::IssueFetched,
        )
    }

    /// Persist a new comment (optionally a reply to local comment `parent_id`) on
    /// the open detail issue or project and enqueue its push. Returns whether it was saved.
    /// Replies beyond `MAX_NESTING_DEPTH` are refused (Rails rejects them too).
    fn create_comment(&mut self, body: String, parent_id: Option<String>) -> bool {
        let (issue_id, project_id) = match (&self.view, &self.issue_detail, &self.project_detail) {
            (View::ProjectDetail(_), _, Some(p)) => (None, Some(p.project_id.clone())),
            (_, Some(d), _) => (Some(d.issue_id.clone()), None),
            _ => return false,
        };
        // A top-level comment takes the id its staged files are owned by.
        // (and pasted images); a reply takes the id its pasted images were owned by.
        let (local_id, staged) = match (&parent_id, self.comment_drafts_mut()) {
            (None, Some(d)) => (Some(d.files.comment_id.clone()), !d.files.files.is_empty()),
            (Some(_), Some(d)) => (Some(d.files.reply_id.clone()), false),
            _ => (None, false),
        };
        if !comment_has_content(&body, staged) {
            return false;
        }
        if let Some(pid) = &parent_id {
            let depth = Comment::thread_order(&self.detail_comments)
                .into_iter()
                .find(|(_, c)| &c.id == pid)
                .map(|(d, _)| d);
            if !matches!(depth, Some(d) if Comment::can_reply_at(d)) {
                return false;
            }
        }
        let conn = self.db();
        // Stamp the signed-in user as author for immediate local display;
        // Rails also forces the author server-side from the token.
        let author_id = self.config.current_user_remote_id.and_then(|rid| {
            queries::find_member_by_remote_id(&conn, rid)
                .ok()
                .flatten()
                .map(|m| m.id)
        });
        let now = crate::db::now_iso();
        let comment = Comment {
            id: local_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            remote_id: None,
            issue_id,
            project_id,
            author_id,
            body,
            parent_id,
            created_at: now.clone(),
            updated_at: now,
        };
        if queries::insert_comment(&conn, &comment).is_err() {
            return false;
        }
        drop(conn);
        self.enqueue_sync(crate::db::attachments::COMMENT_ENTITY, &comment.id, "create", None);
        if let Some(d) = self.comment_drafts_mut() {
            // Fresh owner id for the next comment / reply (the old ones now belong to this one).
            if comment.parent_id.is_none() {
                d.files.files.clear();
                d.files.comment_id = uuid::Uuid::new_v4().to_string();
            } else {
                d.files.reply_id = uuid::Uuid::new_v4().to_string();
            }
        }
        self.reload_data();
        true
    }

    /// Build the async task that pulls an issue's comments from Rails. Returns
    /// `Task::none()` unless the issue + team are synced and a client can be
    /// built from the configured token.
    fn fetch_comments_task(&self, conn: &rusqlite::Connection, issue: &Issue) -> Task<Message> {
        let issue_remote_id = match issue.remote_id {
            Some(id) => id,
            None => return Task::none(),
        };
        let team_remote_id = match queries::remote_id_for(conn, "teams", &issue.team_id) {
            Ok(Some(id)) => id,
            _ => return Task::none(),
        };
        let base_url = self.config.api_base_url.trim().to_string();
        let token = self.config.api_token.clone().unwrap_or_default();
        let client = match ApiClient::new(base_url, token) {
            Ok(c) => c,
            Err(_) => return Task::none(),
        };
        let issue_local_id = issue.id.clone();
        Task::perform(
            async move {
                let result = crate::api::comments::list(&client, team_remote_id, issue_remote_id)
                    .await
                    .map_err(|e| e.to_string());
                (issue_local_id, result)
            },
            |(id, result)| Message::CommentsFetched(id, result),
        )
    }

    /// Apply a live sidebar change (Status/Priority/Project) to the issue behind
    /// the open detail view: mutate, persist, enqueue sync, and reload so the
    /// pick_lists reflect DB truth. Buffered text fields are left untouched.
    fn apply_issue_update(&mut self, mutate: impl FnOnce(&mut Issue)) {
        let id = match &self.issue_detail {
            Some(d) => d.issue_id.clone(),
            None => return,
        };
        self.update_issue_by_id(&id, mutate);
    }

    /// The issue a card shortcut (Rails' `l`/`s`/`p`/`j`) acts on, and the
    /// card to anchor its popover to: the open issue on its detail page; on
    /// the board, the card under the pointer (as Rails does), falling back to
    /// the keyboard-focused card (unanchored).
    fn key_target(&self) -> Option<(&Issue, Option<property_picker::Anchor>)> {
        if matches!(self.view, View::IssueDetail(_)) {
            let id = &self.issue_detail.as_ref()?.issue_id;
            return self.issues.iter().find(|i| i.id == *id).map(|i| (i, None));
        }
        if matches!(self.view, View::Board | View::MyIssues) {
            let hovered = self.hovered_card.as_ref().and_then(|h| {
                let on_board = self
                    .board_columns()
                    .into_iter()
                    .flat_map(|c| c.issues)
                    .any(|i| i.id == h.issue_id);
                on_board.then(|| (h.issue_id.clone(), h.rect))
            });
            if let Some((id, rect)) = hovered {
                return self
                    .issues
                    .iter()
                    .find(|i| i.id == id)
                    .map(|i| (i, Some(property_picker::Anchor { rect })));
            }
            return self.focused_issue().map(|i| (i, None));
        }
        None
    }

    /// Build the popover for `field` on `issue`, loading the candidates for
    /// the issue's own team (My Issues mixes teams).
    fn property_picker_state(
        &self,
        issue: &Issue,
        field: property_picker::Field,
        anchor: Option<property_picker::Anchor>,
    ) -> property_picker::PickerState {
        use property_picker::{Choice, Field};
        let conn = self.db();
        let team_id = issue.team_id.clone();
        let lanes = queries::lanes_for_team(&conn, &team_id).unwrap_or_default();
        let projects = queries::projects_for_team(&conn, &team_id).unwrap_or_default();
        let (labels, usage, current) = if field == Field::Labels {
            let labels = queries::labels_for_team(&conn, &team_id).unwrap_or_default();
            let mut usage: HashMap<String, usize> = HashMap::new();
            for (_, label) in queries::issue_labels_for_team(&conn, &team_id).unwrap_or_default() {
                *usage.entry(label.id).or_default() += 1;
            }
            let current = queries::label_ids_for_issue(&conn, &issue.id)
                .unwrap_or_default()
                .into_iter()
                .map(Choice::Label)
                .collect();
            (labels, usage, current)
        } else {
            let current = match field {
                Field::Status => Choice::Lane(issue.lane_id.clone()),
                Field::Priority => Choice::Priority(issue.priority),
                _ => Choice::Project(issue.project_id.clone()),
            };
            (Vec::new(), HashMap::new(), vec![current])
        };
        let ident = match self.teams.iter().find(|t| t.id == team_id) {
            Some(team) => format!("{}-{}", team.identifier, issue.team_number.unwrap_or(0)),
            None => format!("#{}", issue.team_number.unwrap_or(0)),
        };
        property_picker::PickerState::new(
            issue.id.clone(),
            team_id,
            field,
            property_picker::header(&ident, &issue.title),
            anchor,
            current,
            property_picker::Source {
                lanes: &lanes,
                projects: &projects,
                labels: &labels,
                label_usage: &usage,
            },
        )
    }

    /// Apply popover row `i`. Status, priority and project write and close;
    /// a label toggles (or is created and added) and the popover stays open.
    fn choose_property(&mut self, i: usize) -> Task<Message> {
        use property_picker::Choice;
        let Modal::PropertyPicker(ref state) = self.modal else {
            return Task::none();
        };
        let Some(choice) = state.items().get(i).map(|o| o.choice.clone()) else {
            return Task::none();
        };
        let issue_id = state.issue_id.clone();
        let team_id = state.team_id.clone();
        let label_id = match choice {
            Choice::Lane(id) => {
                self.modal = Modal::None;
                self.update_issue_by_id(&issue_id, |issue| issue.lane_id = id);
                return Task::none();
            }
            Choice::Priority(p) => {
                self.modal = Modal::None;
                self.update_issue_by_id(&issue_id, |issue| issue.priority = p);
                return Task::none();
            }
            Choice::Project(id) => {
                self.modal = Modal::None;
                self.update_issue_by_id(&issue_id, |issue| issue.project_id = id);
                return Task::none();
            }
            Choice::Label(id) => id,
            Choice::CreateLabel(name) => {
                // A random palette colour, like Rails' `randomColor`.
                let id = uuid::Uuid::new_v4();
                let palette = labels_view::PALETTE;
                let color = palette[id.as_bytes()[0] as usize % palette.len()];
                let now = crate::db::now_iso();
                let label = Label {
                    id: id.to_string(),
                    remote_id: None,
                    team_id: team_id.clone(),
                    name,
                    color: Some(color.to_string()),
                    created_at: now.clone(),
                    updated_at: now,
                };
                if queries::insert_label(&self.db(), &label).is_err() {
                    self.push_toast("Couldn't create the label", ToastKind::Error, false);
                    return Task::none();
                }
                self.enqueue_sync("label", &label.id, "create", None);
                label.id
            }
        };
        let Modal::PropertyPicker(ref mut state) = self.modal else {
            return Task::none();
        };
        let created = !state.options.iter().any(|o| o.choice == Choice::Label(label_id.clone()));
        state.toggle_label(&label_id);
        let ids = state.label_ids();
        let conn = self.db();
        if queries::edit_issue_queued(&conn, &issue_id, "update", None, |c| {
            queries::set_issue_labels(c, &issue_id, &ids)
        })
        .is_err()
        {
            self.push_toast("Couldn't save the labels", ToastKind::Error, false);
        }
        self.reload_data();
        if created {
            // Rebuild so the new label is listed (checked), keeping the anchor.
            if let Some(issue) = self.issues.iter().find(|i| i.id == issue_id).cloned() {
                if let Modal::PropertyPicker(ref old) = self.modal {
                    let mut state = self.property_picker_state(&issue, old.field, old.anchor);
                    state.in_sidebar = old.in_sidebar;
                    self.modal = Modal::PropertyPicker(state);
                }
            }
        }
        Task::none()
    }

    /// Mutate one issue, persist it and enqueue its sync.
    fn update_issue_by_id(&mut self, id: &str, mutate: impl FnOnce(&mut Issue)) {
        if let Some(mut issue) = self.issues.iter().find(|i| i.id == id).cloned() {
            mutate(&mut issue);
            issue.updated_at = crate::db::now_iso();
            let conn = self.db();
            let _ = queries::update_issue_queued(&conn, &issue);
            self.reload_data();
        }
    }

    /// Set (or clear) `issue_id`'s parent, persisting and enqueueing a push.
    /// A validation failure shows as an error toast and writes nothing.
    fn set_parent(&mut self, issue_id: &str, parent_id: Option<&str>) {
        let result = queries::set_issue_parent_queued(&self.db(), issue_id, parent_id);
        match result {
            Ok(_) => self.reload_data(),
            Err(e) => self.push_toast(e.to_string(), ToastKind::Error, false),
        }
    }

    /// Create a dependency edge from the open detail issue to `target_id`, in the
    /// active picker's direction. Persists locally, then enqueues a push (the
    /// sync consumer re-reads the row, so no payload is needed). A self-link or a
    /// duplicate edge (UNIQUE constraint) is surfaced as a toast, not a write.
    fn add_dependency(&mut self, target_id: &str) {
        let direction = match &self.issue_picker {
            Some(p) => p.direction,
            None => return,
        };
        let current_id = match &self.issue_detail {
            Some(d) => d.issue_id.clone(),
            None => return,
        };
        if current_id == target_id {
            self.push_toast("An issue can't depend on itself", ToastKind::Error, false);
            return;
        }
        let conn = self.db();
        // Rails allows one link per pair, in either order and of any kind.
        if queries::issue_dependency_pair_exists(&conn, &current_id, target_id).unwrap_or(false) {
            self.push_toast("Those issues are already linked", ToastKind::Error, false);
            return;
        }
        let (blocking_issue_id, blocked_issue_id, kind) =
            direction.stored_edge(&current_id, target_id);
        let now = crate::db::now_iso();
        let dep = IssueDependency {
            id: uuid::Uuid::new_v4().to_string(),
            remote_id: None,
            blocking_issue_id,
            blocked_issue_id,
            kind: kind.into(),
            direction: crate::api::dependencies::direction_for_edge(kind)
                .as_str()
                .into(),
            created_at: now.clone(),
            updated_at: now,
        };
        match queries::insert_issue_dependency_queued(&conn, &dep) {
            Ok(_) => {
                self.reload_data();
            }
            Err(_) => {
                // Almost always the UNIQUE(blocking, blocked) constraint.
                self.push_toast("Those issues are already linked", ToastKind::Error, false);
            }
        }
    }

    /// Move `issue_id` to `target_team_id`: apply the local move rules at once,
    /// queue `issue/move`, and toast what was cleared. Any open detail edits are
    /// flushed first so they are queued before the move. The moved issue leaves
    /// the current team's list, so an open detail for it is closed.
    fn move_issue_to_team(&mut self, issue_id: &str, target_team_id: &str) {
        self.commit_issue_detail();
        let conn = self.db();
        let Ok(Some(issue)) = queries::get_issue(&conn, issue_id) else {
            return;
        };
        match queries::move_issue_to_team(&conn, issue_id, target_team_id) {
            Ok(summary) => {
                let _ = queries::queue_issue_move(&conn, issue_id, &issue.team_id);
                let name = self
                    .teams
                    .iter()
                    .find(|t| t.id == target_team_id)
                    .map_or("team", |t| t.name.as_str())
                    .to_string();
                self.push_toast(summary.toast(&name), ToastKind::Info, false);
                if self.issue_detail.as_ref().is_some_and(|d| d.issue_id == issue_id) {
                    if self.back_to_dashboard() {
                        self.reload_data();
                        return;
                    }
                    let back = if self.detail_from_my_issues {
                        View::MyIssues
                    } else if self.detail_from_inbox {
                        View::Inbox
                    } else {
                        View::Board
                    };
                    self.detail_from_my_issues = false;
                    self.detail_from_inbox = false;
                    self.detail_from_dashboard = None;
                    self.issue_detail = None;
                    self.switch_view(back);
                }
                self.reload_data();
            }
            Err(e) => self.push_toast(format!("Move failed: {e}"), ToastKind::Error, false),
        }
    }

    /// Remove a dependency edge. Captures the remote ids the sync delete needs
    /// BEFORE dropping the local row (mirrors the issue-delete path). A
    /// never-synced edge (no `remote_id`) is deleted locally with nothing
    /// enqueued — there is nothing on the server to remove.
    fn remove_dependency(&mut self, dep_id: &str) {
        let conn = self.db();
        let _ = queries::delete_issue_dependency_queued(&conn, dep_id);
        self.reload_data();
    }

    /// Flush the buffered detail fields (title/description/estimate/due date) to
    /// the DB. Called on a text field's Enter and before navigating away. Blank
    /// title or non-numeric estimate set an inline error and skip the write
    /// rather than discard the user's text.
    fn commit_issue_detail(&mut self) {
        if !matches!(self.view, View::IssueDetail(_)) {
            return;
        }
        let conn = self.db();
        let Some(detail) = self.issue_detail.as_mut() else {
            return;
        };

        // Untouched buffers write nothing: they may be older than a pulled copy.
        if !detail.is_dirty() {
            return;
        }
        if detail.fields().title.is_empty() {
            detail.error = Some("Title can't be blank".into());
            return;
        }

        let Ok(Some(mut issue)) = queries::get_issue(&conn, &detail.issue_id) else {
            return;
        };
        match detail.apply_edits(&mut issue) {
            Err(e) => {
                detail.error = Some(e);
                return;
            }
            Ok(false) => return,
            Ok(true) => {}
        }
        detail.error = None;
        detail.mark_committed();
        issue.updated_at = crate::db::now_iso();
        let _ = queries::update_issue_queued(&conn, &issue);
        drop(conn);
        self.reload_data();
    }

    /// Switch the right pane to the detail view for `project`, flushing any open
    /// detail buffers first so unsaved edits aren't lost. The project's issues
    /// are already loaded for the selected team by `reload_data`, so there's no
    /// background fetch to kick off.
    fn open_project_detail(&mut self, project: &Project) -> Task<Message> {
        self.commit_issue_detail();
        self.commit_project_detail();
        if let Some(old) = self.project_detail.take() {
            self.drop_drafts(&[old.composer_files.comment_id, old.composer_files.reply_id]);
        }
        self.project_detail = Some(project_detail::ProjectDetailState::from_project(project));
        self.view = View::ProjectDetail(project.id.clone());
        self.composer_focused = false;
        let conn = self.db();
        self.load_detail_dependencies(&conn);
        self.fetch_project_comments_task(&conn, project)
    }

    /// Build the async task that fetches a project's comments (nested replies)
    /// once when it is opened. No-op unless the project + team are synced and a
    /// client can be built.
    fn fetch_project_comments_task(
        &self,
        conn: &rusqlite::Connection,
        project: &Project,
    ) -> Task<Message> {
        let Some(project_remote_id) = project.remote_id else {
            return Task::none();
        };
        let Ok(Some(team_remote_id)) = queries::remote_id_for(conn, "teams", &project.team_id)
        else {
            return Task::none();
        };
        let base_url = self.config.api_base_url.trim().to_string();
        let token = self.config.api_token.clone().unwrap_or_default();
        let Ok(client) = ApiClient::new(base_url, token) else {
            return Task::none();
        };
        let project_local_id = project.id.clone();
        Task::perform(
            async move {
                let result =
                    crate::api::comments::list_project(&client, team_remote_id, project_remote_id)
                        .await
                        .map_err(|e| e.to_string());
                (project_local_id, result)
            },
            |(id, result)| Message::ProjectCommentsFetched(id, result),
        )
    }

    /// Apply a live sidebar change (Status/Priority/Lead) to the project behind
    /// the open detail view: mutate, persist, enqueue sync, and reload so the
    /// pick_lists reflect DB truth. Buffered text fields are left untouched.
    fn apply_project_update(&mut self, mutate: impl FnOnce(&mut Project)) {
        let id = match &self.project_detail {
            Some(d) => d.project_id.clone(),
            None => return,
        };
        let conn = self.db();
        if let Ok(Some(mut project)) = queries::get_project(&conn, &id) {
            mutate(&mut project);
            project.updated_at = crate::db::now_iso();
            let _ = queries::update_project(&conn, &project);
            let payload = serde_json::to_string(&project).ok();
            self.enqueue_sync("project", &project.id, "update", payload.as_deref());
            self.reload_data();
        }
    }

    /// Flush the buffered project-detail fields (name/description/dates) to the
    /// DB. Called on a text field's Enter and before navigating away. A blank
    /// name sets an inline error and skips the write rather than discarding text.
    fn commit_project_detail(&mut self) {
        if !matches!(self.view, View::ProjectDetail(_)) {
            return;
        }
        let conn = self.db();
        let Some(detail) = self.project_detail.as_mut() else {
            return;
        };
        // Untouched buffers write nothing: they may be older than a pulled copy.
        if !detail.is_dirty() {
            return;
        }
        if detail.name.trim().is_empty() {
            detail.error = Some("Name can't be blank".into());
            return;
        }
        detail.error = None;

        let Ok(Some(mut project)) = queries::get_project(&conn, &detail.project_id) else {
            return;
        };
        if !detail.apply_edits(&mut project) {
            return;
        }
        detail.mark_committed();
        project.updated_at = crate::db::now_iso();
        let _ = queries::update_project(&conn, &project);
        drop(conn);
        let payload = serde_json::to_string(&project).ok();
        self.enqueue_sync("project", &project.id, "update", payload.as_deref());
        self.reload_data();
    }
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let (config, config_error) = config::load();
        let mut updater = crate::updater::machine::UpdaterState::new(
            semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("crate version is semver"),
        );
        config.updater.restore(&mut updater);
        let install = crate::updater::install::detect(&crate::updater::install::SystemProbe);
        updater.set_can_self_install(matches!(
            install,
            crate::updater::install::InstallContext::SelfInstall { .. }
        ));

        let conn = connection::open().expect("Failed to open database");
        // Skip the local "My Project" seed when the user has configured an API
        // token — the first sync will populate teams from Rails. Dev builds keep
        // the seed without a token so they stay usable offline; release builds
        // never seed, or the stray team would outlive the first sync.
        if cfg!(debug_assertions) && config.api_token.is_none() {
            connection::seed_default_data(&conn).expect("Failed to seed data");
        }

        let teams = queries::all_teams(&conn).unwrap_or_default();
        let saved_views = crate::db::saved_views::grouped_by_team(&conn).unwrap_or_default();
        let home_page = crate::db::home_page::get(&conn).unwrap_or_default();
        let sidebar = crate::db::sidebar_layout::load(&conn).unwrap_or_default();
        let dashboards = crate::db::dashboards::list(&conn).unwrap_or_default();
        // Open the stored Home page; anything unresolvable falls back to the
        // first team's board.
        let launch = crate::db::home_page::launch_target(
            home_page.as_ref(),
            &teams,
            &saved_views,
            &dashboards,
        );
        use crate::db::home_page::LaunchTarget;
        let (selected_team_id, launch_view, launch_dashboard) = match launch {
            LaunchTarget::Team(id) => (Some(id), None, None),
            LaunchTarget::View { view_id, team_id } => (Some(team_id), Some(view_id), None),
            LaunchTarget::Dashboard(id) => (default_team_id(&teams), None, Some(id)),
            LaunchTarget::Default => (default_team_id(&teams), None, None),
        };

        let comment_counts = selected_team_id
            .as_ref()
            .map(|tid| queries::comment_counts_for_team(&conn, tid).unwrap_or_default())
            .unwrap_or_default();
        let block_counts = selected_team_id
            .as_ref()
            .map(|tid| queries::block_counts_for_team(&conn, tid).unwrap_or_default())
            .unwrap_or_default();
        let (lanes, issues, projects, members, labels, issue_labels) =
            if let Some(ref tid) = selected_team_id {
                (
                    queries::lanes_for_team(&conn, tid).unwrap_or_default(),
                    queries::issues_for_team(&conn, tid).unwrap_or_default(),
                    queries::projects_for_team(&conn, tid).unwrap_or_default(),
                    queries::members_for_team(&conn, tid).unwrap_or_default(),
                    queries::labels_for_team(&conn, tid).unwrap_or_default(),
                    build_issue_labels_map(&conn, tid),
                )
            } else {
                (vec![], vec![], vec![], vec![], vec![], HashMap::new())
            };

        let pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);

        // Expand the boot-selected team so its sub-rows are visible on first paint.
        let expanded_teams = selected_team_id.iter().cloned().collect();

        let mut app = Self {
            teams,
            selected_team_id,
            view: View::Board,
            expanded_teams,
            team_menu_open: None,
            show_archived_teams: false,
            team_members: Vec::new(),
            lanes,
            issues,
            projects,
            members,
            labels,
            issue_labels,
            comment_counts,
            block_counts,
            detail_deps: Default::default(),
            map_deps: Vec::new(),
            map_flipped: HashSet::new(),
            board_deps: Vec::new(),
            detail_members: Vec::new(),
            detail_from_my_issues: false,
            detail_from_inbox: false,
            detail_from_dashboard: None,
            detail_from_page: None,
            map_return: None,
            detail_comments: Vec::new(),
            detail_comment_authors: Vec::new(),
            detail_activity: Vec::new(),
            detail_attachments: Vec::new(),
            detail_pending_paths: Default::default(),
            composer_focused: false,
            drop_buffer: Default::default(),
            images: Default::default(),
            detail_comment_attachments: Default::default(),
            detail_references: Vec::new(),
            detail_origin: None,
            issue_picker: None,
            focused_lane: 0,
            focused_issue: 0,
            hovered_card: None,
            collapsed_sections: HashSet::new(),
            scroll_state: HashMap::new(),
            modal: Modal::None,
            issue_form: None,
            date_picker: None,
            issue_detail: None,
            new_issue_scope: None,
            project_form_from_issue: false,
            project_detail: None,
            project_map: None,
            project_sort: ProjectSort::Created,
            project_sort_dir: SortDir::Asc,
            project_issue_sort: display::ProjectIssueSort::default(),
            project_issue_filter: display::ProjectIssueFilter::default(),
            hide_completed_projects: true,
            search_query: String::new(),
            search_view: View::Board,
            project_form: None,
            settings_form: None,
            label_form: None,
            csv_import: None,
            show_help: false,
            more_properties: false,
            pending_sync_count,
            config,
            config_error,
            last_sync_error: None,
            inaccessible_teams: queries::inaccessible_team_identifiers(&conn).unwrap_or_default(),
            sync_read_only: false,
            sync_in_progress: false,
            notif_unread_count: crate::db::notifications::unread_count(&conn).unwrap_or(0),
            saved_views,
            dashboards,
            dashboard_form: DashboardForm::default(),
            dashboard_menu: None,
            roadmap_ui: roadmap_view::RoadmapUi::default(),
            group_form: GroupForm::default(),
            dashboard_groups: Vec::new(),
            dashboard_header: Default::default(),
            collapsed_dashboard_groups: Default::default(),
            dashboard_sessions: Default::default(),
            tab_set: None,
            saved_tabs: None,
            restored_pages: HashMap::new(),
            saved_panels: None,
            restored_panels: HashMap::new(),
            window_width: 1100.0,
            window_height: 700.0,
            tab_bar_offset: 0.0,
            tab_drag: None,
            closed_tabs: Vec::new(),
            tab_projects: HashMap::new(),
            text_undo: Default::default(),
            terminals: {
                let mut terminals = crate::terminal::TerminalState::new();
                if let Ok(dirs) = connection::open().and_then(|c| crate::db::terminal_dirs::all(&c)) {
                    terminals.set_team_dirs(dirs);
                }
                if let Ok(dirs) = connection::open().and_then(|c| crate::db::terminal_dirs::all_projects(&c)) {
                    terminals.set_project_dirs(dirs);
                }
                match connection::open().and_then(|c| crate::db::terminal_records::all(&c)) {
                    Ok(stored) => terminals.restore_records(stored),
                    Err(e) => eprintln!("could not load Terminal records: {e}"),
                }
                terminals
            },
            parked_tabs: Default::default(),
            open_view: None,
            save_view_name: String::new(),
            save_view_error: None,
            save_view_from_open: false,
            home_page,
            sidebar,
            last_notif_poll: None,
            inbox: Default::default(),
            recurring_rows: Vec::new(),
            recurring_form: None,
            scheduler: Scheduler::with_notifications(std::time::Instant::now()),
            sync_next_delay: Some(crate::sync::scheduler::FOCUSED_INTERVAL),
            toasts: Vec::new(),
            body_hover: crate::body_hover::BodyHover::new(),
            hover_content: None,
            next_toast_id: 0,
            sync_paused_auth: false,
            updater,
            install,
            update_edge: std::sync::Arc::new(crate::updater::edge::HttpEdge::from_env()),
            restart_exiting: false,
        };

        // Drop partial downloads and unverified extracts from a past run, once,
        // before any download can take the update lock.
        if let Some(staging_dir) = app.install.staging_dir() {
            let _ = crate::updater::staging::cleanup_at_launch(
                &crate::updater::staging::RealStagingFs,
                staging_dir,
            );
            // A verified Staged update from a past run waits across launches.
            if let Some(version) =
                crate::updater::verify::restore_staged(staging_dir, app.updater.installed())
            {
                app.updater
                    .update(crate::updater::machine::Event::StagedRestored { version });
            }
        }

        app.terminals.set_font(crate::views::terminal_panel::TermFont::from_key(&app.config.terminal_font, app.config.terminal_font_size));
        app.terminals.set_dock(crate::terminal::panel::Dock::from_name(&app.config.terminal_dock));
        app.terminals.set_width(app.config.terminal_panel_width);
        app.terminals.set_sidebar_width(app.sidebar_width());

        // Restore the stored Tabs and focus the Home page's. Doing this before
        // anything reloads keeps `ensure_tab_set` from opening a stray team Tab.
        let restored = app.launch_tabs();
        if let Some(id) = app.selected_team_id.clone() {
            app.expanded_teams.insert(id);
        }
        let opened = match (launch_view, launch_dashboard) {
            (Some(view_id), _) => app.update_inner(Message::OpenSavedView(view_id)),
            (None, Some(id)) => app.update_inner(Message::OpenDashboard(id)),
            _ => Task::none(),
        };
        let task = Task::batch([restored, opened]);
        // A release build does nothing without a token, so ask for one up front.
        if !cfg!(debug_assertions) && app.config.api_token.is_none() {
            app.settings_form = Some(settings::SettingsFormState::from_config(&app.config));
            app.modal = Modal::Settings;
        }

        // The app delegate exists once the event loop is up; hook terminate then.
        let task = Task::batch([task, Task::done(Message::InstallTerminateHook)]);
        // Launch check, after the staging cleanup and restore above. The stored
        // auto-check setting was already applied by `config.updater.restore`.
        app.show_relaunch_toast();
        let launch_check = app.updater_background(updater_wiring::Trigger::Launched);
        let task = Task::batch([task, launch_check]);

        (app, task)
    }

    /// The single quit path for the close button and Cmd+Q. Today it just
    /// exits, as exit-on-close did. This is the hook for asking before quitting
    /// (JRST-410): return `Task::none()` to stay open, and send `iced::exit()`
    /// once the user confirms.
    fn quit(&mut self) -> Task<Message> {
        self.quit_with(false)
    }

    /// Runs once per quit, after the ask is answered (or skipped because
    /// nothing is busy) and before any Terminal is closed or waited on. Every
    /// quit path lands here: close button, Cmd+Q, Dock, AppleScript, and
    /// logout/restart/shutdown. Installs the Staged update (JRST-519) and
    /// drops an in-flight download. Keep it quick; the Terminal wait that
    /// follows can take up to `QUIT_WAIT`.
    fn on_quit_confirmed(&mut self) -> Task<Message> {
        self.install_on_quit()
    }

    /// Quit: ask first if any Terminal is busy (any team, shown or hidden),
    /// then close every Terminal and exit as soon as they have all gone,
    /// waiting at most `QUIT_WAIT` so a stuck process can't keep the app open.
    fn quit_with(&mut self, confirmed: bool) -> Task<Message> {
        use crate::terminal::panel::QuitPlan;
        // The restart already swapped and will exit; a pending terminate is
        // answered by `QuitReady` when the Terminals are gone.
        if crate::updater::swap::ignores_quit(self.restart_exiting) {
            return Task::none();
        }
        match self.terminals.quit_plan(confirmed) {
            QuitPlan::Ignore => Task::none(),
            QuitPlan::Ask => {
                self.modal = Modal::QuitConfirm;
                // A logout, restart or shutdown may find the app in the background.
                crate::quit::bring_to_front();
                Task::none()
            }
            QuitPlan::Shutdown => {
                self.modal = Modal::None;
                let update = self.on_quit_confirmed();
                self.shutdown_after(update)
            }
        }
    }

    /// Close every Terminal and exit once they have gone (at most `QUIT_WAIT`),
    /// running `update` alongside. Restart now calls this directly, after its
    /// own swap, so the quit hook doesn't swap a second time.
    pub(super) fn shutdown_after(&mut self, update: Task<Message>) -> Task<Message> {
        let closings = self.terminals.begin_shutdown();
        if closings.is_empty() {
            // Through `QuitReady`, not a bare `iced::exit()`: a pending Cmd+Q /
            // Dock / AppleScript terminate must be answered first, or iced frees
            // the window while AppKit is still in its terminate-later loop and
            // the leftover release segfaults.
            return Task::batch([update, Task::done(Message::QuitReady)]);
        }
        let (tx, rx) = iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            crate::terminal::wait_all(closings, QUIT_WAIT);
            let _ = tx.send(());
        });
        Task::batch([
            update,
            Task::perform(async move { let _ = rx.await; }, |_| Message::QuitReady),
        ])
    }

    pub fn title(&self) -> String {
        "JAIT — Issue Tracker".to_string()
    }

    /// Adopt `new_cfg` for this session and persist it, surfacing a save
    /// failure in the Settings form (and clearing a stale `config_error` on success).
    /// Width the left teams sidebar takes now: its fixed width, or nothing
    /// while collapsed (Cmd+B).
    pub(crate) fn sidebar_width(&self) -> f32 {
        if self.config.sidebar_collapsed {
            0.0
        } else {
            crate::terminal::panel::SIDEBAR_WIDTH
        }
    }

    fn save_config_change(&mut self, new_cfg: Config) {
        let saved = config::save(&new_cfg);
        self.config = new_cfg;
        if let Some(ref mut form) = self.settings_form {
            form.status = saved.as_ref().err().map(|e| format!("Error: {}", e));
        }
        if saved.is_ok() {
            self.config_error = None;
        }
    }

    pub fn theme(&self) -> Theme {
        t::iced_theme()
    }

    /// The window's clear color: the page background while opaque, and clear
    /// while translucent so only the base layers (see `theme::base`) tint it.
    pub fn style(&self, _theme: &Theme) -> iced::application::Appearance {
        iced::application::Appearance {
            background_color: if t::translucent() {
                iced::Color::TRANSPARENT
            } else {
                t::bg_primary()
            },
            text_color: t::text_primary(),
        }
    }

    /// Set the window's background blur from the open Settings form, or the
    /// config. No blur while the window is opaque: nothing would show it.
    fn apply_window_blur(&self) -> Task<Message> {
        let radius = self
            .settings_form
            .as_ref()
            .map_or(self.config.window_blur, |f| f.window_blur);
        let radius = if t::translucent() { radius } else { 0 };
        crate::window_effects::set_blur(radius)
    }

    /// What the confirm button of the open running-program dialog sends.
    fn confirm_accept_message(&self) -> Option<Message> {
        match &self.modal {
            Modal::QuitConfirm => Some(Message::QuitConfirmed),
            Modal::RestartConfirm(_) => Some(Message::RestartConfirmed),
            Modal::TabCloseConfirm(key) => Some(Message::Tab(TabMessage::CloseConfirmed(key.clone()))),
            Modal::TerminalCloseConfirm(id) => Some(Message::Terminal(
                crate::terminal::TerminalMessage::CloseConfirmed(*id),
            )),
            _ => None,
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let keys = event::listen_with(|event, status, _id| {
            if let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            {
                // A focused text field captures keystrokes; don't also treat them
                // as shortcuts. Escape still passes through so it closes modals.
                let escape = key == keyboard::Key::Named(keyboard::key::Named::Escape);
                if status == event::Status::Captured && !escape {
                    // Cmd+Z / Cmd+Shift+Z, which iced inputs capture
                    // without acting on (they have no undo).
                    return crate::undo_input::key_action(&key, modifiers).map(Message::TextKey);
                }
                Some(Message::KeyPressed(key, modifiers))
            } else {
                None
            }
        });
        // Any key press (even one a text field captured), click or scroll
        // counts as activity for idle detection; window focus drives cadence.
        let activity = event::listen_with(|event, _status, _id| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed { .. })
            | iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_))
            | iced::Event::Mouse(iced::mouse::Event::WheelScrolled { .. }) => {
                Some(Message::UserInput)
            }
            iced::Event::Window(iced::window::Event::FileDropped(path)) => {
                Some(Message::FileDropped(path))
            }
            iced::Event::Window(iced::window::Event::Focused) => Some(Message::WindowFocused),
            iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::WindowUnfocused),
            _ => None,
        });

        // Scrolling moves content out from under a card, so it closes.
        let scrolls = event::listen_with(|event, _status, _id| match event {
            iced::Event::Mouse(iced::mouse::Event::WheelScrolled { .. }) => {
                Some(Message::HoverScrolled)
            }
            _ => None,
        });

        let banner_clicks = crate::sync::banner::clicks().map(Message::BannerClicked);
        let resizes = iced::window::resize_events().map(|(_, size)| Message::WindowResized(size));
        let close_requests = iced::window::close_requests().map(|_| Message::QuitRequested);
        let opens = iced::window::open_events().map(|_| Message::WindowOpened);

        // While a Terminal has focus its key listener replaces the shortcuts.
        let modal_open = self.modal != Modal::None;
        let keys = if self.terminals.takes_keys(modal_open) {
            Subscription::none()
        } else {
            keys
        };
        let mut subs = vec![
            keys,
            activity,
            banner_clicks,
            close_requests,
            // Cmd+Q, Dock Quit and AppleScript quit (see `quit.rs`).
            Subscription::run(crate::quit::terminate_requests).map(|_| Message::QuitRequested),
            opens,
            resizes,
            self.terminals.subscription(modal_open),
            self.updater_subscription(),
        ];
        // A pressed Tab follows the pointer until release.
        if self.tab_drag.is_some() {
            subs.push(event::listen_with(|e, _status, _id| match e {
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::Tab(TabMessage::DragMove(position.x)))
                }
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(_)) => {
                    Some(Message::Tab(TabMessage::DragEnd))
                }
                _ => None,
            }));
        }
        // A pressed roadmap card follows the pointer until release.
        if self.roadmap_ui.drag.is_some() {
            subs.push(event::listen_with(|e, _status, _id| match e {
                iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Some(Message::RoadmapDragMove(position))
                }
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(_)) => {
                    Some(Message::RoadmapDragEnd)
                }
                _ => None,
            }));
        }
        // Only listen for wheel events while a card is pending or showing.
        if self.body_hover.is_active() {
            subs.push(scrolls);
        }
        // The scheduler picks the delay; the timer restarts whenever it changes.
        if let Some(delay) = self.sync_next_delay {
            subs.push(
                iced::time::every(delay.max(std::time::Duration::from_millis(250)))
                    .map(|_| Message::SyncTick),
            );
        }
        // Only run the 1s toast-expiry timer while toasts are on screen.
        if !self.toasts.is_empty() {
            subs.push(
                iced::time::every(std::time::Duration::from_secs(1)).map(|_| Message::ToastTick),
            );
        }
        Subscription::batch(subs)
    }

    /// Turn a hover-card effect into the delayed message the machine asked for.
    fn hover_timer(effect: Option<crate::hover_card::Effect>) -> Task<Message> {
        match effect {
            Some(e) => Task::perform(tokio::time::sleep(e.delay), move |_| {
                Message::HoverTimer(e.generation)
            }),
            None => Task::none(),
        }
    }

    /// Feed the sync scheduler an event, re-arm its timer, and start the cycle
    /// it asks for. Local writes arrive here as `LocalWrite` (see `update`).
    fn drive_sync(&mut self, event: SchedEvent) -> Task<Message> {
        let decision = self.scheduler.handle(event, std::time::Instant::now());
        self.sync_next_delay = decision.next_delay;
        match decision.action() {
            Some(Run::Data(kind)) => self.start_sync(kind),
            Some(Run::Poll) => self.start_poll(),
            None => Task::none(),
        }
    }

    /// Start a Notification poll the scheduler already admitted. Reports
    /// `CycleFinished` through `NotifPollCompleted` (or at once, if no client).
    fn start_poll(&mut self) -> Task<Message> {
        let base_url = self.config.api_base_url.trim().to_string();
        let token = self.config.api_token.clone().unwrap_or_default();
        match ApiClient::new(base_url, token) {
            Ok(client) => {
                self.sync_in_progress = true;
                Task::perform(
                    async move { notif_poll::poll(&client).await },
                    Message::NotifPollCompleted,
                )
            }
            Err(_) => self.drive_sync(SchedEvent::CycleFinished { success: false }),
        }
    }

    /// Start a sync cycle the scheduler already admitted. If it can't start
    /// (no token), report the cycle as failed so the scheduler backs off.
    fn start_sync(&mut self, kind: CycleKind) -> Task<Message> {
        let base_url = self.config.api_base_url.trim().to_string();
        let token = self.config.api_token.clone().unwrap_or_default();
        match ApiClient::new(base_url, token) {
            Ok(client) => {
                self.sync_in_progress = true;
                self.last_sync_error = None;
                return match kind {
                    CycleKind::Full => Task::perform(
                        async move { SyncEngine::full_cycle(&client).await },
                        Message::SyncCompleted,
                    ),
                    CycleKind::PushOnly => Task::perform(
                        async move { SyncEngine::push_cycle(&client).await },
                        Message::SyncCompleted,
                    ),
                };
            }
            Err(ApiError::MissingToken) => {
                self.last_sync_error = Some("set an API token in Settings".into());
            }
            Err(e) => {
                self.last_sync_error = Some(e.to_string());
            }
        }
        self.pending_sync_count = sync_queue::pending_count(&self.db()).unwrap_or(0);
        self.drive_sync(SchedEvent::CycleFinished { success: false })
    }

    /// Cached attachment/inline files are only served while an API token is
    /// configured (spec story 68).
    fn files_unlocked(&self) -> bool {
        self.config.api_token.as_deref().is_some_and(|t| !t.trim().is_empty())
    }

    /// The token lost access: delete the file cache (never the outbox) and
    /// forget what is on screen, so cached files are no longer reachable.
    fn lock_files(&mut self) {
        let _ = crate::files::cache::FileCache::at_default_root().clear(&self.db());
        self.images.clear();
    }

    /// The existing 401 path: pause sync and ask for a new token.
    fn pause_for_auth(&mut self) {
        self.lock_files();
        self.sync_paused_auth = true;
        let msg = "Token expired or revoked — please re-enter";
        if self.settings_form.is_none() {
            self.settings_form = Some(settings::SettingsFormState::from_config(&self.config));
        }
        if let Some(ref mut f) = self.settings_form {
            f.status = Some(format!("Error: {msg}"));
        }
        self.modal = Modal::Settings;
        self.push_toast(msg, ToastKind::Error, false);
    }

    /// Apply a detail-view key message. Leaving a description editor is
    /// skipped when another editor (the comment composer) has focus, so
    /// Escape there doesn't also close the description. Focus is read with
    /// an operation, then the stop is sent.
    fn update_unless_focus_elsewhere(&mut self, msg: Message) -> Task<Message> {
        let (editor, stop): (_, fn() -> Message) = match msg {
            Message::StopEditingIssueDescription => {
                (issue_detail::description_editor_id(), || Message::StopEditingIssueDescription)
            }
            Message::StopEditingProjectDescription => {
                (project_detail::description_editor_id(), || Message::StopEditingProjectDescription)
            }
            other => return self.update(other),
        };
        iced::advanced::widget::operate(issue_detail::FocusedOutside::new(editor)).then(move |elsewhere| {
            if elsewhere { Task::none() } else { Task::done(stop()) }
        })
    }

    /// Whether either detail view has its description editor open.
    fn editing_description(&self) -> bool {
        self.issue_detail.as_ref().is_some_and(|d| d.editing_description)
            || self.project_detail.as_ref().is_some_and(|d| d.editing_description)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        // Discard stale signals so only writes made by this message count.
        sync_queue::take_local_write();
        let view_before = self.view.clone();
        let editing_before = self.editing_description();
        let tabs_before = self.tab_position();
        // A form page is left by going elsewhere, like a web page: its draft
        // is dropped (JRST-454).
        let form_page_before = self.form_page_open();
        let tab_before = self.active_tab_key().cloned();
        let saved_view_before = self.open_view.as_ref().map(|v| v.view_id.clone());
        let team_nav = matches!(message, Message::SelectTeamView(..));
        let mut task = self.update_inner(message);
        if form_page_before
            && self.form_page_open()
            && (team_nav
                || self.view != view_before
                || self.active_tab_key() != tab_before.as_ref()
                || self.open_view.as_ref().map(|v| v.view_id.as_str()) != saved_view_before.as_deref())
        {
            self.close_form_page();
        }
        task = Task::batch([task, self.reveal_active_tab(tabs_before)]);
        // The one place that closes hover cards on a view change, modal open
        // or description editor toggle, whatever message caused it (keyboard
        // included).
        if self.view != view_before
            || self.modal != Modal::None
            || self.editing_description() != editing_before
        {
            let effect = self.body_hover.event(crate::hover_card::Event::ViewChanged);
            task = Task::batch([task, Self::hover_timer(effect)]);
        }
        if self.view != view_before {
            self.hovered_card = None;
        }
        self.stash_dashboard_session();
        self.retain_open_view();
        self.discard_dead_composers();
        self.save_tabs();
        self.save_terminal_records();
        self.save_panel();
        self.save_panel_width();
        let panel_scope = self.panel_scope();
        self.terminals.set_pinned(self.removed_team_tab());
        self.terminals.sync(panel_scope.as_ref());
        let task = Task::batch([task, self.request_visible_images()]);
        if sync_queue::take_local_write() && self.config.api_token.is_some() {
            return Task::batch([task, self.drive_sync(SchedEvent::LocalWrite)]);
        }
        task
    }

    /// Store the right-docked panel's width once a drag ends. Device-wide.
    fn save_panel_width(&mut self) {
        let width = self.terminals.width();
        if self.terminals.dragging() || width == self.config.terminal_panel_width {
            return;
        }
        let mut new_cfg = self.config.clone();
        new_cfg.terminal_panel_width = width;
        self.save_config_change(new_cfg);
    }

    /// Store the panel when it changed since the last message. Local only.
    /// Post the banners the Terminals queued: only while the window is in
    /// the background (in front, the Tab and strip dots say it) and banners
    /// are on.
    fn post_terminal_alerts(&mut self) {
        let alerts = self.terminals.take_alerts();
        if alerts.is_empty() || self.scheduler.is_focused() || !self.config.native_notifications {
            return;
        }
        crate::sync::banner::post(
            alerts
                .into_iter()
                .map(|a| crate::db::notifications::Banner {
                    title: a.title,
                    body: a.body,
                    target: crate::db::notifications::BannerTarget::Terminal(a.id.0),
                })
                .collect(),
        );
    }

    /// Open the Tab whose strip has Terminal `id`, then show it and give it
    /// the keyboard. A team Tab on a project's page goes back to the Board,
    /// whose strip is the team's own.
    fn reveal_terminal(&mut self, id: crate::terminal::TerminalId) -> Task<Message> {
        let Some(scope) = self.terminals.scope_of(id) else {
            return Task::none();
        };
        let task = match &scope.project {
            Some(project) => self.update(Message::OpenProjectDetail(project.clone())),
            None => {
                let task = self.focus_team_tab(&scope.team);
                if self.panel_scope().as_ref() == Some(&scope) {
                    task
                } else {
                    Task::batch([task, self.update(Message::SelectTeamView(scope.team.clone(), View::Board))])
                }
            }
        };
        let now = self.panel_scope();
        self.terminals.reveal(id, now.as_ref());
        task
    }

    fn save_panel(&mut self) {
        let Some(set) = &self.tab_set else {
            return;
        };
        if self.terminals.dragging() {
            return;
        }
        let rows = crate::tab_state::panel_rows(
            set,
            &self.terminals.stored_panel(),
            &self.parked_tabs,
            &self.restored_panels,
        );
        if self.saved_panels.as_ref() == Some(&rows) {
            return;
        }
        match crate::db::panel_states::save(&self.db(), &rows) {
            Ok(()) => self.saved_panels = Some(rows),
            Err(e) => eprintln!("could not save panel state: {e}"),
        }
    }

    /// Store what changed in the Terminal strips since the last message.
    fn save_terminal_records(&mut self) {
        for note in self.terminals.take_notices() {
            self.push_toast(note, ToastKind::Error, false);
        }
        let changes = self.terminals.take_record_changes();
        if changes.is_empty() {
            return;
        }
        if let Err(e) = crate::db::terminal_records::apply(&self.db(), &changes) {
            eprintln!("could not save Terminal records: {e}");
        }
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        // Cross-team pages span several teams: lane moves need a single team's
        // lanes, so they're no-ops here, and so is create on a Dashboard. My
        // Issues creates in the most recently active team Tab's team.
        // Priority and delete act on the issue's own row/team, so they work
        // cross-team.
        if self.view.is_cross_team()
            && (matches!(
                message,
                Message::MoveIssueNextLane | Message::MoveIssuePrevLane
            ) || (matches!(message, Message::CreateIssue) && self.view != View::MyIssues))
        {
            return Task::none();
        }
        // A removed team's Tab holds only its Terminals: no board or issue
        // action may act on the team that is gone.
        if tab_update::blocked_on_removed_team(&message) && self.removed_team_tab() {
            return Task::none();
        }
        match message {
            Message::TextInputEdited(edit) => {
                let message = self.text_undo.record(edit);
                return self.update(message);
            }
            Message::TextKey(action) => {
                let keys = self.text_undo.keys();
                if keys.is_empty() {
                    return Task::none();
                }
                return crate::undo_input::focused(keys)
                    .map(move |key| Message::TextKeyOn { key, action });
            }
            Message::TextKeyOn { key: Some(key), action } => {
                let shown = crate::undo_input::shown(&key);
                let redo = action == crate::undo_input::KeyAction::Redo;
                let Some((message, restored)) = self.text_undo.undo(&key, redo, shown.as_deref())
                else {
                    return Task::none();
                };
                let cursor = crate::undo_input::move_cursor(key, &restored.text, restored.cursor);
                return self.update(message).chain(cursor);
            }
            Message::TextKeyOn { key: None, .. } => return Task::none(),
            Message::KeyPressed(key, modifiers) => {
                // Esc in the strip's rename field cancels the rename.
                if key == keyboard::Key::Named(keyboard::key::Named::Escape)
                    && self.modal == Modal::None
                    && self.terminals.is_renaming()
                {
                    return self.update(Message::Terminal(
                        crate::terminal::TerminalMessage::RenameCancel,
                    ));
                }
                // Esc with a hover card open only closes the card.
                if key == keyboard::Key::Named(keyboard::key::Named::Escape)
                    && self.body_hover.is_open()
                {
                    let effect = self.body_hover.event(crate::hover_card::Event::Escape);
                    return Self::hover_timer(effect);
                }
                // Esc with an autocomplete list open only closes the list.
                if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                    if let Some(slot) = self.open_suggest_mut() {
                        if slot.take().is_some() {
                            return Task::none();
                        }
                    }
                }
                if self.modal == Modal::IssuePicker
                    && self
                        .issue_picker
                        .as_ref()
                        .is_some_and(|p| p.mode == issue_picker::PickerMode::Dependency)
                    && key == keyboard::Key::Named(keyboard::key::Named::Tab)
                {
                    return self.update(Message::CycleDependencyKind(!modifiers.shift()));
                }
                if let Modal::PropertyPicker(_) = self.modal {
                    if let Some(msg) = keys::handle_property_picker_key(&key, modifiers) {
                        return self.update(msg);
                    }
                }
                // Enter accepts a running-program confirm; Esc cancels it below.
                if key == keyboard::Key::Named(keyboard::key::Named::Enter) && modifiers.is_empty() {
                    if let Some(msg) = self.confirm_accept_message() {
                        return self.update(msg);
                    }
                }
                if let Modal::Lightbox(_) = self.modal {
                    if let Some(msg) = keys::handle_lightbox_key(&key, modifiers) {
                        return self.update(msg);
                    }
                }
                let modal_open = self.modal != Modal::None;
                let palette_open = matches!(self.modal, Modal::CommandPalette(_));
                let on_board = matches!(self.view, View::Board | View::MyIssues);
                if self.view == View::Inbox && !modal_open {
                    if let Some(msg) = keys::handle_inbox_key(&key, modifiers) {
                        return self.update(msg);
                    }
                }
                // Escape on the dependency map returns to its issue.
                if matches!(self.view, View::DependencyMap { .. })
                    && !modal_open
                    && key == keyboard::Key::Named(keyboard::key::Named::Escape)
                {
                    return self.update(Message::CloseDependencyMap);
                }
                if matches!(self.view, View::ProjectDependencyMap { .. })
                    && !modal_open
                    && key == keyboard::Key::Named(keyboard::key::Named::Escape)
                {
                    return self.update(Message::CloseProjectDependencyMap);
                }
                if matches!(self.view, View::IssueDetail(_)) && !modal_open {
                    let editing_description =
                        self.issue_detail.as_ref().is_some_and(|d| d.editing_description);
                    if let Some(msg) = keys::handle_issue_detail_key(&key, modifiers, editing_description) {
                        return self.update_unless_focus_elsewhere(msg);
                    }
                }
                if matches!(self.view, View::ProjectDetail(_)) && !modal_open {
                    let editing_description =
                        self.project_detail.as_ref().is_some_and(|d| d.editing_description);
                    if let Some(msg) = keys::handle_project_detail_key(&key, modifiers, editing_description) {
                        return self.update_unless_focus_elsewhere(msg);
                    }
                }
                if matches!(self.view, View::Dashboard(_)) && !modal_open {
                    if let Some(msg) = keys::handle_dashboard_key(&key, modifiers) {
                        return self.update(msg);
                    }
                }
                if matches!(self.view, View::Dashboard(_))
                    && !modal_open
                    && modifiers.shift()
                    && !modifiers.command()
                    && matches!(&key, iced::keyboard::Key::Character(c) if c.as_str().eq_ignore_ascii_case("c"))
                {
                    return self.update(Message::ToggleFocusedSection);
                }
                if let Some(msg) = keys::handle_key_press(key, modifiers, modal_open, palette_open, on_board) {
                    return self.update(msg);
                }
            }

            Message::ToggleTeamExpanded(team_id) => {
                if self.expanded_teams.remove(&team_id) {
                    // The menu hangs off the expanded header; collapse closes it.
                    if self.team_menu_open.as_deref() == Some(team_id.as_str()) {
                        self.team_menu_open = None;
                    }
                } else {
                    self.expanded_teams.insert(team_id);
                }
            }

            Message::SelectTeamRow(team_id) => {
                self.expanded_teams.insert(team_id.clone());
                // Focus the Tab as it was left. A team's Tab never shows a
                // cross-team page (those are Tabs of their own), so there is
                // always something of the team's to return to.
                let task = self.focus_team_tab(&team_id);
                // Open Terminals are why a click may need to show the panel:
                // this row is the way back to shells with no Tab open.
                self.terminals.reveal_for_click(&team_id);
                return task;
            }

            Message::OpenSavedView(view_id) => {
                let found = self
                    .saved_views
                    .values()
                    .flatten()
                    .find(|v| v.id == view_id)
                    .cloned();
                let Some(sv) = found else {
                    return Task::none();
                };
                // Switch to the view's team Tab first when needed (this
                // replaces any view open there).
                let task = if self.selected_team_id.as_deref() != Some(sv.team_id.as_str())
                    || self.view != View::Board
                {
                    self.expanded_teams.insert(sv.team_id.clone());
                    self.update_inner(Message::SelectTeamView(sv.team_id.clone(), View::Board))
                } else {
                    Task::none()
                };
                // The team Tab's own board search, read once that Tab is live
                // (the Tab that was active may be another team's).
                let board_search = if self.open_view.is_none() && self.search_view == View::Board {
                    self.search_query.clone()
                } else {
                    String::new()
                };
                let local = crate::saved_view::BoardState {
                    display: self
                        .config
                        .display_for(&config::BoardScope::Team(sv.team_id.clone()))
                        .clone(),
                    filters: self
                        .config
                        .filters_for(&config::BoardScope::Team(sv.team_id.clone()))
                        .clone(),
                    search: board_search,
                };
                let mirror = crate::saved_view::Mirror {
                    lanes: &self.lanes,
                    members: &self.members,
                    labels: &self.labels,
                    projects: &self.projects,
                };
                self.open_view = Some(crate::saved_view::OpenView::open(&sv, &mirror, &local));
                self.scroll_state.clear();
                self.focused_lane = 0;
                self.focused_issue = 0;
                return task;
            }

            Message::OpenSaveView => {
                if self.save_view_block_reason().is_none() {
                    self.save_view_name.clear();
                    self.save_view_error = None;
                    self.save_view_from_open = false;
                    self.modal = Modal::SaveView;
                    return text_input::focus(save_view_input_id());
                }
            }

            Message::OpenSaveAsNewView => {
                if self.save_view_block_reason().is_none() && self.open_view.is_some() {
                    self.save_view_name.clear();
                    self.save_view_error = None;
                    self.save_view_from_open = true;
                    self.modal = Modal::SaveView;
                    return text_input::focus(save_view_input_id());
                }
            }

            Message::SaveViewNameChanged(name) => {
                self.save_view_name = name;
                self.save_view_error = None;
            }

            Message::OpenNewDashboard => {
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't create a dashboard",
                        ToastKind::Error,
                        false,
                    );
                } else {
                    self.dashboard_form = DashboardForm::default();
                    self.modal = Modal::NewDashboard;
                    return text_input::focus(dashboard_name_input_id());
                }
            }

            Message::OpenEditDashboard(id) => {
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't edit a dashboard",
                        ToastKind::Error,
                        false,
                    );
                } else if let Ok(Some(d)) = crate::db::dashboards::get(&self.db(), &id) {
                    self.dashboard_form = DashboardForm {
                        editing: Some(d.id),
                        name: d.name,
                        description: d.description,
                        error: None,
                    };
                    self.modal = Modal::EditDashboard;
                    return text_input::focus(dashboard_name_input_id());
                }
            }

            Message::EditDashboardSubmit => {
                if self.modal != Modal::EditDashboard {
                    return Task::none();
                }
                let Some(id) = self.dashboard_form.editing.clone() else {
                    return Task::none();
                };
                if self.sync_read_only {
                    self.modal = Modal::None;
                    self.push_toast(
                        "Read-only token: can't edit a dashboard",
                        ToastKind::Error,
                        false,
                    );
                    return Task::none();
                }
                match crate::db::dashboards::update_local(
                    &self.db(),
                    &id,
                    &self.dashboard_form.name,
                    &self.dashboard_form.description,
                ) {
                    Ok(_) => {
                        self.modal = Modal::None;
                        self.dashboard_form = DashboardForm::default();
                        self.reload_data();
                    }
                    Err(crate::db::dashboards::CreateError::Name(e)) => {
                        self.dashboard_form.error = Some(e.message());
                    }
                    Err(crate::db::dashboards::CreateError::Db(e)) => {
                        eprintln!("[dashboards] update failed: {e}");
                        self.dashboard_form.error = Some("Couldn't save the dashboard");
                    }
                }
            }

            Message::NewDashboardNameChanged(name) => {
                self.dashboard_form.name = name;
                self.dashboard_form.error = None;
            }

            Message::NewDashboardDescriptionChanged(desc) => {
                self.dashboard_form.description = desc;
            }

            Message::NewDashboardSubmit => {
                if self.modal != Modal::NewDashboard {
                    return Task::none();
                }
                if self.sync_read_only {
                    self.modal = Modal::None;
                    self.push_toast(
                        "Read-only token: can't create a dashboard",
                        ToastKind::Error,
                        false,
                    );
                    return Task::none();
                }
                match crate::db::dashboards::create_local(
                    &self.db(),
                    &self.dashboard_form.name,
                    &self.dashboard_form.description,
                ) {
                    Ok(d) => {
                        self.modal = Modal::None;
                        self.dashboard_form = DashboardForm::default();
                        self.reload_data();
                        return self.update_inner(Message::OpenDashboard(d.id));
                    }
                    Err(crate::db::dashboards::CreateError::Name(e)) => {
                        self.dashboard_form.error = Some(e.message());
                    }
                    Err(crate::db::dashboards::CreateError::Db(e)) => {
                        eprintln!("[dashboards] create failed: {e}");
                        self.dashboard_form.error = Some("Couldn't create the dashboard");
                    }
                }
            }

            Message::OpenNewGroup => {
                if !matches!(self.view, View::Dashboard(_)) {
                    return Task::none();
                }
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't add a group",
                        ToastKind::Error,
                        false,
                    );
                } else {
                    self.group_form = GroupForm::default();
                    self.modal = Modal::NewGroup;
                    return text_input::focus(crate::views::group_form::name_input_id());
                }
            }

            Message::OpenEditGroup(group_id) => {
                if !matches!(self.view, View::Dashboard(_)) {
                    return Task::none();
                }
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't edit a group",
                        ToastKind::Error,
                        false,
                    );
                    return Task::none();
                }
                match crate::db::dashboards::group_draft(&self.db(), &group_id) {
                    Ok(Some(draft)) => {
                        self.group_form = GroupForm { draft, error: None };
                        self.modal = Modal::EditGroup(group_id);
                        return text_input::focus(crate::views::group_form::name_input_id());
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("[dashboards] group read failed: {e}"),
                }
            }

            Message::GroupNameChanged(name) => {
                self.group_form.draft.name = name;
                self.group_form.error = None;
            }

            Message::GroupDescriptionChanged(desc) => {
                self.group_form.draft.description = desc;
            }

            Message::GroupColorPicked(hex) => {
                self.group_form.draft.color = hex;
            }

            Message::GroupToggleSource(kind, remote_id) => {
                self.group_form.draft.toggle_source(kind, remote_id);
            }

            Message::GroupSetIncludeAll(kind, remote_id, on) => {
                self.group_form.draft.set_include_all(kind, remote_id, on);
            }

            Message::GroupSubmit => {
                use crate::db::dashboards::{create_group_local, update_group_local, GroupError};
                let editing = match &self.modal {
                    Modal::NewGroup => None,
                    Modal::EditGroup(id) => Some(id.clone()),
                    _ => return Task::none(),
                };
                let View::Dashboard(dashboard_id) = self.view.clone() else {
                    return Task::none();
                };
                if self.sync_read_only {
                    self.modal = Modal::None;
                    self.push_toast(
                        "Read-only token: can't change a group",
                        ToastKind::Error,
                        false,
                    );
                    return Task::none();
                }
                let saved = match &editing {
                    Some(id) => update_group_local(&self.db(), id, &self.group_form.draft),
                    None => create_group_local(&self.db(), &dashboard_id, &self.group_form.draft),
                };
                match saved {
                    Ok(_) => {
                        self.modal = Modal::None;
                        self.group_form = GroupForm::default();
                        self.reload_data();
                    }
                    Err(GroupError::Name(e)) => self.group_form.error = Some(e.message()),
                    Err(GroupError::NoDashboard) => {
                        self.group_form.error = Some("This dashboard no longer exists");
                    }
                    Err(GroupError::NoGroup) => {
                        self.group_form.error = Some("This group no longer exists");
                    }
                    Err(GroupError::Db(e)) => {
                        eprintln!("[dashboards] group create failed: {e}");
                        self.group_form.error = Some("Couldn't add the group");
                    }
                }
            }

            Message::OpenRenameSavedView(view_id) => {
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't rename a view",
                        ToastKind::Error,
                        false,
                    );
                } else if let Some(sv) = self.saved_views.values().flatten().find(|v| v.id == view_id)
                {
                    self.save_view_name = sv.name.clone();
                    self.save_view_error = None;
                    self.modal = Modal::RenameSavedView(view_id);
                    return text_input::focus(save_view_input_id());
                }
            }

            Message::SaveViewSubmit => {
                if let Modal::RenameSavedView(view_id) = self.modal.clone() {
                    if self.sync_read_only {
                        self.modal = Modal::None;
                        self.push_toast(
                            "Read-only token: can't rename a view",
                            ToastKind::Error,
                            false,
                        );
                        return Task::none();
                    }
                    let name = match crate::saved_view::validate_name(&self.save_view_name) {
                        Ok(n) => n,
                        Err(e) => {
                            self.save_view_error = Some(e.message());
                            return Task::none();
                        }
                    };
                    match crate::db::saved_views::rename_local(&self.db(), &view_id, &name) {
                        Ok(true) => {
                            self.enqueue_sync(
                                "saved_view",
                                &view_id,
                                "rename",
                                None,
                            );
                            self.modal = Modal::None;
                            self.save_view_name.clear();
                            self.reload_data();
                        }
                        Ok(false) => self.modal = Modal::None,
                        Err(e) => eprintln!("[saved_views] rename failed: {e}"),
                    }
                    return Task::none();
                }
                if self.modal != Modal::SaveView {
                    return Task::none();
                }
                if let Some(reason) = self.save_view_block_reason() {
                    self.modal = Modal::None;
                    self.push_toast(reason, ToastKind::Error, false);
                    return Task::none();
                }
                let name = match crate::saved_view::validate_name(&self.save_view_name) {
                    Ok(n) => n,
                    Err(e) => {
                        self.save_view_error = Some(e.message());
                        return Task::none();
                    }
                };
                let Some(team_id) = self.selected_team_id.clone() else {
                    return Task::none();
                };
                let state = crate::saved_view::BoardState {
                    display: self.board_display().clone(),
                    filters: self.board_filters().clone(),
                    search: self.active_search().to_string(),
                };
                let mirror = crate::saved_view::Mirror {
                    lanes: &self.lanes,
                    members: &self.members,
                    labels: &self.labels,
                    projects: &self.projects,
                };
                let query = match (&self.open_view, self.save_view_from_open) {
                    (Some(v), true) => v.encoded(&mirror),
                    _ => crate::saved_view::new_view_query(&state, &mirror),
                };
                let dropped_unassigned = crate::saved_view::has_unassigned(&state);
                let query_json = serde_json::to_string(&query).unwrap_or_else(|_| "{}".into());
                match crate::db::saved_views::create_local(&self.db(), &team_id, &name, &query_json)
                {
                    Ok(sv) => {
                        self.modal = Modal::None;
                        self.save_view_name.clear();
                        if dropped_unassigned {
                            self.push_toast(
                                "Unassigned isn't saved: Rails can't store it in a view",
                                ToastKind::Info,
                                false,
                            );
                        }
                        self.reload_data();
                        return self.update_inner(Message::OpenSavedView(sv.id));
                    }
                    Err(e) => {
                        eprintln!("[saved_views] create failed: {e}");
                        self.save_view_error = Some("Couldn't save the view");
                    }
                }
            }

            Message::UpdateSavedView => {
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't update a view",
                        ToastKind::Error,
                        false,
                    );
                    return Task::none();
                }
                let mirror = crate::saved_view::Mirror {
                    lanes: &self.lanes,
                    members: &self.members,
                    labels: &self.labels,
                    projects: &self.projects,
                };
                let Some(v) = &mut self.open_view else {
                    return Task::none();
                };
                if !v.is_modified(&mirror) {
                    return Task::none();
                }
                v.mark_saved(&mirror);
                let (view_id, query) = (v.view_id.clone(), v.raw.clone());
                match crate::db::saved_views::update_query_local(&self.db(), &view_id, &query) {
                    Ok(true) => {
                        self.enqueue_sync("saved_view", &view_id, "update", None);
                        self.saved_views =
                            crate::db::saved_views::grouped_by_team(&self.db()).unwrap_or_default();
                    }
                    Ok(false) => {}
                    Err(e) => eprintln!("[saved_views] update failed: {e}"),
                }
            }

            Message::RevertSavedView => {
                // Local only: nothing is written or queued.
                if let Some(v) = &mut self.open_view {
                    v.revert();
                }
                self.scroll_state.clear();
                self.focused_lane = 0;
                self.focused_issue = 0;
            }

            Message::ConfirmDeleteSavedView(view_id) => {
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't delete a view",
                        ToastKind::Error,
                        false,
                    );
                } else {
                    self.modal = Modal::DeleteSavedViewConfirm(view_id);
                }
            }

            Message::ConfirmDeleteDashboard(id) => {
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't delete a dashboard",
                        ToastKind::Error,
                        false,
                    );
                } else {
                    self.modal = Modal::DeleteDashboardConfirm(id);
                }
            }

            Message::DeleteDashboardConfirmed => {
                let Modal::DeleteDashboardConfirm(id) =
                    std::mem::replace(&mut self.modal, Modal::None)
                else {
                    return Task::none();
                };
                self.delete_dashboard(&id);
                if self.view == View::Dashboard(id) {
                    return self.update_inner(Message::NavigateToView(View::Board));
                }
            }

            Message::ConfirmDeleteDashboardGroup(id) => {
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't delete a group",
                        ToastKind::Error,
                        false,
                    );
                } else {
                    self.modal = Modal::DeleteDashboardGroupConfirm(id);
                }
            }

            Message::DeleteDashboardGroupConfirmed => {
                let Modal::DeleteDashboardGroupConfirm(id) =
                    std::mem::replace(&mut self.modal, Modal::None)
                else {
                    return Task::none();
                };
                self.delete_dashboard_group(&id);
            }

            Message::DeleteSavedViewConfirmed => {
                let Modal::DeleteSavedViewConfirm(view_id) =
                    std::mem::replace(&mut self.modal, Modal::None)
                else {
                    return Task::none();
                };
                let team_id = self
                    .saved_views
                    .values()
                    .flatten()
                    .find(|v| v.id == view_id)
                    .map(|v| v.team_id.clone());
                match crate::db::saved_views::delete_local(&self.db(), &view_id) {
                    Ok(Some(payload)) => {
                        self.enqueue_sync("saved_view", &view_id, "delete", Some(payload.as_str()))
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("[saved_views] delete failed: {e}"),
                }
                let was_open = self.open_view.as_ref().is_some_and(|v| v.view_id == view_id);
                self.reload_data();
                if was_open {
                    if let Some(team_id) = team_id {
                        return self.update_inner(Message::SelectTeamView(team_id, View::Board));
                    }
                    self.open_view = None;
                }
            }

            Message::Tab(m) => return self.update_tab(m),
            Message::Terminal(crate::terminal::TerminalMessage::Close(id))
                if self.terminals.close_needs_confirm(id) =>
            {
                self.modal = Modal::TerminalCloseConfirm(id);
            }
            Message::Terminal(crate::terminal::TerminalMessage::DirPicked(Some(dir))) => {
                if let Some(scope) = self.panel_scope() {
                    let stored = match &scope.project {
                        Some(p) => crate::db::terminal_dirs::set_project(&self.db(), p, &dir),
                        None => crate::db::terminal_dirs::set(&self.db(), &scope.team, &dir),
                    };
                    match stored {
                        Ok(()) => {
                            let whose = if scope.project.is_some() { "this project" } else { "this team" };
                            self.push_toast(
                                format!(
                                    "Folder saved. New Terminals for {whose} start in {}. Open Terminals stay \
                                     where they are: close one and open a new one to start there.",
                                    crate::terminal::display_path(&dir)
                                ),
                                ToastKind::Info,
                                false,
                            );
                            self.terminals.set_dir(&scope, Some(dir));
                        }
                        Err(e) => self.push_toast(format!("Could not save the folder: {e}"), ToastKind::Error, false),
                    }
                }
            }
            Message::Terminal(crate::terminal::TerminalMessage::ClearDir) => {
                if let Some(scope) = self.panel_scope() {
                    let cleared = match &scope.project {
                        Some(p) => crate::db::terminal_dirs::clear_project(&self.db(), p),
                        None => crate::db::terminal_dirs::clear(&self.db(), &scope.team),
                    };
                    match cleared {
                        Ok(()) => self.terminals.set_dir(&scope, None),
                        Err(e) => self.push_toast(format!("Could not clear the folder: {e}"), ToastKind::Error, false),
                    }
                }
            }
            Message::Terminal(m) => {
                if matches!(m, crate::terminal::TerminalMessage::CloseConfirmed(_)) {
                    self.modal = Modal::None;
                }
                let scope = self.panel_scope();
                // Closing a removed team's last Terminal closes its Tab.
                let closes = matches!(
                    m,
                    crate::terminal::TerminalMessage::Close(_)
                        | crate::terminal::TerminalMessage::CloseConfirmed(_)
                );
                let task = self.terminals.update(m, scope.as_ref());
                self.post_terminal_alerts();
                return if closes {
                    Task::batch([task, self.close_removed_tabs()])
                } else {
                    task
                };
            }

            Message::SelectTeamView(team_id, view) => {
                let before = self.tab_set.as_ref().map(|s| s.active().clone());
                let scroll = self.focus_team_tab(&team_id);
                let moved = before != self.tab_set.as_ref().map(|s| s.active().clone());
                // Going to a team's own pages leaves any open Saved view. A
                // switch of Tab doesn't: the Tab's Modified view came back
                // with it, and `retain_open_view` below drops it if the
                // page asked for isn't its board.
                if !moved {
                    self.open_view = None;
                }
                self.commit_issue_detail();
                self.commit_project_detail();
                self.detail_from_my_issues = false;
                self.detail_from_inbox = false;
                self.detail_from_dashboard = None;
                let from_cross = self.view.is_cross_team();
                if self.selected_team_id.as_deref() != Some(team_id.as_str()) || from_cross {
                    self.selected_team_id = Some(team_id);
                    self.focused_lane = 0;
                    self.focused_issue = 0;
                    self.view = view.clone();
                    self.reload_data();
                }
                self.view = view;
                self.retain_open_view();
                if self.view == View::RecurringIssues {
                    let conn = self.db();
                    self.reload_recurring(&conn);
                }
                return scroll;
            }

            Message::InboxMove(delta) => {
                self.inbox.step(delta);
                return self.scroll_inbox_to_selection();
            }
            Message::InboxMarkRead => {
                self.mark_selected_inbox_read();
            }
            Message::InboxOpen => {
                let Some(n) = self.mark_selected_inbox_read() else {
                    return Task::none();
                };
                let conn = self.db();
                match crate::inbox::resolve_target(&conn, &n) {
                    crate::inbox::OpenTarget::Local(issue) => {
                        drop(conn);
                        return self.open_issue_from_inbox(*issue);
                    }
                    crate::inbox::OpenTarget::Fetch { team_remote_id, local_team_id, issue_remote_id } => {
                        let base_url = self.config.api_base_url.trim().to_string();
                        let token = self.config.api_token.clone().unwrap_or_default();
                        let Ok(client) = ApiClient::new(base_url, token) else {
                            self.push_toast("Can't open issue: not signed in", ToastKind::Error, false);
                            return Task::none();
                        };
                        return Task::perform(
                            async move {
                                SyncEngine::fetch_issue(&client, team_remote_id, &local_team_id, issue_remote_id)
                                    .await
                            },
                            move |r| Message::InboxIssueFetched(issue_remote_id, r),
                        );
                    }
                    crate::inbox::OpenTarget::Unavailable => {
                        self.push_toast("This notification has no issue to open", ToastKind::Info, false);
                    }
                }
            }
            Message::InboxIssueFetched(issue_remote_id, result) => {
                if self.view != View::Inbox {
                    return Task::none();
                }
                let found = match result {
                    Ok(()) => queries::find_issue_by_remote_id(&self.db(), issue_remote_id)
                        .ok()
                        .flatten(),
                    Err(_) => None,
                };
                match found {
                    Some(issue) => return self.open_issue_from_inbox(issue),
                    None => self.push_toast("Couldn't load that issue", ToastKind::Error, false),
                }
            }
            Message::BackToInbox => {
                self.commit_issue_detail();
                self.detail_from_inbox = false;
                self.detail_from_dashboard = None;
                self.issue_detail = None;
                self.switch_view(View::Inbox);
            }
            Message::BannerClicked(target) => {
                // macOS usually activates the app on a click; make sure.
                let focus = iced::window::get_latest().and_then(iced::window::gain_focus);
                // Don't navigate out from under an open modal (unsaved input).
                if self.modal != Modal::None {
                    return focus;
                }
                if let crate::db::notifications::BannerTarget::Terminal(raw) = target {
                    return Task::batch([focus, self.reveal_terminal(crate::terminal::TerminalId(raw))]);
                }
                self.commit_issue_detail();
                self.commit_project_detail();
                self.detail_from_my_issues = false;
                self.detail_from_inbox = false;
                self.detail_from_dashboard = None;
                let focus = Task::batch([focus, self.focus_tab(crate::tabs::TabKey::Inbox)]);
                self.switch_view(View::Inbox);
                let crate::db::notifications::BannerTarget::Notification(remote_id) = target else {
                    return Task::batch([focus, self.scroll_inbox_to_selection()]);
                };
                // Already read (e.g. on another device) hides it from Unread.
                if self.inbox.index_of(remote_id).is_none()
                    && self.inbox.filter == crate::db::notifications::Filter::Unread
                {
                    let all = crate::db::notifications::inbox_list(
                        &self.db(),
                        crate::db::notifications::Filter::All,
                    )
                    .unwrap_or_default();
                    if all.iter().any(|n| n.remote_id == remote_id) {
                        self.inbox.set_filter(crate::db::notifications::Filter::All);
                        self.inbox.set_items(all);
                    }
                }
                // Pruned locally: stay on the Inbox.
                let open = match self.inbox.index_of(remote_id) {
                    Some(i) => self.update(Message::InboxOpenAt(i)),
                    None => self.scroll_inbox_to_selection(),
                };
                return Task::batch([focus, open]);
            }
            Message::InboxMarkAllRead => {
                let conn = self.db();
                let now = crate::db::now_iso();
                if crate::db::notifications::mark_all_read(&conn, &now).unwrap_or(0) > 0 {
                    self.notif_unread_count =
                        crate::db::notifications::unread_count(&conn).unwrap_or(0);
                    self.reload_inbox();
                }
            }
            Message::InboxOpenAt(i) => {
                if i < self.inbox.items.len() {
                    self.inbox.selected = i;
                    return self.update(Message::InboxOpen);
                }
            }

            Message::InboxToggleFilter => {
                let next = crate::inbox::other_filter(self.inbox.filter);
                self.inbox.set_filter(next);
                self.reload_inbox();
                return self.scroll_inbox_to_selection();
            }
            Message::InboxSetFilter(filter) => {
                self.inbox.set_filter(filter);
                self.reload_inbox();
                return self.scroll_inbox_to_selection();
            }

            Message::MoveDashboardGroup(group_id, direction) => {
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't reorder groups",
                        ToastKind::Error,
                        false,
                    );
                    return Task::none();
                }
                match crate::db::dashboards::move_group(&self.db(), &group_id, direction) {
                    Ok(true) => self.reload_data(),
                    Ok(false) => {}
                    Err(e) => eprintln!("[dashboards] move group failed: {e}"),
                }
            }

            Message::OpenDashboard(id) => {
                // Each Dashboard has its own Tab.
                let scroll = self.focus_tab(crate::tabs::TabKey::Dashboard(id.clone()));
                self.commit_issue_detail();
                self.commit_project_detail();
                self.detail_from_my_issues = false;
                self.detail_from_inbox = false;
                self.detail_from_dashboard = None;
                self.switch_view(View::Dashboard(id));
                return scroll;
            }

            Message::ResetDashboard => {
                if let View::Dashboard(id) = self.view.clone() {
                    self.dashboard_sessions.reset(&id);
                    self.restore_dashboard_session();
                }
            }

            Message::SetDashboardSort(sort) => {
                self.dashboard_header.sort = sort;
                self.focused_lane = 0;
                self.focused_issue = 0;
            }

            Message::SetDashboardTeam(team) => {
                self.dashboard_header.team = team;
                self.focused_lane = 0;
                self.focused_issue = 0;
            }

            Message::SetDashboardStatus(status) => {
                self.dashboard_header.status = status;
                self.focused_lane = 0;
                self.focused_issue = 0;
            }

            Message::SetDashboardAssignee(pick) => {
                self.dashboard_header.assignee = pick;
                self.focused_lane = 0;
                self.focused_issue = 0;
            }

            Message::SetDashboardLabel(label) => {
                self.dashboard_header.label = label;
                self.focused_lane = 0;
                self.focused_issue = 0;
            }

            Message::ToggleDashboardMenu(target) => {
                self.dashboard_menu =
                    (self.dashboard_menu.as_ref() != Some(&target)).then_some(target);
            }
            Message::CloseDashboardMenu => self.dashboard_menu = None,
            Message::DashboardMenuPick(inner) => {
                self.dashboard_menu = None;
                return self.update_inner(*inner);
            }
            Message::ToggleDashboardMine => {
                self.dashboard_header.mine = !self.dashboard_header.mine;
                self.focused_lane = 0;
                self.focused_issue = 0;
            }

            Message::SetDashboardFilter(filter) => {
                self.dashboard_header.filter = filter;
                self.focused_lane = 0;
                self.focused_issue = 0;
            }

            Message::NavigateToView(view) => {
                // A team page asked for from a Tab that isn't a team's goes
                // to that team's Tab, so a Tab never leaves its own root.
                if let Some(team_id) = self.team_tab_for_navigation(&view) {
                    return self.update_inner(Message::SelectTeamView(team_id, view));
                }
                // Cross-team pages each have their own Tab.
                let scroll = match crate::tab_state::cross_team_key(&view) {
                    Some(key) => self.focus_tab(key),
                    None => Task::none(),
                };
                self.commit_issue_detail();
                self.commit_project_detail();
                self.detail_from_my_issues = false;
                self.detail_from_inbox = false;
                self.detail_from_dashboard = None;
                self.switch_view(view);
                return scroll;
            }

            Message::ToggleTeamMenu(team_id) => {
                if self.team_menu_open.as_deref() == Some(team_id.as_str()) {
                    self.team_menu_open = None;
                } else {
                    self.team_menu_open = Some(team_id);
                }
            }

            Message::SetTeamAsHome(team_id) => {
                self.team_menu_open = None;
                let remote_id = self
                    .teams
                    .iter()
                    .find(|t| t.id == team_id)
                    .and_then(|t| t.remote_id);
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't change the Home page",
                        ToastKind::Error,
                        false,
                    );
                } else if let Some(rid) = remote_id {
                    if let Err(e) = crate::db::home_page::set_local(&self.db(), "team", rid) {
                        eprintln!("[home_page] set failed: {e}");
                    }
                    self.home_page = crate::db::home_page::get(&self.db()).unwrap_or_default();
                } else {
                    self.push_toast("This team hasn't synced yet", ToastKind::Error, false);
                }
            }

            Message::SetDashboardAsHome(dashboard_id) => {
                let found = self.dashboards.iter().find(|d| d.id == dashboard_id).map(|d| d.remote_id);
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't change the Home page",
                        ToastKind::Error,
                        false,
                    );
                } else if let Some(remote_id) = found {
                    if let Err(e) =
                        crate::db::home_page::set_dashboard_local(&self.db(), &dashboard_id, remote_id)
                    {
                        eprintln!("[home_page] set failed: {e}");
                    }
                    self.home_page = crate::db::home_page::get(&self.db()).unwrap_or_default();
                }
            }

            Message::SetSavedViewAsHome(view_id) => {
                let found = self
                    .saved_views
                    .values()
                    .flatten()
                    .find(|v| v.id == view_id)
                    .map(|v| v.remote_id);
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't change the Home page",
                        ToastKind::Error,
                        false,
                    );
                } else if let Some(remote_id) = found {
                    // An unpushed view is queued by local id; the push waits
                    // for its create.
                    if let Err(e) =
                        crate::db::home_page::set_view_local(&self.db(), &view_id, remote_id)
                    {
                        eprintln!("[home_page] set failed: {e}");
                    }
                    self.home_page = crate::db::home_page::get(&self.db()).unwrap_or_default();
                }
            }

            Message::ToggleShowArchivedTeams => {
                self.show_archived_teams = !self.show_archived_teams;
            }

            Message::ToggleSidebarGroup(group_id) => {
                let collapsed = self
                    .sidebar
                    .as_ref()
                    .and_then(|s| s.groups.iter().find(|g| g.id == group_id))
                    .map(|g| g.collapsed);
                if let Some(collapsed) = collapsed {
                    let conn = self.db();
                    // A read-only token can't push: the toggle stays local and
                    // the next pull resets it.
                    if let Err(e) = crate::db::sidebar_layout::toggle_local(
                        &conn,
                        &group_id,
                        !collapsed,
                        !self.sync_read_only,
                    ) {
                        eprintln!("[sidebar] collapse failed: {e}");
                    }
                    self.sidebar = crate::db::sidebar_layout::load(&conn).unwrap_or_default();
                }
            }

            Message::OpenTeamMembers(team_id) => {
                self.team_members =
                    queries::members_for_team(&self.db(), &team_id).unwrap_or_default();
                self.team_menu_open = None;
                self.modal = Modal::TeamMembers(team_id);
            }

            Message::OpenTeamWebSettings(team_id, section) => {
                self.team_menu_open = None;
                let remote_id = self
                    .teams
                    .iter()
                    .find(|t| t.id == team_id)
                    .and_then(|t| t.remote_id);
                let Some(remote_id) = remote_id else {
                    self.push_toast(
                        "This team isn't synced to the web app yet",
                        ToastKind::Error,
                        false,
                    );
                    return Task::none();
                };
                let url = team_web_url(&self.config.api_base_url, remote_id, section);
                if let Err(e) = std::process::Command::new("open").arg(&url).spawn() {
                    self.push_toast(
                        format!("Couldn't open browser: {e}"),
                        ToastKind::Error,
                        false,
                    );
                }
            }

            Message::CreateIssue => {
                if self.view == View::MyIssues {
                    // My Issues spans teams: create in the most recently active
                    // team Tab's team (see `TabSet::new_issue_team`).
                    let Some(team_id) = self.cross_team_new_issue_team() else {
                        return Task::none();
                    };
                    self.selected_team_id = Some(team_id);
                }
                self.new_issue_scope = if self.view == View::MyIssues {
                    self.new_issue_scope()
                } else {
                    None
                };
                let lanes = self.new_issue_scope.as_ref().map_or(&self.lanes, |(l, _)| l);
                let default_lane = lanes.first().map(|l| l.id.clone()).unwrap_or_default();
                self.issue_form = Some(issue_form::IssueFormState::new(default_lane));
                self.modal = Modal::IssueForm;
            }

            Message::CreateSubIssue(parent_id) => {
                if let Some(parent) = self.issues.iter().find(|i| i.id == parent_id) {
                    let default_lane = self.lanes.first().map(|l| l.id.clone()).unwrap_or_default();
                    self.issue_form = Some(issue_form::IssueFormState::new_sub_issue(
                        default_lane,
                        parent,
                    ));
                    self.modal = Modal::IssueForm;
                }
            }

            Message::EditFocusedIssue => {
                if let Some(issue) = self.focused_issue().cloned() {
                    return self.open_focused_issue(issue);
                }
            }

            Message::IssueLabelToggled(label_id) => {
                if let Some(ref mut form) = self.issue_form {
                    form.toggle_label(&label_id);
                }
            }

            Message::IssueFormMenuToggled(which) => {
                if let Some(ref mut form) = self.issue_form {
                    form.open_menu = (form.open_menu != Some(which)).then_some(which);
                }
            }
            Message::DatePickerToggled(field, value) => {
                self.date_picker = match self.date_picker {
                    Some(s) if s.field == field => None,
                    _ => Some(crate::views::date_picker::DatePickerState::open(field, &value, chrono::Local::now().date_naive())),
                };
                if let Some(ref mut form) = self.issue_form {
                    form.open_menu = None;
                }
            }
            Message::DatePickerMonth(delta) => {
                if let Some(ref mut s) = self.date_picker {
                    s.shift(delta);
                }
            }
            Message::DatePickerClosed => {
                self.date_picker = None;
            }
            Message::DatePicked(msgs) => {
                self.date_picker = None;
                return Task::batch(msgs.into_iter().map(|m| self.update(m)).collect::<Vec<_>>());
            }
            Message::IssueFormMenuClosed => {
                if let Some(ref mut form) = self.issue_form {
                    form.open_menu = None;
                }
            }

            Message::IssueTitleChanged(v) => {
                if let Some(ref mut form) = self.issue_form {
                    form.title = v;
                }
            }
            Message::IssueDescriptionChanged(action) => {
                if let Some(ref mut form) = self.issue_form {
                    form.description.apply(action);
                }
                self.refresh_form_suggestions();
            }
            Message::IssuePriorityChanged(p) => {
                if let Some(ref mut form) = self.issue_form {
                    form.priority = p;
                    form.open_menu = None;
                }
            }
            Message::IssueLaneChanged(lane_id) => {
                if let Some(ref mut form) = self.issue_form {
                    form.lane_id = lane_id;
                    form.open_menu = None;
                }
            }
            Message::IssueAssigneeChanged(member_id) => {
                if let Some(ref mut form) = self.issue_form {
                    form.assignee_id = member_id;
                    form.open_menu = None;
                }
            }
            Message::IssueProjectChanged(project_id) => {
                if let Some(ref mut form) = self.issue_form {
                    form.project_id = project_id;
                    form.open_menu = None;
                }
            }
            Message::IssueFormNewProject => {
                if let Some(ref mut form) = self.issue_form {
                    form.open_menu = None;
                }
                if self.issue_form.is_some() && self.selected_team_id.is_some() {
                    self.project_form = Some(projects_view::ProjectFormState::new());
                    self.project_form_from_issue = true;
                    self.modal = Modal::ProjectForm;
                }
            }
            Message::IssueEstimateChanged(points) => {
                if let Some(ref mut form) = self.issue_form {
                    form.estimate = points;
                    form.open_menu = None;
                }
            }
            Message::IssueDueDateChanged(v) => {
                if let Some(ref mut form) = self.issue_form {
                    form.due_date = v;
                }
            }
            Message::IssueCreateMoreToggled(on) => {
                if let Some(ref mut form) = self.issue_form {
                    form.create_more = on;
                }
            }
            Message::PickIssueFormFiles => {
                if self.issue_form.is_none() {
                    return Task::none();
                }
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .pick_files()
                            .await
                            .map(|hs| hs.iter().map(|h| h.path().to_path_buf()).collect())
                            .unwrap_or_default()
                    },
                    Message::IssueFormFilesPicked,
                );
            }
            Message::IssueFormFilesPicked(paths) => {
                // Screened now so a bad file is refused at once; staged and
                // queued on Save, once the issue exists to own it.
                let (ok, refused) = crate::files::intake::screen_files(paths);
                if let Some(ref mut form) = self.issue_form {
                    form.add_files(ok);
                }
                if !refused.is_empty() {
                    self.push_toast(refused.join("\n"), ToastKind::Error, false);
                }
            }
            Message::RemoveIssueFormFile(index) => {
                if let Some(ref mut form) = self.issue_form {
                    if index < form.files.len() {
                        form.files.remove(index);
                    }
                }
            }

            Message::SaveIssue => {
                // Client-side validation (mirrors Rails' rules) — show inline
                // and keep the modal open rather than failing silently.
                if let Some(ref mut form) = self.issue_form {
                    if form.title.trim().is_empty() {
                        form.error = Some("Title can't be blank".into());
                        return Task::none();
                    }
                    if form.lane_id.trim().is_empty() {
                        form.error = Some("Pick a lane".into());
                        return Task::none();
                    }
                    if let Err(e) = form.parsed_due_date() {
                        form.error = Some(e);
                        return Task::none();
                    }
                    form.error = None;
                }
                // The saved issue's id, for queuing the form's files.
                let mut saved_id = None;
                if let Some(ref form) = self.issue_form {
                    let due_date = form.parsed_due_date().ok().flatten();
                    let conn = self.db();
                    let now = crate::db::now_iso();

                    let description_body = form.description_text();
                    let description = if description_body.trim().is_empty() {
                        None
                    } else {
                        Some(description_body.trim_end().to_string())
                    };

                    if let Some(ref issue_id) = form.editing_issue_id {
                        // Update existing
                        if let Some(mut issue) =
                            self.issues.iter().find(|i| i.id == *issue_id).cloned()
                        {
                            issue.title = form.title.trim().to_string();
                            issue.description = description.clone();
                            issue.priority = form.priority.to_i32();
                            issue.lane_id = form.lane_id.clone();
                            issue.assignee_id = form.assignee_id.clone();
                            issue.project_id = form.project_id.clone();
                            issue.estimate = form.estimate;
                            issue.due_date = due_date.clone();
                            issue.updated_at = now;
                            saved_id = Some(issue.id.clone());
                            let payload = serde_json::to_string(&issue).ok();
                            let _ = queries::edit_issue_queued(
                                &conn,
                                &issue.id,
                                "update",
                                payload.as_deref(),
                                |c| {
                                    queries::update_issue(c, &issue)?;
                                    queries::set_issue_labels(
                                        c,
                                        &issue.id,
                                        &form.selected_label_ids,
                                    )
                                },
                            );
                        }
                    } else {
                        // Create new
                        let team_id = self.selected_team_id.clone().unwrap_or_default();
                        let team_number = queries::next_team_number(&conn, &team_id).unwrap_or(1);
                        // Stamp the signed-in user as creator (local member UUID
                        // resolved from their Rails id). Rails also forces the
                        // creator server-side; this is for immediate local display.
                        let creator_id = self.config.current_user_remote_id.and_then(|rid| {
                            queries::find_member_by_remote_id(&conn, rid)
                                .ok()
                                .flatten()
                                .map(|m| m.id)
                        });
                        let issue = Issue {
                            id: uuid::Uuid::new_v4().to_string(),
                            title: form.title.trim().to_string(),
                            description: description.clone(),
                            priority: form.priority.to_i32(),
                            team_number: Some(team_number),
                            due_date,
                            estimate: form.estimate,
                            started_at: None,
                            completed_at: None,
                            canceled_at: None,
                            lane_id: form.lane_id.clone(),
                            team_id: team_id.clone(),
                            project_id: form.project_id.clone(),
                            parent_issue_id: form.parent_issue_id.clone(),
                            assignee_id: form.assignee_id.clone(),
                            creator_id,
                            remote_id: None,
                            created_at: now.clone(),
                            updated_at: now,
                        };
                        let _ = queries::insert_issue(&conn, &issue);
                        let _ =
                            queries::set_issue_labels(&conn, &issue.id, &form.selected_label_ids);

                        let payload = serde_json::to_string(&issue).ok();
                        self.enqueue_sync("issue", &issue.id, "create", payload.as_deref());
                        saved_id = Some(issue.id.clone());
                    }
                }
                // Queued after the issue's own entry, so its upload waits for
                // the create to give the issue a remote id.
                if let (Some(id), Some(form)) = (&saved_id, &self.issue_form) {
                    if !form.files.is_empty() {
                        let conn = self.db();
                        let intake = crate::files::intake::queue_files(
                            &conn,
                            &crate::files::outbox::default_root(),
                            crate::db::attachments::OwnerKind::Issue,
                            id,
                            &form.files,
                        );
                        if let Some(toast) = intake.refusal_toast() {
                            self.push_toast(toast, ToastKind::Error, false);
                        }
                    }
                }
                let more = self
                    .issue_form
                    .as_ref()
                    .filter(|f| f.create_more && f.editing_issue_id.is_none() && saved_id.is_some());
                if let Some(form) = more {
                    // "Create more": a blank form for the next issue, as the
                    // web redirects back to `new` (keeping the parent).
                    let lanes = match (&self.new_issue_scope, &form.parent_issue_id) {
                        (Some((lanes, _)), None) => lanes,
                        _ => &self.lanes,
                    };
                    let default_lane = lanes.first().map(|l| l.id.clone()).unwrap_or_default();
                    self.issue_form = Some(form.next_after_create(default_lane));
                    self.push_toast("Issue created. Create another?", ToastKind::Info, false);
                } else {
                    self.modal = Modal::None;
                    self.issue_form = None;
                }
                self.reload_data();
            }

            // --- Issue detail: buffered fields ---
            Message::IssueDetailTitleChanged(action) => {
                if let Some(d) = &mut self.issue_detail {
                    if let Some(action) = action.filter_action(issue_detail::single_line) {
                        d.title.apply(action);
                    }
                }
            }
            Message::IssueDetailDescriptionChanged(action) => {
                if let Some(d) = &mut self.issue_detail {
                    d.description.apply(action);
                }
                self.refresh_suggestions(crate::mentions::EditorTarget::Description);
            }

            Message::StopEditingIssueDescription => {
                if let Some(d) = &mut self.issue_detail {
                    d.editing_description = false;
                }
            }
            Message::EditIssueDescription => {
                if let Some(d) = &mut self.issue_detail {
                    d.editing_description = true;
                    return iced::advanced::widget::operate(issue_detail::FocusFirstIn::new(
                        issue_detail::description_editor_id(),
                    ))
                    .discard();
                }
            }

            Message::StopEditingProjectDescription => {
                if let Some(d) = &mut self.project_detail {
                    d.editing_description = false;
                }
            }
            Message::EditProjectDescription => {
                if let Some(d) = &mut self.project_detail {
                    d.editing_description = true;
                    return iced::advanced::widget::operate(issue_detail::FocusFirstIn::new(
                        project_detail::description_editor_id(),
                    ))
                    .discard();
                }
            }

            // --- Issue detail: comments ---
            Message::CommentComposerChanged(action) => {
                if let Some(d) = self.comment_drafts_mut() {
                    d.comment.apply(action);
                }
                self.refresh_suggestions(crate::mentions::EditorTarget::Comment);
            }
            Message::PickSuggestion(idx) => self.pick_suggestion(Some(idx)),
            Message::AcceptSuggestion => self.pick_suggestion(None),
            Message::MoveSuggestion(delta) => {
                if let Some(Some(s)) = self.open_suggest_mut() {
                    s.move_selection(delta);
                }
            }
            Message::SubmitComment => {
                let body = match self.comment_drafts_mut() {
                    Some(d) => d.comment.text().trim().to_string(),
                    None => return Task::none(),
                };
                if self.create_comment(body, None) {
                    if let Some(d) = self.comment_drafts_mut() {
                        *d.comment = crate::undo_editor::UndoableContent::new();
                        *d.suggest = None;
                    }
                }
            }
            Message::StartReply(parent_id) => {
                let mut dropped = None;
                if let Some(d) = self.comment_drafts_mut() {
                    *d.reply_to = Some(parent_id);
                    *d.suggest = None;
                    *d.reply = crate::undo_editor::UndoableContent::new();
                    // The previous reply draft is gone: its pasted images go too.
                    dropped = Some(std::mem::replace(
                        &mut d.files.reply_id,
                        uuid::Uuid::new_v4().to_string(),
                    ));
                }
                if let Some(id) = dropped {
                    self.drop_drafts(&[id]);
                }
            }
            Message::CancelReply => {
                let mut dropped = None;
                if let Some(d) = self.comment_drafts_mut() {
                    *d.reply_to = None;
                    *d.reply = crate::undo_editor::UndoableContent::new();
                    *d.suggest = None;
                    dropped = Some(std::mem::replace(
                        &mut d.files.reply_id,
                        uuid::Uuid::new_v4().to_string(),
                    ));
                }
                if let Some(id) = dropped {
                    self.drop_drafts(&[id]);
                }
            }
            Message::ReplyComposerChanged(action) => {
                if let Some(d) = self.comment_drafts_mut() {
                    d.reply.apply(action);
                }
                self.refresh_suggestions(crate::mentions::EditorTarget::Reply);
            }
            Message::SubmitReply => {
                let (body, parent) = match self.comment_drafts_mut() {
                    Some(d) => match d.reply_to {
                        Some(p) => (d.reply.text().trim().to_string(), p.clone()),
                        None => return Task::none(),
                    },
                    None => return Task::none(),
                };
                if self.create_comment(body, Some(parent)) {
                    if let Some(d) = self.comment_drafts_mut() {
                        *d.reply_to = None;
                        *d.reply = crate::undo_editor::UndoableContent::new();
                        *d.suggest = None;
                    }
                }
            }
            Message::IssueFetched(result) => {
                if result.is_ok() {
                    self.reload_data();
                }
            }
            Message::OpenAttachment(att) => return self.open_attachment(att),
            Message::OpenLightbox(att) => {
                if let Some(state) = crate::files::lightbox::LightboxState::at(
                    self.lightbox_images(),
                    |i| matches!(i, crate::files::lightbox::LightboxImage::Attachment(a) if a.id == att.id),
                ) {
                    self.modal = Modal::Lightbox(state);
                    return self.request_lightbox_images();
                }
                return self.open_attachment(att);
            }
            Message::OpenInlineImage(img) => {
                if let Some(state) = crate::files::lightbox::LightboxState::at(
                    self.lightbox_images(),
                    |i| i.key() == img.key,
                ) {
                    self.modal = Modal::Lightbox(state);
                    return self.request_lightbox_images();
                }
            }
            Message::LightboxStep(delta) => {
                if let Modal::Lightbox(state) = &mut self.modal {
                    state.step(delta);
                    return self.request_lightbox_images();
                }
            }
            Message::OpenLightboxExternally => {
                let target = match &self.modal {
                    Modal::Lightbox(state) => state
                        .open_target(&self.images)
                        .map(|(key, name, path)| (key, name.to_string(), path)),
                    _ => return Task::none(),
                };
                match target {
                    Some((key, name, cached)) => {
                        let root = crate::files::default_root();
                        let opened = crate::files::open::materialize(&root, &key, &name, &cached)
                            .and_then(|p| crate::files::open::open_in_default_app(&p));
                        if let Err(e) = opened {
                            self.push_toast(
                                format!("Couldn't open {name}: {e}"),
                                ToastKind::Error,
                                false,
                            );
                        }
                    }
                    None => self.push_toast("Image hasn't loaded yet", ToastKind::Error, false),
                }
            }
            Message::AttachmentDownloaded(att, result) => match result {
                Ok(bytes) => {
                    let conn = self.db();
                    let cache = crate::files::cache::FileCache::at_default_root();
                    let key = crate::files::open::blob_key(&att).unwrap_or_default();
                    match cache.put(&conn, &key, &bytes) {
                        Ok(path) => self.open_cached_attachment(&att, &key, &path),
                        Err(e) => self.push_toast(
                            format!("Couldn't cache {}: {e}", att.filename),
                            ToastKind::Error,
                            false,
                        ),
                    }
                }
                Err(e) => self.push_toast(
                    format!("Couldn't download {}: {e}", att.filename),
                    ToastKind::Error,
                    false,
                ),
            },
            Message::ImageFetched(key, result) => {
                let cache = crate::files::cache::FileCache::at_default_root();
                match result.map_err(|e| e.to_string()).and_then(|bytes| {
                    cache.put(&self.db(), &key, &bytes).map_err(|e| e.to_string())
                }) {
                    Ok(path) => self.images.set_ready(&key, path),
                    Err(_) => self.images.set_failed(&key),
                }
            }
            Message::ActivityFetched(issue_id, stored) => {
                // Only refresh when entries landed and that issue is still open.
                if stored > 0
                    && matches!(&self.view, View::IssueDetail(id) if *id == issue_id)
                {
                    self.reload_data();
                }
            }
            Message::CommentsFetched(issue_id, result) => {
                if let Ok(dtos) = result {
                    let conn = self.db();
                    crate::api::dto::merge_comment_tree(
                        &conn,
                        &crate::db::models::CommentOwner::Issue(issue_id),
                        dtos,
                        None,
                    );
                    drop(conn);
                    // Refresh so the open detail's comment list reflects the merge.
                    self.reload_data();
                }
            }
            Message::ProjectCommentsFetched(project_id, result) => {
                if let Ok(dtos) = result {
                    let conn = self.db();
                    crate::api::dto::merge_comment_tree(
                        &conn,
                        &crate::db::models::CommentOwner::Project(project_id),
                        dtos,
                        None,
                    );
                    drop(conn);
                    self.reload_data();
                }
            }
            Message::IssueDetailDueDateChanged(v) => {
                if let Some(d) = &mut self.issue_detail {
                    d.due_date = v;
                }
            }
            Message::IssueDetailEstimateChanged(v) => {
                if let Some(d) = &mut self.issue_detail {
                    d.estimate = v;
                }
            }
            Message::CommitIssueDetail => {
                self.commit_issue_detail();
            }
            Message::IssueDetailClickedOutside => {
                if let Some(d) = &mut self.issue_detail {
                    d.editing_description = false;
                }
                self.commit_issue_detail();
            }

            // --- Issue detail: live-apply pick_lists ---
            Message::IssueDetailAssigneeChanged(member_id) => {
                self.apply_issue_update(|i| i.assignee_id = member_id);
            }

            // --- Issue detail: dependencies (blocking / blocked-by) ---
            Message::BackToMyIssues => {
                self.commit_issue_detail();
                self.detail_from_my_issues = false;
                self.issue_detail = None;
                self.switch_view(View::MyIssues);
            }
            Message::BackToDashboard => {
                self.back_to_dashboard();
            }
            Message::HoverScrolled => {
                let effect = self.body_hover.event(crate::hover_card::Event::Scrolled);
                return Self::hover_timer(effect);
            }
            // No cards behind a modal.
            Message::BodyHover(..) | Message::RowHover(..) if self.modal != Modal::None => {}
            Message::RowHover(owner, hover) => {
                let effect = self
                    .body_hover
                    .body_hover(owner, hover.map(|(anchor, rect)| (Some(anchor), rect)));
                return Self::hover_timer(effect);
            }
            Message::BodyHover(owner, hover) => {
                let span = hover.map(|h| {
                    let anchor = h.link.as_ref().and_then(crate::body_hover::Anchor::from_link);
                    // The widget saw no pointer move yet (fresh after a rebuild):
                    // fall back to the content-space rectangle.
                    let rect = h.window_bounds.unwrap_or(h.bounds);
                    (anchor, rect)
                });
                let effect = self.body_hover.body_hover(owner, span);
                return Self::hover_timer(effect);
            }
            Message::HoverTimer(generation) => {
                // A card opening afresh re-reads, so edits made while it was
                // closed show.
                if !self.body_hover.is_open() {
                    self.hover_content = None;
                }
                let effect = self.body_hover.event(crate::hover_card::Event::TimerFired(generation));
                self.refresh_hover_content();
                return Self::hover_timer(effect);
            }
            Message::HoverCardEntered => {
                let effect = self.body_hover.event(crate::hover_card::Event::EnteredCard);
                return Self::hover_timer(effect);
            }
            Message::HoverCardLeft => {
                let effect = self.body_hover.event(crate::hover_card::Event::LeftCard);
                return Self::hover_timer(effect);
            }
            Message::OpenBodyLink(target) => {
                // A click ends the hover: close the card, cancel a pending one.
                self.body_hover.event(crate::hover_card::Event::Clicked);
                match target {
                    crate::mentions::LinkTarget::Issue(id) => {
                        return self.update(Message::OpenIssueDetail(id));
                    }
                    crate::mentions::LinkTarget::External(url) => {
                        // Defence in depth: the document module already filters.
                        if crate::document::is_openable_link(&url) {
                            if let Err(e) = std::process::Command::new("open").arg(url.trim()).spawn() {
                                self.push_toast(format!("Couldn't open link: {e}"), ToastKind::Error, false);
                            }
                        }
                    }
                    // A later ticket fills this.
                    crate::mentions::LinkTarget::Mention(_) => {}
                }
            }
            Message::OpenIssueDetail(issue_id) => {
                // A click on a row ends its hover (the view may not change).
                self.body_hover.event(crate::hover_card::Event::Clicked);
                // A link can name another team's issue, which isn't loaded.
                let issue = match self.issues.iter().find(|i| i.id == issue_id).cloned() {
                    Some(issue) => Some(issue),
                    None => queries::get_issue(&self.db(), &issue_id).ok().flatten(),
                };
                if let Some(issue) = issue {
                    return self.open_focused_issue(issue);
                }
            }
            Message::OpenDependencyPicker(direction) => {
                self.issue_picker = Some(issue_picker::IssuePickerState::new(direction));
                self.modal = Modal::IssuePicker;
            }
            Message::CycleDependencyKind(forward) => {
                if let Some(ref mut picker) = self.issue_picker {
                    picker.direction = picker.direction.cycle(forward);
                }
            }
            Message::SetDependencyKind(kind) => {
                if let Some(ref mut picker) = self.issue_picker {
                    picker.direction = kind;
                }
            }
            Message::DependencyPickerSearchChanged(query) => {
                if let Some(ref mut picker) = self.issue_picker {
                    picker.query = query;
                }
            }
            Message::OpenParentPicker => {
                self.issue_picker = Some(issue_picker::IssuePickerState::hierarchy(
                    issue_picker::PickerMode::Parent,
                ));
                self.modal = Modal::IssuePicker;
            }
            Message::OpenChildPicker => {
                self.issue_picker = Some(issue_picker::IssuePickerState::hierarchy(
                    issue_picker::PickerMode::Child,
                ));
                self.modal = Modal::IssuePicker;
            }
            Message::RemoveParent => {
                if let Some(id) = self.issue_detail.as_ref().map(|d| d.issue_id.clone()) {
                    self.set_parent(&id, None);
                }
            }
            Message::DetachSubIssue(child_id) => {
                self.body_hover.event(crate::hover_card::Event::Clicked);
                self.set_parent(&child_id, None);
            }
            Message::DependencyPickerSelected(target_id) => {
                let open = self.issue_detail.as_ref().map(|d| d.issue_id.clone());
                let mode = self.issue_picker.as_ref().map(|p| p.mode);
                match (mode, open) {
                    (Some(issue_picker::PickerMode::Dependency), _) => {
                        self.add_dependency(&target_id);
                    }
                    (Some(mode), Some(id)) => {
                        if let Some((child, parent)) =
                            issue_picker::picked_parent_change(mode, &id, &target_id)
                        {
                            self.set_parent(child, Some(parent));
                        }
                    }
                    _ => {}
                }
                self.issue_picker = None;
                self.modal = Modal::None;
            }
            Message::RemoveDependency(dep_id) => {
                // The row (and its hover anchor) is about to vanish.
                self.body_hover.event(crate::hover_card::Event::Clicked);
                self.remove_dependency(&dep_id);
            }
            Message::OpenMoveTeamPicker(issue_id) => {
                self.modal = Modal::MoveTeam(issue_id);
            }
            Message::MoveFocusedIssueToTeam => {
                if let Some(issue) = self.focused_issue() {
                    self.modal = Modal::MoveTeam(issue.id.clone());
                }
            }
            Message::MoveTeamSelected(target_id) => {
                if let Modal::MoveTeam(issue_id) = std::mem::replace(&mut self.modal, Modal::None) {
                    // Same rule as the picker: only synced, live teams.
                    let ok = self
                        .teams
                        .iter()
                        .any(|t| t.id == target_id && t.remote_id.is_some() && t.archived_at.is_none());
                    if ok {
                        self.move_issue_to_team(&issue_id, &target_id);
                    }
                }
            }

            Message::ConfirmDeleteFocusedIssue => {
                if let Some(issue) = self.focused_issue() {
                    self.modal = Modal::DeleteConfirm(issue.id.clone());
                }
            }

            Message::DeleteConfirmed => {
                if let Modal::DeleteConfirm(ref issue_id) = self.modal {
                    let conn = self.db();
                    let payload = match queries::get_issue(&conn, issue_id) {
                        Ok(Some(issue)) => match issue.remote_id {
                            Some(remote_id) => {
                                queries::remote_id_for(&conn, "teams", &issue.team_id)
                                    .ok()
                                    .flatten()
                                    .map(|team_remote_id| {
                                        serde_json::json!({
                                            "remote_id": remote_id,
                                            "team_remote_id": team_remote_id,
                                        })
                                        .to_string()
                                    })
                            }
                            None => None,
                        },
                        _ => None,
                    };
                    if let Some(p) = &payload {
                        self.enqueue_sync("issue", issue_id, "delete", Some(p.as_str()));
                    }
                    let _ = queries::delete_issue(&conn, issue_id);
                }
                self.modal = Modal::None;
                self.reload_data();
            }

            Message::MoveIssueNextLane => {
                // Lanes are only columns when grouped by status.
                if self.board_display().group_by != GroupBy::Status {
                    self.push_toast(
                        "Moving between lanes needs Display > Columns: Status",
                        ToastKind::Info,
                        false,
                    );
                    return Task::none();
                }
                if let Some(issue) = self.focused_issue().cloned() {
                    let current_lane_idx = self
                        .lanes
                        .iter()
                        .position(|l| l.id == issue.lane_id)
                        .unwrap_or(0);
                    if current_lane_idx + 1 < self.lanes.len() {
                        let target = &self.lanes[current_lane_idx + 1];
                        let conn = self.db();
                        let _ = queries::edit_issue_queued(&conn, &issue.id, "update", None, |c| {
                            queries::move_issue_to_lane(c, &issue.id, &target.id)
                        });
                        self.reload_data();
                        self.focus_issue_by_id(&issue.id);
                    }
                }
            }

            Message::MoveIssuePrevLane => {
                // Lanes are only columns when grouped by status.
                if self.board_display().group_by != GroupBy::Status {
                    self.push_toast(
                        "Moving between lanes needs Display > Columns: Status",
                        ToastKind::Info,
                        false,
                    );
                    return Task::none();
                }
                if let Some(issue) = self.focused_issue().cloned() {
                    let current_lane_idx = self
                        .lanes
                        .iter()
                        .position(|l| l.id == issue.lane_id)
                        .unwrap_or(0);
                    if current_lane_idx > 0 {
                        let target = &self.lanes[current_lane_idx - 1];
                        let conn = self.db();
                        let _ = queries::edit_issue_queued(&conn, &issue.id, "update", None, |c| {
                            queries::move_issue_to_lane(c, &issue.id, &target.id)
                        });
                        self.reload_data();
                        self.focus_issue_by_id(&issue.id);
                    }
                }
            }

            Message::SetFocusedPriority(p) => {
                if let Some(issue) = self.focused_issue().cloned() {
                    let conn = self.db();
                    let _ = queries::edit_issue_queued(&conn, &issue.id, "update", None, |c| {
                        queries::set_issue_priority(c, &issue.id, p)
                    });
                    self.reload_data();
                }
            }

            Message::FocusNextIssue
            | Message::FocusPrevIssue
            | Message::FocusNextLane
            | Message::FocusPrevLane
                if !self.list_nav_active() && self.swimlanes_active() =>
            {
                self.swimlane_nav(&message);
                return self.scroll_to_focus();
            }
            Message::FocusNextIssue | Message::FocusPrevIssue
                if self.list_nav_active() =>
            {
                // List mode: j/k walk rows top-to-bottom across sections.
                let counts = self.list_nav_counts();
                let delta = if matches!(message, Message::FocusNextIssue) {
                    1
                } else {
                    -1
                };
                (self.focused_lane, self.focused_issue) =
                    display::list_step(&counts, self.focused_lane, self.focused_issue, delta);
                return self.scroll_to_focus();
            }
            Message::FocusNextLane | Message::FocusPrevLane
                if self.list_nav_active() =>
            {
                // List mode: h/l jump to the next/previous non-empty section.
                let counts = self.list_nav_counts();
                let forward = matches!(message, Message::FocusNextLane);
                let lane = display::list_jump_section(&counts, self.focused_lane, forward);
                if lane != self.focused_lane {
                    self.focused_lane = lane;
                    self.focused_issue = 0;
                }
                return self.scroll_to_focus();
            }
            Message::FocusNextIssue => {
                let count = self.issues_in_focused_lane().len();
                if count > 0 && self.focused_issue + 1 < count {
                    self.focused_issue += 1;
                }
                return self.scroll_to_focus();
            }
            Message::FocusPrevIssue => {
                if self.focused_issue > 0 {
                    self.focused_issue -= 1;
                }
                return self.scroll_to_focus();
            }
            Message::FocusNextLane => {
                if self.focused_lane + 1 < self.board_columns().len() {
                    self.focused_lane += 1;
                    self.focused_issue = 0;
                    self.clamp_focus();
                }
                return self.scroll_to_focus();
            }
            Message::FocusPrevLane => {
                if self.focused_lane > 0 {
                    self.focused_lane -= 1;
                    self.focused_issue = 0;
                    self.clamp_focus();
                }
                return self.scroll_to_focus();
            }

            Message::CardPressed(lane_idx, issue_idx) => {
                self.focused_lane = lane_idx;
                self.focused_issue = issue_idx;
                self.clamp_focus();
                if let Some(issue) = self.focused_issue().cloned() {
                    // With the overlay on, a card that has lines opens its map.
                    if self.card_opens_map(&issue.id) {
                        let from = self.view.clone();
                        let task = self.update(Message::OpenDependencyMap(issue.id.clone()));
                        if matches!(self.view, View::DependencyMap { .. }) {
                            self.map_return = Some(crate::tab_state::MapReturn { back: from });
                        }
                        return task;
                    }
                    return self.open_focused_issue(issue);
                }
            }

            Message::HeaderSearchChanged(query) => {
                self.set_view_search(query);
                if matches!(self.view, View::Board | View::MyIssues) {
                    self.focused_issue = 0;
                    self.clamp_focus();
                } else if matches!(self.view, View::Dashboard(_)) {
                    self.focused_lane = 0;
                    self.focused_issue = 0;
                }
            }
            Message::FocusSearch => {
                return text_input::focus(header::search_input_id());
            }
            Message::OpenDisplayOptions => self.modal = Modal::DisplayOptions,
            Message::OpenFilterPanel => self.modal = Modal::FilterPanel,
            Message::Noop => {}
            Message::SetViewMode(m) => {
                self.board_display_mut().view_mode = m;
                self.scroll_state.clear();
                self.persist_display();
            }
            Message::ToggleViewMode => {
                let next = match self.board_display().view_mode {
                    ViewMode::Board => ViewMode::List,
                    ViewMode::List => ViewMode::Board,
                };
                return self.update(Message::SetViewMode(next));
            }
            Message::ToggleSection(title) => {
                if !self.collapsed_sections.remove(&title) {
                    self.collapsed_sections.insert(title);
                }
            }
            Message::ToggleSwimlane(title) => {
                let key = swimlane_key(&title);
                if !self.collapsed_sections.remove(&key) {
                    self.collapsed_sections.insert(key);
                }
                self.clamp_focus();
            }
            Message::ToggleDashboardGroup(group_id) => {
                if let View::Dashboard(did) = self.view.clone() {
                    crate::views::dashboard::toggle_group(
                        &mut self.collapsed_dashboard_groups,
                        &did,
                        &group_id,
                    );
                }
            }
            Message::ToggleFocusedSection if matches!(self.view, View::Dashboard(_)) => {
                let gid = self
                    .dashboard_evaluation()
                    .groups
                    .get(self.focused_lane)
                    .map(|g| g.group.id.clone());
                if let Some(gid) = gid {
                    return self.update(Message::ToggleDashboardGroup(gid));
                }
            }
            Message::ToggleFocusedSection if self.swimlanes_active() => {
                let title = {
                    let columns = self.board_columns();
                    columns.get(self.focused_lane).and_then(|c| {
                        display::swimlane_row_of(&c.sections, self.focused_issue)
                            .map(|r| c.sections[r].title.clone())
                    })
                };
                if let Some(title) = title {
                    let _ = self.update(Message::ToggleSwimlane(title));
                    return self.scroll_to_focus();
                }
            }
            Message::ToggleFocusedSection => {
                if self.board_display().view_mode == ViewMode::List {
                    let title = self
                        .board_columns()
                        .get(self.focused_lane)
                        .map(|c| c.title.clone());
                    if let Some(title) = title {
                        let expanded = self.collapsed_sections.remove(&title);
                        if !expanded {
                            self.collapsed_sections.insert(title);
                        } else {
                            return self.scroll_to_focus();
                        }
                    }
                }
            }
            Message::Scrolled(key, offset, viewport) => {
                self.scroll_state.insert(key, (offset, viewport));
            }
            Message::SetGroupBy(g) => {
                self.board_display_mut().group_by = g;
                // A sub-group repeating the new group-by axis is meaningless.
                if self.board_display().sub_group_conflicts(self.board_display().sub_group) {
                    self.board_display_mut().sub_group = SubGroup::None;
                }
                self.focused_lane = 0;
                self.focused_issue = 0;
                self.persist_display();
            }
            Message::SetSubGroup(g) => {
                if !self.board_display().sub_group_conflicts(g) {
                    self.board_display_mut().sub_group = g;
                    self.persist_display();
                }
            }
            Message::SetOrderBy(o) => {
                self.board_display_mut().order_by = o;
                self.persist_display();
            }
            Message::SetCompletedFilter(f) => {
                self.board_display_mut().set_completed_filter(f);
                self.persist_display();
            }
            Message::ToggleShowSubIssues => {
                let d = self.board_display_mut();
                d.show_sub_issues = !d.show_sub_issues;
                self.persist_display();
            }
            Message::ToggleShowEmptyGroups => {
                let d = self.board_display_mut();
                d.show_empty_groups = !d.show_empty_groups;
                self.persist_display();
            }
            Message::ToggleShowEmptyRows => {
                let d = self.board_display_mut();
                d.show_empty_rows = !d.show_empty_rows;
                self.persist_display();
            }
            Message::ToggleShowDependencies => {
                let d = self.board_display_mut();
                d.show_dependencies = !d.show_dependencies;
                self.persist_display();
            }
            Message::ToggleCardProperty(p) => {
                self.board_display_mut().toggle_property(p);
                self.persist_display();
            }
            Message::ToggleFilter(field, value) => {
                self.board_filters_mut().toggle(field, &value);
                self.focused_issue = 0;
                self.persist_display();
            }
            Message::ClearFilters => {
                self.board_filters_mut().clear();
                // The panel's search box mirrors the header search; clear it too.
                self.clear_view_search();
                self.focused_issue = 0;
                self.persist_display();
            }

            Message::CloseModal if self.form_page_open() => {
                self.date_picker = None;
                self.close_form_page()
            }

            Message::CloseModal => {
                // "Not now": the Staged update stays, installing on the next quit.
                if matches!(self.modal, Modal::RestartConfirm(_)) {
                    self.modal = Modal::None;
                    return self.apply_updater_event(crate::updater::machine::Event::RestartDeclined);
                }
                // Cancelling the quit ask also cancels the terminate behind it.
                if self.modal == Modal::QuitConfirm {
                    crate::quit::reply_to_terminate(false);
                }
                // Cancelling the label-delete confirm returns to the panel.
                if matches!(self.modal, Modal::DeleteLabelConfirm(_)) {
                    self.modal = Modal::Labels;
                    return Task::none();
                }
                // Cancelling a project started from the issue form returns to it.
                if self.modal == Modal::ProjectForm && std::mem::take(&mut self.project_form_from_issue) {
                    self.project_form = None;
                    if self.issue_form.is_some() {
                        self.modal = Modal::IssueForm;
                        return Task::none();
                    }
                }
                self.modal = Modal::None;
                self.issue_form = None;
                self.settings_form = None;
                self.project_form = None;
                self.recurring_form = None;
                self.label_form = None;
                self.team_members.clear();
                self.csv_import = None;
            }

            Message::OpenDependencyMap(issue_id) => {
                if matches!(self.view, View::DependencyMap { .. }) {
                    return Task::none();
                }
                self.commit_issue_detail();
                if let Some(old) = self.issue_detail.take() {
                    self.drop_drafts(&[old.composer_files.comment_id, old.composer_files.reply_id]);
                }
                self.composer_focused = false;
                self.map_return = None;
                self.view = View::dependency_map(issue_id);
                self.map_flipped.clear();
                let conn = self.db();
                self.load_detail_dependencies(&conn);
            }
            Message::OpenHierarchyTree(issue_id) => {
                let task = self.update(Message::OpenDependencyMap(issue_id));
                if let View::DependencyMap { ref mut tab, .. } = self.view {
                    *tab = DependencyMapTab::Hierarchy;
                }
                return task;
            }
            Message::ToggleHierarchyNode(id) => {
                if !self.map_flipped.remove(&id) {
                    self.map_flipped.insert(id);
                }
            }
            Message::SetDependencyMapTab(next) => {
                if let View::DependencyMap { ref mut tab, .. } = self.view {
                    *tab = next;
                }
            }
            Message::FocusDependencyMap(next) => {
                if let View::DependencyMap { ref mut issue_id, .. } = self.view {
                    if *issue_id != next {
                        *issue_id = next;
                        self.map_flipped.clear();
                        let conn = self.db();
                        self.load_detail_dependencies(&conn);
                    }
                }
            }
            Message::SetDependencyMapDepth(next) => {
                if let View::DependencyMap { ref mut depth, .. } = self.view {
                    *depth = next.clamp(1, MAX_DEPENDENCY_DEPTH);
                }
            }
            Message::CloseDependencyMap => {
                let View::DependencyMap { issue_id, .. } = &self.view else {
                    return Task::none();
                };
                // Opened from a board card: Back returns to that board.
                // The slot lasts for the map session, so it still applies
                // after a refocus onto another issue.
                if let Some(crate::tab_state::MapReturn { back }) = self.map_return.take() {
                    self.view = back;
                    return Task::none();
                }
                let issue = match self.issues.iter().find(|i| i.id == *issue_id).cloned() {
                    Some(issue) => Some(issue),
                    None => queries::get_issue(&self.db(), issue_id).ok().flatten(),
                };
                match issue {
                    Some(issue) => return self.open_issue_detail(&issue),
                    None => {
                        if let Some(key) = self.active_tab_key().cloned() {
                            self.view = crate::tab_state::root_view(&key);
                        }
                    }
                }
            }

            Message::OpenProjectDependencyMap(project_id) => {
                if matches!(self.view, View::ProjectDependencyMap { .. }) {
                    return Task::none();
                }
                self.commit_project_detail();
                self.composer_focused = false;
                self.view = View::project_dependency_map(project_id);
                self.refresh_project_map();
            }
            Message::SetProjectMapActiveOnly(next) => {
                if let View::ProjectDependencyMap { ref mut active_only, .. } = self.view {
                    *active_only = next;
                    self.refresh_project_map();
                }
            }
            Message::CloseProjectDependencyMap => {
                let View::ProjectDependencyMap { project_id, .. } = &self.view else {
                    return Task::none();
                };
                match queries::get_project(&self.db(), project_id) {
                    Ok(Some(project)) => return self.open_project_detail(&project),
                    _ => {
                        if let Some(key) = self.active_tab_key().cloned() {
                            self.view = crate::tab_state::root_view(&key);
                        }
                    }
                }
            }

            Message::OpenCommandPalette => {
                // Never opens over another modal.
                if self.modal == Modal::None {
                    self.modal = Modal::CommandPalette(PaletteState::new(
                        self.palette_data(),
                    ));
                    // A fresh list widget starts at the top.
                    self.scroll_state.remove(&ScrollKey::Palette);
                    return text_input::focus(command_palette::input_id());
                }
            }
            Message::PaletteQueryChanged(query) => {
                if let Modal::CommandPalette(ref mut palette) = self.modal {
                    palette.set_query(query);
                    // The highlight resets to the first row; follow it.
                    self.scroll_state.remove(&ScrollKey::Palette);
                    return ScrollKey::Palette.scroll_to(0.0);
                }
            }
            Message::PaletteMove(delta) => {
                if let Modal::CommandPalette(ref mut palette) = self.modal {
                    palette.move_selection(delta);
                    let (offset, viewport) =
                        self.scroll_state.get(&ScrollKey::Palette).copied().unwrap_or((
                            0.0,
                            scroll::palette_list_height(palette.rows.len()),
                        ));
                    let new = scroll::palette_offset_to_reveal(palette.selected, offset, viewport);
                    if (new - offset).abs() >= 0.5 {
                        self.scroll_state.insert(ScrollKey::Palette, (new, viewport));
                        return ScrollKey::Palette.scroll_to(new);
                    }
                }
            }
            // An open popover owns the hover context (Rails' `pickerOpen`):
            // reaching for its rows drags the pointer across other cards.
            Message::CardHover(..) if matches!(self.modal, Modal::PropertyPicker(_)) => {}
            Message::CardHover(owner, Some((issue_id, rect))) => {
                self.hovered_card = Some(HoveredCard { owner, issue_id, rect });
            }
            Message::CardHover(owner, None) => {
                // Moving between cards can deliver the next enter first.
                if self.hovered_card.as_ref().is_some_and(|h| h.owner == owner) {
                    self.hovered_card = None;
                }
            }
            Message::OpenPropertyPicker(field) => {
                // The issue sidebar's toggle button: a second click closes it.
                if let Modal::PropertyPicker(ref open) = self.modal {
                    if open.in_sidebar && open.field == field {
                        self.modal = Modal::None;
                    }
                    return Task::none();
                }
                if self.modal != Modal::None {
                    return Task::none();
                }
                if let Some((issue, anchor)) = self.key_target() {
                    let issue = issue.clone();
                    let mut state = self.property_picker_state(&issue, field, anchor);
                    // The issue page opens the Properties field's own dropdown, as
                    // Rails does; with Properties hidden it opens as a modal.
                    state.in_sidebar = matches!(self.view, View::IssueDetail(_))
                        && self.config.issue_properties_open;
                    let focus = state.has_search();
                    self.modal = Modal::PropertyPicker(state);
                    if focus {
                        return text_input::focus(property_picker::search_id());
                    }
                }
            }
            Message::PropertyPickerQuery(query) => {
                if let Modal::PropertyPicker(ref mut state) = self.modal {
                    state.set_query(query);
                }
            }
            Message::PropertyPickerMove(delta) => {
                if let Modal::PropertyPicker(ref mut state) = self.modal {
                    state.move_focus(delta);
                }
            }
            Message::PropertyPickerSubmit => {
                // Enter does nothing until a row is highlighted, as in Rails.
                if let Modal::PropertyPicker(ref state) = self.modal {
                    if let Some(i) = state.focus {
                        return self.update(Message::PropertyPickerChoose(i));
                    }
                }
            }
            Message::PropertyPickerChoose(i) => return self.choose_property(i),
            Message::PropertyPickerNewProject => {
                self.modal = Modal::None;
                return self.update(Message::CreateProject);
            }
            Message::PaletteSubmit => {
                let nav = match self.modal {
                    Modal::CommandPalette(ref palette) => palette.chosen(),
                    _ => None,
                };
                if let Some(nav) = nav {
                    return self.update(Message::PaletteChoose(nav));
                }
            }
            Message::PaletteChoose(nav) => {
                self.modal = Modal::None;
                match nav {
                    // Same path as any other view navigation: commits open
                    // issue/project detail edits first.
                    PaletteNav::View(view) => return self.update(Message::NavigateToView(view)),
                    // Same path as a sidebar team click, so selection follows.
                    PaletteNav::Team(id) => {
                        return self.update(Message::SelectTeamView(id, View::Board))
                    }
                    PaletteNav::SavedView(id) => return self.update(Message::OpenSavedView(id)),
                    PaletteNav::Dashboard(id) => return self.update(Message::OpenDashboard(id)),
                    PaletteNav::AttachFiles => return self.update(Message::PickAttachmentFiles),
                    PaletteNav::OpenProjectDependencyMap => {
                        if let View::ProjectDetail(id) = &self.view {
                            return self.update(Message::OpenProjectDependencyMap(id.clone()));
                        }
                    }
                    PaletteNav::OpenDependencyMap => {
                        if let View::IssueDetail(id) = &self.view {
                            return self.update(Message::OpenDependencyMap(id.clone()));
                        }
                    }
                    PaletteNav::ToggleProperties => {
                        return self.update(Message::ToggleProperties)
                    }
                    PaletteNav::ToggleProjectCardsStacked => {
                        return self.update(Message::ToggleProjectCardsStacked)
                    }
                    PaletteNav::ToggleTerminalDock => return self.update(Message::ToggleTerminalDock),
                    PaletteNav::FocusTerminal => {
                        return self.update(Message::Terminal(crate::terminal::TerminalMessage::FocusTerminal))
                    }
                    PaletteNav::HidePanel => {
                        return self.update(Message::Terminal(crate::terminal::TerminalMessage::Hide))
                    }
                    PaletteNav::FocusApp => {
                        return self.update(Message::Terminal(crate::terminal::TerminalMessage::FocusApp))
                    }
                    PaletteNav::Tab(i) => return self.update(Message::Tab(TabMessage::Focus(i))),
                    PaletteNav::Settings => return self.update(Message::OpenSettings),
                    PaletteNav::RestartToUpdate => return self.update(Message::RestartNowClicked),
                    PaletteNav::ToggleSidebar => return self.update(Message::ToggleSidebar),
                    // Status renders only in Settings, so open it to make the check visible.
                    PaletteNav::CheckForUpdates => {
                        let open = self.update(Message::OpenSettings);
                        let check = self.update(Message::UpdateCheckClicked);
                        return Task::batch([open, check]);
                    }
                    PaletteNav::Issue(id) => {
                        let Ok(Some(issue)) = queries::get_issue(&self.db(), &id) else {
                            return Task::none();
                        };
                        // The same rule as every other open path: the issue's
                        // team Tab from a team or project Tab, the current
                        // Tab from a cross-team one. Commits open edits first.
                        return self.open_focused_issue(issue);
                    }
                    PaletteNav::Project { project_id, .. } => {
                        // The project's Tab loads its own team's data.
                        return self.update(Message::OpenProjectDetail(project_id));
                    }
                }
            }

            Message::ToggleHelp => {
                self.show_help = !self.show_help;
            }

            Message::ToggleSidebar => {
                let mut new_cfg = self.config.clone();
                new_cfg.sidebar_collapsed = !new_cfg.sidebar_collapsed;
                self.save_config_change(new_cfg);
                self.terminals.set_sidebar_width(self.sidebar_width());
            }

            Message::ToggleMoreProperties => {
                self.more_properties = !self.more_properties;
            }

            Message::ToggleProperties => {
                let mut new_cfg = self.config.clone();
                match self.view {
                    View::ProjectDetail(_) => {
                        new_cfg.project_properties_open = !new_cfg.project_properties_open
                    }
                    View::IssueDetail(_) => {
                        new_cfg.issue_properties_open = !new_cfg.issue_properties_open;
                        // A sidebar dropdown loses its anchor with them.
                        if matches!(self.modal, Modal::PropertyPicker(ref p) if p.in_sidebar) {
                            self.modal = Modal::None;
                        }
                    }
                    _ => return Task::none(),
                }
                self.save_config_change(new_cfg);
            }

            Message::SendIssuePrompt(issue_id) => {
                let Some(issue) = self.issues.iter().find(|i| i.id == issue_id) else {
                    return Task::none();
                };
                let team = self.teams.iter().find(|t| t.id == issue.team_id);
                let project_remote = issue
                    .project_id
                    .as_ref()
                    .and_then(|pid| self.projects.iter().find(|p| p.id == *pid))
                    .and_then(|p| p.remote_id);
                let prompt = match (team.and_then(|t| t.remote_id.map(|r| (t, r))), issue.team_number) {
                    (Some((team, team_remote)), Some(n)) => crate::mcp_prompt::for_issue(
                        team_remote,
                        project_remote,
                        &format!("{}-{n}", team.identifier),
                    ),
                    _ => {
                        self.push_toast("This issue hasn't synced yet, so JAIT can't find it", ToastKind::Error, false);
                        return Task::none();
                    }
                };
                if !self.terminals.type_into_focused(prompt) {
                    self.push_toast("Open a Terminal in the panel first", ToastKind::Error, false);
                }
            }

            Message::ToggleProjectCardsStacked => {
                let mut new_cfg = self.config.clone();
                new_cfg.project_cards_stacked = !new_cfg.project_cards_stacked;
                self.save_config_change(new_cfg);
            }

            Message::ToggleTerminalDock => {
                use crate::terminal::panel::Dock;
                let dock = match self.terminals.dock() {
                    Dock::Bottom => Dock::Right,
                    Dock::Right => Dock::Bottom,
                };
                self.terminals.set_dock(dock);
                let mut new_cfg = self.config.clone();
                new_cfg.terminal_dock = dock.name().to_string();
                self.save_config_change(new_cfg);
            }

            Message::OpenSettings => {
                self.settings_form = Some(settings::SettingsFormState::from_config(&self.config));
                self.modal = Modal::Settings;
            }

            Message::SettingsBaseUrlChanged(v) => {
                if let Some(ref mut form) = self.settings_form {
                    form.api_base_url = v;
                    form.status = None;
                }
            }

            Message::SettingsTokenChanged(v) => {
                if let Some(ref mut form) = self.settings_form {
                    form.api_token = v;
                    form.status = None;
                }
            }

            Message::SettingsThemeChanged(choice) => {
                let mut new_cfg = self.config.clone();
                new_cfg.theme = choice.key.to_string();
                t::set_by_key(choice.key);
                if let Some(ref mut form) = self.settings_form {
                    form.theme = choice;
                }
                // Applied for this session even if the save fails.
                self.save_config_change(new_cfg);
            }

            Message::SettingsFontChanged(choice) => {
                let mut new_cfg = self.config.clone();
                new_cfg.font = choice.key.to_string();
                if let Some(ref mut form) = self.settings_form {
                    form.font = choice;
                }
                // No live apply: Iced fixes the default font at launch.
                self.save_config_change(new_cfg);
            }

            Message::SettingsTerminalFontChanged(choice) => {
                let mut new_cfg = self.config.clone();
                new_cfg.terminal_font = choice.key.to_string();
                if let Some(ref mut form) = self.settings_form {
                    form.terminal_font = choice;
                }
                // Applied live: the Terminals re-measure and resize on the next sync.
                self.terminals.set_font(crate::views::terminal_panel::TermFont::from_key(choice.key, self.config.terminal_font_size));
                self.save_config_change(new_cfg);
            }

            Message::SettingsTerminalSizeChanged(size) => {
                let mut new_cfg = self.config.clone();
                new_cfg.terminal_font_size = size as f32;
                if let Some(ref mut form) = self.settings_form {
                    form.terminal_size = size;
                }
                self.terminals.set_font(crate::views::terminal_panel::TermFont::from_key(&new_cfg.terminal_font, new_cfg.terminal_font_size));
                self.save_config_change(new_cfg);
            }

            Message::SettingsWindowOpacityChanged(pct) => {
                t::set_window_opacity(pct);
                if let Some(ref mut form) = self.settings_form {
                    form.window_opacity = t::window_opacity();
                }
                // Blur is off at 100%, so crossing it changes the blur too.
                return self.apply_window_blur();
            }

            Message::SettingsWindowBlurChanged(radius) => {
                if let Some(ref mut form) = self.settings_form {
                    form.window_blur = radius.min(crate::window_effects::MAX_BLUR_RADIUS);
                }
                return self.apply_window_blur();
            }

            Message::SettingsWindowEffectsReleased => {
                if let Some(form) = self.settings_form.as_ref() {
                    let mut new_cfg = self.config.clone();
                    new_cfg.window_opacity = form.window_opacity;
                    new_cfg.window_blur = form.window_blur;
                    self.save_config_change(new_cfg);
                }
            }

            Message::WindowOpened => return self.apply_window_blur(),

            Message::SettingsNativeNotificationsToggled(on) => {
                let mut new_cfg = self.config.clone();
                new_cfg.native_notifications = on;
                if let Some(ref mut form) = self.settings_form {
                    form.native_notifications = on;
                }
                self.save_config_change(new_cfg);
            }

            Message::ExportCsv => {
                let Some(team) = self.selected_team().cloned() else {
                    return Task::none();
                };
                let conn = self.db();
                let issues = queries::issues_for_team(&conn, &team.id).unwrap_or_default();
                let lanes = queries::lanes_for_team(&conn, &team.id).unwrap_or_default();
                let projects = queries::projects_for_team(&conn, &team.id).unwrap_or_default();
                let members = queries::members_for_team(&conn, &team.id).unwrap_or_default();
                let mut labels: HashMap<String, Vec<String>> = HashMap::new();
                for (issue_id, label) in
                    queries::issue_labels_for_team(&conn, &team.id).unwrap_or_default()
                {
                    labels.entry(issue_id).or_default().push(label.name);
                }
                let csv = csv_io::export_csv(&csv_io::ExportData {
                    team: &team,
                    issues: &issues,
                    lanes: &lanes,
                    projects: &projects,
                    members: &members,
                    labels: &labels,
                });
                let csv = match csv {
                    Ok(c) => c,
                    Err(e) => {
                        self.push_toast(format!("Export failed: {e}"), ToastKind::Error, false);
                        return Task::none();
                    }
                };
                let filename = csv_io::export_filename(&team, chrono::Local::now().date_naive());
                return Task::perform(
                    async move {
                        let Some(handle) = rfd::AsyncFileDialog::new()
                            .set_file_name(filename)
                            .add_filter("CSV", &["csv"])
                            .save_file()
                            .await
                        else {
                            return Ok(None);
                        };
                        let path = handle.path().to_path_buf();
                        std::fs::write(&path, csv).map_err(|e| e.to_string())?;
                        Ok(Some(path.display().to_string()))
                    },
                    Message::CsvExported,
                );
            }

            Message::CsvExported(result) => match result {
                Ok(Some(path)) => {
                    self.push_toast(format!("Exported to {path}"), ToastKind::Info, false)
                }
                Ok(None) => {}
                Err(e) => self.push_toast(format!("Export failed: {e}"), ToastKind::Error, false),
            },

            Message::PickAttachmentFiles => {
                if !matches!(self.view, View::IssueDetail(_) | View::ProjectDetail(_)) {
                    return Task::none();
                }
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .pick_files()
                            .await
                            .map(|hs| hs.iter().map(|h| h.path().to_path_buf()).collect())
                            .unwrap_or_default()
                    },
                    Message::FilesPicked,
                );
            }

            Message::PasteImageIntoDescription => {
                let View::IssueDetail(ref issue_id) = self.view else {
                    return Task::none();
                };
                let issue_id = issue_id.clone();
                let crate::files::paste::ClipboardPaste::Image(png) =
                    crate::files::paste::read_clipboard()
                else {
                    return Task::none();
                };
                let conn = self.db();
                match crate::files::paste::paste_image_placeholder(
                    &conn,
                    &crate::files::outbox::default_root(),
                    crate::db::attachments::OwnerKind::Issue,
                    &issue_id,
                    &png,
                ) {
                    Ok(text) => {
                        if let Some(d) = &mut self.issue_detail {
                            d.description.perform(iced::widget::text_editor::Action::Edit(
                                iced::widget::text_editor::Edit::Paste(std::sync::Arc::new(text)),
                            ));
                        }
                        self.load_detail_dependencies(&conn);
                        self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
                    }
                    Err(reason) => self.push_toast(reason, ToastKind::Error, false),
                }
            }

            Message::PasteImageIntoProjectDescription => {
                let View::ProjectDetail(ref project_id) = self.view else {
                    return Task::none();
                };
                let project_id = project_id.clone();
                let crate::files::paste::ClipboardPaste::Image(png) =
                    crate::files::paste::read_clipboard()
                else {
                    return Task::none();
                };
                let conn = self.db();
                match crate::files::paste::paste_image_placeholder(
                    &conn,
                    &crate::files::outbox::default_root(),
                    crate::db::attachments::OwnerKind::Project,
                    &project_id,
                    &png,
                ) {
                    Ok(text) => {
                        if let Some(d) = &mut self.project_detail {
                            d.description.perform(iced::widget::text_editor::Action::Edit(
                                iced::widget::text_editor::Edit::Paste(std::sync::Arc::new(text)),
                            ));
                        }
                        self.load_detail_dependencies(&conn);
                        self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
                    }
                    Err(reason) => self.push_toast(reason, ToastKind::Error, false),
                }
            }

            Message::PasteImageIntoComment | Message::PasteImageIntoReply => {
                let reply = matches!(message, Message::PasteImageIntoReply);
                let crate::files::paste::ClipboardPaste::Image(png) =
                    crate::files::paste::read_clipboard()
                else {
                    return Task::none();
                };
                let conn = self.db();
                let Some(d) = self.comment_drafts_mut() else {
                    return Task::none();
                };
                let owner = if reply { d.files.reply_id.clone() } else { d.files.comment_id.clone() };
                match crate::files::paste::paste_image_placeholder(
                    &conn,
                    &crate::files::outbox::default_root(),
                    crate::db::attachments::OwnerKind::Comment,
                    &owner,
                    &png,
                ) {
                    Ok(text) => {
                        let editor = if reply { d.reply } else { d.comment };
                        editor.perform(iced::widget::text_editor::Action::Edit(
                            iced::widget::text_editor::Edit::Paste(std::sync::Arc::new(text)),
                        ));
                        self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
                    }
                    Err(reason) => self.push_toast(reason, ToastKind::Error, false),
                }
            }

            // A multi-file drop arrives as one event per file: buffer them and
            // route the batch once the window closes.
            Message::FileDropped(path) => {
                if self.drop_buffer.push(path) {
                    return Task::perform(
                        async { tokio::time::sleep(crate::files::drop::BATCH_WINDOW).await },
                        |_| Message::FlushDrops,
                    );
                }
            }

            Message::FlushDrops => {
                use crate::files::drop::{drop_target, DropTarget};
                let paths = self.drop_buffer.take();
                match drop_target(&self.view, &self.modal, self.composer_focused) {
                    DropTarget::Issue | DropTarget::Project => {
                        return self.update(Message::FilesPicked(paths))
                    }
                    DropTarget::CommentComposer => {
                        return self.update(Message::CommentFilesPicked(paths))
                    }
                    DropTarget::IssueForm => {
                        return self.update(Message::IssueFormFilesPicked(paths))
                    }
                    DropTarget::Ignore => {}
                }
            }

            Message::FilesPicked(paths) => {
                let (owner_kind, owner_id) = match &self.view {
                    View::IssueDetail(id) => (crate::db::attachments::OwnerKind::Issue, id.clone()),
                    View::ProjectDetail(id) => {
                        (crate::db::attachments::OwnerKind::Project, id.clone())
                    }
                    _ => return Task::none(),
                };
                if paths.is_empty() {
                    return Task::none();
                }
                let conn = self.db();
                let intake = crate::files::intake::queue_files(
                    &conn,
                    &crate::files::outbox::default_root(),
                    owner_kind,
                    &owner_id,
                    &paths,
                );
                if let Some(toast) = intake.refusal_toast() {
                    self.push_toast(toast, ToastKind::Error, false);
                }
                self.load_detail_dependencies(&conn);
                self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
            }

            Message::PickCommentFiles => {
                if self.comment_drafts_mut().is_none() {
                    return Task::none();
                }
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .pick_files()
                            .await
                            .map(|hs| hs.iter().map(|h| h.path().to_path_buf()).collect())
                            .unwrap_or_default()
                    },
                    Message::CommentFilesPicked,
                );
            }

            Message::CommentFilesPicked(paths) => {
                if paths.is_empty() {
                    return Task::none();
                }
                let conn = self.db();
                let Some(d) = self.comment_drafts_mut() else {
                    return Task::none();
                };
                let intake = crate::files::intake::queue_files(
                    &conn,
                    &crate::files::outbox::default_root(),
                    crate::db::attachments::OwnerKind::Comment,
                    &d.files.comment_id,
                    &paths,
                );
                d.files.files.extend(intake.queued);
                if !intake.refused.is_empty() {
                    self.push_toast(intake.refused.join("\n"), ToastKind::Error, false);
                }
                self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
            }

            Message::RemoveStagedFile(id) => {
                let conn = self.db();
                if let Ok(Some(path)) = crate::db::attachments::remove_staged(&conn, &id) {
                    let _ = crate::files::outbox::remove(std::path::Path::new(&path));
                }
                if let Some(d) = self.comment_drafts_mut() {
                    d.files.files.retain(|a| a.id != id);
                }
                self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
            }

            Message::PickCsvImport => {
                if self.selected_team_id.is_none() {
                    return Task::none();
                }
                return Task::perform(
                    async {
                        let Some(handle) = rfd::AsyncFileDialog::new()
                            .add_filter("CSV", &["csv"])
                            .pick_file()
                            .await
                        else {
                            return Ok(None);
                        };
                        let bytes = handle.read().await;
                        Ok(Some((
                            handle.file_name(),
                            String::from_utf8_lossy(&bytes).into_owned(),
                        )))
                    },
                    Message::CsvImportLoaded,
                );
            }

            Message::CsvImportLoaded(result) => match result {
                Ok(Some((file_name, text))) => {
                    let Some(team_id) = self.selected_team_id.clone() else {
                        return Task::none();
                    };
                    let Some(team) = self.teams.iter().find(|t| t.id == team_id) else {
                        return Task::none();
                    };
                    let conn = self.db();
                    let issues = queries::issues_for_team(&conn, &team_id).unwrap_or_default();
                    let fallback_creator_id = self.config.current_user_remote_id.and_then(|rid| {
                        queries::find_member_by_remote_id(&conn, rid)
                            .ok()
                            .flatten()
                            .map(|m| m.id)
                    });
                    let ctx = csv_io::ImportContext {
                        team,
                        lanes: &self.lanes,
                        projects: &self.projects,
                        members: &self.members,
                        issues: &issues,
                        fallback_creator_id: fallback_creator_id.as_deref(),
                    };
                    match csv_io::parse_import(&text, &ctx) {
                        Ok(preview) => {
                            self.csv_import =
                                Some(csv_import::CsvImportState { file_name, preview });
                            self.modal = Modal::CsvImport;
                        }
                        Err(e) => {
                            self.push_toast(format!("Import failed: {e}"), ToastKind::Error, false)
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => self.push_toast(format!("Import failed: {e}"), ToastKind::Error, false),
            },

            Message::ConfirmCsvImport => {
                let (Some(state), Some(team_id)) =
                    (self.csv_import.take(), self.selected_team_id.clone())
                else {
                    return Task::none();
                };
                let conn = self.db();
                let skipped = state.preview.errors.len();
                let duplicates = state.preview.duplicates.len();
                match csv_io::apply_import(&conn, &team_id, &state.preview.rows) {
                    Ok(outcome) => {
                        let mut msg = format!("Imported {} issue(s)", outcome.imported);
                        if outcome.projects_created > 0 {
                            msg.push_str(&format!(
                                ", created {} project(s)",
                                outcome.projects_created
                            ));
                        }
                        if duplicates > 0 {
                            msg.push_str(&format!(", skipped {duplicates} duplicate(s)"));
                        }
                        if skipped > 0 {
                            msg.push_str(&format!(", skipped {skipped} bad row(s)"));
                        }
                        self.push_toast(msg, ToastKind::Info, false);
                    }
                    Err(e) => {
                        self.push_toast(format!("Import failed: {e}"), ToastKind::Error, false)
                    }
                }
                self.modal = Modal::None;
                self.reload_data();
            }

            Message::TestConnection => {
                if let Some(ref mut form) = self.settings_form {
                    if form.testing {
                        return Task::none();
                    }
                    let base_url = form.api_base_url.trim().to_string();
                    let token = form.api_token.trim().to_string();
                    match ApiClient::new(base_url, token) {
                        Ok(client) => {
                            form.testing = true;
                            form.status = Some("Testing…".to_string());
                            return Task::perform(
                                async move {
                                    client
                                        .get::<Vec<TeamDto>>("/api/v1/teams")
                                        .await
                                        .map(|teams| teams.len())
                                        .map_err(|e| e.to_string())
                                },
                                Message::TestConnectionResult,
                            );
                        }
                        Err(ApiError::MissingToken) => {
                            form.status = Some("Failed: set an API token first.".to_string());
                        }
                        Err(e) => {
                            form.status = Some(format!("Failed: {}", e));
                        }
                    }
                }
            }

            Message::TestConnectionResult(result) => {
                if let Some(ref mut form) = self.settings_form {
                    form.testing = false;
                    form.status = Some(match result {
                        Ok(n) => format!("Connected — {} teams visible", n),
                        Err(msg) => format!("Failed: {}", msg),
                    });
                }
            }

            Message::SaveSettings => {
                if let Some(form) = self.settings_form.as_ref() {
                    let mut new_cfg = self.config.clone();
                    new_cfg.api_base_url = form.api_base_url.trim().to_string();
                    let token = form.api_token.trim();
                    new_cfg.api_token = if token.is_empty() {
                        None
                    } else {
                        Some(token.to_string())
                    };
                    match config::save(&new_cfg) {
                        Ok(()) => {
                            self.config = new_cfg;
                            if self.files_unlocked() {
                                // Failed fetches (no token before) may retry.
                                self.images.clear();
                            } else {
                                self.lock_files();
                            }
                            self.config_error = None;
                            self.modal = Modal::None;
                            self.settings_form = None;
                            // Resume sync: a fresh token clears the auth pause and
                            // any stale error so the next tick pushes queued edits.
                            self.sync_paused_auth = false;
                            self.last_sync_error = None;
                            self.push_toast("Settings saved", ToastKind::Info, false);
                            // Un-pausing the scheduler runs a full cycle now.
                            return self.drive_sync(SchedEvent::AuthPaused(false));
                        }
                        Err(e) => {
                            if let Some(ref mut f) = self.settings_form {
                                f.status = Some(format!("Error: {}", e));
                            }
                        }
                    }
                }
            }

            Message::UpdateCheckClicked => return self.update_check_clicked(),
            Message::UpdateAutoCheckToggled(on) => return self.update_auto_check_toggled(on),
            Message::UpdaterTick => return self.updater_background(updater_wiring::Trigger::Tick),
            Message::UpdaterWoke => return self.updater_background(updater_wiring::Trigger::Woke),
            Message::UpdateDmgClicked => return self.update_dmg_clicked(),
            Message::Updater(event) => return self.apply_updater_event(event),
            Message::OpenUpdatePopover => {
                if self.updater.view().ready.is_some() {
                    self.modal = Modal::UpdatePopover;
                }
            }
            Message::RestartNowClicked => {
                self.modal = Modal::None;
                let busy_terminals = self.terminals.any_busy();
                return self.apply_updater_event(crate::updater::machine::Event::RestartRequested { busy_terminals });
            }

            Message::RestartConfirmed => {
                self.modal = Modal::None;
                return self.apply_updater_event(crate::updater::machine::Event::RestartConfirmed);
            }

            Message::SyncTick => return self.drive_sync(SchedEvent::Tick),
            Message::UserInput => return self.drive_sync(SchedEvent::Input),
            Message::WindowResized(size) => {
                self.window_width = size.width;
                self.window_height = size.height;
            }
            Message::WindowFocused => {
                self.terminals.set_window_focused(true);
                return self.drive_sync(SchedEvent::FocusGained);
            }
            Message::WindowUnfocused => {
                self.terminals.set_window_focused(false);
                return self.drive_sync(SchedEvent::FocusLost);
            }
            Message::QuitRequested => return self.quit(),
            Message::QuitConfirmed => return self.quit_with(true),
            Message::QuitReady => {
                // Lets a pending system terminate go ahead (a no-op for the close button).
                crate::quit::reply_to_terminate(true);
                return iced::exit();
            }
            Message::InstallTerminateHook => crate::quit::install_terminate_hook(),
            Message::SyncNow => return self.drive_sync(SchedEvent::ManualSync),

            Message::SyncCompleted(report) => {
                let cycle_ok = report.cycle_succeeded();
                self.sync_in_progress = false;
                // Read-only is a state, not an error: show the banner instead.
                self.sync_read_only = report.read_only();
                self.last_sync_error = if self.sync_read_only {
                    None
                } else {
                    report.last_error().map(str::to_string)
                };
                self.inaccessible_teams =
                    queries::inaccessible_team_identifiers(&self.db()).unwrap_or_default();

                self.leave_deleted_teams(&report.deleted_teams);
                // Error kind so it stays until dismissed: losing a team can happen
                // in a background sync, and an Info toast would be gone in seconds.
                for notice in &report.pull.notices {
                    self.push_toast(notice.clone(), ToastKind::Error, false);
                }

                // Classify failures before touching UI state (ends the borrow of
                // `report` before we mutate `self`). A 401 outranks everything:
                // sync is paused, so we surface only the re-auth prompt. Network
                // failures stay in the status bar (no toast). A 422 on an issue
                // reopens that form; other server errors become error toasts.
                let auth_failed = report.auth_failed();
                let mut validation_issue: Option<(String, Vec<String>)> = None;
                let mut error_toasts: Vec<(String, bool)> = Vec::new();
                if !auth_failed {
                    for f in report.failures() {
                        match &f.kind {
                            SyncErrorKind::Auth | SyncErrorKind::Network | SyncErrorKind::TeamAccess => {}
                            // A refused move (422/404) is dropped; the next full pull
                            // restores the issue, so the toast just carries the reason.
                            SyncErrorKind::Validation(errs) if f.entity_type == "issue_move" => {
                                error_toasts
                                    .push((format!("Move rejected: {}", errs.join("; ")), false));
                            }
                            SyncErrorKind::Other if f.entity_type == "issue_move" => {
                                error_toasts.push((format!("Move rejected: {}", f.message), false));
                            }
                            SyncErrorKind::Validation(errs) if f.entity_type == "issue" => {
                                validation_issue = Some((f.entity_id.clone(), errs.clone()));
                            }
                            SyncErrorKind::Validation(errs) => {
                                error_toasts
                                    .push((format!("Sync rejected: {}", errs.join("; ")), false));
                            }
                            SyncErrorKind::Forbidden => {
                                error_toasts.push((format!("Sync blocked: {}", f.message), false));
                            }
                            SyncErrorKind::Other => {
                                error_toasts.push((format!("Sync error: {}", f.message), true));
                            }
                        }
                    }
                }
                let clean = report.pull.failed == 0 && report.push.failed == 0;

                // `/me` can succeed even when a later pull step fails, so persist
                // the signed-in user's identity independently of the failed count.
                let mut config_dirty = false;
                if let Some((rid, name)) = &report.current_user {
                    if self.config.current_user_remote_id != Some(*rid)
                        || self.config.current_user_name.as_deref() != Some(name.as_str())
                    {
                        self.config.current_user_remote_id = Some(*rid);
                        self.config.current_user_name = Some(name.clone());
                        config_dirty = true;
                    }
                }
                if !report.push_only && report.pull.failed == 0 {
                    self.config.last_synced_at = Some(chrono::Utc::now());
                    config_dirty = true;
                }
                if config_dirty {
                    let _ = config::save(&self.config);
                }
                self.pending_sync_count = sync_queue::pending_count(&self.db()).unwrap_or(0);
                self.reload_data();
                // Tabs whose team, project or Dashboard the pull removed.
                let removed_tabs = self.close_removed_tabs();

                if auth_failed {
                    self.pause_for_auth();
                } else {
                    if clean {
                        self.sync_paused_auth = false;
                    }
                    // Reopen the offending issue form with the server's messages.
                    if let Some((issue_id, errs)) = validation_issue {
                        let conn = self.db();
                        if let Ok(Some(issue)) = queries::get_issue(&conn, &issue_id) {
                            let label_ids =
                                queries::label_ids_for_issue(&conn, &issue_id).unwrap_or_default();
                            let mut form =
                                issue_form::IssueFormState::from_issue(&issue, label_ids);
                            form.error = Some(errs.join("; "));
                            self.issue_form = Some(form);
                            self.modal = Modal::IssueForm;
                        }
                    }
                    for (msg, retry) in error_toasts {
                        self.push_toast(msg, ToastKind::Error, retry);
                    }
                }

                // Tell the scheduler how the cycle went (backoff / follow-up).
                let finish = self.drive_sync(SchedEvent::CycleFinished { success: cycle_ok });
                // A Data sync just refreshed the mirror: re-fetch the open
                // issue's Activity so new server entries show up.
                let refetch = if report.push_only || auth_failed {
                    Task::none()
                } else {
                    self.refetch_open_issue_activity()
                };
                if self.sync_paused_auth {
                    return Task::batch([
                        finish,
                        self.drive_sync(SchedEvent::AuthPaused(true)),
                        refetch,
                        removed_tabs,
                    ]);
                }
                return Task::batch([finish, refetch, removed_tabs]);
            }

            Message::NotifPollCompleted(outcome) => {
                self.sync_in_progress = false;
                if outcome.auth_failed() {
                    self.pause_for_auth();
                }
                let ok = outcome.succeeded();
                if let PollOutcome::Polled(report) = outcome {
                    self.notif_unread_count = report.unread_count;
                    if self.view == View::Inbox {
                        self.reload_inbox();
                    }
                    crate::sync::banner::post(crate::db::notifications::banners_to_fire(
                        &report.new_unread,
                        self.scheduler.is_focused(),
                        report.backfill,
                        self.config.native_notifications,
                    ));
                    self.last_notif_poll = Some(report);
                }
                let finish = self.drive_sync(SchedEvent::CycleFinished {
                    success: ok,
                });
                if self.sync_paused_auth {
                    return Task::batch([finish, self.drive_sync(SchedEvent::AuthPaused(true))]);
                }
                return finish;
            }

            Message::DismissToast(id) => {
                self.toasts.retain(|t| t.id != id);
            }

            Message::ToastTick => {
                // Expire non-error toasts after ~4s; errors persist until the
                // user dismisses or retries them.
                self.toasts
                    .retain(|t| t.sticky || t.kind == ToastKind::Error || t.created.elapsed().as_secs() < 4);
            }

            Message::ToastActed(id, inner) => {
                self.toasts.retain(|t| t.id != id);
                return self.update(*inner);
            }

            Message::RetrySyncFromToast(id) => {
                self.toasts.retain(|t| t.id != id);
                return self.drive_sync(SchedEvent::ManualSync);
            }

            Message::CreateProject => {
                if self.selected_team_id.is_none() {
                    return Task::none();
                }
                self.project_form = Some(projects_view::ProjectFormState::new());
                self.project_form_from_issue = false;
                self.modal = Modal::ProjectForm;
            }

            Message::ProjectNameChanged(v) => {
                if let Some(form) = self.project_form.as_mut() {
                    form.name = v;
                }
            }
            Message::ProjectDescriptionChanged(action) => {
                if let Some(form) = self.project_form.as_mut() {
                    form.description.apply(action);
                }
            }
            Message::ProjectStatusChanged(s) => {
                if let Some(form) = self.project_form.as_mut() {
                    form.status = s;
                }
            }
            Message::ProjectPriorityChanged(p) => {
                if let Some(form) = self.project_form.as_mut() {
                    form.priority = p;
                }
            }
            Message::ProjectLeadChanged(id) => {
                if let Some(form) = self.project_form.as_mut() {
                    form.lead_id = id;
                }
            }
            Message::ProjectStartDateChanged(v) => {
                if let Some(form) = self.project_form.as_mut() {
                    form.start_date = v;
                }
            }
            Message::ProjectDueDateChanged(v) => {
                if let Some(form) = self.project_form.as_mut() {
                    form.due_date = v;
                }
            }
            Message::ProjectRoadmapCommitmentChanged(c) => {
                if let Some(form) = self.project_form.as_mut() {
                    form.roadmap_commitment = c;
                }
            }

            // --- Projects table (list view) ---
            Message::OpenProjectDetail(project_id) => {
                // The project's own Tab: focused if open, else opened.
                let Ok(Some(project)) = queries::get_project(&self.db(), &project_id) else {
                    return Task::none();
                };
                let scroll = self.focus_tab(crate::tabs::TabKey::Project(project_id));
                return Task::batch([scroll, self.open_project_detail(&project)]);
            }
            Message::ProjectSortChanged(sort) => {
                if self.project_sort == sort {
                    self.project_sort_dir = self.project_sort_dir.toggled();
                } else {
                    self.project_sort = sort;
                    self.project_sort_dir = SortDir::Asc;
                }
            }
            Message::ProjectIssueSortChanged(sort) => {
                self.project_issue_sort = sort;
            }
            Message::ProjectIssueFilterChanged(filter) => {
                self.project_issue_filter = filter;
            }
            Message::ToggleHideCompletedProjects => {
                self.hide_completed_projects = !self.hide_completed_projects;
            }

            // --- Roadmap ---
            Message::OpenRoadmapAdd(commitment) => {
                self.modal = Modal::RoadmapAdd(commitment);
            }
            Message::RoadmapAddLaneChanged(commitment) => {
                if matches!(self.modal, Modal::RoadmapAdd(_)) {
                    self.modal = Modal::RoadmapAdd(commitment);
                }
            }
            Message::RoadmapAssignProject(project_id) => {
                if let Modal::RoadmapAdd(commitment) = self.modal {
                    self.set_roadmap_commitment(&project_id, Some(commitment));
                }
                self.modal = Modal::None;
            }
            Message::ToggleRoadmapMenu(project_id) => {
                self.roadmap_ui.menu =
                    (self.roadmap_ui.menu.as_ref() != Some(&project_id)).then_some(project_id);
            }
            Message::CloseRoadmapMenu => self.roadmap_ui.menu = None,
            Message::SetRoadmapCommitment(project_id, commitment) => {
                self.roadmap_ui.menu = None;
                self.set_roadmap_commitment(&project_id, commitment);
            }
            Message::RoadmapDragPress(project_id, from) => {
                self.roadmap_ui.menu = None;
                self.roadmap_ui.drag = Some(roadmap_view::RoadmapDrag::new(project_id, from));
            }
            Message::RoadmapDragMove(position) => {
                if let Some(drag) = self.roadmap_ui.drag.as_mut() {
                    drag.move_to(position);
                }
            }
            Message::RoadmapDragEnter(lane) => {
                if let Some(drag) = self.roadmap_ui.drag.as_mut() {
                    drag.enter(lane);
                }
            }
            Message::RoadmapDragLeave(lane) => {
                if let Some(drag) = self.roadmap_ui.drag.as_mut() {
                    drag.leave(lane);
                }
            }
            Message::RoadmapDragEnd => {
                if let Some(drag) = self.roadmap_ui.drag.take() {
                    match drag.outcome() {
                        roadmap_view::DragOutcome::Click(id) => {
                            return self.update(Message::OpenProjectDetail(id));
                        }
                        roadmap_view::DragOutcome::Move(id, lane) => {
                            self.set_roadmap_commitment(&id, Some(lane));
                        }
                        roadmap_view::DragOutcome::Cancel => {}
                    }
                }
            }

            // --- Project detail: buffered text fields ---
            Message::ProjectDetailNameChanged(v) => {
                if let Some(detail) = self.project_detail.as_mut() {
                    detail.name = v;
                }
            }
            Message::ProjectDetailDescriptionChanged(action) => {
                if let Some(detail) = self.project_detail.as_mut() {
                    detail.description.apply(action);
                }
            }
            Message::ProjectDetailStartDateChanged(v) => {
                if let Some(detail) = self.project_detail.as_mut() {
                    detail.start_date = v;
                }
            }
            Message::ProjectDetailDueDateChanged(v) => {
                if let Some(detail) = self.project_detail.as_mut() {
                    detail.due_date = v;
                }
            }
            Message::CommitProjectDetail => {
                self.commit_project_detail();
            }
            Message::ProjectDetailClickedOutside => {
                if let Some(d) = &mut self.project_detail {
                    d.editing_description = false;
                }
                self.commit_project_detail();
            }

            // --- Project detail: live-apply pick_lists ---
            Message::ProjectDetailStatusChanged(s) => {
                self.apply_project_update(|p| p.status = Some(s));
            }
            Message::ProjectDetailPriorityChanged(p) => {
                self.apply_project_update(|proj| proj.priority = p.to_i32());
            }
            Message::ProjectDetailLeadChanged(lead_id) => {
                self.apply_project_update(|p| p.lead_id = lead_id);
            }

            Message::SaveProject => {
                let mut created_project = None;
                if let Some(ref mut form) = self.project_form {
                    if form.name.trim().is_empty() {
                        form.error = Some("Name can't be blank".into());
                        return Task::none();
                    }
                    form.error = None;
                }
                if let Some(ref form) = self.project_form {
                    let conn = self.db();
                    let now = crate::db::now_iso();

                    let description_body = form.description_text();
                    let description = if description_body.trim().is_empty() {
                        None
                    } else {
                        Some(description_body.trim_end().to_string())
                    };
                    let start_date = if form.start_date.trim().is_empty() {
                        None
                    } else {
                        Some(form.start_date.trim().to_string())
                    };
                    let due_date = if form.due_date.trim().is_empty() {
                        None
                    } else {
                        Some(form.due_date.trim().to_string())
                    };

                    let team_id = self.selected_team_id.clone().unwrap_or_default();
                    let project = Project {
                        id: uuid::Uuid::new_v4().to_string(),
                        remote_id: None,
                        team_id,
                        name: form.name.trim().to_string(),
                        description,
                        status: Some(form.status),
                        priority: form.priority.to_i32(),
                        lead_id: form.lead_id.clone(),
                        start_date,
                        due_date,
                        roadmap_commitment: form.roadmap_commitment,
                        velocity_score: 0,
                        completed_issues_count: 0,
                        total_issues_count: 0,
                        progress_percentage: 0,
                        behind_schedule: false,
                        created_at: now.clone(),
                        updated_at: now,
                    };
                    let _ = queries::insert_project(&conn, &project);

                    let payload = serde_json::to_string(&project).ok();
                    self.enqueue_sync("project", &project.id, "create", payload.as_deref());
                    created_project = Some(project.id);
                }
                self.modal = Modal::None;
                self.project_form = None;
                // Made from the issue form's "New Project": back to it, picked.
                if std::mem::take(&mut self.project_form_from_issue) {
                    if let Some(form) = self.issue_form.as_mut() {
                        form.project_id = created_project;
                        self.modal = Modal::IssueForm;
                    }
                }
                self.reload_data();
            }

            Message::ConfirmDeleteComment(comment_id) => {
                self.modal = Modal::DeleteCommentConfirm(comment_id);
            }

            Message::DeleteCommentConfirmed => {
                if let Modal::DeleteCommentConfirm(ref comment_id) = self.modal {
                    let conn = self.db();
                    if let Ok((_, Some(payload))) = queries::delete_comment(&conn, comment_id) {
                        self.enqueue_sync(crate::db::attachments::COMMENT_ENTITY, comment_id, "delete", Some(payload.as_str()));
                    }
                    if let Some(d) = &mut self.issue_detail {
                        if d.reply_to.as_deref() == Some(comment_id.as_str()) {
                            d.reply_to = None;
                        }
                    }
                }
                self.modal = Modal::None;
                self.reload_data();
            }

            Message::ConfirmDeleteAttachment(id) => {
                if self.sync_read_only {
                    self.push_toast(
                        "Read-only token: can't delete an attachment",
                        ToastKind::Error,
                        false,
                    );
                } else {
                    self.modal = Modal::DeleteAttachmentConfirm(id);
                }
            }

            Message::CancelAttachmentUpload(id) => {
                let _ = crate::db::attachments::remove_local(&self.db(), &id);
                self.pending_sync_count = sync_queue::pending_count(&self.db()).unwrap_or(0);
                self.reload_data();
            }

            Message::DeleteAttachmentConfirmed => {
                if let Modal::DeleteAttachmentConfirm(ref id) = self.modal {
                    let conn = self.db();
                    if let Ok(Some(payload)) =
                        crate::db::attachments::remove_synced_attachment(&conn, id)
                    {
                        self.enqueue_sync(crate::db::attachments::ATTACHMENT_ENTITY, id, "delete", Some(payload.as_str()));
                    }
                }
                self.modal = Modal::None;
                self.reload_data();
            }

            Message::RetryUpload(id) => {
                let conn = self.db();
                let _ = crate::db::attachments::retry_failed(&conn, &id);
                self.load_detail_dependencies(&conn);
                self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
            }

            Message::RemoveFailedUpload(id) => {
                let conn = self.db();
                let row = crate::db::attachments::get(&conn, &id)
                    .ok()
                    .flatten()
                    .filter(|a| a.upload_state == crate::db::attachments::UploadState::Failed);
                if let Some(row) = row {
                    if let Ok(true) = crate::db::attachments::remove_local(&conn, &id) {
                        self.strip_removed_inline(&conn, &row);
                    }
                }
                self.load_detail_dependencies(&conn);
                self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
            }

            Message::OpenLabels => {
                // Scope the modal to the focused issue when there is one, so its
                // labels can be toggled; otherwise open as a team-label browser.
                let (issue_id, selected) = match self.focused_issue() {
                    Some(issue) => {
                        let ids =
                            queries::label_ids_for_issue(&self.db(), &issue.id).unwrap_or_default();
                        (Some(issue.id.clone()), ids)
                    }
                    None => (None, Vec::new()),
                };
                self.label_form = Some(labels_view::LabelFormState::new(issue_id, selected));
                self.modal = Modal::Labels;
            }

            Message::LabelNameChanged(v) => {
                if let Some(ref mut form) = self.label_form {
                    form.name = v;
                }
            }

            Message::LabelColorChanged(hex) => {
                if let Some(ref mut form) = self.label_form {
                    form.color = hex;
                }
            }

            Message::CreateLabel => {
                if let Some(ref mut form) = self.label_form {
                    if form.name.trim().is_empty() {
                        form.error = Some("Name can't be blank".into());
                        return Task::none();
                    }
                    form.error = None;
                }
                if let Some(ref form) = self.label_form {
                    let name = form.name.trim().to_string();
                    let Some(team_id) = self.selected_team_id.clone() else {
                        return Task::none();
                    };
                    let conn = self.db();
                    let now = crate::db::now_iso();
                    let label = Label {
                        id: uuid::Uuid::new_v4().to_string(),
                        remote_id: None,
                        team_id,
                        name,
                        color: Some(form.color.clone()),
                        created_at: now.clone(),
                        updated_at: now,
                    };
                    if queries::insert_label(&conn, &label).is_ok() {
                        self.enqueue_sync("label", &label.id, "create", None);
                        // Clear the name for the next entry but keep the modal
                        // open and the color choice sticky.
                        if let Some(ref mut form) = self.label_form {
                            form.name.clear();
                        }
                        self.reload_data();
                    }
                }
            }

            Message::SelectLabelForEdit(label_id) => {
                let label = self.labels.iter().find(|l| l.id == label_id).cloned();
                if let (Some(form), Some(label)) = (self.label_form.as_mut(), label) {
                    form.editing_id = Some(label.id);
                    form.edit_name = label.name;
                    form.edit_color = label
                        .color
                        .unwrap_or_else(|| labels_view::PALETTE[0].to_string());
                    form.edit_error = None;
                }
            }

            Message::EditLabelNameChanged(v) => {
                if let Some(ref mut form) = self.label_form {
                    form.edit_name = v;
                }
            }

            Message::EditLabelColorChanged(hex) => {
                if let Some(ref mut form) = self.label_form {
                    form.edit_color = hex;
                }
            }

            Message::SaveLabelEdit => {
                let Some((id, name, color)) = self
                    .label_form
                    .as_ref()
                    .and_then(|f| Some((f.editing_id.clone()?, f.edit_name.clone(), f.edit_color.clone())))
                else {
                    return Task::none();
                };
                match queries::edit_label(&self.db(), &id, &name, &color) {
                    Ok(needs_update) => {
                        if needs_update {
                            self.enqueue_sync("label", &id, "update", None);
                        }
                        if let Some(ref mut form) = self.label_form {
                            form.editing_id = None;
                            form.edit_error = None;
                        }
                        self.reload_data();
                    }
                    Err(e) => {
                        if let Some(ref mut form) = self.label_form {
                            form.edit_error = Some(e.message().into());
                        }
                    }
                }
            }

            Message::ConfirmDeleteLabel => {
                if let Some(id) = self.label_form.as_ref().and_then(|f| f.editing_id.clone()) {
                    self.modal = Modal::DeleteLabelConfirm(id);
                }
            }

            Message::ToggleRecurringActive(id, active) => {
                let conn = self.db();
                let _ = crate::db::recurring_issues::set_active(&conn, &id, active);
                self.reload_recurring(&conn);
            }

            Message::NewRecurringIssue => {
                if self.selected_team_id.is_some() && self.modal == Modal::None {
                    self.recurring_form = Some(
                        crate::views::recurring_issue_form::RecurringFormState::new(
                            chrono::Local::now().date_naive(),
                        )
                        .with_routing_options(&self.members, &self.projects, &self.lanes),
                    );
                    self.modal = Modal::RecurringForm;
                }
            }
            Message::EditRecurringIssue(id) => {
                if self.modal == Modal::None {
                    let conn = self.db();
                    if let Ok(Some(row)) = crate::db::recurring_issues::get(&conn, &id) {
                        let labels =
                            crate::db::recurring_issues::label_ids(&conn, &id).unwrap_or_default();
                        self.recurring_form = Some(
                            crate::views::recurring_issue_form::RecurringFormState::for_edit(
                                &row,
                                labels,
                                chrono::Local::now().date_naive(),
                            )
                            .with_routing_options(&self.members, &self.projects, &self.lanes),
                        );
                        self.modal = Modal::RecurringForm;
                    }
                }
            }
            Message::RecurringFormTitleChanged(v) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.title = v;
                }
            }
            Message::RecurringFormDescriptionChanged(action) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.description.apply(action);
                }
            }
            Message::RecurringFormTimeZoneChanged(z) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.time_zone = z;
                }
            }
            Message::RecurringFormPriorityChanged(p) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.priority = p;
                }
            }
            Message::RecurringFormFrequencyChanged(v) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.frequency = v;
                }
            }
            Message::RecurringFormIntervalChanged(v) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.interval = v;
                }
            }
            Message::RecurringFormWeekdayChanged(v) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.weekday = v;
                }
            }
            Message::RecurringFormDayOfMonthChanged(v) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.day_of_month = v;
                }
            }
            Message::RecurringFormStartsOnChanged(v) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.starts_on = v;
                    f.starts_on_touched = true;
                }
            }
            Message::RecurringFormAssigneeChanged(v) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.assignee = v;
                }
            }
            Message::RecurringFormProjectChanged(v) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.project = v;
                }
            }
            Message::RecurringFormLaneChanged(v) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.lane = v;
                }
            }
            Message::RecurringFormLabelToggled(label_id) => {
                if let Some(f) = self.recurring_form.as_mut() {
                    f.toggle_label(&label_id);
                }
            }
            Message::SaveRecurringIssue => {
                let (Some(form), Some(team_id)) =
                    (self.recurring_form.as_mut(), self.selected_team_id.clone())
                else {
                    return Task::none();
                };
                if let Some(edit_id) = form.editing.clone() {
                    match crate::views::recurring_issue_form::validate_edit(form) {
                        Err(errors) => form.errors = *errors,
                        Ok(edit) => {
                            let conn = self.db();
                            if crate::db::recurring_issues::update_content(&conn, &edit_id, &edit)
                                .is_ok()
                            {
                                self.modal = Modal::None;
                                self.recurring_form = None;
                                self.pending_sync_count =
                                    sync_queue::pending_count(&conn).unwrap_or(0);
                                self.reload_recurring(&conn);
                                self.reload_detail_origin(&conn);
                            }
                        }
                    }
                    return Task::none();
                }
                match crate::views::recurring_issue_form::validate(form) {
                    Err(errors) => form.errors = *errors,
                    Ok(new) => {
                        let conn = self.db();
                        if crate::db::recurring_issues::create(&conn, &team_id, &new).is_ok() {
                            self.modal = Modal::None;
                            self.recurring_form = None;
                            self.pending_sync_count =
                                sync_queue::pending_count(&conn).unwrap_or(0);
                            self.reload_recurring(&conn);
                        }
                    }
                }
            }

            Message::DeleteRecurring(id) => {
                self.modal = Modal::DeleteRecurringConfirm(id);
            }

            Message::DeleteRecurringConfirmed => {
                if let Modal::DeleteRecurringConfirm(ref id) = self.modal {
                    let conn = self.db();
                    let _ = crate::db::recurring_issues::delete_local(&conn, id);
                    self.modal = Modal::None;
                    self.reload_recurring(&conn);
                    self.reload_detail_origin(&conn);
                }
            }

            Message::DeleteLabelConfirmed => {
                if let Modal::DeleteLabelConfirm(ref label_id) = self.modal {
                    let label_id = label_id.clone();
                    if let Ok(Some(payload)) = queries::delete_label(&self.db(), &label_id) {
                        self.enqueue_sync("label", &label_id, "delete", Some(payload.as_str()));
                    }
                    if let Some(ref mut form) = self.label_form {
                        form.selected_label_ids.retain(|id| *id != label_id);
                        form.editing_id = None;
                        form.edit_error = None;
                    }
                }
                self.modal = Modal::Labels;
                self.reload_data();
            }

            Message::ToggleIssueLabel(label_id) => {
                if let Some(ref mut form) = self.label_form {
                    if let Some(pos) = form
                        .selected_label_ids
                        .iter()
                        .position(|id| *id == label_id)
                    {
                        form.selected_label_ids.remove(pos);
                    } else {
                        form.selected_label_ids.push(label_id);
                    }
                }
            }

            Message::SaveIssueLabels => {
                if let Some(ref form) = self.label_form {
                    if let Some(ref issue_id) = form.issue_id {
                        let conn = self.db();
                        if queries::edit_issue_queued(&conn, issue_id, "update", None, |c| {
                            queries::set_issue_labels(c, issue_id, &form.selected_label_ids)
                        })
                        .is_err()
                        {
                            self.push_toast("Couldn't save the labels", ToastKind::Error, false);
                        }
                    }
                }
                self.modal = Modal::None;
                self.label_form = None;
                self.reload_data();
            }
        }

        Task::none()
    }

    /// Keep `hover_content` in step with the card on screen: read the local
    /// database once when a card opens on a new anchor, drop it when the card
    /// closes. Called where a card can open (the show timer), not per frame.
    fn refresh_hover_content(&mut self) {
        let Some((anchor, _)) = self.body_hover.current() else {
            self.hover_content = None;
            return;
        };
        if self.hover_content.as_ref().is_some_and(|(a, _)| a == anchor) {
            return;
        }
        let anchor = anchor.clone();
        let content = match &anchor {
            crate::body_hover::Anchor::Issue(id) => {
                let conn = self.db();
                crate::db::queries::card_records(&conn, id)
                    .ok()
                    .and_then(|r| crate::body_hover::content_for(&anchor, &r.records()))
            }
            crate::body_hover::Anchor::Project(id) => {
                let conn = self.db();
                crate::db::queries::project_card_records(&conn, id)
                    .ok()
                    .and_then(|r| crate::body_hover::content_for(&anchor, &r.records()))
            }
            // A mention resolves on the body's team, whichever team is
            // selected: that team's members and issues, read once as the
            // card opens.
            crate::body_hover::Anchor::Mention(_) => self.detail_team_id().and_then(|team_id| {
                let conn = self.db();
                crate::db::queries::mention_records(&conn, team_id)
                    .ok()
                    .and_then(|r| crate::body_hover::content_for(&anchor, &r.records()))
            }),
        };
        self.hover_content = Some((anchor, content));
    }

    /// The hover card for the current anchor, positioned under its span.
    fn hover_card_overlay(&self) -> Option<Element<'_, Message>> {
        if self.modal != Modal::None {
            return None;
        }
        let (anchor, rect) = self.body_hover.current()?;
        let (cached, content) = self.hover_content.as_ref()?;
        if cached != anchor {
            return None;
        }
        let content = content.as_ref()?;
        Some(crate::views::hover_card_view::overlay(content, rect))
    }

    pub fn view(&self) -> Element<'_, Message> {
        let open_terminals: HashMap<String, usize> = self
            .teams
            .iter()
            .map(|t| (t.id.clone(), self.terminals.open_count(&t.id)))
            .filter(|(_, n)| *n > 0)
            .collect();
        let terminal_needs: HashMap<String, crate::terminal::Need> = self
            .teams
            .iter()
            .filter_map(|t| Some((t.id.clone(), self.terminals.team_need(&t.id)?)))
            .collect();
        let team_sidebar = team_list::view(team_list::SidebarState {
            open_terminals,
            terminal_needs,
            teams: &self.teams,
            selected_team_id: self.selected_team_id.as_deref(),
            expanded_teams: &self.expanded_teams,
            current_view: &self.view,
            team_menu_open: self.team_menu_open.as_deref(),
            show_archived: self.show_archived_teams,
            unread_count: self.notif_unread_count,
            read_only: self.sync_read_only,
            saved_views: &self.saved_views,
            open_view_id: self.open_view.as_ref().map(|v| v.view_id.as_str()),
            home: self.home_page.as_ref(),
            sidebar: self.sidebar.as_ref(),
            dashboards: &self.dashboards,
            open_dashboard_id: match &self.view {
                View::Dashboard(id) => Some(id.as_str()),
                _ => None,
            },
        });
        let sidebar_column = column![
            iced::widget::scrollable(team_sidebar).height(Length::Fill),
            team_list::footer(self.selected_team_id.is_some(), self.updater.view().needs_attention()),
        ];

        let search = self.active_search();
        let needle = crate::search_match::needle(search);
        let header = if let View::Dashboard(id) = &self.view {
            let dashboard = self.dashboards.iter().find(|d| d.id == *id);
            header::dashboard_bar(
                self.breadcrumb(),
                dashboard.map(|d| {
                    crate::views::dashboard::dashboard_menu(
                        d,
                        self.dashboard_menu.as_ref(),
                        self.sync_read_only,
                        self.home_page.as_ref().is_some_and(|h| h.matches_dashboard(d)),
                    )
                }),
            )
        } else {
            header::view(
            self.breadcrumb(),
            !matches!(
                self.view,
                View::ProjectDetail(_)
                    | View::IssueDetail(_)
                    | View::DependencyMap { .. }
                    | View::ProjectDependencyMap { .. }
            ),
            search,
            self.search_enabled(),
            matches!(self.view, View::Board | View::MyIssues)
            .then(|| self.active_filter_count()),
            self.new_action(),
            self.back_message(),
            self.update_view_action(),
            )
        };

        let (columns, hidden_columns) = if matches!(self.view, View::Board | View::MyIssues) {
            self.board_layout()
        } else {
            (Vec::new(), Vec::new())
        };

        let my_empty = if self.view == View::MyIssues {
            self.my_issues_empty()
        } else {
            None
        };
        // The Terminal panel: under the page, or down its right side. On the
        // project page a right-docked panel sits between the page and its
        // Properties, so the page view takes it.
        let panel_scope = self.panel_scope();
        let dock_right = self.terminals.dock() == crate::terminal::panel::Dock::Right;
        let mut terminal_panel = self.terminals.panel_view(panel_scope.as_ref());
        let terminal_width = self.terminals.right_width(panel_scope.as_ref());
        let mut right_panel = if dock_right { terminal_panel.take() } else { None };

        let main_pane: Element<'_, Message> = match self.view {
            View::MyIssues if my_empty.is_some() => iced::widget::container(
                crate::views::text(my_empty.unwrap_or_default())
                    .size(14)
                    .color(t::text_muted()),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into(),
            View::Board | View::MyIssues if self.board_display().view_mode == ViewMode::List => {
                issue_list::view(
                    &columns,
                    board::CardData {
                        teams: &self.teams,
                        lanes: &self.lanes,
                        members: &self.members,
                        projects: &self.projects,
                        issue_labels: &self.issue_labels,
                        comment_counts: &self.comment_counts,
                        block_counts: &self.block_counts,
                        display: self.board_display(),
                        hovered: self.hovered_card.as_ref().map(|h| h.issue_id.as_str()),
                        deps: None,
                    },
                    &self.collapsed_sections,
                    self.focused_lane,
                    self.focused_issue,
                )
            }
            View::Board | View::MyIssues => {
                let dep_lines = self.board_display().show_dependencies.then(|| {
                    crate::views::dep_overlay::DepLines::new(
                        columns.iter().flat_map(|c| c.issues.iter().map(|i| i.id.as_str())),
                        &self.board_deps,
                    )
                });
                let board_el = board::view(
                &columns,
                board::CardData {
                    teams: &self.teams,
                    lanes: &self.lanes,
                    members: &self.members,
                    projects: &self.projects,
                    issue_labels: &self.issue_labels,
                    comment_counts: &self.comment_counts,
                    block_counts: &self.block_counts,
                    display: self.board_display(),
                    hovered: self.hovered_card.as_ref().map(|h| h.issue_id.as_str()),
                    deps: dep_lines,
                },
                &self.swimlane_collapsed(&columns),
                &hidden_columns,
                self.focused_lane,
                self.focused_issue,
                );
                if self.board_display().show_dependencies {
                    // Legend bar under the header (web: #dependency_legend).
                    iced::widget::column![
                        iced::widget::container(crate::views::dep_map_canvas::legend())
                            .padding([6, 16])
                            .width(Length::Fill),
                        board_el
                    ]
                    .into()
                } else {
                    board_el
                }
            }
            View::ProjectList => {
                let mut list: Vec<&Project> = self
                    .projects
                    .iter()
                    .filter(|p| {
                        !self.hide_completed_projects
                            || !matches!(
                                p.status,
                                Some(ProjectStatus::Completed | ProjectStatus::Cancelled)
                            )
                    })
                    .filter(|p| header::project_matches(p, &needle))
                    .collect();
                sort_projects(&mut list, self.project_sort, self.project_sort_dir);
                projects_view::list_view(
                    &list,
                    &self.members,
                    self.project_sort,
                    self.project_sort_dir,
                    self.hide_completed_projects,
                    &self.issues,
                )
            }
            View::ProjectDetail(ref project_id) => {
                match (
                    self.projects.iter().find(|p| p.id == *project_id),
                    &self.project_detail,
                ) {
                    (Some(project), Some(detail)) if detail.project_id == *project_id => {
                        project_detail::view(
                            project,
                            // A project Tab's root is the project: no Back.
                            !matches!(self.active_tab_key(), Some(crate::tabs::TabKey::Project(_))),
                            detail,
                            self.detail_team(),
                            &self.detail_members,
                            &self.detail_comment_authors,
                            &self.issues,
                            self.project_detail_issues(project_id, &needle),
                            self.board_display(),
                            self.project_issue_sort,
                            self.project_issue_filter,
                            &self.lanes,
                            &self.teams,
                            &self.detail_comments,
                            &self.detail_attachments,
                            &self.detail_comment_attachments,
                            &self.images,
                            &self.config.api_base_url,
                            &self.detail_pending_paths,
                            self.window_width - self.sidebar_width(),
                            self.config.project_properties_open,
                            self.config.project_cards_stacked,
                            terminal_width,
                            self.more_properties,
                            self.terminals.panel_showing(panel_scope.as_ref()),
                            right_panel.take(),
                            self.date_picker.as_ref(),
                        )
                    }
                    // Project removed by a background sync, or state desync → fallback.
                    _ => placeholder::view("Project"),
                }
            }
            View::ProjectDependencyMap { ref project_id, active_only } => {
                match (self.projects.iter().find(|p| p.id == *project_id), &self.project_map) {
                    (Some(project), Some(data)) => {
                        crate::views::project_dependency_map::view(project, data, active_only)
                    }
                    _ => placeholder::view("Project dependency map"),
                }
            }
            View::DependencyMap { ref issue_id, tab, depth } => {
                match self.issues.iter().find(|i| i.id == *issue_id) {
                    Some(issue) => crate::views::dependency_map::view(
                        issue,
                        tab,
                        depth,
                        &crate::views::dependency_map::MapData {
                            issues: &self.issues,
                            lanes: &self.lanes,
                            teams: &self.teams,
                            members: &self.members,
                            issue_labels: &self.issue_labels,
                            deps: &self.map_deps,
                            flipped: &self.map_flipped,
                        },
                    ),
                    // Issue removed by a background sync, or state desync → fallback.
                    None => placeholder::view("Dependency map"),
                }
            }
            View::IssueDetail(ref issue_id) => {
                match (
                    self.issues.iter().find(|i| i.id == *issue_id),
                    &self.issue_detail,
                ) {
                    (Some(issue), Some(detail)) if detail.issue_id == *issue_id => {
                        let project_name = issue
                            .project_id
                            .as_ref()
                            .and_then(|pid| self.projects.iter().find(|p| p.id == *pid))
                            .map(|p| p.name.as_str());
                        let parent = issue
                            .parent_issue_id
                            .as_ref()
                            .and_then(|pid| self.issues.iter().find(|i| i.id == *pid));
                        let sub_issues: Vec<&Issue> = self
                            .issues
                            .iter()
                            .filter(|i| i.parent_issue_id.as_deref() == Some(issue.id.as_str()))
                            .collect();
                        let my_member_ids = display::my_member_ids(
                            &self.members,
                            self.config.current_user_remote_id,
                        );
                        issue_detail::view(
                            issue,
                            detail,
                            self.detail_team(),
                            &self.teams,
                            &self.lanes,
                            &self.projects,
                            &self.detail_members,
                            &self.detail_comment_authors,
                            project_name,
                            &self.detail_deps,
                            &self.detail_comments,
                            &self.detail_activity,
                            &self.detail_references,
                            &self.detail_attachments,
                            &self.detail_comment_attachments,
                            &self.images,
                            &self.issues,
                            parent,
                            sub_issues,
                            self.detail_origin.as_deref(),
                            &my_member_ids,
                            &self.config.api_base_url,
                            &self.detail_pending_paths,
                            match &self.modal {
                                Modal::PropertyPicker(p) if p.in_sidebar && p.issue_id == issue.id => {
                                    Some(p)
                                }
                                _ => None,
                            },
                            self.config.issue_properties_open,
                            terminal_width,
                            self.more_properties,
                            self.terminals.panel_showing(panel_scope.as_ref()),
                        )
                    }
                    // Issue removed by a background sync, or state desync → graceful fallback.
                    _ => placeholder::view("Issue"),
                }
            }
            View::Roadmap => {
                roadmap_view::view(&self.projects, &self.issues, self.selected_team(), &needle, &self.roadmap_ui)
            }
            View::RecurringIssues => crate::views::recurring_issues::view(&self.recurring_rows),
            View::Inbox => crate::views::inbox::view(&self.inbox),
            View::Dashboard(ref id) => {
                let evaluation = self.dashboard_evaluation();
                crate::views::dashboard::view(
                    self.dashboards.iter().find(|d| d.id == *id),
                    &evaluation,
                    board::CardData {
                        teams: &self.teams,
                        lanes: &self.lanes,
                        members: &self.members,
                        projects: &self.projects,
                        issue_labels: &self.issue_labels,
                        comment_counts: &self.comment_counts,
                        block_counts: &self.block_counts,
                        display: DisplayOptions::dashboard_rows(),
                        hovered: None,
                        deps: None,
                    },
                    self.dashboard_header.clone(),
                    &crate::views::dashboard::ViewState {
                        collapsed: self.collapsed_dashboard_groups.get(id).unwrap_or(&HashSet::new()),
                        focused_group: self.focused_lane,
                        focused_issue: self.focused_issue,
                        read_only: self.sync_read_only,
                        search,
                        open_menu: self.dashboard_menu.as_ref(),
                    },
                )
            }
        };

        let token_configured = self
            .config
            .api_token
            .as_deref()
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false);
        let status = status_bar::view(
            self.pending_sync_count,
            self.show_help,
            self.config_error.as_deref(),
            self.last_sync_error.as_deref(),
            self.sync_in_progress,
            self.config.last_synced_at,
            token_configured,
            &self.inaccessible_teams,
            self.sync_read_only,
            self.view == View::Inbox,
            matches!(self.view, View::ProjectDetail(_)),
            crate::views::update_chip::chip(&self.updater.view()),
        );

        // An open create/edit form takes the page, as on the web (JRST-454).
        let (header, main_pane) = match self.form_page_view() {
            Some((crumbs, page)) => (
                header::view(crumbs, false, "", false, None, None, Some(Message::CloseModal), None),
                page,
            ),
            None => (header, main_pane),
        };

        let main_pane = if self.removed_team_tab() {
            iced::widget::container(
                crate::views::text(
                    "This team is no longer available. Its Terminals keep running below until you close them.",
                )
                .size(14)
                .color(t::text_muted()),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
        } else {
            main_pane
        };

        // The main pane is one of the window's base layers (see `theme::base`);
        // the sidebar, bars, Terminal panel and status bar are the others.
        let main_pane: Element<'_, Message> = iced::widget::container(main_pane)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_theme: &Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(t::base(t::bg_primary()))),
                ..Default::default()
            })
            .into();

        let main_pane: Element<'_, Message> = match right_panel {
            Some(panel) => row![main_pane, panel].width(Length::Fill).height(Length::Fill).into(),
            None => main_pane,
        };

        let main_content = column![]
            .push(tab_bar::view(
                &self.tab_labels(),
                &self.tab_needs(),
                &self.tab_unread(),
                self.window_width,
                self.config.sidebar_collapsed,
                self.tab_drag.as_ref().filter(|d| d.moved).map(|d| d.index),
                !self.closed_tabs.is_empty(),
            ))
            .push_maybe((!self.removed_team_tab()).then_some(header))
            .push(main_pane)
            .push_maybe(terminal_panel)
            .push(status)
            .width(Length::Fill)
            .height(Length::Fill);

        let sidebar_w = self.sidebar_width();
        let layout = row![]
            .push_maybe((sidebar_w > 0.0).then(|| {
                container(sidebar_column)
                    .width(Length::Fixed(sidebar_w))
                    .height(Length::Fill)
                    .style(|_theme: &Theme| iced::widget::container::Style {
                        background: Some(iced::Background::Color(t::base(t::bg_menu()))),
                        border: iced::Border {
                            color: t::border(),
                            width: 0.0,
                            radius: 0.0.into(),
                        },
                        ..Default::default()
                    })
            }))
            .push(main_content)
            .width(Length::Fill)
            .height(Length::Fill);

        let base: Element<'_, Message> = container(layout)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_theme: &Theme| iced::widget::container::Style {
                // Under the base layers while opaque; dropped while translucent.
                background: t::repaint(t::bg_primary()),
                ..Default::default()
            })
            .into();

        // Overlay modals
        let content: Element<'_, Message> = match &self.modal {
            Modal::DisplayOptions => {
                let panel = display_panel::display_view(self.board_display(), self.save_view_block_reason());
                iced::widget::stack![base, display_panel::popover(panel)].into()
            }
            Modal::FilterPanel => {
                let panel = display_panel::filter_view(
                    self.board_filters(),
                    search,
                    display_panel::FilterData {
                        lanes: &self.lanes,
                        members: &self.members,
                        labels: &self.labels,
                        projects: &self.projects,
                        cross_team: self.view == View::MyIssues,
                        scoped_to_project: matches!(self.view, View::ProjectDetail(_)),
                        // Rails can't store "unassigned" in a Saved view.
                        offer_unassigned: self.open_view.is_none(),
                    },
                );
                iced::widget::stack![base, display_panel::popover(panel)].into()
            }
            // Full pages in the main pane (see `form_page_view`), not overlays.
            Modal::IssueForm | Modal::ProjectForm | Modal::RecurringForm => base,
            Modal::Labels => {
                if let Some(ref form) = self.label_form {
                    let modal = labels_view::modal_view(form, &self.labels);
                    iced::widget::stack![base, modal].into()
                } else {
                    base
                }
            }
            Modal::CsvImport => match self.csv_import {
                Some(ref state) => iced::widget::stack![base, csv_import::view(state)].into(),
                None => base,
            },
            Modal::DeleteConfirm(issue_id) => {
                let issue_title = self
                    .issues
                    .iter()
                    .find(|i| i.id == *issue_id)
                    .map(|i| i.title.as_str())
                    .unwrap_or("this issue");

                let confirm = delete_confirm_modal(issue_title);
                iced::widget::stack![base, confirm].into()
            }
            Modal::QuitConfirm => {
                let pending = self.updater.view().ready.map(|r| r.version);
                let pending = crate::updater::swap::quit_plan(&self.install, pending.as_ref())
                    .map(|(_, _, v)| v);
                let confirm = crate::views::terminal_panel::confirm_modal(
                    "Quit JAIT?",
                    crate::updater::swap::quit_confirm_body(pending.as_ref()),
                    "Quit",
                    Message::QuitConfirmed,
                );
                iced::widget::stack![base, confirm].into()
            }
            Modal::RestartConfirm(version) => {
                let confirm = crate::views::terminal_panel::confirm_modal_with(
                    "Restart to update?",
                    crate::updater::swap::restart_confirm_body(version),
                    "Restart",
                    Message::RestartConfirmed,
                    "Not now",
                    Message::CloseModal,
                );
                iced::widget::stack![base, confirm].into()
            }
            Modal::TabCloseConfirm(key) => {
                let names = self
                    .tab_terminal_scope(key)
                    .map(|s| self.terminals.busy_names(&s))
                    .unwrap_or_default();
                let body = match names.as_slice() {
                    [one] => format!("A program is still running in \"{one}\". Closing this Tab will stop it."),
                    many => format!(
                        "Programs are still running in {} Terminals in this Tab. Closing it will stop them.",
                        many.len()
                    ),
                };
                let confirm = crate::views::terminal_panel::confirm_modal(
                    "Close Tab?",
                    body,
                    "Close",
                    Message::Tab(TabMessage::CloseConfirmed(key.clone())),
                );
                iced::widget::stack![base, confirm].into()
            }
            Modal::TerminalCloseConfirm(id) => {
                let name = self.terminals.name_of(*id).unwrap_or_else(|| "this Terminal".into());
                let confirm = crate::views::terminal_panel::confirm_modal(
                    "Close Terminal?",
                    format!("A program is still running in \"{name}\". Closing it will stop the program."),
                    "Close",
                    Message::Terminal(crate::terminal::TerminalMessage::CloseConfirmed(*id)),
                );
                iced::widget::stack![base, confirm].into()
            }
            Modal::UpdatePopover => match self.updater.view().ready {
                Some(ready) => {
                    let ctx = crate::views::body::BodyCtx {
                        teams: &self.teams,
                        issues: &[],
                        members: &[],
                        images: &self.images,
                        host: &self.config.api_base_url,
                        pending_paths: None,
                        on_image: None,
                    };
                    let pop = crate::views::update_chip::popover(&ready, self.updater.installed(), &ctx);
                    iced::widget::stack![base, pop].into()
                }
                None => base,
            },
            Modal::Settings => {
                if let Some(ref form) = self.settings_form {
                    let ctx = crate::views::body::BodyCtx {
                        teams: &self.teams,
                        issues: &[],
                        members: &[],
                        images: &self.images,
                        host: &self.config.api_base_url,
                        pending_paths: None,
                        on_image: None,
                    };
                    let modal = settings::view(
                        form,
                        &settings::UpdatesInfo {
                            installed: self.updater.installed().clone(),
                            view: self.updater.view(),
                            busy: self.updater.is_busy(),
                            auto_check: self.updater.auto_check(),
                        },
                        &ctx,
                    );
                    iced::widget::stack![base, modal].into()
                } else {
                    base
                }
            }
            Modal::NewDashboard => iced::widget::stack![
                base,
                dashboard_form_modal(
                    "New dashboard",
                    "Create",
                    Message::NewDashboardSubmit,
                    &self.dashboard_form
                )
            ]
            .into(),
            Modal::EditDashboard => iced::widget::stack![
                base,
                dashboard_form_modal(
                    "Edit dashboard",
                    "Save",
                    Message::EditDashboardSubmit,
                    &self.dashboard_form
                )
            ]
            .into(),
            Modal::NewGroup => iced::widget::stack![
                base,
                crate::views::group_form::modal(
                    &self.group_form.draft,
                    &self.teams,
                    &self.projects,
                    &self.labels,
                    self.group_form.error,
                    false
                )
            ]
            .into(),
            Modal::EditGroup(_) => iced::widget::stack![
                base,
                crate::views::group_form::modal(
                    &self.group_form.draft,
                    &self.teams,
                    &self.projects,
                    &self.labels,
                    self.group_form.error,
                    true
                )
            ]
            .into(),
            Modal::SaveView => iced::widget::stack![
                base,
                save_view_modal("Save view", &self.save_view_name, self.save_view_error)
            ]
            .into(),
            Modal::RenameSavedView(_) => iced::widget::stack![
                base,
                save_view_modal("Rename view", &self.save_view_name, self.save_view_error)
            ]
            .into(),
            Modal::DeleteDashboardConfirm(id) => {
                let name = self
                    .dashboards
                    .iter()
                    .find(|d| d.id == *id)
                    .map(|d| d.name.clone())
                    .unwrap_or_default();
                let body = format!(
                    "Delete the dashboard \"{name}\" and its groups? Issues are not affected."
                );
                iced::widget::stack![
                    base,
                    delete_modal("Delete Dashboard?", body, Message::DeleteDashboardConfirmed)
                ]
                .into()
            }
            Modal::DeleteDashboardGroupConfirm(id) => {
                let name = self
                    .dashboard_groups
                    .iter()
                    .find(|c| c.group.id == *id)
                    .map(|c| c.group.name.clone())
                    .unwrap_or_default();
                let body = format!("Delete the group \"{name}\"? Issues are not affected.");
                iced::widget::stack![
                    base,
                    delete_modal("Delete Group?", body, Message::DeleteDashboardGroupConfirmed)
                ]
                .into()
            }
            Modal::DeleteSavedViewConfirm(view_id) => {
                let name = self
                    .saved_views
                    .values()
                    .flatten()
                    .find(|v| v.id == *view_id)
                    .map(|v| v.name.clone())
                    .unwrap_or_default();
                let body = format!("Delete the view \"{name}\"? Issues are not affected.");
                iced::widget::stack![
                    base,
                    delete_modal("Delete View?", body, Message::DeleteSavedViewConfirmed)
                ]
                .into()
            }
            Modal::DeleteLabelConfirm(label_id) => {
                let conn = self.db();
                let name = queries::get_label(&conn, label_id)
                    .ok()
                    .flatten()
                    .map(|l| l.name)
                    .unwrap_or_default();
                let count = queries::label_issue_count(&conn, label_id).unwrap_or(0);
                iced::widget::stack![base, delete_label_confirm_modal(name, count)].into()
            }
            Modal::DeleteRecurringConfirm(id) => {
                let title = self
                    .recurring_rows
                    .iter()
                    .find(|r| r.id == *id)
                    .map(|r| r.title.clone())
                    .unwrap_or_default();
                let body = format!(
                    "Delete the recurring issue \"{title}\"? It will stop creating issues; \
                     issues it already created are kept."
                );
                iced::widget::stack![
                    base,
                    delete_modal("Delete Recurring Issue?", body, Message::DeleteRecurringConfirmed)
                ]
                .into()
            }
            Modal::DeleteCommentConfirm(comment_id) => {
                let replies = queries::comment_reply_count(&self.db(), comment_id).unwrap_or(0);
                iced::widget::stack![base, delete_comment_confirm_modal(replies)].into()
            }
            Modal::Lightbox(state) => {
                iced::widget::stack![base, crate::views::lightbox::view(state, &self.images)].into()
            }
            Modal::DeleteAttachmentConfirm(id) => {
                let name = crate::db::attachments::get(&self.db(), id)
                    .ok()
                    .flatten()
                    .map(|a| a.filename)
                    .unwrap_or_default();
                let body = format!("Delete the attachment \"{name}\"? This can't be undone.");
                iced::widget::stack![
                    base,
                    delete_modal("Delete Attachment?", body, Message::DeleteAttachmentConfirmed)
                ]
                .into()
            }
            Modal::IssuePicker => match (&self.issue_picker, &self.issue_detail) {
                (Some(picker), Some(detail)) => {
                    // Exclude the open issue itself and any issue already linked
                    // in this direction (a duplicate would hit the UNIQUE edge).
                    let d = &self.detail_deps;
                    let existing = match picker.direction {
                        DepDirection::Blocking => &d.blocking,
                        DepDirection::BlockedBy => &d.blocked_by,
                        DepDirection::Relates => &d.related,
                        DepDirection::Duplicates => &d.duplicates,
                        DepDirection::DuplicatedBy => &d.duplicated_by,
                    };
                    let mut excluded: HashSet<&str> =
                        existing.iter().map(|l| l.issue_id.as_str()).collect();
                    excluded.insert(detail.issue_id.as_str());
                    if picker.mode != issue_picker::PickerMode::Dependency {
                        // Hierarchy modes offer only the spec's candidates.
                        excluded = issue_picker::hierarchy_excluded(
                            picker.mode,
                            &detail.issue_id,
                            &self.issues,
                            &self.lanes,
                        );
                    }
                    let modal =
                        issue_picker::view(picker, &self.issues, self.selected_team(), &excluded);
                    iced::widget::stack![base, modal].into()
                }
                _ => base,
            },
            // Drawn by the issue sidebar as the field's dropdown.
            Modal::PropertyPicker(state) if state.in_sidebar => base,
            Modal::PropertyPicker(state) => {
                let window = iced::Size::new(self.window_width, self.window_height);
                iced::widget::stack![base, property_picker::view(state, window)].into()
            }
            Modal::MoveTeam(issue_id) => {
                let source = self
                    .issues
                    .iter()
                    .find(|i| i.id == *issue_id)
                    .map(|i| i.team_id.as_str());
                let targets = team_picker::move_targets(&self.teams, source.unwrap_or_default());
                iced::widget::stack![base, team_picker::view(&targets)].into()
            }
            Modal::RoadmapAdd(commitment) => {
                let modal =
                    roadmap_view::add_modal_view(*commitment, &self.projects, self.selected_team());
                iced::widget::stack![base, modal].into()
            }
            Modal::TeamMembers(team_id) => match self.teams.iter().find(|t| t.id == *team_id) {
                Some(team) => {
                    let modal = team_members::modal_view(team, &self.team_members);
                    iced::widget::stack![base, modal].into()
                }
                None => base,
            },
            Modal::CommandPalette(palette) => {
                iced::widget::stack![base, command_palette::view(palette)].into()
            }
            Modal::None => base,
        };

        // The title strip goes outside the modals so they don't cover it, but
        // inside the stacks below: the hover card is placed in window coordinates.
        let content: Element<'_, Message> = if cfg!(target_os = "macos") {
            column![header::titlebar(self.title()), content].into()
        } else {
            content
        };

        // The hover card and toasts always get a layer, even an empty one:
        // wrapping `content` in a stack only while they show changes the
        // tree's shape, and iced then rebuilds the widget state underneath,
        // which scrolls a long body back to the top.
        let empty = || Element::from(iced::widget::Space::new(0, 0));
        let card = self.hover_card_overlay().unwrap_or_else(empty);
        // Toasts sit above everything, including any open modal.
        let toasts = if self.toasts.is_empty() {
            empty()
        } else {
            toast::overlay(&self.toasts)
        };
        iced::widget::stack![content, card, toasts].into()
    }
}

/// Boot/default team selection: the first non-archived team, falling back to
/// the first team at all so an all-archived install still shows something.
/// `collapsed_sections` key for a board sub-group row, kept apart from the
/// list's section titles that share the set.
fn swimlane_key(title: &str) -> String {
    format!("\u{1}row:{title}")
}

fn default_team_id(teams: &[Team]) -> Option<String> {
    teams
        .iter()
        .find(|t| t.archived_at.is_none())
        .or_else(|| teams.first())
        .map(|t| t.id.clone())
}

/// Header breadcrumb for `view`, e.g. ["JAIT", "Projects", "Desktop Application"]
/// or ["JAIT", "JAIT-111"]. Team-scoped views lead with the team identifier,
/// which links to the team's board; "Projects" on a project links to the list.
fn breadcrumb(
    view: &View,
    team: Option<&Team>,
    issues: &[Issue],
    projects: &[Project],
) -> Vec<header::Crumb> {
    use header::Crumb;
    if *view == View::MyIssues {
        return vec![Crumb::plain("My Issues")];
    }
    if *view == View::Inbox {
        return vec![Crumb::plain("Inbox")];
    }
    if matches!(view, View::Dashboard(_)) {
        return vec![Crumb::plain("Dashboards")];
    }
    let team_page = |v: View| team.map(|t| Message::SelectTeamView(t.id.clone(), v));
    let mut crumbs = vec![match team {
        Some(t) => Crumb::new(t.identifier.clone(), team_page(View::Board)),
        None => Crumb::plain("JAIT"),
    }];
    match view {
        View::Board => crumbs.push(Crumb::plain("Issues")),
        View::ProjectList => crumbs.push(Crumb::plain("Projects")),
        View::Roadmap => crumbs.push(Crumb::plain("Roadmap")),
        View::RecurringIssues => crumbs.push(Crumb::plain("Recurring issues")),
        View::ProjectDetail(id) => {
            crumbs.push(Crumb::new("Projects", team_page(View::ProjectList)));
            if let Some(p) = projects.iter().find(|p| p.id == *id) {
                crumbs.push(Crumb::plain(p.name.clone()));
                // The Rails id (what the JAIT MCP's `get_project` takes);
                // absent until synced.
                if let Some(rid) = p.remote_id {
                    crumbs.push(Crumb::plain(format!("#{rid}")));
                }
            }
        }
        View::IssueDetail(id) => {
            if let Some(issue) = issues.iter().find(|i| i.id == *id) {
                let number = issue.team_number.unwrap_or(0);
                crumbs.push(Crumb::plain(match team {
                    Some(t) => format!("{}-{number}", t.identifier),
                    None => format!("#{number}"),
                }));
            } else {
                crumbs.push(Crumb::plain("Issues"));
            }
        }
        View::ProjectDependencyMap { project_id, .. } => {
            crumbs.push(Crumb::new("Projects", team_page(View::ProjectList)));
            if let Some(p) = projects.iter().find(|p| p.id == *project_id) { crumbs.push(Crumb::new(
                p.name.clone(),
                Some(Message::CloseProjectDependencyMap),
            )) }
            crumbs.push(Crumb::plain("Dependencies"));
        }
        View::DependencyMap { issue_id, tab, .. } => {
            if let Some(issue) = issues.iter().find(|i| i.id == *issue_id) {
                let number = issue.team_number.unwrap_or(0);
                crumbs.push(Crumb::new(
                    match team {
                        Some(t) => format!("{}-{number}", t.identifier),
                        None => format!("#{number}"),
                    },
                    Some(Message::CloseDependencyMap),
                ));
            }
            crumbs.push(Crumb::plain(tab.label()));
        }
        View::MyIssues | View::Inbox | View::Dashboard(_) => {}
    }
    crumbs
}

/// Group the team's issue↔label join rows into a per-issue map for the board.
fn build_issue_labels_map(
    conn: &rusqlite::Connection,
    team_id: &str,
) -> HashMap<String, Vec<Label>> {
    let mut map: HashMap<String, Vec<Label>> = HashMap::new();
    if let Ok(pairs) = queries::issue_labels_for_team(conn, team_id) {
        for (issue_id, label) in pairs {
            map.entry(issue_id).or_default().push(label);
        }
    }
    map
}

/// Order the Projects table in place per the active sort key + direction.
/// Missing due dates always sort last (mirrors the Rails `due_date IS NULL`
/// clause), regardless of direction.
fn sort_projects(list: &mut [&Project], sort: ProjectSort, dir: SortDir) {
    list.sort_by(|a, b| {
        if let ProjectSort::DueDate = sort {
            return cmp_option_nulls_last(&a.due_date, &b.due_date, dir);
        }
        let ord = match sort {
            ProjectSort::Created => a.created_at.cmp(&b.created_at),
            ProjectSort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            ProjectSort::Priority => a.priority.cmp(&b.priority),
            ProjectSort::Velocity => a.velocity_score.cmp(&b.velocity_score),
            ProjectSort::DueDate => Ordering::Equal, // handled above
        };
        match dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        }
    });
}

fn cmp_option_nulls_last(a: &Option<String>, b: &Option<String>, dir: SortDir) -> Ordering {
    match (
        a.as_deref().filter(|s| !s.is_empty()),
        b.as_deref().filter(|s| !s.is_empty()),
    ) {
        (Some(x), Some(y)) => {
            let o = x.cmp(y);
            match dir {
                SortDir::Asc => o,
                SortDir::Desc => o.reverse(),
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn delete_confirm_modal(issue_title: &str) -> Element<'_, Message> {
    use iced::widget::{button, column, container, row};
    use crate::views::text;
    use iced::Border;

    let content = column![]
        .spacing(16)
        .push(text("Delete Issue?").size(18).color(t::text_primary()))
        .push(
            text(format!(
                "Are you sure you want to delete \"{}\"?",
                issue_title
            ))
            .size(14)
            .color(t::text_secondary()),
        )
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text("Delete").size(14).color(iced::Color::WHITE))
                        .on_press(Message::DeleteConfirmed)
                        .padding([8, 24])
                        .style(move |_theme: &Theme, _status| button::Style {
                            background: Some(iced::Background::Color(t::danger())),
                            text_color: iced::Color::WHITE,
                            border: Border {
                                radius: 4.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                )
                .push(
                    button(text("Cancel").size(14))
                        .on_press(Message::CloseModal)
                        .padding([8, 24])
                        .style(button::secondary),
                ),
        );

    let modal =
        container(content)
            .padding(28)
            .style(|_theme: &Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(t::bg_modal())),
                border: iced::Border {
                    color: t::border(),
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            });

    crate::views::util::modal_backdrop(modal)
        .into()
}

fn delete_label_confirm_modal<'a>(name: String, issues: usize) -> Element<'a, Message> {
    let body = match issues {
        0 => format!("Delete the label \"{name}\"? No issues carry it."),
        1 => format!("Delete the label \"{name}\"? It will be removed from 1 issue."),
        n => format!("Delete the label \"{name}\"? It will be removed from {n} issues."),
    };
    delete_modal("Delete Label?", body, Message::DeleteLabelConfirmed)
}

fn delete_comment_confirm_modal<'a>(replies: usize) -> Element<'a, Message> {
    let body = match replies {
        0 => "Delete this comment?".to_string(),
        1 => "Delete this comment and its 1 reply?".to_string(),
        n => format!("Delete this comment and its {n} replies?"),
    };
    delete_modal("Delete Comment?", body, Message::DeleteCommentConfirmed)
}

const DASHBOARD_NAME_KEY: &str = "dashboard-name";

fn dashboard_name_input_id() -> text_input::Id {
    crate::undo_input::id(DASHBOARD_NAME_KEY)
}

/// Name + optional description modal for dashboards (create now, rename later).
fn dashboard_form_modal<'a>(
    title: &'a str,
    submit_label: &'a str,
    submit: Message,
    form: &'a DashboardForm,
) -> Element<'a, Message> {
    use iced::widget::{button};
    use crate::views::text;

    let mut content = column![]
        .spacing(12)
        .push(text(title).size(18).color(t::text_primary()))
        .push(
            crate::undo_input::text_input(DASHBOARD_NAME_KEY, "Name", &form.name, Message::NewDashboardNameChanged)
                .on_submit(submit.clone())
                .padding([6, 10])
                .size(14)
                .width(Length::Fixed(360.0)),
        );
    if let Some(e) = form.error {
        content = content.push(text(e).size(12).color(t::danger()));
    }
    content = content
        .push(
            crate::undo_input::text_input("dashboard-description", "Description (optional)", &form.description, Message::NewDashboardDescriptionChanged)
                .on_submit(submit.clone())
                .padding([6, 10])
                .size(14)
                .width(Length::Fixed(360.0)),
        )
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text(submit_label).size(14))
                        .on_press(submit)
                        .padding([8, 24])
                        .style(button::primary),
                )
                .push(
                    button(text("Cancel").size(14))
                        .on_press(Message::CloseModal)
                        .padding([8, 24])
                        .style(button::secondary),
                ),
        );

    let modal =
        container(content)
            .padding(28)
            .style(|_theme: &Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(t::bg_modal())),
                border: iced::Border {
                    color: t::border(),
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            });

    crate::views::util::modal_backdrop(modal)
        .into()
}

const SAVE_VIEW_KEY: &str = "save-view-name";

fn save_view_input_id() -> text_input::Id {
    crate::undo_input::id(SAVE_VIEW_KEY)
}

/// Name prompt for "Save view…".
fn save_view_modal<'a>(title: &'a str, name: &'a str, error: Option<&'static str>) -> Element<'a, Message> {
    use iced::widget::{button};
    use crate::views::text;

    let mut content = column![]
        .spacing(12)
        .push(text(title).size(18).color(t::text_primary()))
        .push(
            crate::undo_input::text_input(SAVE_VIEW_KEY, "View name", name, Message::SaveViewNameChanged)
                .on_submit(Message::SaveViewSubmit)
                .padding([6, 10])
                .size(14)
                .width(Length::Fixed(320.0)),
        );
    if let Some(e) = error {
        content = content.push(text(e).size(12).color(t::danger()));
    }
    content = content.push(
        row![]
            .spacing(8)
            .push(
                button(text("Save").size(14))
                    .on_press(Message::SaveViewSubmit)
                    .padding([8, 24])
                    .style(button::primary),
            )
            .push(
                button(text("Cancel").size(14))
                    .on_press(Message::CloseModal)
                    .padding([8, 24])
                    .style(button::secondary),
            ),
    );

    let modal =
        container(content)
            .padding(28)
            .style(|_theme: &Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(t::bg_modal())),
                border: iced::Border {
                    color: t::border(),
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            });

    crate::views::util::modal_backdrop(modal)
        .into()
}

fn delete_modal<'a>(title: &'a str, body: String, confirm: Message) -> Element<'a, Message> {
    use iced::widget::{button, column, container, row};
    use crate::views::text;
    use iced::Border;

    let content = column![]
        .spacing(16)
        .push(text(title).size(18).color(t::text_primary()))
        .push(text(body).size(14).color(t::text_secondary()))
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text("Delete").size(14).color(iced::Color::WHITE))
                        .on_press(confirm)
                        .padding([8, 24])
                        .style(move |_theme: &Theme, _status| button::Style {
                            background: Some(iced::Background::Color(t::danger())),
                            text_color: iced::Color::WHITE,
                            border: Border {
                                radius: 4.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                )
                .push(
                    button(text("Cancel").size(14))
                        .on_press(Message::CloseModal)
                        .padding([8, 24])
                        .style(button::secondary),
                ),
        );

    let modal =
        container(content)
            .padding(28)
            .style(|_theme: &Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(t::bg_modal())),
                border: iced::Border {
                    color: t::border(),
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            });

    crate::views::util::modal_backdrop(modal)
        .into()
}

/// Pull a focus position back onto a real row given each group's row count:
/// the lane to the last group, the issue to the last row of that group.
fn clamped_focus(counts: &[usize], lane: usize, issue: usize) -> (usize, usize) {
    let Some(last) = counts.len().checked_sub(1) else {
        return (0, 0);
    };
    let lane = lane.min(last);
    let issue = match counts[lane] {
        0 => 0,
        n => issue.min(n - 1),
    };
    (lane, issue)
}

#[cfg(test)]
mod tests {
    #[test]
    fn restored_focus_is_clamped_onto_a_real_row() {
        // A group vanished: lane 3 of 2 groups lands on the last group.
        assert_eq!(super::clamped_focus(&[2, 4], 3, 1), (1, 1));
        // Rows shrank: issue 9 lands on the last row.
        assert_eq!(super::clamped_focus(&[2, 4], 1, 9), (1, 3));
        assert_eq!(super::clamped_focus(&[2, 0], 1, 5), (1, 0));
        assert_eq!(super::clamped_focus(&[], 2, 5), (0, 0));
    }

    #[test]
    fn view_names_its_issue_or_project_across_detail_and_map() {
        assert_eq!(super::View::IssueDetail("i".into()).issue_id(), Some("i"));
        assert_eq!(super::View::dependency_map("i").issue_id(), Some("i"));
        assert_eq!(super::View::dependency_map("i").project_id(), None);
        assert_eq!(super::View::ProjectDetail("p".into()).project_id(), Some("p"));
        assert_eq!(super::View::project_dependency_map("p").project_id(), Some("p"));
        assert_eq!(super::View::Board.issue_id(), None);
    }

    #[test]
    fn comment_content_rule_allows_files_without_text() {
        assert!(!super::comment_has_content("", false));
        assert!(super::comment_has_content("", true));
        assert!(super::comment_has_content("hi", false));
    }

    use super::*;

    fn dash(id: &str) -> crate::db::dashboards::Dashboard {
        crate::db::dashboards::Dashboard {
            id: id.into(),
            remote_id: None,
            name: id.into(),
            description: String::new(),
            position: 0,
            updated_at: String::new(),
        }
    }

    #[test]
    fn open_dashboard_removed_by_a_pull_falls_back_to_the_board() {
        let list = [dash("a")];
        assert_eq!(View::Dashboard("a".into()).without_missing_dashboard(&list), View::Dashboard("a".into()));
        assert_eq!(View::Dashboard("gone".into()).without_missing_dashboard(&list), View::Board);
        assert_eq!(View::Dashboard("a".into()).without_missing_dashboard(&[]), View::Board);
        assert_eq!(View::MyIssues.without_missing_dashboard(&[]), View::MyIssues);
    }

    fn team(id: &str, archived: bool) -> Team {
        Team {
            id: id.into(),
            name: id.into(),
            identifier: id.into(),
            remote_id: None,
            archived_at: archived.then(|| "2026-01-01T00:00:00.000Z".into()),
            role: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn team_web_url_targets_settings_section() {
        assert_eq!(
            team_web_url("http://localhost:3005/", 7, TeamWebSection::Lanes),
            "http://localhost:3005/teams/7/edit?section=lanes"
        );
        assert_eq!(
            team_web_url(" https://jait.app ", 7, TeamWebSection::General),
            "https://jait.app/teams/7/edit?section=general"
        );
    }

    fn issue(id: &str, title: &str, number: i32) -> Issue {
        Issue {
            id: id.into(),
            title: title.into(),
            description: None,
            priority: 0,
            team_number: Some(number),
            due_date: None,
            estimate: None,
            started_at: None,
            completed_at: None,
            canceled_at: None,
            lane_id: "lane".into(),
            team_id: "t".into(),
            project_id: None,
            parent_issue_id: None,
            assignee_id: None,
            creator_id: None,
            remote_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn breadcrumb_follows_view() {
        let jait = team("JAIT", false);
        let issues = vec![issue("i1", "Header bar", 184)];
        let crumbs = |v: View| -> Vec<String> {
            breadcrumb(&v, Some(&jait), &issues, &[]).into_iter().map(|c| c.label).collect()
        };
        assert_eq!(crumbs(View::Board), ["JAIT", "Issues"]);
        assert_eq!(crumbs(View::Roadmap), ["JAIT", "Roadmap"]);
        assert_eq!(crumbs(View::IssueDetail("i1".into())), ["JAIT", "JAIT-184"]);
        assert_eq!(
            crumbs(View::dependency_map("i1")),
            ["JAIT", "JAIT-184", "Dependencies"]
        );
        assert_eq!(
            crumbs(View::DependencyMap {
                issue_id: "i1".into(),
                tab: DependencyMapTab::Hierarchy,
                depth: 3,
            }),
            ["JAIT", "JAIT-184", "Hierarchy"]
        );
        // The project map: Projects › Name › Dependencies.
        let projects = vec![Project {
            id: "p1".into(),
            remote_id: None,
            team_id: "t".into(),
            name: "Proj".into(),
            description: None,
            status: None,
            priority: 0,
            lead_id: None,
            start_date: None,
            due_date: None,
            roadmap_commitment: None,
            velocity_score: 0,
            completed_issues_count: 0,
            total_issues_count: 0,
            progress_percentage: 0,
            behind_schedule: false,
            created_at: String::new(),
            updated_at: String::new(),
        }];
        let map_crumbs = breadcrumb(&View::project_dependency_map("p1"), Some(&jait), &issues, &projects);
        assert_eq!(
            map_crumbs.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(),
            ["JAIT", "Projects", "Proj", "Dependencies"]
        );
        assert!(matches!(map_crumbs[2].on_press, Some(Message::CloseProjectDependencyMap)));
        // A synced project shows its Rails id after the name.
        let mut synced = projects.clone();
        synced[0].remote_id = Some(42);
        assert_eq!(
            breadcrumb(&View::ProjectDetail("p1".into()), Some(&jait), &issues, &synced)
                .into_iter()
                .map(|c| c.label)
                .collect::<Vec<_>>(),
            ["JAIT", "Projects", "Proj", "#42"]
        );
        assert_eq!(crumbs(View::MyIssues), ["My Issues"]);
        assert_eq!(crumbs(View::Inbox), ["Inbox"]);
        // Missing project falls back to the list crumb rather than a blank.
        assert_eq!(
            crumbs(View::ProjectDetail("gone".into())),
            ["JAIT", "Projects"]
        );
    }

    #[test]
    fn breadcrumb_parents_link_up() {
        let jait = team("JAIT", false);
        let crumbs = breadcrumb(&View::ProjectDetail("gone".into()), Some(&jait), &[], &[]);
        assert!(matches!(
            &crumbs[0].on_press,
            Some(Message::SelectTeamView(id, View::Board)) if *id == jait.id
        ));
        assert!(matches!(
            &crumbs[1].on_press,
            Some(Message::SelectTeamView(_, View::ProjectList))
        ));
        // Without a team there's nowhere to go.
        assert!(breadcrumb(&View::Board, None, &[], &[])[0].on_press.is_none());
    }

    #[test]
    fn issue_search_matches_title_or_identifier() {
        let jait = team("JAIT", false);
        let i = issue("i1", "Header bar — Search", 184);
        let m = |q: &str| crate::search_match::issue_matches(&i, Some(&jait), &crate::search_match::needle(q));
        assert!(m(""));
        assert!(m("  HEADER "));
        assert!(m("jait-184"));
        assert!(m("184"));
        assert!(!m("roadmap"));
    }

    #[test]
    fn default_team_skips_archived() {
        let teams = vec![team("a", true), team("b", false)];
        assert_eq!(default_team_id(&teams).as_deref(), Some("b"));
    }

    #[test]
    fn default_team_falls_back_when_all_archived() {
        let teams = vec![team("a", true)];
        assert_eq!(default_team_id(&teams).as_deref(), Some("a"));
        assert_eq!(default_team_id(&[]), None);
    }

    #[test]
    fn dep_direction_stores_source_and_kind() {
        let edge = |d: DepDirection| d.stored_edge("me", "other");
        assert_eq!(edge(DepDirection::Blocking), ("me".into(), "other".into(), "blocks"));
        assert_eq!(edge(DepDirection::BlockedBy), ("other".into(), "me".into(), "blocks"));
        assert_eq!(edge(DepDirection::Relates), ("me".into(), "other".into(), "relates"));
        assert_eq!(edge(DepDirection::Duplicates), ("me".into(), "other".into(), "duplicates"));
        assert_eq!(
            edge(DepDirection::DuplicatedBy),
            ("other".into(), "me".into(), "duplicates")
        );
    }

    #[test]
    fn dep_direction_cycles_through_every_kind() {
        assert_eq!(DepDirection::Relates.cycle(true), DepDirection::Duplicates);
        assert_eq!(DepDirection::Duplicates.cycle(true), DepDirection::DuplicatedBy);
        assert_eq!(DepDirection::DuplicatedBy.cycle(true), DepDirection::Blocking);
        assert_eq!(DepDirection::Blocking.cycle(false), DepDirection::DuplicatedBy);
    }
}
