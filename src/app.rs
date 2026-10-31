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
use crate::db::events::{self, IssueEvent};
use crate::db::{connection, queries, sync_queue};
use crate::display::{self, CardProperty, FilterField, GroupBy, OrderBy, SubGroup, ViewMode};
use crate::fonts;
use crate::keys;
use crate::scroll;
use crate::sync::engine::{FullSyncReport, SyncEngine, SyncErrorKind};
use crate::sync::scheduler::{CycleKind, Event as SchedEvent, Scheduler};
use crate::theme as t;
use crate::views::toast::{Toast, ToastKind};
use crate::views::{
    board, csv_import, display_panel, header, issue_detail, issue_form, issue_list,
    issue_picker, labels as labels_view, placeholder,
    project_detail, projects as projects_view, roadmap as roadmap_view, settings, status_bar,
    team_list, team_members, toast,
};

#[derive(Debug, Clone)]
pub enum Message {
    // Navigation
    /// Toggle a team's sidebar expansion (chevron / team-header click).
    ToggleTeamExpanded(String),
    /// Select a team and switch the right pane to `view` (team sub-row click).
    SelectTeamView(String, View),
    /// Switch the right pane to a non-team-scoped view (e.g. My Issues).
    NavigateToView(View),

    // Team menu (sidebar kebab)
    /// Open/close the per-team menu on an expanded team header.
    ToggleTeamMenu(String),
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

    // Issue detail — comments (flat thread, v1)
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
    /// Autocomplete row `n` chosen (click or Tab) in the open `@`/`#` suggestion list.
    PickSuggestion(usize),
    /// Enter/Tab in an editor with the suggestion list open: take the highlighted row.
    AcceptSuggestion,
    /// Up/Down (or Ctrl-p/n) in an editor with the suggestion list open.
    MoveSuggestion(i32),
    /// Comments fetched from Rails for a local issue UUID; merged into SQLite.
    CommentsFetched(String, Result<Vec<CommentDto>, String>),
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

    // Header bar
    /// Header search box changed; filters the current view's items.
    HeaderSearchChanged(String),
    /// Focus the header search box (Cmd+F).
    FocusSearch,
    /// "Display" button: open the display-options popover.
    OpenDisplayOptions,
    /// "Filter" button: open the filter popover.
    OpenFilterPanel,
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

    // Keyboard event
    KeyPressed(keyboard::Key, keyboard::Modifiers),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    None,
    IssueForm,
    DeleteConfirm(String),
    Settings,
    ProjectForm,
    DeleteProjectConfirm(String),
    /// Confirm deleting an unsynced comment (local id) and its replies.
    DeleteCommentConfirm(String),
    Labels,
    /// Searchable picker for adding an issue dependency (see `issue_picker`).
    IssuePicker,
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

    /// Relates and duplicates links can't be created yet.
    pub fn enabled(self) -> bool {
        matches!(self, DepDirection::Blocking | DepDirection::BlockedBy)
    }

    /// The next (or previous) enabled kind, wrapping around.
    pub fn cycle(self, forward: bool) -> DepDirection {
        let n = Self::ALL.len();
        let start = Self::ALL.iter().position(|d| *d == self).unwrap_or(0);
        (1..=n)
            .map(|step| {
                let i = if forward { start + step } else { start + n - step };
                Self::ALL[i % n]
            })
            .find(|d| d.enabled())
            .unwrap_or(self)
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
    MyIssues,
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

pub struct App {
    teams: Vec<Team>,
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
    /// Comments on the open detail issue, oldest first. Repopulated each reload
    /// only while `view` is `IssueDetail`.
    detail_comments: Vec<Comment>,
    /// Local change history for the open issue (see `db::events`).
    detail_events: Vec<IssueEvent>,
    /// Issues whose description/comments mention the open detail issue ("Referenced In").
    detail_references: Vec<crate::mentions::Reference>,
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

impl App {
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

        if self.selected_team_id.is_none() {
            self.selected_team_id = default_team_id(&self.teams);
        }

        if self.view == View::MyIssues {
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

        // Filters are global but hold per-team ids; drop the ones this data
        // doesn't contain (My Issues loads every team, so its ids all stay).
        if self.selected_team_id.is_some()
            && self.config.filters.prune(
                &self.members,
                &self.labels,
                &self.lanes,
                &self.projects,
            )
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

    /// Switch to `view`, reloading data when crossing in or out of the
    /// cross-team My Issues view (its data replaces the per-team data).
    fn switch_view(&mut self, view: View) {
        let crossing = (self.view == View::MyIssues) != (view == View::MyIssues);
        self.view = view;
        if crossing {
            self.focused_lane = 0;
            self.focused_issue = 0;
            self.reload_data();
        }
    }

    /// Resolve dependency edges for the open detail issue so the sidebar can
    /// render them; other views don't need this, so keep it cheap.
    fn load_detail_dependencies(&mut self, conn: &rusqlite::Connection) {
        if let View::IssueDetail(ref issue_id) = self.view {
            self.detail_deps =
                queries::dependency_groups_for_issue(conn, issue_id).unwrap_or_default();
            self.detail_comments = queries::comments_for_issue(conn, issue_id).unwrap_or_default();
            self.detail_events = events::events_for_issue_resolved(conn, issue_id, self.config.current_user_remote_id).unwrap_or_default();
            self.detail_references = self.compute_references(conn, issue_id);
        } else {
            self.detail_references.clear();
            self.detail_deps = Default::default();
            self.detail_comments.clear();
            self.detail_events.clear();
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
        let Some(d) = &self.issue_detail else { return };
        let content = match target {
            EditorTarget::Description => &d.description,
            EditorTarget::Comment => &d.comment_composer,
            EditorTarget::Reply => &d.reply_composer,
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
                &d.issue_id,
            ),
            _ => None,
        };
        if let Some(d) = &mut self.issue_detail {
            d.suggest = state;
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
        self.issue_detail.as_mut().map(|d| &mut d.suggest)
    }

    fn pick_suggestion(&mut self, idx: Option<usize>) {
        use crate::mentions::EditorTarget;
        if self.modal == Modal::IssueForm {
            if let Some(form) = &mut self.issue_form {
                if let Some(s) = form.suggest.take() {
                    Self::insert_suggestion(&mut form.description, &s, idx);
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

    /// The header search text if it applies to the current view, else "".
    fn active_search(&self) -> &str {
        if self.search_view == self.view {
            &self.search_query
        } else {
            ""
        }
    }

    fn breadcrumb(&self) -> Vec<String> {
        let mut crumbs = breadcrumb(
            &self.view,
            self.selected_team(),
            &self.issues,
            &self.projects,
        );
        if self.detail_from_my_issues && matches!(self.view, View::IssueDetail(_)) {
            crumbs.insert(0, "My Issues".into());
        }
        crumbs
    }

    /// Empty-state text for My Issues, when there's something to explain.
    fn my_issues_empty(&self) -> Option<&'static str> {
        let ids = display::my_member_ids(&self.members, self.config.current_user_remote_id);
        let visible: usize = self.board_columns().iter().map(|c| c.issues.len()).sum();
        let narrowed = self.config.filters.count() > self.config.filters.assignees.len()
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
            let filters = display::my_issues_filters(&self.config.filters, &ids, &self.lanes);
            return display::build_columns(&data, &self.config.display, &filters, &needle);
        }
        display::build_columns(&data, &self.config.display, &self.config.filters, &needle)
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
        let mut filters = self.config.filters.clone();
        filters.projects.clear();
        display::visible_from(
            self.issues
                .iter()
                .filter(|i| i.project_id.as_deref() == Some(project_id)),
            &data,
            &self.config.display,
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
        let list = self.config.display.view_mode == ViewMode::List;
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
                    display: &self.config.display,
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
        if let Err(e) = config::save(&self.config) {
            self.config_error = Some(e);
        }
        self.clamp_focus();
    }

    /// Active-filter badge count: selected filter values plus a non-empty search.
    fn active_filter_count(&self) -> usize {
        // My Issues pins the assignee, so a saved assignee filter doesn't count.
        // Project detail is scoped to one project, so project filters don't count.
        let ignored = match self.view {
            View::MyIssues => self.config.filters.assignees.len(),
            View::ProjectDetail(_) => self.config.filters.projects.len(),
            _ => 0,
        };
        self.config.filters.count() - ignored
            + usize::from(!header::needle(self.active_search()).is_empty())
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
        if self.view == View::MyIssues {
            self.selected_team_id = Some(issue.team_id.clone());
            self.view = View::Board;
            self.reload_data();
            self.detail_from_my_issues = true;
        }
        self.open_issue_detail(&issue)
    }

    fn open_issue_detail(&mut self, issue: &Issue) -> Task<Message> {
        self.commit_issue_detail();
        self.commit_project_detail();
        self.issue_detail = Some(issue_detail::IssueDetailState::from_issue(issue));
        self.view = View::IssueDetail(issue.id.clone());
        let conn = self.db();
        self.load_detail_dependencies(&conn);
        Task::batch([
            self.fetch_comments_task(&conn, issue),
            self.fetch_issue_task(&conn, issue),
        ])
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
    /// the open detail issue and enqueue its push. Returns whether it was saved.
    /// Replies beyond `MAX_NESTING_DEPTH` are refused (Rails rejects them too).
    fn create_comment(&mut self, body: String, parent_id: Option<String>) -> bool {
        let issue_id = match &self.issue_detail {
            Some(d) => d.issue_id.clone(),
            None => return false,
        };
        if body.is_empty() {
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
            id: uuid::Uuid::new_v4().to_string(),
            remote_id: None,
            issue_id: Some(issue_id),
            project_id: None,
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
        self.enqueue_sync("comment", &comment.id, "create", None);
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
            let _ = queries::update_issue(&conn, &issue);
            let payload = serde_json::to_string(&issue).ok();
            self.enqueue_sync("issue", &issue.id, "update", payload.as_deref());
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
        let conn = self.db();
        match queries::insert_issue_dependency(&conn, &dep) {
            Ok(()) => {
                self.enqueue_sync("dependency", &dep.id, "create", None);
                self.reload_data();
            }
            Err(_) => {
                // Almost always the UNIQUE(blocking, blocked) constraint.
                self.push_toast("Those issues are already linked", ToastKind::Error, false);
            }
        }
    }

    /// Remove a dependency edge. Captures the remote ids the sync delete needs
    /// BEFORE dropping the local row (mirrors the issue-delete path). A
    /// never-synced edge (no `remote_id`) is deleted locally with nothing
    /// enqueued — there is nothing on the server to remove.
    fn remove_dependency(&mut self, dep_id: &str) {
        let conn = self.db();
        let dep = match queries::get_issue_dependency(&conn, dep_id) {
            Ok(Some(d)) => d,
            _ => return,
        };
        if let Some(remote_id) = dep.remote_id {
            // The Rails edge lives under the BLOCKING issue's endpoint.
            let payload = queries::get_issue(&conn, &dep.blocking_issue_id)
                .ok()
                .flatten()
                .and_then(|blocking| {
                    let issue_remote_id = blocking.remote_id?;
                    let team_remote_id =
                        queries::remote_id_for(&conn, "teams", &blocking.team_id)
                            .ok()
                            .flatten()?;
                    Some(
                        serde_json::json!({
                            "team_remote_id": team_remote_id,
                            "issue_remote_id": issue_remote_id,
                            "remote_id": remote_id,
                        })
                        .to_string(),
                    )
                });
            if let Some(p) = &payload {
                self.enqueue_sync("dependency", dep_id, "delete", Some(p.as_str()));
            }
        }
        let _ = queries::delete_issue_dependency(&conn, dep_id);
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
            let _ = queries::update_issue(&conn, &issue);
            let payload = serde_json::to_string(&issue).ok();
            self.enqueue_sync("issue", &issue.id, "update", payload.as_deref());
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
        self.project_detail = Some(project_detail::ProjectDetailState::from_project(project));
        self.view = View::ProjectDetail(project.id.clone());
        Task::none()
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
        let selected_team_id = default_team_id(&teams);

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

        let app = Self {
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
            detail_comments: Vec::new(),
            detail_events: Vec::new(),
            detail_references: Vec::new(),
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
            scheduler: Scheduler::new(std::time::Instant::now()),
            sync_next_delay: Some(crate::sync::scheduler::FOCUSED_INTERVAL),
            toasts: Vec::new(),
            next_toast_id: 0,
            sync_paused_auth: false,
        };

        (app, Task::none())
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
            iced::Event::Window(iced::window::Event::Focused) => Some(Message::WindowFocused),
            iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::WindowUnfocused),
            _ => None,
        });

        let mut subs = vec![keys, activity];
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
        match decision.run {
            Some(kind) => self.start_sync(kind),
            None => Task::none(),
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

    pub fn update(&mut self, message: Message) -> Task<Message> {
        // Discard stale signals so only writes made by this message count.
        sync_queue::take_local_write();
        let task = self.update_inner(message);
        if sync_queue::take_local_write() && self.config.api_token.is_some() {
            return Task::batch([task, self.drive_sync(SchedEvent::LocalWrite)]);
        }
        task
    }

    fn update_inner(&mut self, message: Message) -> Task<Message> {
        // My Issues spans several teams: create and lane moves need a single
        // team's lanes, so they're no-ops here. Priority and delete act on the
        // issue's own row/team, so they work cross-team.
        if self.view == View::MyIssues
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
                let modal_open = self.modal != Modal::None;
                let on_board = matches!(self.view, View::Board | View::MyIssues);
                if let Some(msg) = keys::handle_key_press(key, modifiers, modal_open, on_board) {
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

            Message::SelectTeamView(team_id, view) => {
                self.commit_issue_detail();
                self.commit_project_detail();
                self.detail_from_my_issues = false;
                let from_my = self.view == View::MyIssues;
                if self.selected_team_id.as_deref() != Some(team_id.as_str()) || from_my {
                    self.selected_team_id = Some(team_id);
                    self.focused_lane = 0;
                    self.focused_issue = 0;
                    self.view = view.clone();
                    self.reload_data();
                }
                self.view = view;
            }

            Message::NavigateToView(view) => {
                self.commit_issue_detail();
                self.commit_project_detail();
                self.detail_from_my_issues = false;
                self.switch_view(view);
            }

            Message::ToggleTeamMenu(team_id) => {
                if self.team_menu_open.as_deref() == Some(team_id.as_str()) {
                    self.team_menu_open = None;
                } else {
                    self.team_menu_open = Some(team_id);
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
                            let _ = queries::update_issue(&conn, &issue);
                            let _ = queries::set_issue_labels(
                                &conn,
                                &issue.id,
                                &form.selected_label_ids,
                            );

                            let payload = serde_json::to_string(&issue).ok();
                            self.enqueue_sync("issue", &issue.id, "update", payload.as_deref());
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
                if let Some(d) = &mut self.issue_detail {
                    d.comment_composer.perform(action);
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
                let body = match &self.issue_detail {
                    Some(d) => d.comment_text().trim().to_string(),
                    None => return Task::none(),
                };
                if self.create_comment(body, None) {
                    if let Some(d) = &mut self.issue_detail {
                        d.comment_composer = iced::widget::text_editor::Content::new();
                        d.suggest = None;
                    }
                }
            }
            Message::StartReply(parent_id) => {
                if let Some(d) = &mut self.issue_detail {
                    d.reply_to = Some(parent_id);
                    d.suggest = None;
                    d.reply_composer = iced::widget::text_editor::Content::new();
                }
            }
            Message::CancelReply => {
                if let Some(d) = &mut self.issue_detail {
                    d.reply_to = None;
                    d.reply_composer = iced::widget::text_editor::Content::new();
                    d.suggest = None;
                }
            }
            Message::ReplyComposerChanged(action) => {
                if let Some(d) = &mut self.issue_detail {
                    d.reply_composer.perform(action);
                }
                self.refresh_suggestions(crate::mentions::EditorTarget::Reply);
            }
            Message::SubmitReply => {
                let (body, parent) = match &self.issue_detail {
                    Some(d) => match &d.reply_to {
                        Some(p) => (d.reply_text().trim().to_string(), p.clone()),
                        None => return Task::none(),
                    },
                    None => return Task::none(),
                };
                if self.create_comment(body, Some(parent)) {
                    if let Some(d) = &mut self.issue_detail {
                        d.reply_to = None;
                        d.reply_composer = iced::widget::text_editor::Content::new();
                        d.suggest = None;
                    }
                }
            }
            Message::IssueFetched(result) => {
                if result.is_ok() {
                    self.reload_data();
                }
            }
            Message::CommentsFetched(issue_id, result) => {
                if let Ok(dtos) = result {
                    let conn = self.db();
                    crate::api::dto::merge_comment_tree(&conn, &issue_id, dtos, None);
                    drop(conn);
                    // Refresh so the open detail's comment list reflects the merge.
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
                    if kind.enabled() {
                        picker.direction = kind;
                    }
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
                if self.config.display.group_by != GroupBy::Status {
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
                        let _ = queries::move_issue_to_lane(&conn, &issue.id, &target.id);
                        self.enqueue_sync("issue", &issue.id, "update", None);
                        self.focused_lane = current_lane_idx + 1;
                        self.reload_data();
                    }
                }
            }

            Message::MoveIssuePrevLane => {
                // Lanes are only columns when grouped by status.
                if self.config.display.group_by != GroupBy::Status {
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
                        let _ = queries::move_issue_to_lane(&conn, &issue.id, &target.id);
                        self.enqueue_sync("issue", &issue.id, "update", None);
                        self.focused_lane = current_lane_idx - 1;
                        self.reload_data();
                    }
                }
            }

            Message::SetFocusedPriority(p) => {
                if let Some(issue) = self.focused_issue().cloned() {
                    let conn = self.db();
                    let _ = queries::set_issue_priority(&conn, &issue.id, p);
                    self.enqueue_sync("issue", &issue.id, "update", None);
                    self.reload_data();
                }
            }

            Message::FocusNextIssue | Message::FocusPrevIssue
                if self.config.display.view_mode == ViewMode::List =>
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
                if self.config.display.view_mode == ViewMode::List =>
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
                self.search_query = query;
                self.search_view = self.view.clone();
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
                self.config.display.view_mode = m;
                self.scroll_state.clear();
                self.persist_display();
            }
            Message::ToggleViewMode => {
                let next = match self.config.display.view_mode {
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
                if self.config.display.view_mode == ViewMode::List {
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
                self.config.display.group_by = g;
                // A sub-group repeating the new group-by axis is meaningless.
                if self.config.display.sub_group_conflicts(self.config.display.sub_group) {
                    self.config.display.sub_group = SubGroup::None;
                }
                self.focused_lane = 0;
                self.focused_issue = 0;
                self.persist_display();
            }
            Message::SetSubGroup(g) => {
                if !self.config.display.sub_group_conflicts(g) {
                    self.config.display.sub_group = g;
                    self.persist_display();
                }
            }
            Message::SetOrderBy(o) => {
                self.config.display.order_by = o;
                self.persist_display();
            }
            Message::SetCompletedFilter(f) => {
                self.config.display.set_completed_filter(f);
                self.persist_display();
            }
            Message::ToggleShowSubIssues => {
                let d = &mut self.config.display;
                d.show_sub_issues = !d.show_sub_issues;
                self.persist_display();
            }
            Message::ToggleShowEmptyGroups => {
                let d = &mut self.config.display;
                d.show_empty_groups = !d.show_empty_groups;
                self.persist_display();
            }
            Message::ToggleCardProperty(p) => {
                self.config.display.toggle_property(p);
                self.persist_display();
            }
            Message::ToggleFilter(field, value) => {
                self.config.filters.toggle(field, &value);
                self.focused_issue = 0;
                self.persist_display();
            }
            Message::ClearFilters => {
                self.config.filters.clear();
                // The panel's search box mirrors the header search; clear it too.
                self.search_query.clear();
                self.focused_issue = 0;
                self.persist_display();
            }

            Message::CloseModal => {
                self.modal = Modal::None;
                self.issue_form = None;
                self.settings_form = None;
                self.project_form = None;
                self.label_form = None;
                self.team_members.clear();
                self.csv_import = None;
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
                    self.sync_paused_auth = true;
                    let msg = "Token expired or revoked — please re-enter";
                    if self.settings_form.is_none() {
                        self.settings_form =
                            Some(settings::SettingsFormState::from_config(&self.config));
                    }
                    if let Some(ref mut f) = self.settings_form {
                        f.status = Some(format!("Error: {msg}"));
                    }
                    self.modal = Modal::Settings;
                    self.push_toast(msg, ToastKind::Error, false);
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
                    let _ = queries::delete_unsynced_comment(&conn, comment_id);
                    if let Some(d) = &mut self.issue_detail {
                        if d.reply_to.as_deref() == Some(comment_id.as_str()) {
                            d.reply_to = None;
                        }
                    }
                }
                self.modal = Modal::None;
                self.reload_data();
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
                        let _ =
                            queries::set_issue_labels(&conn, issue_id, &form.selected_label_ids);
                        self.enqueue_sync("issue", issue_id, "update", None);
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
        let team_sidebar = team_list::view(
            &self.teams,
            self.selected_team_id.as_deref(),
            &self.expanded_teams,
            &self.view,
            self.team_menu_open.as_deref(),
            self.show_archived_teams,
        );
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
                .then_some(Message::BackToMyIssues),
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
            View::Board | View::MyIssues if self.config.display.view_mode == ViewMode::List => {
                issue_list::view(
                    &columns,
                    board::CardData {
                        teams: &self.teams,
                        lanes: &self.lanes,
                        members: &self.members,
                        projects: &self.projects,
                        issue_labels: &self.issue_labels,
                        display: &self.config.display,
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
                    display: &self.config.display,
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
                            &self.config.display,
                            &self.lanes,
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
                            &self.detail_events,
                            &self.detail_references,
                            &self.issues,
                            parent,
                            sub_issues,
                            &my_member_ids,
                        )
                    }
                    // Issue removed by a background sync, or state desync → graceful fallback.
                    _ => placeholder::view("Issue"),
                }
            }
            View::Roadmap => roadmap_view::view(&self.projects, self.selected_team(), &needle),
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
                let panel = display_panel::display_view(&self.config.display);
                iced::widget::stack![base, display_panel::popover(panel)].into()
            }
            Modal::FilterPanel => {
                let panel = display_panel::filter_view(
                    &self.config.filters,
                    search,
                    display_panel::FilterData {
                        lanes: &self.lanes,
                        members: &self.members,
                        labels: &self.labels,
                        projects: &self.projects,
                        cross_team: self.view == View::MyIssues,
                        scoped_to_project: matches!(self.view, View::ProjectDetail(_)),
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
            Modal::DeleteCommentConfirm(comment_id) => {
                let replies = queries::comment_reply_count(&self.db(), comment_id).unwrap_or(0);
                iced::widget::stack![base, delete_comment_confirm_modal(replies)].into()
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
    let mut crumbs = vec![team
        .map(|t| t.identifier.clone())
        .unwrap_or_else(|| "JAIT".into())];
    match view {
        View::Board => crumbs.push("Issues".into()),
        View::ProjectList => crumbs.push("Projects".into()),
        View::Roadmap => crumbs.push("Roadmap".into()),
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
        View::MyIssues => {}
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

fn delete_comment_confirm_modal<'a>(replies: usize) -> Element<'a, Message> {
    let body = match replies {
        0 => "Delete this comment?".to_string(),
        1 => "Delete this comment and its 1 reply?".to_string(),
        n => format!("Delete this comment and its {n} replies?"),
    };
    delete_modal("Delete Comment?", body, Message::DeleteCommentConfirmed)
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
    use super::*;

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
    fn dep_direction_cycles_only_through_enabled_kinds() {
        assert_eq!(DepDirection::Blocking.cycle(true), DepDirection::BlockedBy);
        assert_eq!(DepDirection::BlockedBy.cycle(true), DepDirection::Blocking);
        assert_eq!(DepDirection::Blocking.cycle(false), DepDirection::BlockedBy);
    }
}
