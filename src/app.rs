use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use iced::keyboard;
use iced::widget::{column, container, row, text};
use iced::{event, Element, Length, Subscription, Task, Theme};

use crate::api::{ApiClient, ApiError, CommentDto, TeamDto};
use crate::config::{self, Config};
use crate::db::models::{
    Comment, Issue, IssueDependency, Label, Lane, Member, Priority, Project, ProjectStatus,
    RoadmapCommitment, Team,
};
use crate::db::{connection, queries, sync_queue};
use crate::keys;
use crate::sync::engine::{FullSyncReport, SyncEngine, SyncErrorKind};
use crate::theme as t;
use crate::views::toast::{Toast, ToastKind};
use crate::views::{
    board, issue_detail, issue_form, issue_picker, labels as labels_view, placeholder,
    project_detail, projects as projects_view, roadmap as roadmap_view, settings, status_bar,
    team_list, toast,
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
    /// Comments fetched from Rails for a local issue UUID; merged into SQLite.
    CommentsFetched(String, Result<Vec<CommentDto>, String>),

    // Issue detail — dependencies (blocking / blocked-by)
    /// Open the issue-detail view for another issue by its local UUID (e.g. a
    /// click on a linked dependency row). Rebuilds the detail buffer, unlike
    /// `NavigateToView(View::IssueDetail(_))` which only swaps the view.
    OpenIssueDetail(String),
    /// Open the searchable issue picker to add a dependency in `direction`.
    OpenDependencyPicker(DepDirection),
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

    // Settings
    OpenSettings,
    SettingsBaseUrlChanged(String),
    SettingsTokenChanged(String),
    TestConnection,
    TestConnectionResult(Result<usize, String>),
    SaveSettings,

    // Sync
    SyncNow,
    SyncTick,
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
    Labels,
    /// Searchable picker for adding an issue dependency (see `issue_picker`).
    IssuePicker,
    /// "Add project to roadmap" picker; carries the lane a chosen project lands in.
    RoadmapAdd(RoadmapCommitment),
}

/// Which dependency section a picker/add-action targets, relative to the open
/// issue. `Blocking` = the open issue blocks the target; `BlockedBy` = the target
/// blocks the open issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepDirection {
    Blocking,
    BlockedBy,
}

/// Which screen the right-hand pane renders. `MyIssues` still renders a titled
/// "Coming soon" placeholder until its ticket lands (JAIT-186).
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
    detail_blocking: Vec<queries::DependencyLink>,
    detail_blocked_by: Vec<queries::DependencyLink>,
    /// Comments on the open detail issue, oldest first. Repopulated each reload
    /// only while `view` is `IssueDetail`.
    detail_comments: Vec<Comment>,
    /// Search + direction state for the open issue-dependency picker modal.
    issue_picker: Option<issue_picker::IssuePickerState>,
    focused_lane: usize,
    focused_issue: usize,
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
    project_form: Option<projects_view::ProjectFormState>,
    settings_form: Option<settings::SettingsFormState>,
    label_form: Option<labels_view::LabelFormState>,
    show_help: bool,
    pending_sync_count: i32,
    config: Config,
    config_error: Option<String>,
    last_sync_error: Option<String>,
    sync_in_progress: bool,
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
            if let Some(first) = self.teams.first() {
                self.selected_team_id = Some(first.id.clone());
            }
        }

        if let Some(ref team_id) = self.selected_team_id {
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

        self.load_detail_dependencies(&conn);

        self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
        self.clamp_focus();
    }

    /// Resolve dependency edges for the open detail issue so the sidebar can
    /// render them; other views don't need this, so keep it cheap.
    fn load_detail_dependencies(&mut self, conn: &rusqlite::Connection) {
        if let View::IssueDetail(ref issue_id) = self.view {
            self.detail_blocking =
                queries::blocking_links_for_issue(conn, issue_id).unwrap_or_default();
            self.detail_blocked_by =
                queries::blocked_by_links_for_issue(conn, issue_id).unwrap_or_default();
            self.detail_comments = queries::comments_for_issue(conn, issue_id).unwrap_or_default();
        } else {
            self.detail_blocking.clear();
            self.detail_blocked_by.clear();
            self.detail_comments.clear();
        }
    }

    fn selected_team(&self) -> Option<&Team> {
        self.selected_team_id
            .as_ref()
            .and_then(|id| self.teams.iter().find(|t| t.id == *id))
    }

    fn issues_in_focused_lane(&self) -> Vec<&Issue> {
        if let Some(lane) = self.lanes.get(self.focused_lane) {
            self.issues
                .iter()
                .filter(|i| i.lane_id == lane.id)
                .collect()
        } else {
            vec![]
        }
    }

    fn focused_issue(&self) -> Option<&Issue> {
        let lane_issues = self.issues_in_focused_lane();
        lane_issues.get(self.focused_issue).copied()
    }

    fn clamp_focus(&mut self) {
        if self.lanes.is_empty() {
            self.focused_lane = 0;
            self.focused_issue = 0;
            return;
        }
        if self.focused_lane >= self.lanes.len() {
            self.focused_lane = self.lanes.len() - 1;
        }
        let count = self.issues_in_focused_lane().len();
        if count == 0 {
            self.focused_issue = 0;
        } else if self.focused_issue >= count {
            self.focused_issue = count - 1;
        }
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
    fn open_issue_detail(&mut self, issue: &Issue) -> Task<Message> {
        self.commit_issue_detail();
        self.commit_project_detail();
        self.issue_detail = Some(issue_detail::IssueDetailState::from_issue(issue));
        self.view = View::IssueDetail(issue.id.clone());
        let conn = self.db();
        self.load_detail_dependencies(&conn);
        self.fetch_comments_task(&conn, issue)
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
            issue.updated_at = chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string();
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
        // A row means `blocking_issue_id` blocks `blocked_issue_id`.
        let (blocking_issue_id, blocked_issue_id) = match direction {
            DepDirection::Blocking => (current_id, target_id.to_string()),
            DepDirection::BlockedBy => (target_id.to_string(), current_id),
        };
        let now = chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string();
        let dep = IssueDependency {
            id: uuid::Uuid::new_v4().to_string(),
            remote_id: None,
            blocking_issue_id,
            blocked_issue_id,
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
            issue.updated_at = chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string();
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
            project.updated_at = chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string();
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
            project.updated_at = chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string();
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
        let selected_team_id = teams.first().map(|t| t.id.clone());

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
            lanes,
            issues,
            projects,
            members,
            labels,
            issue_labels,
            detail_blocking: Vec::new(),
            detail_blocked_by: Vec::new(),
            detail_comments: Vec::new(),
            issue_picker: None,
            focused_lane: 0,
            focused_issue: 0,
            modal: Modal::None,
            issue_form: None,
            issue_detail: None,
            project_detail: None,
            project_sort: ProjectSort::Created,
            project_sort_dir: SortDir::Asc,
            hide_completed_projects: true,
            project_form: None,
            settings_form: None,
            label_form: None,
            show_help: false,
            pending_sync_count,
            config,
            config_error,
            last_sync_error: None,
            sync_in_progress: false,
            toasts: Vec::new(),
            next_toast_id: 0,
            sync_paused_auth: false,
        };

        (app, Task::none())
    }

    pub fn title(&self) -> String {
        "JAIT — Issue Tracker".to_string()
    }

    pub fn theme(&self) -> Theme {
        Theme::Dark
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let keys = event::listen_with(|event, _status, _id| {
            if let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            {
                Some(Message::KeyPressed(key, modifiers))
            } else {
                None
            }
        });
        let tick = iced::time::every(std::time::Duration::from_secs(
            self.config.sync_interval_secs.max(1),
        ))
        .map(|_| Message::SyncTick);

        let mut subs = vec![keys, tick];
        // Only run the 1s toast-expiry timer while toasts are on screen.
        if !self.toasts.is_empty() {
            subs.push(
                iced::time::every(std::time::Duration::from_secs(1)).map(|_| Message::ToastTick),
            );
        }
        Subscription::batch(subs)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::KeyPressed(key, modifiers) => {
                let modal_open = self.modal != Modal::None;
                let on_board = self.view == View::Board;
                if let Some(msg) = keys::handle_key_press(key, modifiers, modal_open, on_board) {
                    return self.update(msg);
                }
            }

            Message::ToggleTeamExpanded(team_id) => {
                if !self.expanded_teams.remove(&team_id) {
                    self.expanded_teams.insert(team_id);
                }
            }

            Message::SelectTeamView(team_id, view) => {
                self.commit_issue_detail();
                self.commit_project_detail();
                if self.selected_team_id.as_deref() != Some(team_id.as_str()) {
                    self.selected_team_id = Some(team_id);
                    self.focused_lane = 0;
                    self.focused_issue = 0;
                    self.reload_data();
                }
                self.view = view;
            }

            Message::NavigateToView(view) => {
                self.commit_issue_detail();
                self.commit_project_detail();
                self.view = view;
            }

            Message::CreateIssue => {
                let default_lane = self.lanes.first().map(|l| l.id.clone()).unwrap_or_default();
                self.issue_form = Some(issue_form::IssueFormState::new(default_lane));
                self.modal = Modal::IssueForm;
            }

            Message::EditFocusedIssue => {
                if let Some(issue) = self.focused_issue().cloned() {
                    return self.open_issue_detail(&issue);
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
                    let now = chrono::Utc::now()
                        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                        .to_string();

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
                            project_id: None,
                            parent_issue_id: None,
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
            }

            // --- Issue detail: comments ---
            Message::CommentComposerChanged(action) => {
                if let Some(d) = &mut self.issue_detail {
                    d.comment_composer.perform(action);
                }
            }
            Message::SubmitComment => {
                let (issue_id, body) = match &self.issue_detail {
                    Some(d) => (d.issue_id.clone(), d.comment_text().trim().to_string()),
                    None => return Task::none(),
                };
                if body.is_empty() {
                    return Task::none();
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
                let now = chrono::Utc::now()
                    .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                    .to_string();
                let comment = Comment {
                    id: uuid::Uuid::new_v4().to_string(),
                    remote_id: None,
                    issue_id,
                    author_id,
                    body,
                    parent_id: None,
                    created_at: now.clone(),
                    updated_at: now,
                };
                if queries::insert_comment(&conn, &comment).is_ok() {
                    self.enqueue_sync("comment", &comment.id, "create", None);
                    if let Some(d) = &mut self.issue_detail {
                        d.comment_composer = iced::widget::text_editor::Content::new();
                    }
                    self.reload_data();
                }
            }
            Message::CommentsFetched(issue_id, result) => {
                if let Ok(dtos) = result {
                    let conn = self.db();
                    for dto in dtos {
                        let existing = queries::find_comment_by_remote_id(&conn, dto.id)
                            .ok()
                            .flatten()
                            .map(|c| c.id);
                        let comment = dto.into_model(&conn, issue_id.clone(), existing);
                        let _ = queries::upsert_comment(&conn, &comment);
                    }
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
            Message::OpenIssueDetail(issue_id) => {
                if let Some(issue) = self.issues.iter().find(|i| i.id == issue_id).cloned() {
                    return self.open_issue_detail(&issue);
                }
            }
            Message::OpenDependencyPicker(direction) => {
                self.issue_picker = Some(issue_picker::IssuePickerState::new(direction));
                self.modal = Modal::IssuePicker;
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

            Message::FocusNextIssue => {
                let count = self.issues_in_focused_lane().len();
                if count > 0 && self.focused_issue + 1 < count {
                    self.focused_issue += 1;
                }
            }
            Message::FocusPrevIssue => {
                if self.focused_issue > 0 {
                    self.focused_issue -= 1;
                }
            }
            Message::FocusNextLane => {
                if self.focused_lane + 1 < self.lanes.len() {
                    self.focused_lane += 1;
                    self.focused_issue = 0;
                    self.clamp_focus();
                }
            }
            Message::FocusPrevLane => {
                if self.focused_lane > 0 {
                    self.focused_lane -= 1;
                    self.focused_issue = 0;
                    self.clamp_focus();
                }
            }

            Message::CardPressed(lane_idx, issue_idx) => {
                self.focused_lane = lane_idx;
                self.focused_issue = issue_idx;
                self.clamp_focus();
                if let Some(issue) = self.focused_issue().cloned() {
                    return self.open_issue_detail(&issue);
                }
            }

            Message::CloseModal => {
                self.modal = Modal::None;
                self.issue_form = None;
                self.settings_form = None;
                self.project_form = None;
                self.label_form = None;
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
                        }
                        Err(e) => {
                            if let Some(ref mut f) = self.settings_form {
                                f.status = Some(format!("Error: {}", e));
                            }
                        }
                    }
                }
            }

            Message::SyncTick => {
                if self.sync_in_progress || self.sync_paused_auth {
                    return Task::none();
                }
                let has_token = self
                    .config
                    .api_token
                    .as_deref()
                    .map(|t| !t.trim().is_empty())
                    .unwrap_or(false);
                if !has_token {
                    return Task::none();
                }
                return self.update(Message::SyncNow);
            }

            Message::SyncNow => {
                if self.sync_in_progress {
                    return Task::none();
                }
                let base_url = self.config.api_base_url.trim().to_string();
                let token = self.config.api_token.clone().unwrap_or_default();
                match ApiClient::new(base_url, token) {
                    Ok(client) => {
                        self.sync_in_progress = true;
                        self.last_sync_error = None;
                        return Task::perform(
                            async move {
                                let outcome = SyncEngine::pull(&client).await;
                                let push = SyncEngine::drain(&client).await;
                                FullSyncReport {
                                    pull: outcome.report,
                                    push,
                                    deleted_orphan_team: outcome.deleted_orphan_team,
                                    current_user: outcome.current_user,
                                }
                            },
                            Message::SyncCompleted,
                        );
                    }
                    Err(ApiError::MissingToken) => {
                        self.last_sync_error = Some("set an API token in Settings".into());
                    }
                    Err(e) => {
                        self.last_sync_error = Some(e.to_string());
                    }
                }
                self.pending_sync_count = sync_queue::pending_count(&self.db()).unwrap_or(0);
            }

            Message::SyncCompleted(report) => {
                self.sync_in_progress = false;
                self.last_sync_error = report.last_error().map(str::to_string);

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
                            SyncErrorKind::Auth | SyncErrorKind::Network => {}
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
                if report.pull.failed == 0 {
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
                return self.update(Message::SyncNow);
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
                        project.updated_at = chrono::Utc::now()
                            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                            .to_string();
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
                self.apply_project_update(|p| p.status = s);
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
                    let now = chrono::Utc::now()
                        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                        .to_string();

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
                            project.status = form.status;
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
                            status: form.status,
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
                    let now = chrono::Utc::now()
                        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                        .to_string();
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
        );
        let sidebar_column = iced::widget::scrollable(team_sidebar).height(Length::Fill);

        let header_content: Element<'_, Message> = match self.selected_team() {
            Some(team) => row![]
                .spacing(10)
                .align_y(iced::Alignment::Center)
                .push(text(&team.identifier).size(18).color(t::TEXT_PRIMARY))
                .push(text(&team.name).size(14).color(t::TEXT_MUTED))
                .into(),
            None => text("JAIT").size(18).color(t::TEXT_PRIMARY).into(),
        };

        let header = container(header_content)
            .padding([12, 16])
            .width(Length::Fill)
            .style(|_theme: &Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(t::BG_SECONDARY)),
                border: iced::Border {
                    color: t::BORDER,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            });

        let main_pane: Element<'_, Message> = match self.view {
            View::Board => board::view(
                self.selected_team(),
                &self.lanes,
                &self.issues,
                &self.issue_labels,
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
                                ProjectStatus::Completed | ProjectStatus::Cancelled
                            )
                    })
                    .collect();
                sort_projects(&mut list, self.project_sort, self.project_sort_dir);
                projects_view::list_view(
                    &list,
                    &self.members,
                    self.project_sort,
                    self.project_sort_dir,
                    self.hide_completed_projects,
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
                        issue_detail::view(
                            issue,
                            detail,
                            self.selected_team(),
                            &self.lanes,
                            &self.projects,
                            &self.members,
                            project_name,
                            &self.detail_blocking,
                            &self.detail_blocked_by,
                            &self.detail_comments,
                        )
                    }
                    // Issue removed by a background sync, or state desync → graceful fallback.
                    _ => placeholder::view("Issue"),
                }
            }
            View::Roadmap => roadmap_view::view(&self.projects, self.selected_team()),
            View::MyIssues => placeholder::view("My Issues"),
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
                        background: Some(iced::Background::Color(t::BG_MENU)),
                        border: iced::Border {
                            color: t::BORDER,
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
                background: Some(iced::Background::Color(t::BG_PRIMARY)),
                ..Default::default()
            })
            .into();

        // Overlay modals
        let content: Element<'_, Message> = match &self.modal {
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
            Modal::IssuePicker => match (&self.issue_picker, &self.issue_detail) {
                (Some(picker), Some(detail)) => {
                    // Exclude the open issue itself and any issue already linked
                    // in this direction (a duplicate would hit the UNIQUE edge).
                    let existing = match picker.direction {
                        DepDirection::Blocking => &self.detail_blocking,
                        DepDirection::BlockedBy => &self.detail_blocked_by,
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
    match (a.as_deref().filter(|s| !s.is_empty()), b.as_deref().filter(|s| !s.is_empty())) {
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
        .push(text("Delete Issue?").size(18).color(t::TEXT_PRIMARY))
        .push(
            text(format!(
                "Are you sure you want to delete \"{}\"?",
                issue_title
            ))
            .size(14)
            .color(t::TEXT_SECONDARY),
        )
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text("Delete").size(14).color(iced::Color::WHITE))
                        .on_press(Message::DeleteConfirmed)
                        .padding([8, 24])
                        .style(move |_theme: &Theme, _status| button::Style {
                            background: Some(iced::Background::Color(t::DANGER)),
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
                background: Some(iced::Background::Color(t::BG_MODAL)),
                border: iced::Border {
                    color: t::BORDER,
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
    use iced::widget::{button, column, container, row, text};
    use iced::Border;

    let content = column![]
        .spacing(16)
        .push(text("Delete Project?").size(18).color(t::TEXT_PRIMARY))
        .push(
            text(format!(
                "Are you sure you want to delete \"{}\"?",
                project_name
            ))
            .size(14)
            .color(t::TEXT_SECONDARY),
        )
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text("Delete").size(14).color(iced::Color::WHITE))
                        .on_press(Message::DeleteProjectConfirmed)
                        .padding([8, 24])
                        .style(move |_theme: &Theme, _status| button::Style {
                            background: Some(iced::Background::Color(t::DANGER)),
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
                background: Some(iced::Background::Color(t::BG_MODAL)),
                border: iced::Border {
                    color: t::BORDER,
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
