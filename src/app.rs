use iced::keyboard;
use iced::widget::{column, container, row, text};
use iced::{event, Element, Length, Subscription, Task, Theme};

use crate::api::{ApiClient, ApiError, TeamDto};
use crate::config::{self, Config};
use crate::db::models::{Issue, Lane, Priority, Team};
use crate::db::{connection, queries, sync_queue};
use crate::keys;
use crate::theme as t;
use crate::views::{board, issue_form, settings, status_bar, team_list};

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
    IssueDescriptionChanged(String),
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

    // Keyboard event
    KeyPressed(keyboard::Key, keyboard::Modifiers),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    None,
    IssueForm,
    DeleteConfirm(String),
    Settings,
}

pub struct App {
    teams: Vec<Team>,
    selected_team_id: Option<String>,
    lanes: Vec<Lane>,
    issues: Vec<Issue>,
    focused_lane: usize,
    focused_issue: usize,
    modal: Modal,
    issue_form: Option<issue_form::IssueFormState>,
    settings_form: Option<settings::SettingsFormState>,
    show_help: bool,
    pending_sync_count: i32,
    config: Config,
    config_error: Option<String>,
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
        connection::seed_default_data(&conn).expect("Failed to seed data");

        let teams = queries::all_teams(&conn).unwrap_or_default();
        let selected_team_id = teams.first().map(|t| t.id.clone());

        let (lanes, issues) = if let Some(ref tid) = selected_team_id {
            (
                queries::lanes_for_team(&conn, tid).unwrap_or_default(),
                queries::issues_for_team(&conn, tid).unwrap_or_default(),
            )
        } else {
            (vec![], vec![])
        };

        let pending_sync_count = sync_queue::pending_count(&conn).unwrap_or(0);

        let app = Self {
            teams,
            selected_team_id,
            lanes,
            issues,
            focused_lane: 0,
            focused_issue: 0,
            modal: Modal::None,
            issue_form: None,
            settings_form: None,
            show_help: false,
            pending_sync_count,
            config,
            config_error,
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
        event::listen_with(|event, _status, _id| {
            if let iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key, modifiers, ..
            }) = event
            {
                Some(Message::KeyPressed(key, modifiers))
            } else {
                None
            }
        })
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
                    let mut form = issue_form::IssueFormState::new(issue.lane_id.clone());
                    form.title = issue.title.clone();
                    form.description = issue.description.clone().unwrap_or_default();
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
            Message::IssueDescriptionChanged(v) => {
                if let Some(ref mut form) = self.issue_form {
                    form.description = v;
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

                    if let Some(ref issue_id) = form.editing_issue_id {
                        // Update existing
                        if let Some(mut issue) = self
                            .issues
                            .iter()
                            .find(|i| i.id == *issue_id)
                            .cloned()
                        {
                            issue.title = form.title.trim().to_string();
                            issue.description = if form.description.trim().is_empty() {
                                None
                            } else {
                                Some(form.description.trim().to_string())
                            };
                            issue.priority = form.priority as i32;
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
                            description: if form.description.trim().is_empty() {
                                None
                            } else {
                                Some(form.description.trim().to_string())
                            },
                            priority: form.priority as i32,
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
                    self.enqueue_sync("issue", issue_id, "delete", None);
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

            Message::CloseModal => {
                self.modal = Modal::None;
                self.issue_form = None;
                self.settings_form = None;
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

            Message::SyncNow => {
                // Stubbed: just reload count
                self.pending_sync_count = sync_queue::pending_count(&self.db()).unwrap_or(0);
            }
        }

        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let team_sidebar = team_list::view(&self.teams, self.selected_team_id.as_deref());

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

        let status = status_bar::view(
            self.pending_sync_count,
            self.show_help,
            self.config_error.as_deref(),
        );

        let main_content = column![]
            .push(header)
            .push(board_view)
            .push(status)
            .width(Length::Fill)
            .height(Length::Fill);

        let layout = row![]
            .push(
                container(team_sidebar)
                    .width(Length::Fixed(190.0))
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
