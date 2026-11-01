use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use iced::keyboard;
use iced::widget::{column, container, row, scrollable, text_input};
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
use crate::scroll;
use crate::sync::engine::{FullSyncReport, SyncEngine, SyncErrorKind};
use crate::sync::notif_poll::{self, PollOutcome, PollReport};
use crate::sync::scheduler::{CycleKind, Event as SchedEvent, Run, Scheduler};
use crate::palette_results::{PaletteData, PaletteNav, PaletteState};
use crate::theme as t;
use crate::views::toast::{Toast, ToastKind};
use crate::views::{
    board, command_palette, csv_import, display_panel, header, issue_detail, issue_form, issue_list,
    issue_picker, labels as labels_view, placeholder,
    project_detail, projects as projects_view, roadmap as roadmap_view, settings, status_bar,
    team_list, team_members, team_picker, toast,
};

/// Where an issue detail opened from a dashboard returns to.
#[derive(Debug, Clone, PartialEq)]
struct DashboardOrigin {
    dashboard_id: String,
    /// The team selected before the detail switched to the issue's own team.
    prev_team_id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    // Navigation
    /// Toggle a team's sidebar expansion (chevron / team-header click).
    ToggleTeamExpanded(String),
    /// Select a team and switch the right pane to `view` (team sub-row click).
    SelectTeamView(String, View),
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
    RecurringFormDescriptionChanged(iced::widget::text_editor::Action),
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

    // Team menu (sidebar kebab)
    /// Open/close the per-team menu on an expanded team header.
    ToggleTeamMenu(String),
    /// Make a team's board the user's Home page (local UUID).
    SetTeamAsHome(String),
    /// Show/hide archived teams in the sidebar.
    ToggleShowArchivedTeams,
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
    IssueDescriptionChanged(iced::widget::text_editor::Action),
    IssuePriorityChanged(Priority),
    IssueLaneChanged(String),
    /// Toggle a label in the issue form's multi-select (by local UUID).
    IssueLabelToggled(String),

    // Issue detail — buffered text fields (committed on Enter / nav-away)
    IssueDetailTitleChanged(String),
    IssueDetailDescriptionChanged(iced::widget::text_editor::Action),
    IssueDetailDueDateChanged(String),
    IssueDetailEstimateChanged(String),
    /// Flush buffered detail fields to the DB (text field `on_submit`).
    CommitIssueDetail,
    // Issue detail — live-apply sidebar pick_lists
    IssueDetailStatusChanged(String),
    IssueDetailPriorityChanged(Priority),
    IssueDetailProjectChanged(Option<String>),
    /// Assignee picker changed; `None` = "Unassigned". Carries a local member UUID.
    IssueDetailAssigneeChanged(Option<String>),

    // Issue and project detail — comments (threaded)
    /// Composer text-editor action.
    CommentComposerChanged(iced::widget::text_editor::Action),
    /// "Comment" button pressed: persist the composer body locally + enqueue push.
    SubmitComment,
    /// "Reply" pressed on a comment (local id): open the inline reply composer.
    StartReply(String),
    CancelReply,
    ReplyComposerChanged(iced::widget::text_editor::Action),
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
    /// "+ Add sub-issue" pressed: open the create modal with the parent's local
    /// UUID pre-filled as `parent_issue_id`.
    CreateSubIssue(String),

    // Issue detail — dependencies (blocking / blocked-by)
    /// Open the issue-detail view for another issue by its local UUID (e.g. a
    /// click on a linked dependency row). Rebuilds the detail buffer, unlike
    /// `NavigateToView(View::IssueDetail(_))` which only swaps the view.
    OpenIssueDetail(String),
    /// Leave an issue detail that was opened from My Issues and return there.
    BackToMyIssues,
    BackToDashboard,
    /// Open the searchable issue picker to add a dependency in `direction`.
    OpenDependencyPicker(DepDirection),
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

    // UI
    CloseModal,
    ToggleHelp,
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

    // Header bar
    /// Header search box changed; filters the current view's items.
    HeaderSearchChanged(String),
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
    /// List mode `c`: collapse/expand the focused section.
    ToggleFocusedSection,
    /// A board lane / the list scrolled: (key, absolute y offset, viewport height).
    Scrolled(usize, f32, f32),
    SetGroupBy(GroupBy),
    SetSubGroup(SubGroup),
    SetOrderBy(OrderBy),
    SetCompletedFilter(display::CompletedFilter),
    ToggleShowSubIssues,
    ToggleShowEmptyGroups,
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
    SettingsNativeNotificationsToggled(bool),
    TestConnection,
    TestConnectionResult(Result<usize, String>),
    SaveSettings,

    // Sync
    /// Manual sync (`s`, toast retry): runs immediately.
    SyncNow,
    /// Scheduler timer fired.
    SyncTick,
    /// Keyboard/mouse activity, for idle tracking.
    UserInput,
    WindowFocused,
    WindowUnfocused,
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

    // Project CRUD
    CreateProject,
    EditProject(String),
    SaveProject,
    ConfirmDeleteProject,
    DeleteProjectConfirmed,

    // Project form fields
    ProjectNameChanged(String),
    ProjectDescriptionChanged(iced::widget::text_editor::Action),
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
    /// Toggle the "Hide completed" filter.
    ToggleHideCompletedProjects,

    // Roadmap
    /// Open the "Add project to roadmap" picker preset to `commitment`.
    OpenRoadmapAdd(RoadmapCommitment),

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
    ProjectDetailDescriptionChanged(iced::widget::text_editor::Action),
    ProjectDetailStartDateChanged(String),
    ProjectDetailDueDateChanged(String),
    /// Flush buffered detail fields to the DB (text field `on_submit`).
    CommitProjectDetail,
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
}

/// The comment composers of the open detail view (issue or project).
struct CommentDrafts<'a> {
    comment: &'a mut iced::widget::text_editor::Content,
    reply_to: &'a mut Option<String>,
    reply: &'a mut iced::widget::text_editor::Content,
    suggest: &'a mut Option<crate::mentions::SuggestState>,
    files: &'a mut crate::views::issue_detail::ComposerFiles,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    None,
    IssueForm,
    DeleteConfirm(String),
    Settings,
    ProjectForm,
    /// "New recurring issue" form; state in `App::recurring_form`.
    RecurringForm,
    DeleteProjectConfirm(String),
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
    /// Name prompt for saving the current board as a new Saved view.
    SaveView,
    /// "New dashboard" form; state in `App::dashboard_form`.
    NewDashboard,
    /// Rename/description form; target id in `DashboardForm::editing`.
    EditDashboard,
    /// "Add group" form on the open dashboard; state in `App::group_form`.
    NewGroup,
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

pub struct App {
    teams: Vec<Team>,
    /// Mirrored dashboards for the sidebar (reloaded with the rest after a sync).
    dashboards: Vec<crate::db::dashboards::Dashboard>,
    /// State of the dashboard name/description modal (create now, rename later).
    dashboard_form: DashboardForm,
    /// State of the dashboard "Add group" modal.
    group_form: GroupForm,
    /// The open dashboard's groups with their sources (reloaded with the mirror).
    dashboard_groups: Vec<crate::dashboard_eval::GroupConfig>,
    /// Header state of the open dashboard. Per-dashboard memory is a later ticket.
    dashboard_header: crate::dashboard_eval::Header,
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
    /// Dependency edges for the open detail issue, resolved for the sidebar.
    /// Repopulated each reload only while `view` is `IssueDetail`.
    detail_deps: queries::DependencyGroups,
    /// The open issue detail was reached from My Issues, so Back returns there.
    detail_from_my_issues: bool,
    /// The open issue detail was reached from the Inbox (Back returns there).
    detail_from_inbox: bool,
    /// The open issue detail was reached from a dashboard (Back returns there).
    detail_from_dashboard: Option<DashboardOrigin>,
    /// Comments on the open detail issue, oldest first. Repopulated each reload
    /// only while `view` is `IssueDetail`.
    detail_comments: Vec<Comment>,
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
    /// List-mode sections (by title) whose rows are hidden.
    collapsed_sections: HashSet<String>,
    /// Last reported (offset, viewport height) per scrollable: board lane
    /// index, or `issue_list::SCROLL_KEY`. Drives scroll-to-focus.
    scroll_state: HashMap<usize, (f32, f32)>,
    modal: Modal,
    issue_form: Option<issue_form::IssueFormState>,
    /// Buffered edit state while the right pane shows `View::IssueDetail`.
    issue_detail: Option<issue_detail::IssueDetailState>,
    /// Buffered edit state while the right pane shows `View::ProjectDetail`.
    project_detail: Option<project_detail::ProjectDetailState>,
    /// Projects table sort key + direction (session-only).
    project_sort: ProjectSort,
    project_sort_dir: SortDir,
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
    /// Monotonic id source for toasts (so dismiss/retry target the right one).
    next_toast_id: u64,
    /// Set when sync hit a 401; pauses the sync timer until the user saves a
    /// fresh token in Settings, so we don't hammer the API with a dead token.
    sync_paused_auth: bool,
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

    /// Before opening an issue or project from the palette: when it belongs to
    /// another team (or we are on My Issues, whose data is mixed), select its
    /// team like a sidebar click, so the sidebar follows and its data is loaded.
    fn select_team_for_palette(&mut self, team_id: &str) -> Task<Message> {
        if self.selected_team_id.as_deref() == Some(team_id) && !self.view.is_cross_team() {
            return Task::none();
        }
        self.expanded_teams.insert(team_id.to_string());
        self.update(Message::SelectTeamView(team_id.to_string(), View::Board))
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
        });
    }

    fn reload_data(&mut self) {
        let conn = self.db();
        self.teams = queries::all_teams(&conn).unwrap_or_default();
        self.saved_views = crate::db::saved_views::grouped_by_team(&conn).unwrap_or_default();
        self.dashboards = crate::db::dashboards::list(&conn).unwrap_or_default();
        self.home_page = crate::db::home_page::get(&conn).unwrap_or_default();

        let kept = self.view.clone().without_missing_dashboard(&self.dashboards);
        if kept != self.view {
            self.view = kept;
            self.focused_lane = 0;
            self.focused_issue = 0;
        }

        if self.selected_team_id.is_none() {
            self.selected_team_id = default_team_id(&self.teams);
        }

        if self.view.is_cross_team() {
            self.load_cross_team(&conn);
        } else if let Some(ref team_id) = self.selected_team_id {
            self.lanes = queries::lanes_for_team(&conn, team_id).unwrap_or_default();
            self.issues = queries::issues_for_team(&conn, team_id).unwrap_or_default();
            self.projects = queries::projects_for_team(&conn, team_id).unwrap_or_default();
            self.members = queries::members_for_team(&conn, team_id).unwrap_or_default();
            self.labels = queries::labels_for_team(&conn, team_id).unwrap_or_default();
            self.issue_labels = build_issue_labels_map(&conn, team_id);
        } else {
            self.projects.clear();
            self.members.clear();
            self.labels.clear();
            self.issue_labels.clear();
        }

        self.reload_dashboard_config(&conn);
        self.reload_recurring(&conn);
        self.reload_detail_origin(&conn);
        self.refresh_open_view();

        // Filters are global but hold per-team ids; drop the ones this data
        // doesn't contain (My Issues loads every team, so its ids all stay).
        // Skipped while a dashboard-opened detail is up: the selected team is
        // temporarily the issue's, and pruning would drop the board filters of
        // the team that was selected before.
        if self.selected_team_id.is_some()
            && self.detail_from_dashboard.is_none()
            && self.prune_board_filters()
        {
            if let Err(e) = config::save(&self.config) {
                self.config_error = Some(e);
            }
        }

        self.load_detail_dependencies(&conn);

        self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
        self.clamp_focus();
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
            header: self.dashboard_header,
        })
    }

    /// Switch to `view`, reloading data when crossing in or out of the
    /// cross-team views (their data replaces the per-team data).
    fn switch_view(&mut self, view: View) {
        let crossing = self.view.is_cross_team() != view.is_cross_team()
            || matches!(view, View::Dashboard(_));
        self.view = view;
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
    }

    /// Re-read the selected team's Recurring issue rows from the mirror.
    /// Refresh the open issue detail's "From recurring issue" title.
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
            return;
        } else {
            self.detail_comment_attachments.clear();
            self.detail_attachments.clear();
            self.detail_references.clear();
            self.detail_deps = Default::default();
            self.detail_comments.clear();
            self.detail_activity.clear();
        }
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
                    d.description = iced::widget::text_editor::Content::with_text(&body);
                }
                "issue"
            }
            OwnerKind::Project => {
                if let Some(d) = self.project_detail.as_mut().filter(|d| d.project_id == row.owner_id) {
                    d.description = iced::widget::text_editor::Content::with_text(&body);
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
        content: &mut iced::widget::text_editor::Content,
        s: &crate::mentions::SuggestState,
        idx: Option<usize>,
    ) {
        use iced::widget::text_editor::{Action, Edit};
        if let Some(item) = s.items.get(idx.unwrap_or(s.selected)) {
            for _ in 0..s.delete_chars {
                content.perform(Action::Edit(Edit::Backspace));
            }
            content.perform(Action::Edit(Edit::Paste(std::sync::Arc::new(
                item.insert.clone(),
            ))));
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
            None => &self.config.display,
        }
    }

    fn board_display_mut(&mut self) -> &mut DisplayOptions {
        match &mut self.open_view {
            Some(v) => &mut v.state.display,
            None => &mut self.config.display,
        }
    }

    fn board_filters(&self) -> &Filters {
        match &self.open_view {
            Some(v) => &v.state.filters,
            None => &self.config.filters,
        }
    }

    fn board_filters_mut(&mut self) -> &mut Filters {
        match &mut self.open_view {
            Some(v) => &mut v.state.filters,
            None => &mut self.config.filters,
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
                let local = crate::saved_view::BoardState {
                    display: self.config.display.clone(),
                    filters: self.config.filters.clone(),
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
        self.config.filters.prune(
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

    fn breadcrumb(&self) -> Vec<String> {
        let mut crumbs = breadcrumb(
            &self.view,
            self.selected_team(),
            &self.issues,
            &self.projects,
        );
        if let View::Dashboard(id) = &self.view {
            if let Some(d) = self.dashboards.iter().find(|d| d.id == *id) {
                crumbs.push(d.name.clone());
            }
        }
        if self.detail_from_my_issues && matches!(self.view, View::IssueDetail(_)) {
            crumbs.insert(0, "My Issues".into());
        }
        if let (Some(o), View::IssueDetail(_)) = (&self.detail_from_dashboard, &self.view) {
            if let Some(d) = self.dashboards.iter().find(|d| d.id == o.dashboard_id) {
                crumbs.insert(0, d.name.clone());
            }
        }
        if self.detail_from_inbox && matches!(self.view, View::IssueDetail(_)) {
            crumbs.insert(0, "Inbox".into());
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
                *last = if v.is_modified(&mirror) {
                    format!("{} (Modified)", v.name)
                } else {
                    v.name.clone()
                };
            }
        }
        crumbs
    }

    /// Empty-state text for My Issues, when there's something to explain.
    fn my_issues_empty(&self) -> Option<&'static str> {
        let ids = display::my_member_ids(&self.members, self.config.current_user_remote_id);
        let visible: usize = self.board_columns().iter().map(|c| c.issues.len()).sum();
        let narrowed = self.board_filters().count() > self.board_filters().assignees.len()
            || !header::needle(self.active_search()).is_empty();
        display::my_issues_empty_message(!ids.is_empty(), visible, narrowed)
    }

    /// Views with a list to filter; detail/placeholder views disable search.
    fn search_enabled(&self) -> bool {
        matches!(
            self.view,
            View::Board
                | View::MyIssues
                | View::ProjectList
                | View::ProjectDetail(_)
                | View::Roadmap
        )
    }

    /// What the header "+ New" button creates on the current view, if anything.
    fn new_action(&self) -> Option<Message> {
        self.selected_team()?;
        match self.view {
            View::Board => Some(Message::CreateIssue),
            View::ProjectList => Some(Message::CreateProject),
            View::Roadmap => Some(Message::OpenRoadmapAdd(RoadmapCommitment::Now)),
            _ => None,
        }
    }

    /// Board columns for the current display options, filters and search.
    /// Focus indices (`focused_lane`/`focused_issue`) index into these.
    fn board_columns(&self) -> Vec<display::Column<'_>> {
        if matches!(self.view, View::Dashboard(_)) {
            return self
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
        let needle = header::needle(self.active_search());
        if my {
            let ids = display::my_member_ids(&self.members, self.config.current_user_remote_id);
            let filters = display::my_issues_filters(self.board_filters(), &ids, &self.lanes);
            return display::build_columns(&data, self.board_display(), &filters, &needle);
        }
        display::build_columns(&data, self.board_display(), self.board_filters(), &needle)
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
        let mut filters = self.board_filters().clone();
        filters.projects.clear();
        display::visible_from(
            self.issues
                .iter()
                .filter(|i| i.project_id.as_deref() == Some(project_id)),
            &data,
            self.board_display(),
            &filters,
            needle,
        )
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

    /// Per-section counts for list-mode navigation: collapsed sections count
    /// as empty so j/k and h/l skip their hidden rows.
    fn list_nav_counts(&self) -> Vec<usize> {
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

    /// Scroll the board lane / list so the focused card or row is fully in
    /// view. Positions come from fixed element heights (see `scroll`); the
    /// current offset and viewport height come from `Message::Scrolled`.
    fn scroll_to_focus(&mut self) -> Task<Message> {
        if !matches!(self.view, View::Board | View::MyIssues) {
            return Task::none();
        }
        let list = self.board_display().view_mode == ViewMode::List;
        let (key, id, top, height) = {
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
                (issue_list::SCROLL_KEY, issue_list::scroll_id(), top, h)
            } else {
                let data = board::CardData {
                    teams: &self.teams,
                    lanes: &self.lanes,
                    members: &self.members,
                    projects: &self.projects,
                    issue_labels: &self.issue_labels,
                    display: self.board_display(),
                };
                let (top, h) = board::focused_span(col, &data, self.focused_issue);
                (
                    self.focused_lane,
                    board::lane_scroll_id(self.focused_lane),
                    top,
                    h,
                )
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
        scrollable::scroll_to(id, scrollable::AbsoluteOffset { x: 0.0, y: new })
    }

    /// Keep the selected Inbox row in view. Rows are fixed-height and sit in
    /// their own scrollable below the filter tabs, so row `i` starts at
    /// `i * ROW_HEIGHT`.
    fn scroll_inbox_to_selection(&mut self) -> Task<Message> {
        use crate::views::inbox as inbox_view;
        let top = self.inbox.selected as f32 * inbox_view::ROW_HEIGHT;
        let (offset, viewport) = self
            .scroll_state
            .get(&inbox_view::SCROLL_KEY)
            .copied()
            .unwrap_or((0.0, scroll::FALLBACK_VIEWPORT_H));
        let new = scroll::offset_to_reveal(top, inbox_view::ROW_HEIGHT, offset, viewport);
        if (new - offset).abs() < 0.5 {
            return Task::none();
        }
        self.scroll_state.insert(inbox_view::SCROLL_KEY, (new, viewport));
        scrollable::scroll_to(
            inbox_view::scroll_id(),
            scrollable::AbsoluteOffset { x: 0.0, y: new },
        )
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
        if counts.is_empty() {
            self.focused_lane = 0;
            self.focused_issue = 0;
            return;
        }
        if self.focused_lane >= counts.len() {
            self.focused_lane = counts.len() - 1;
        }
        let count = counts[self.focused_lane];
        if count == 0 {
            self.focused_issue = 0;
        } else if self.focused_issue >= count {
            self.focused_issue = count - 1;
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
            + usize::from(!header::needle(self.active_search()).is_empty())
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
    fn open_focused_issue(&mut self, issue: Issue) -> Task<Message> {
        if let View::Dashboard(dashboard_id) = self.view.clone() {
            return self.open_issue_from_dashboard(dashboard_id, issue);
        }
        if self.view == View::MyIssues {
            self.selected_team_id = Some(issue.team_id.clone());
            self.view = View::Board;
            self.reload_data();
            self.detail_from_my_issues = true;
        }
        self.open_issue_detail(&issue)
    }

    /// Open `issue` from a dashboard: select its own team (like My Issues) so
    /// the detail is that team's, remembering the dashboard and the team that
    /// was selected before, so Back restores both.
    fn open_issue_from_dashboard(&mut self, dashboard_id: String, issue: Issue) -> Task<Message> {
        self.detail_from_dashboard = Some(DashboardOrigin {
            dashboard_id,
            prev_team_id: self.selected_team_id.clone(),
        });
        self.selected_team_id = Some(issue.team_id.clone());
        self.view = View::Board;
        self.reload_data();
        self.open_issue_detail(&issue)
    }

    /// Leave an issue detail opened from a dashboard: put the previously
    /// selected team back and return to the dashboard. False when the detail
    /// didn't come from one.
    fn back_to_dashboard(&mut self) -> bool {
        let Some(origin) = self.detail_from_dashboard.take() else {
            return false;
        };
        self.commit_issue_detail();
        self.issue_detail = None;
        self.selected_team_id = origin.prev_team_id;
        self.switch_view(View::Dashboard(origin.dashboard_id));
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
        if let Some(mut issue) = self.issues.iter().find(|i| i.id == id).cloned() {
            mutate(&mut issue);
            issue.updated_at = crate::db::now_iso();
            let conn = self.db();
            let _ = queries::update_issue_queued(&conn, &issue);
            self.reload_data();
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
        let Some(detail) = self.issue_detail.as_mut() else {
            return;
        };

        if detail.title.trim().is_empty() {
            detail.error = Some("Title can't be blank".into());
            return;
        }
        let estimate = if detail.estimate.trim().is_empty() {
            None
        } else {
            match detail.estimate.trim().parse::<f64>() {
                Ok(n) => Some(n),
                Err(_) => {
                    detail.error = Some("Estimate must be a number".into());
                    return;
                }
            }
        };
        detail.error = None;

        let id = detail.issue_id.clone();
        let title = detail.title.trim().to_string();
        let due_date = if detail.due_date.trim().is_empty() {
            None
        } else {
            Some(detail.due_date.trim().to_string())
        };
        let body = detail.description_text();
        let description = if body.trim().is_empty() {
            None
        } else {
            Some(body.trim_end().to_string())
        };

        if let Some(mut issue) = self.issues.iter().find(|i| i.id == id).cloned() {
            issue.title = title;
            issue.description = description;
            issue.estimate = estimate;
            issue.due_date = due_date;
            issue.updated_at = crate::db::now_iso();
            let conn = self.db();
            let _ = queries::update_issue_queued(&conn, &issue);
            self.reload_data();
        }
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
        let Some(detail) = self.project_detail.as_mut() else {
            return;
        };
        if detail.name.trim().is_empty() {
            detail.error = Some("Name can't be blank".into());
            return;
        }
        detail.error = None;

        let id = detail.project_id.clone();
        let name = detail.name.trim().to_string();
        let start_date = if detail.start_date.trim().is_empty() {
            None
        } else {
            Some(detail.start_date.trim().to_string())
        };
        let due_date = if detail.due_date.trim().is_empty() {
            None
        } else {
            Some(detail.due_date.trim().to_string())
        };
        let body = detail.description_text();
        let description = if body.trim().is_empty() {
            None
        } else {
            Some(body.trim_end().to_string())
        };

        let conn = self.db();
        if let Ok(Some(mut project)) = queries::get_project(&conn, &id) {
            project.name = name;
            project.description = description;
            project.start_date = start_date;
            project.due_date = due_date;
            project.updated_at = crate::db::now_iso();
            let _ = queries::update_project(&conn, &project);
            let payload = serde_json::to_string(&project).ok();
            self.enqueue_sync("project", &project.id, "update", payload.as_deref());
            self.reload_data();
        }
    }
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let (config, config_error) = config::load();

        let conn = connection::open().expect("Failed to open database");
        // Skip the local "My Project" seed when the user has configured an API
        // token — the first sync will populate teams from Rails. Without a
        // token we keep the seed so the app is usable purely offline.
        if config.api_token.is_none() {
            connection::seed_default_data(&conn).expect("Failed to seed data");
        }

        let teams = queries::all_teams(&conn).unwrap_or_default();
        let saved_views = crate::db::saved_views::grouped_by_team(&conn).unwrap_or_default();
        let home_page = crate::db::home_page::get(&conn).unwrap_or_default();
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
            detail_deps: Default::default(),
            detail_from_my_issues: false,
            detail_from_inbox: false,
            detail_from_dashboard: None,
            detail_comments: Vec::new(),
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
            collapsed_sections: HashSet::new(),
            scroll_state: HashMap::new(),
            modal: Modal::None,
            issue_form: None,
            issue_detail: None,
            project_detail: None,
            project_sort: ProjectSort::Created,
            project_sort_dir: SortDir::Asc,
            hide_completed_projects: true,
            search_query: String::new(),
            search_view: View::Board,
            project_form: None,
            settings_form: None,
            label_form: None,
            csv_import: None,
            show_help: false,
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
            group_form: GroupForm::default(),
            dashboard_groups: Vec::new(),
            dashboard_header: Default::default(),
            open_view: None,
            save_view_name: String::new(),
            save_view_error: None,
            save_view_from_open: false,
            home_page,
            last_notif_poll: None,
            inbox: Default::default(),
            recurring_rows: Vec::new(),
            recurring_form: None,
            scheduler: Scheduler::with_notifications(std::time::Instant::now()),
            sync_next_delay: Some(crate::sync::scheduler::FOCUSED_INTERVAL),
            toasts: Vec::new(),
            next_toast_id: 0,
            sync_paused_auth: false,
        };

        let task = match (launch_view, launch_dashboard) {
            (Some(view_id), _) => app.update_inner(Message::OpenSavedView(view_id)),
            (None, Some(id)) => app.update_inner(Message::OpenDashboard(id)),
            _ => Task::none(),
        };
        (app, task)
    }

    pub fn title(&self) -> String {
        "JAIT — Issue Tracker".to_string()
    }

    /// Adopt `new_cfg` for this session and persist it, surfacing a save
    /// failure in the Settings form (and clearing a stale `config_error` on success).
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

    pub fn subscription(&self) -> Subscription<Message> {
        let keys = event::listen_with(|event, status, _id| {
            if let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            {
                // A focused text field captures keystrokes; don't also treat them
                // as shortcuts. Escape still passes through so it closes modals.
                let escape = key == keyboard::Key::Named(keyboard::key::Named::Escape);
                if status == event::Status::Captured && !escape {
                    return None;
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

        let banner_clicks = crate::sync::banner::clicks().map(Message::BannerClicked);

        let mut subs = vec![keys, activity, banner_clicks];
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

    pub fn update(&mut self, message: Message) -> Task<Message> {
        // Discard stale signals so only writes made by this message count.
        sync_queue::take_local_write();
        let task = self.update_inner(message);
        self.retain_open_view();
        self.discard_dead_composers();
        let task = Task::batch([task, self.request_visible_images()]);
        if sync_queue::take_local_write() && self.config.api_token.is_some() {
            return Task::batch([task, self.drive_sync(SchedEvent::LocalWrite)]);
        }
        task
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        // My Issues spans several teams: create and lane moves need a single
        // team's lanes, so they're no-ops here. Priority and delete act on the
        // issue's own row/team, so they work cross-team.
        if self.view.is_cross_team()
            && matches!(
                message,
                Message::CreateIssue | Message::MoveIssueNextLane | Message::MoveIssuePrevLane
            )
        {
            return Task::none();
        }
        match message {
            Message::KeyPressed(key, modifiers) => {
                // Esc with an autocomplete list open only closes the list.
                if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                    if let Some(slot) = self.open_suggest_mut() {
                        if slot.take().is_some() {
                            return Task::none();
                        }
                    }
                }
                if self.modal == Modal::IssuePicker
                    && key == keyboard::Key::Named(keyboard::key::Named::Tab)
                {
                    return self.update(Message::CycleDependencyKind(!modifiers.shift()));
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
                if matches!(self.view, View::IssueDetail(_)) && !modal_open {
                    if let Some(msg) = keys::handle_issue_detail_key(&key, modifiers) {
                        return self.update(msg);
                    }
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
                // The board's own search, read before the team switch may clear it.
                let board_search = if self.open_view.is_none() && self.search_view == View::Board {
                    self.search_query.clone()
                } else {
                    String::new()
                };
                // Switch team first when needed (this drops any open view).
                let task =if self.selected_team_id.as_deref() != Some(sv.team_id.as_str())
                    || self.view != View::Board
                {
                    self.expanded_teams.insert(sv.team_id.clone());
                    self.update_inner(Message::SelectTeamView(sv.team_id.clone(), View::Board))
                } else {
                    Task::none()
                };
                let local = crate::saved_view::BoardState {
                    display: self.config.display.clone(),
                    filters: self.config.filters.clone(),
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
                use crate::db::dashboards::{create_group_local, GroupError};
                if self.modal != Modal::NewGroup {
                    return Task::none();
                }
                let View::Dashboard(dashboard_id) = self.view.clone() else {
                    return Task::none();
                };
                if self.sync_read_only {
                    self.modal = Modal::None;
                    self.push_toast(
                        "Read-only token: can't add a group",
                        ToastKind::Error,
                        false,
                    );
                    return Task::none();
                }
                match create_group_local(&self.db(), &dashboard_id, &self.group_form.draft) {
                    Ok(_) => {
                        self.modal = Modal::None;
                        self.group_form = GroupForm::default();
                        self.reload_data();
                    }
                    Err(GroupError::Name(e)) => self.group_form.error = Some(e.message()),
                    Err(GroupError::NoDashboard) => {
                        self.group_form.error = Some("This dashboard no longer exists");
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

            Message::SelectTeamView(team_id, view) => {
                // Going to a team's own pages leaves any open Saved view.
                self.open_view = None;
                self.commit_issue_detail();
                self.commit_project_detail();
                self.detail_from_my_issues = false;
                self.detail_from_inbox = false;
                self.detail_from_dashboard = None;
                let from_my = self.view == View::MyIssues;
                if self.selected_team_id.as_deref() != Some(team_id.as_str()) || from_my {
                    self.selected_team_id = Some(team_id);
                    self.focused_lane = 0;
                    self.focused_issue = 0;
                    self.view = view.clone();
                    self.reload_data();
                }
                self.view = view;
                if self.view == View::RecurringIssues {
                    let conn = self.db();
                    self.reload_recurring(&conn);
                }
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
                        return self.open_issue_from_inbox(issue);
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
                self.commit_issue_detail();
                self.commit_project_detail();
                self.detail_from_my_issues = false;
                self.detail_from_inbox = false;
                self.detail_from_dashboard = None;
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
                self.commit_issue_detail();
                self.commit_project_detail();
                self.detail_from_my_issues = false;
                self.detail_from_inbox = false;
                self.detail_from_dashboard = None;
                self.switch_view(View::Dashboard(id));
            }

            Message::SetDashboardSort(sort) => {
                self.dashboard_header.sort = sort;
                self.focused_lane = 0;
                self.focused_issue = 0;
            }

            Message::SetDashboardFilter(filter) => {
                self.dashboard_header.filter = filter;
                self.focused_lane = 0;
                self.focused_issue = 0;
            }

            Message::NavigateToView(view) => {
                self.commit_issue_detail();
                self.commit_project_detail();
                self.detail_from_my_issues = false;
                self.detail_from_inbox = false;
                self.detail_from_dashboard = None;
                self.switch_view(view);
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
                let default_lane = self.lanes.first().map(|l| l.id.clone()).unwrap_or_default();
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

            Message::IssueTitleChanged(v) => {
                if let Some(ref mut form) = self.issue_form {
                    form.title = v;
                }
            }
            Message::IssueDescriptionChanged(action) => {
                if let Some(ref mut form) = self.issue_form {
                    form.description.perform(action);
                }
                self.refresh_form_suggestions();
            }
            Message::IssuePriorityChanged(p) => {
                if let Some(ref mut form) = self.issue_form {
                    form.priority = p;
                }
            }
            Message::IssueLaneChanged(lane_id) => {
                if let Some(ref mut form) = self.issue_form {
                    form.lane_id = lane_id;
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
                    form.error = None;
                }
                if let Some(ref form) = self.issue_form {
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
                            issue.updated_at = now;
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
                            due_date: None,
                            estimate: None,
                            started_at: None,
                            completed_at: None,
                            canceled_at: None,
                            lane_id: form.lane_id.clone(),
                            team_id: team_id.clone(),
                            project_id: form.project_id.clone(),
                            parent_issue_id: form.parent_issue_id.clone(),
                            assignee_id: None,
                            creator_id,
                            archived_at: None,
                            remote_id: None,
                            created_at: now.clone(),
                            updated_at: now,
                        };
                        let _ = queries::insert_issue(&conn, &issue);
                        let _ =
                            queries::set_issue_labels(&conn, &issue.id, &form.selected_label_ids);

                        let payload = serde_json::to_string(&issue).ok();
                        self.enqueue_sync("issue", &issue.id, "create", payload.as_deref());
                    }
                }
                self.modal = Modal::None;
                self.issue_form = None;
                self.reload_data();
            }

            // --- Issue detail: buffered fields ---
            Message::IssueDetailTitleChanged(v) => {
                if let Some(d) = &mut self.issue_detail {
                    d.title = v;
                }
            }
            Message::IssueDetailDescriptionChanged(action) => {
                if let Some(d) = &mut self.issue_detail {
                    d.description.perform(action);
                }
                self.refresh_suggestions(crate::mentions::EditorTarget::Description);
            }

            // --- Issue detail: comments ---
            Message::CommentComposerChanged(action) => {
                if let Some(d) = self.comment_drafts_mut() {
                    d.comment.perform(action);
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
                        *d.comment = iced::widget::text_editor::Content::new();
                        *d.suggest = None;
                    }
                }
            }
            Message::StartReply(parent_id) => {
                let mut dropped = None;
                if let Some(d) = self.comment_drafts_mut() {
                    *d.reply_to = Some(parent_id);
                    *d.suggest = None;
                    *d.reply = iced::widget::text_editor::Content::new();
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
                    *d.reply = iced::widget::text_editor::Content::new();
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
                    d.reply.perform(action);
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
                        *d.reply = iced::widget::text_editor::Content::new();
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

            // --- Issue detail: live-apply pick_lists ---
            Message::IssueDetailStatusChanged(lane_id) => {
                self.apply_issue_update(|i| i.lane_id = lane_id);
            }
            Message::IssueDetailPriorityChanged(p) => {
                self.apply_issue_update(|i| i.priority = p.to_i32());
            }
            Message::IssueDetailProjectChanged(pid) => {
                self.apply_issue_update(|i| i.project_id = pid);
            }
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
            Message::OpenIssueDetail(issue_id) => {
                if let Some(issue) = self.issues.iter().find(|i| i.id == issue_id).cloned() {
                    return self.open_issue_detail(&issue);
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
            Message::DependencyPickerSelected(target_id) => {
                self.add_dependency(&target_id);
                self.issue_picker = None;
                self.modal = Modal::None;
            }
            Message::RemoveDependency(dep_id) => {
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
                        "Moving between lanes needs Display > Group by: Status",
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
                        self.focused_lane = current_lane_idx + 1;
                        self.reload_data();
                    }
                }
            }

            Message::MoveIssuePrevLane => {
                // Lanes are only columns when grouped by status.
                if self.board_display().group_by != GroupBy::Status {
                    self.push_toast(
                        "Moving between lanes needs Display > Group by: Status",
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
                        self.focused_lane = current_lane_idx - 1;
                        self.reload_data();
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

            Message::FocusNextIssue | Message::FocusPrevIssue
                if self.board_display().view_mode == ViewMode::List =>
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
                if self.board_display().view_mode == ViewMode::List =>
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
                    return self.open_focused_issue(issue);
                }
            }

            Message::HeaderSearchChanged(query) => {
                self.set_view_search(query);
                if matches!(self.view, View::Board | View::MyIssues) {
                    self.focused_issue = 0;
                    self.clamp_focus();
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

            Message::CloseModal => {
                // Cancelling the label-delete confirm returns to the panel.
                if matches!(self.modal, Modal::DeleteLabelConfirm(_)) {
                    self.modal = Modal::Labels;
                    return Task::none();
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

            Message::OpenCommandPalette => {
                // Never opens over another modal.
                if self.modal == Modal::None {
                    self.modal = Modal::CommandPalette(PaletteState::new(
                        self.palette_data(),
                    ));
                    return text_input::focus(command_palette::input_id());
                }
            }
            Message::PaletteQueryChanged(query) => {
                if let Modal::CommandPalette(ref mut palette) = self.modal {
                    palette.set_query(query);
                }
            }
            Message::PaletteMove(delta) => {
                if let Modal::CommandPalette(ref mut palette) = self.modal {
                    palette.move_selection(delta);
                }
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
                    PaletteNav::Issue(id) => {
                        let Ok(Some(issue)) = queries::get_issue(&self.db(), &id) else {
                            return Task::none();
                        };
                        let select = self.select_team_for_palette(&issue.team_id);
                        // open_issue_detail commits any open detail edits first.
                        return Task::batch([select, self.open_issue_detail(&issue)]);
                    }
                    PaletteNav::Project { project_id, team_id } => {
                        // The switch reloads the team's data, so the project is
                        // in memory when its detail opens.
                        let select = self.select_team_for_palette(&team_id);
                        return Task::batch([
                            select,
                            self.update(Message::OpenProjectDetail(project_id)),
                        ]);
                    }
                }
            }

            Message::ToggleHelp => {
                self.show_help = !self.show_help;
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
                if let Some(Some(path)) = crate::db::attachments::remove_staged(&conn, &id).ok() {
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

            Message::SyncTick => return self.drive_sync(SchedEvent::Tick),
            Message::UserInput => return self.drive_sync(SchedEvent::Input),
            Message::WindowFocused => return self.drive_sync(SchedEvent::FocusGained),
            Message::WindowUnfocused => return self.drive_sync(SchedEvent::FocusLost),
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

                // If pull deleted the orphan seed team and that's what was
                // selected, drop the now-dangling selection. reload_data will
                // pick the first available team (likely a Rails-backed one).
                if let Some(deleted) = &report.deleted_orphan_team {
                    if self.selected_team_id.as_deref() == Some(deleted.as_str()) {
                        self.selected_team_id = None;
                        self.focused_lane = 0;
                        self.focused_issue = 0;
                    }
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
                    ]);
                }
                return Task::batch([finish, refetch]);
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
                    .retain(|t| t.kind == ToastKind::Error || t.created.elapsed().as_secs() < 4);
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
                self.modal = Modal::ProjectForm;
            }

            Message::EditProject(project_id) => {
                let conn = self.db();
                if let Ok(Some(project)) = queries::get_project(&conn, &project_id) {
                    self.project_form =
                        Some(projects_view::ProjectFormState::from_project(&project));
                    self.modal = Modal::ProjectForm;
                }
            }

            Message::ProjectNameChanged(v) => {
                if let Some(form) = self.project_form.as_mut() {
                    form.name = v;
                }
            }
            Message::ProjectDescriptionChanged(action) => {
                if let Some(form) = self.project_form.as_mut() {
                    form.description.perform(action);
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
                if let Some(project) = self.projects.iter().find(|p| p.id == project_id).cloned() {
                    return self.open_project_detail(&project);
                }
            }
            Message::ProjectSortChanged(sort) => {
                if self.project_sort == sort {
                    self.project_sort_dir = self.project_sort_dir.toggled();
                } else {
                    self.project_sort = sort;
                    self.project_sort_dir = SortDir::Asc;
                }
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
                    let conn = self.db();
                    if let Ok(Some(mut project)) = queries::get_project(&conn, &project_id) {
                        project.roadmap_commitment = Some(commitment);
                        project.updated_at = crate::db::now_iso();
                        let _ = queries::update_project(&conn, &project);
                        let payload = serde_json::to_string(&project).ok();
                        self.enqueue_sync("project", &project.id, "update", payload.as_deref());
                    }
                }
                self.modal = Modal::None;
                self.reload_data();
            }

            // --- Project detail: buffered text fields ---
            Message::ProjectDetailNameChanged(v) => {
                if let Some(detail) = self.project_detail.as_mut() {
                    detail.name = v;
                }
            }
            Message::ProjectDetailDescriptionChanged(action) => {
                if let Some(detail) = self.project_detail.as_mut() {
                    detail.description.perform(action);
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

                    if let Some(ref project_id) = form.editing_project_id {
                        if let Ok(Some(mut project)) = queries::get_project(&conn, project_id) {
                            project.name = form.name.trim().to_string();
                            project.description = description.clone();
                            project.status = Some(form.status);
                            project.priority = form.priority.to_i32();
                            project.lead_id = form.lead_id.clone();
                            project.start_date = start_date;
                            project.due_date = due_date;
                            project.roadmap_commitment = form.roadmap_commitment;
                            project.updated_at = now;
                            let _ = queries::update_project(&conn, &project);

                            let payload = serde_json::to_string(&project).ok();
                            self.enqueue_sync("project", &project.id, "update", payload.as_deref());
                        }
                    } else {
                        let team_id = self.selected_team_id.clone().unwrap_or_default();
                        let project = Project {
                            id: uuid::Uuid::new_v4().to_string(),
                            remote_id: None,
                            team_id,
                            name: form.name.trim().to_string(),
                            description: description.clone(),
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
                    }
                }
                self.modal = Modal::None;
                self.project_form = None;
                self.reload_data();
            }

            Message::ConfirmDeleteProject => {
                if let Some(ref form) = self.project_form {
                    if let Some(ref id) = form.editing_project_id {
                        self.modal = Modal::DeleteProjectConfirm(id.clone());
                    }
                }
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

            Message::DeleteProjectConfirmed => {
                if let Modal::DeleteProjectConfirm(ref project_id) = self.modal {
                    let conn = self.db();
                    let payload = match queries::get_project(&conn, project_id) {
                        Ok(Some(project)) => match project.remote_id {
                            Some(remote_id) => {
                                queries::remote_id_for(&conn, "teams", &project.team_id)
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
                        self.enqueue_sync("project", project_id, "delete", Some(p.as_str()));
                    }
                    let _ = queries::delete_project(&conn, project_id);
                }
                self.modal = Modal::None;
                self.project_form = None;
                self.reload_data();
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
                    f.description.perform(action);
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
                        Err(errors) => form.errors = errors,
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
                    Err(errors) => form.errors = errors,
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
                        let _ = queries::edit_issue_queued(&conn, issue_id, "update", None, |c| {
                            queries::set_issue_labels(c, issue_id, &form.selected_label_ids)
                        });
                    }
                }
                self.modal = Modal::None;
                self.label_form = None;
                self.reload_data();
            }
        }

        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let team_sidebar = team_list::view(team_list::SidebarState {
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
            dashboards: &self.dashboards,
            open_dashboard_id: match &self.view {
                View::Dashboard(id) => Some(id.as_str()),
                _ => None,
            },
        });
        let sidebar_column = column![
            iced::widget::scrollable(team_sidebar).height(Length::Fill),
            team_list::footer(self.selected_team_id.is_some()),
        ];

        let search = self.active_search();
        let needle = header::needle(search);
        let header = header::view(
            self.breadcrumb(),
            search,
            self.search_enabled(),
            matches!(
                self.view,
                View::Board | View::MyIssues | View::ProjectDetail(_)
            )
            .then(|| self.active_filter_count()),
            self.new_action(),
            (self.detail_from_my_issues && matches!(self.view, View::IssueDetail(_)))
                .then_some(Message::BackToMyIssues)
                .or_else(|| {
                    (self.detail_from_inbox && matches!(self.view, View::IssueDetail(_)))
                        .then_some(Message::BackToInbox)
                })
                .or_else(|| {
                    (self.detail_from_dashboard.is_some()
                        && matches!(self.view, View::IssueDetail(_)))
                    .then_some(Message::BackToDashboard)
                }),
            self.update_view_action(),
        );

        let columns = if matches!(self.view, View::Board | View::MyIssues) {
            self.board_columns()
        } else {
            Vec::new()
        };

        let my_empty = if self.view == View::MyIssues {
            self.my_issues_empty()
        } else {
            None
        };
        let main_pane: Element<'_, Message> = match self.view {
            View::MyIssues if my_empty.is_some() => iced::widget::container(
                iced::widget::text(my_empty.unwrap_or_default())
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
                        display: self.board_display(),
                    },
                    &self.collapsed_sections,
                    self.focused_lane,
                    self.focused_issue,
                )
            }
            View::Board | View::MyIssues => board::view(
                &columns,
                board::CardData {
                    teams: &self.teams,
                    lanes: &self.lanes,
                    members: &self.members,
                    projects: &self.projects,
                    issue_labels: &self.issue_labels,
                    display: self.board_display(),
                },
                self.focused_lane,
                self.focused_issue,
            ),
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
                            detail,
                            self.selected_team(),
                            &self.members,
                            &self.issues,
                            self.project_detail_issues(project_id, &needle),
                            self.board_display(),
                            &self.lanes,
                            &self.teams,
                            &self.detail_comments,
                            &self.detail_attachments,
                            &self.detail_comment_attachments,
                            &self.images,
                            &self.config.api_base_url,
                            &self.detail_pending_paths,
                        )
                    }
                    // Project removed by a background sync, or state desync → fallback.
                    _ => placeholder::view("Project"),
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
                            self.selected_team(),
                            &self.teams,
                            &self.lanes,
                            &self.projects,
                            &self.members,
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
                        )
                    }
                    // Issue removed by a background sync, or state desync → graceful fallback.
                    _ => placeholder::view("Issue"),
                }
            }
            View::Roadmap => roadmap_view::view(&self.projects, self.selected_team(), &needle),
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
                        display: self.board_display(),
                    },
                    self.focused_lane,
                    self.focused_issue,
                    self.dashboard_header,
                    self.sync_read_only,
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
        );

        let main_content = column![]
            .push(header)
            .push(main_pane)
            .push(status)
            .width(Length::Fill)
            .height(Length::Fill);

        let layout = row![]
            .push(
                container(sidebar_column)
                    .width(Length::Fixed(220.0))
                    .height(Length::Fill)
                    .style(|_theme: &Theme| iced::widget::container::Style {
                        background: Some(iced::Background::Color(t::bg_menu())),
                        border: iced::Border {
                            color: t::border(),
                            width: 0.0,
                            radius: 0.0.into(),
                        },
                        ..Default::default()
                    }),
            )
            .push(main_content)
            .width(Length::Fill)
            .height(Length::Fill);

        let base: Element<'_, Message> = container(layout)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_theme: &Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(t::bg_primary())),
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
            Modal::IssueForm => {
                if let Some(ref form) = self.issue_form {
                    let modal = issue_form::view(form, &self.lanes, &self.labels);
                    iced::widget::stack![base, modal].into()
                } else {
                    base
                }
            }
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
            Modal::Settings => {
                if let Some(ref form) = self.settings_form {
                    let modal = settings::view(form);
                    iced::widget::stack![base, modal].into()
                } else {
                    base
                }
            }
            Modal::ProjectForm => {
                if let Some(ref form) = self.project_form {
                    let modal = projects_view::modal_view(form, &self.members);
                    iced::widget::stack![base, modal].into()
                } else {
                    base
                }
            }
            Modal::RecurringForm => match self.recurring_form {
                Some(ref form) => {
                    iced::widget::stack![base, crate::views::recurring_issue_form::view(form, &self.labels)]
                        .into()
                }
                None => base,
            },
            Modal::DeleteProjectConfirm(project_id) => {
                let project_name = self
                    .projects
                    .iter()
                    .find(|p| p.id == *project_id)
                    .map(|p| p.name.as_str())
                    .unwrap_or("this project");
                let confirm = delete_project_confirm_modal(project_name);
                iced::widget::stack![base, confirm].into()
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
                    self.group_form.error
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
                    let modal =
                        issue_picker::view(picker, &self.issues, self.selected_team(), &excluded);
                    iced::widget::stack![base, modal].into()
                }
                _ => base,
            },
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

        // Toasts sit above everything, including any open modal.
        if self.toasts.is_empty() {
            content
        } else {
            iced::widget::stack![content, toast::overlay(&self.toasts)].into()
        }
    }
}

/// Boot/default team selection: the first non-archived team, falling back to
/// the first team at all so an all-archived install still shows something.
fn default_team_id(teams: &[Team]) -> Option<String> {
    teams
        .iter()
        .find(|t| t.archived_at.is_none())
        .or_else(|| teams.first())
        .map(|t| t.id.clone())
}

/// Header breadcrumb for `view`, e.g. ["JAIT", "Projects", "Desktop Application"]
/// or ["JAIT", "JAIT-111"]. Team-scoped views lead with the team identifier.
fn breadcrumb(
    view: &View,
    team: Option<&Team>,
    issues: &[Issue],
    projects: &[Project],
) -> Vec<String> {
    if *view == View::MyIssues {
        return vec!["My Issues".into()];
    }
    if *view == View::Inbox {
        return vec!["Inbox".into()];
    }
    if matches!(view, View::Dashboard(_)) {
        return vec!["Dashboards".into()];
    }
    let mut crumbs = vec![team
        .map(|t| t.identifier.clone())
        .unwrap_or_else(|| "JAIT".into())];
    match view {
        View::Board => crumbs.push("Issues".into()),
        View::ProjectList => crumbs.push("Projects".into()),
        View::Roadmap => crumbs.push("Roadmap".into()),
        View::RecurringIssues => crumbs.push("Recurring issues".into()),
        View::ProjectDetail(id) => {
            crumbs.push("Projects".into());
            if let Some(p) = projects.iter().find(|p| p.id == *id) {
                crumbs.push(p.name.clone());
            }
        }
        View::IssueDetail(id) => {
            if let Some(issue) = issues.iter().find(|i| i.id == *id) {
                let number = issue.team_number.unwrap_or(0);
                crumbs.push(match team {
                    Some(t) => format!("{}-{number}", t.identifier),
                    None => format!("#{number}"),
                });
            } else {
                crumbs.push("Issues".into());
            }
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
    use iced::widget::{button, column, container, row, text};
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

    container(modal)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.0, 0.0, 0.0, 0.7,
            ))),
            ..Default::default()
        })
        .into()
}

fn delete_project_confirm_modal(project_name: &str) -> Element<'_, Message> {
    delete_modal(
        "Delete Project?",
        format!("Are you sure you want to delete \"{}\"?", project_name),
        Message::DeleteProjectConfirmed,
    )
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

fn dashboard_name_input_id() -> text_input::Id {
    text_input::Id::new("dashboard-name")
}

/// Name + optional description modal for dashboards (create now, rename later).
fn dashboard_form_modal<'a>(
    title: &'a str,
    submit_label: &'a str,
    submit: Message,
    form: &'a DashboardForm,
) -> Element<'a, Message> {
    use iced::widget::{button, text};

    let mut content = column![]
        .spacing(12)
        .push(text(title).size(18).color(t::text_primary()))
        .push(
            text_input("Name", &form.name)
                .id(dashboard_name_input_id())
                .on_input(Message::NewDashboardNameChanged)
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
            text_input("Description (optional)", &form.description)
                .on_input(Message::NewDashboardDescriptionChanged)
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

    container(modal)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.0, 0.0, 0.0, 0.7,
            ))),
            ..Default::default()
        })
        .into()
}

fn save_view_input_id() -> text_input::Id {
    text_input::Id::new("save-view-name")
}

/// Name prompt for "Save view…".
fn save_view_modal<'a>(title: &'a str, name: &'a str, error: Option<&'static str>) -> Element<'a, Message> {
    use iced::widget::{button, text};

    let mut content = column![]
        .spacing(12)
        .push(text(title).size(18).color(t::text_primary()))
        .push(
            text_input("View name", name)
                .id(save_view_input_id())
                .on_input(Message::SaveViewNameChanged)
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

    container(modal)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.0, 0.0, 0.0, 0.7,
            ))),
            ..Default::default()
        })
        .into()
}

fn delete_modal<'a>(title: &'a str, body: String, confirm: Message) -> Element<'a, Message> {
    use iced::widget::{button, column, container, row, text};
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

    container(modal)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.0, 0.0, 0.0, 0.7,
            ))),
            ..Default::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
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
            archived_at: None,
            remote_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn breadcrumb_follows_view() {
        let jait = team("JAIT", false);
        let issues = vec![issue("i1", "Header bar", 184)];
        let crumbs = |v: View| breadcrumb(&v, Some(&jait), &issues, &[]);
        assert_eq!(crumbs(View::Board), ["JAIT", "Issues"]);
        assert_eq!(crumbs(View::Roadmap), ["JAIT", "Roadmap"]);
        assert_eq!(crumbs(View::IssueDetail("i1".into())), ["JAIT", "JAIT-184"]);
        assert_eq!(crumbs(View::MyIssues), ["My Issues"]);
        assert_eq!(crumbs(View::Inbox), ["Inbox"]);
        // Missing project falls back to the list crumb rather than a blank.
        assert_eq!(
            crumbs(View::ProjectDetail("gone".into())),
            ["JAIT", "Projects"]
        );
    }

    #[test]
    fn issue_search_matches_title_or_identifier() {
        let jait = team("JAIT", false);
        let i = issue("i1", "Header bar — Search", 184);
        let m = |q: &str| header::issue_matches(&i, Some(&jait), &header::needle(q));
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
