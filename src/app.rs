use iced::keyboard;
use iced::widget::{column, container, row, text};
use iced::{event, Element, Length, Subscription, Task, Theme};

use crate::api::{ApiClient, ApiError, TeamDto};
use crate::config::{self, Config};
use crate::db::models::{Issue, Lane, Member, Priority, Project, ProjectStatus, RoadmapCommitment, Team};
use crate::db::{connection, queries, sync_queue};
use crate::keys;
use crate::sync::engine::{FullSyncReport, SyncEngine};
use crate::theme as t;
use crate::views::{board, issue_form, projects as projects_view, settings, status_bar, team_list};

#[derive(Debug, Clone)]
pub enum Message {
    // Navigation
    SelectTeam(String),

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
}

pub struct App {
    teams: Vec<Team>,
    selected_team_id: Option<String>,
    lanes: Vec<Lane>,
    issues: Vec<Issue>,
    projects: Vec<Project>,
    members: Vec<Member>,
    focused_lane: usize,
    focused_issue: usize,
    modal: Modal,
    issue_form: Option<issue_form::IssueFormState>,
    project_form: Option<projects_view::ProjectFormState>,
    settings_form: Option<settings::SettingsFormState>,
    show_help: bool,
    pending_sync_count: i32,
    config: Config,
    config_error: Option<String>,
    last_sync_error: Option<String>,
    sync_in_progress: bool,
    /// Tracks the most recent card click for manual double-click detection
    /// (iced 0.13's `mouse_area` exposes only `on_press`, not `on_double_click`).
    last_card_click: Option<(usize, usize, std::time::Instant)>,
}

impl App {
    fn db(&self) -> rusqlite::Connection {
        connection::open().expect("Failed to open database")
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
        } else {
            self.projects.clear();
            self.members.clear();
        }

        self.pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);
        self.clamp_focus();
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

    fn enqueue_sync(&self, entity_type: &str, entity_id: &str, action: &str, payload: Option<&str>) {
        let conn = self.db();
        let _ = sync_queue::enqueue(&conn, entity_type, entity_id, action, payload);
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

        let (lanes, issues, projects, members) = if let Some(ref tid) = selected_team_id {
            (
                queries::lanes_for_team(&conn, tid).unwrap_or_default(),
                queries::issues_for_team(&conn, tid).unwrap_or_default(),
                queries::projects_for_team(&conn, tid).unwrap_or_default(),
                queries::members_for_team(&conn, tid).unwrap_or_default(),
            )
        } else {
            (vec![], vec![], vec![], vec![])
        };

        let pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);

        let app = Self {
            teams,
            selected_team_id,
            lanes,
            issues,
            projects,
            members,
            focused_lane: 0,
            focused_issue: 0,
            modal: Modal::None,
            issue_form: None,
            project_form: None,
            settings_form: None,
            show_help: false,
            pending_sync_count,
            config,
            config_error,
            last_sync_error: None,
            sync_in_progress: false,
            last_card_click: None,
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
            if let iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key, modifiers, ..
            }) = event
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
        Subscription::batch([keys, tick])
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::KeyPressed(key, modifiers) => {
                let modal_open = self.modal != Modal::None;
                if let Some(msg) = keys::handle_key_press(key, modifiers, modal_open) {
                    return self.update(msg);
                }
            }

            Message::SelectTeam(team_id) => {
                self.selected_team_id = Some(team_id);
                self.focused_lane = 0;
                self.focused_issue = 0;
                self.reload_data();
            }

            Message::CreateIssue => {
                let default_lane = self
                    .lanes
                    .first()
                    .map(|l| l.id.clone())
                    .unwrap_or_default();
                self.issue_form = Some(issue_form::IssueFormState::new(default_lane));
                self.modal = Modal::IssueForm;
            }

            Message::EditFocusedIssue => {
                if let Some(issue) = self.focused_issue().cloned() {
                    let body = issue.description.clone().unwrap_or_default();
                    let mut form = issue_form::IssueFormState::new(issue.lane_id.clone())
                        .with_description(&body);
                    form.title = issue.title.clone();
                    form.priority = Priority::from_i32(issue.priority);
                    form.lane_id = issue.lane_id.clone();
                    form.editing_issue_id = Some(issue.id.clone());
                    self.issue_form = Some(form);
                    self.modal = Modal::IssueForm;
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
                if let Some(ref form) = self.issue_form {
                    if form.title.trim().is_empty() {
                        return Task::none();
                    }
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
                        if let Some(mut issue) = self
                            .issues
                            .iter()
                            .find(|i| i.id == *issue_id)
                            .cloned()
                        {
                            issue.title = form.title.trim().to_string();
                            issue.description = description.clone();
                            issue.priority = form.priority.to_i32();
                            issue.lane_id = form.lane_id.clone();
                            issue.updated_at = now;
                            let _ = queries::update_issue(&conn, &issue);

                            let payload = serde_json::to_string(&issue).ok();
                            self.enqueue_sync("issue", &issue.id, "update", payload.as_deref());
                        }
                    } else {
                        // Create new
                        let team_id = self
                            .selected_team_id
                            .clone()
                            .unwrap_or_default();
                        let team_number = queries::next_team_number(&conn, &team_id).unwrap_or(1);
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
                            creator_id: None,
                            archived_at: None,
                            remote_id: None,
                            created_at: now.clone(),
                            updated_at: now,
                        };
                        let _ = queries::insert_issue(&conn, &issue);

                        let payload = serde_json::to_string(&issue).ok();
                        self.enqueue_sync("issue", &issue.id, "create", payload.as_deref());
                    }
                }
                self.modal = Modal::None;
                self.issue_form = None;
                self.reload_data();
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
                            Some(remote_id) => queries::remote_id_for(&conn, "teams", &issue.team_id)
                                .ok()
                                .flatten()
                                .map(|team_remote_id| {
                                    serde_json::json!({
                                        "remote_id": remote_id,
                                        "team_remote_id": team_remote_id,
                                    })
                                    .to_string()
                                }),
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
                let now = std::time::Instant::now();
                let is_double_click = self
                    .last_card_click
                    .map(|(l, i, t)| {
                        l == lane_idx
                            && i == issue_idx
                            && now.duration_since(t).as_millis() < 500
                    })
                    .unwrap_or(false);

                self.focused_lane = lane_idx;
                self.focused_issue = issue_idx;
                self.clamp_focus();

                if is_double_click {
                    self.last_card_click = None;
                    return self.update(Message::EditFocusedIssue);
                } else {
                    self.last_card_click = Some((lane_idx, issue_idx, now));
                }
            }

            Message::CloseModal => {
                self.modal = Modal::None;
                self.issue_form = None;
                self.settings_form = None;
                self.project_form = None;
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
                if self.sync_in_progress {
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
                let token = self
                    .config
                    .api_token
                    .clone()
                    .unwrap_or_default();
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
                                }
                            },
                            Message::SyncCompleted,
                        );
                    }
                    Err(ApiError::MissingToken) => {
                        self.last_sync_error =
                            Some("set an API token in Settings".into());
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

                if report.pull.failed == 0 {
                    self.config.last_synced_at = Some(chrono::Utc::now());
                    let _ = config::save(&self.config);
                }
                self.pending_sync_count = sync_queue::pending_count(&self.db()).unwrap_or(0);
                self.reload_data();
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

            Message::SaveProject => {
                if let Some(ref form) = self.project_form {
                    if form.name.trim().is_empty() {
                        return Task::none();
                    }
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
                            self.enqueue_sync(
                                "project",
                                &project.id,
                                "update",
                                payload.as_deref(),
                            );
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
                            created_at: now.clone(),
                            updated_at: now,
                        };
                        let _ = queries::insert_project(&conn, &project);

                        let payload = serde_json::to_string(&project).ok();
                        self.enqueue_sync(
                            "project",
                            &project.id,
                            "create",
                            payload.as_deref(),
                        );
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
        }

        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let team_sidebar = team_list::view(&self.teams, self.selected_team_id.as_deref());
        let projects_section = projects_view::sidebar_section(&self.projects);
        let divider = container(text(""))
            .height(Length::Fixed(1.0))
            .width(Length::Fill)
            .style(|_theme: &Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(t::BORDER)),
                ..Default::default()
            });
        let sidebar_column = iced::widget::scrollable(
            column![]
                .push(team_sidebar)
                .push(divider)
                .push(projects_section)
                .width(Length::Fill),
        )
        .height(Length::Fill);

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

        let board_view = board::view(
            self.selected_team(),
            &self.lanes,
            &self.issues,
            self.focused_lane,
            self.focused_issue,
        );

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
            .push(board_view)
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
        match &self.modal {
            Modal::IssueForm => {
                if let Some(ref form) = self.issue_form {
                    let modal = issue_form::view(form, &self.lanes);
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
            Modal::None => base,
        }
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

    let modal = container(content)
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

    let modal = container(content)
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
