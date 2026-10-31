use iced::widget::{
    button, column, container, horizontal_rule, mouse_area, pick_list, responsive, row, scrollable,
    text, text_editor, text_input,
};
use iced::{Alignment, Border, Color, Element, Length, Theme};

use chrono::Datelike;

use crate::app::{Message, ProjectSort, SortDir};
use crate::db::models::{Issue, Member, Priority, Project, ProjectStatus, RoadmapCommitment};
use crate::theme as t;
use crate::views::progress_chart;

pub struct ProjectFormState {
    pub name: String,
    pub description: text_editor::Content,
    pub status: ProjectStatus,
    pub priority: Priority,
    pub lead_id: Option<String>,
    pub start_date: String,
    pub due_date: String,
    pub roadmap_commitment: Option<RoadmapCommitment>,
    pub editing_project_id: Option<String>,
    /// Inline validation message shown under the name field. `None` = valid.
    pub error: Option<String>,
}

impl ProjectFormState {
    pub fn new() -> Self {
        Self {
            name: String::new(),
            description: text_editor::Content::new(),
            status: ProjectStatus::Backlog,
            priority: Priority::None,
            lead_id: None,
            start_date: String::new(),
            due_date: String::new(),
            roadmap_commitment: None,
            editing_project_id: None,
            error: None,
        }
    }

    pub fn from_project(p: &Project) -> Self {
        Self {
            name: p.name.clone(),
            description: match &p.description {
                Some(body) => text_editor::Content::with_text(body),
                None => text_editor::Content::new(),
            },
            status: p.status.unwrap_or(ProjectStatus::Backlog),
            priority: Priority::from_i32(p.priority),
            lead_id: p.lead_id.clone(),
            start_date: p.start_date.clone().unwrap_or_default(),
            due_date: p.due_date.clone().unwrap_or_default(),
            roadmap_commitment: p.roadmap_commitment,
            editing_project_id: Some(p.id.clone()),
            error: None,
        }
    }

    pub fn description_text(&self) -> String {
        self.description.text()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberOption {
    pub id: Option<String>,
    pub name: String,
}

impl std::fmt::Display for MemberOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoadmapOption(pub Option<RoadmapCommitment>);

impl std::fmt::Display for RoadmapOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(c) => write!(f, "{}", c),
            None => write!(f, "—"),
        }
    }
}

const STATUSES: [ProjectStatus; 5] = [
    ProjectStatus::Backlog,
    ProjectStatus::Started,
    ProjectStatus::Paused,
    ProjectStatus::Completed,
    ProjectStatus::Cancelled,
];

const PRIORITIES: [Priority; 5] = [
    Priority::Urgent,
    Priority::High,
    Priority::Medium,
    Priority::Low,
    Priority::None,
];

const ROADMAPS: [RoadmapOption; 4] = [
    RoadmapOption(None),
    RoadmapOption(Some(RoadmapCommitment::Now)),
    RoadmapOption(Some(RoadmapCommitment::Next)),
    RoadmapOption(Some(RoadmapCommitment::Later)),
];

pub fn modal_view<'a>(form: &'a ProjectFormState, members: &'a [Member]) -> Element<'a, Message> {
    let title = if form.editing_project_id.is_some() {
        "Edit Project"
    } else {
        "New Project"
    };

    let member_options: Vec<MemberOption> = std::iter::once(MemberOption {
        id: None,
        name: "—".to_string(),
    })
    .chain(members.iter().map(|m| MemberOption {
        id: Some(m.id.clone()),
        name: m.name.clone(),
    }))
    .collect();
    let selected_member = member_options
        .iter()
        .find(|opt| opt.id == form.lead_id)
        .cloned();

    let selected_roadmap = RoadmapOption(form.roadmap_commitment);

    let mut form_content = column![]
        .spacing(14)
        .width(Length::Fixed(460.0))
        .push(text(title).size(18).color(t::text_primary()))
        .push(
            text_input("Name", &form.name)
                .on_input(Message::ProjectNameChanged)
                .padding(10)
                .size(14),
        )
        .push_maybe(
            form.error
                .as_deref()
                .map(|e| text(e).size(12).color(t::danger())),
        )
        .push(
            text_editor(&form.description)
                .placeholder("Description")
                .on_action(Message::ProjectDescriptionChanged)
                .padding(10)
                .size(14)
                .height(Length::Fixed(120.0)),
        )
        .push(
            row![]
                .spacing(12)
                .push(
                    column![]
                        .spacing(4)
                        .width(Length::Fill)
                        .push(text("Status").size(12).color(t::text_muted()))
                        .push(pick_list(
                            STATUSES.as_slice(),
                            Some(form.status),
                            Message::ProjectStatusChanged,
                        )),
                )
                .push(
                    column![]
                        .spacing(4)
                        .width(Length::Fill)
                        .push(text("Priority").size(12).color(t::text_muted()))
                        .push(pick_list(
                            PRIORITIES.as_slice(),
                            Some(form.priority),
                            Message::ProjectPriorityChanged,
                        )),
                ),
        )
        .push(
            column![]
                .spacing(4)
                .push(text("Lead").size(12).color(t::text_muted()))
                .push(pick_list(
                    member_options,
                    selected_member,
                    |opt: MemberOption| Message::ProjectLeadChanged(opt.id),
                )),
        )
        .push(
            row![]
                .spacing(12)
                .push(
                    column![]
                        .spacing(4)
                        .width(Length::Fill)
                        .push(text("Start date").size(12).color(t::text_muted()))
                        .push(
                            text_input("YYYY-MM-DD", &form.start_date)
                                .on_input(Message::ProjectStartDateChanged)
                                .padding(8)
                                .size(13),
                        ),
                )
                .push(
                    column![]
                        .spacing(4)
                        .width(Length::Fill)
                        .push(text("Due date").size(12).color(t::text_muted()))
                        .push(
                            text_input("YYYY-MM-DD", &form.due_date)
                                .on_input(Message::ProjectDueDateChanged)
                                .padding(8)
                                .size(13),
                        ),
                ),
        )
        .push(
            column![]
                .spacing(4)
                .push(text("Roadmap commitment").size(12).color(t::text_muted()))
                .push(pick_list(
                    ROADMAPS.as_slice(),
                    Some(selected_roadmap),
                    |opt: RoadmapOption| Message::ProjectRoadmapCommitmentChanged(opt.0),
                )),
        );

    let mut button_row = row![]
        .spacing(8)
        .push(
            button(text("Save").size(14).color(t::bg_primary()))
                .on_press(Message::SaveProject)
                .padding([8, 24])
                .style(move |_theme: &Theme, status| button::Style {
                    background: Some(iced::Background::Color(t::accent_fill(status))),
                    text_color: t::bg_primary(),
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
        );

    if form.editing_project_id.is_some() {
        button_row = button_row.push(iced::widget::horizontal_space()).push(
            button(text("Delete").size(14).color(t::danger()))
                .on_press(Message::ConfirmDeleteProject)
                .padding([8, 24])
                .style(button::secondary),
        );
    }

    form_content = form_content.push(button_row);

    let modal = container(form_content)
        .padding(28)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_modal())),
            border: Border {
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
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.0, 0.0, 0.0, 0.7,
            ))),
            ..Default::default()
        })
        .into()
}

// ── Projects table (JAIT-181) ──

const COL_HEALTH: f32 = 150.0;
const COL_PRIORITY: f32 = 100.0;
const COL_LEAD: f32 = 56.0;
const COL_TARGET: f32 = 120.0;
const COL_STATUS: f32 = 130.0;
const COL_TREND: f32 = 90.0;
/// Table width below which the Burndown column is hidden.
const TREND_MIN_WIDTH: f32 = 760.0;

/// Full-pane Projects table: a controls row (sort + hide-completed + new), a
/// column header, then one clickable row per project. `projects` arrives already
/// filtered + sorted by the caller (`App::view`) so "Hide completed" and sorting
/// never re-query.
pub fn list_view<'a>(
    projects: &[&'a Project],
    members: &'a [Member],
    sort: ProjectSort,
    dir: SortDir,
    hide_completed: bool,
    issues: &'a [Issue],
) -> Element<'a, Message> {
    // ---- Controls row: Sort by … | Hide completed | + New Project ----
    let mut sort_row = row![]
        .spacing(6)
        .align_y(Alignment::Center)
        .push(text("Sort by").size(12).color(t::text_muted()));
    for option in ProjectSort::ALL {
        let active = option == sort;
        let mut label =
            row![]
                .spacing(4)
                .align_y(Alignment::Center)
                .push(text(option.label()).size(12).color(if active {
                    t::text_primary()
                } else {
                    t::text_secondary()
                }));
        if active {
            label = label.push(text(dir.arrow()).size(9).color(t::text_secondary()));
        }
        sort_row = sort_row.push(
            button(label)
                .on_press(Message::ProjectSortChanged(option))
                .padding([4, 8])
                .style(move |_theme: &Theme, _status| button::Style {
                    background: active.then_some(iced::Background::Color(t::bg_card_focused())),
                    text_color: t::text_primary(),
                    border: Border {
                        color: if active {
                            t::border()
                        } else {
                            Color::TRANSPARENT
                        },
                        width: 1.0,
                        radius: 10.0.into(),
                    },
                    ..Default::default()
                }),
        );
    }

    let hide_label = if hide_completed {
        "Show completed"
    } else {
        "Hide completed"
    };
    let controls = row![]
        .spacing(12)
        .align_y(Alignment::Center)
        .push(sort_row)
        .push(iced::widget::horizontal_space())
        .push(
            button(text(hide_label).size(12).color(t::text_secondary()))
                .on_press(Message::ToggleHideCompletedProjects)
                .padding([6, 10])
                .style(button::text),
        )
        .push(
            text(format!(
                "{} project{}",
                projects.len(),
                if projects.len() == 1 { "" } else { "s" }
            ))
            .size(12)
            .color(t::text_muted()),
        )
        .push(
            button(text("+ New Project").size(13).color(t::bg_primary()))
                .on_press(Message::CreateProject)
                .padding([6, 14])
                .style(move |_theme: &Theme, status| button::Style {
                    background: Some(iced::Background::Color(t::accent_fill(status))),
                    text_color: t::bg_primary(),
                    border: Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        );

    // The fixed columns need ~750px; below that (min window 800 minus sidebar)
    // the sparkline column is dropped so Name keeps room. Header + rows are
    // built inside `responsive` since the available width is only known at layout.
    let projects: Vec<&'a Project> = projects.to_vec();
    let table = responsive(move |size| {
        let show_trend = size.width >= TREND_MIN_WIDTH;
        // ---- Column header ----
        let mut header = row![]
            .spacing(12)
            .padding([8, 16])
            .align_y(Alignment::Center)
            .push(header_cell("Name", Length::Fill))
            .push(header_cell("Health", Length::Fixed(COL_HEALTH)))
            .push(header_cell("Priority", Length::Fixed(COL_PRIORITY)))
            .push(header_cell("Lead", Length::Fixed(COL_LEAD)));
        if show_trend {
            header = header.push(header_cell("Burndown", Length::Fixed(COL_TREND)));
        }
        header = header
            .push(header_cell("Target", Length::Fixed(COL_TARGET)))
            .push(header_cell("Status", Length::Fixed(COL_STATUS)));

        // ---- Rows ----
        let mut rows = column![].width(Length::Fill);
        if projects.is_empty() {
            rows = rows.push(
                container(
                    column![]
                        .spacing(10)
                        .align_x(Alignment::Center)
                        .push(text("No projects yet").size(14).color(t::text_muted()))
                        .push(
                            button(
                                text("Create your first project")
                                    .size(13)
                                    .color(t::accent()),
                            )
                                .on_press(Message::CreateProject)
                                .padding(0)
                                .style(button::text),
                        ),
                )
                .center_x(Length::Fill)
                .padding(40),
            );
        } else {
            for &project in &projects {
                rows = rows.push(project_row(project, members, issues, show_trend));
                rows = rows.push(container(horizontal_rule(1)).style(|_theme: &Theme| {
                    container::Style {
                        background: Some(iced::Background::Color(t::border())),
                        ..Default::default()
                    }
                }));
            }
        }

        column![]
            .spacing(0)
            .width(Length::Fill)
            .push(header)
            .push(
                container(horizontal_rule(1)).style(|_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(t::border())),
                    ..Default::default()
                }),
            )
            .push(scrollable(rows).height(Length::Fill))
            .into()
    });

    let body = column![]
        .spacing(0)
        .width(Length::Fill)
        .push(container(controls).padding([10, 16]))
        .push(
            container(horizontal_rule(1)).style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::border())),
                ..Default::default()
            }),
        )
        .push(table);

    container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_primary())),
            ..Default::default()
        })
        .into()
}

fn header_cell<'a>(label: &'a str, width: Length) -> Element<'a, Message> {
    container(text(label).size(11).color(t::text_muted()))
        .width(width)
        .into()
}

fn project_row<'a>(
    project: &'a Project,
    members: &'a [Member],
    issues: &'a [Issue],
    show_trend: bool,
) -> Element<'a, Message> {
    let (health_color, health_label) = project_health(project);
    let priority = project.priority_enum();

    let name_cell = container(
        text(&project.name)
            .size(14)
            .color(t::text_primary())
            .wrapping(text::Wrapping::None),
    )
    .width(Length::Fill);

    let health_cell = container(
        row![]
            .spacing(6)
            .align_y(Alignment::Center)
            .push(text("\u{25CF}").size(9).color(health_color))
            .push(text(health_label).size(12).color(t::text_secondary())),
    )
    .width(Length::Fixed(COL_HEALTH));

    let priority_cell = container(
        row![]
            .spacing(5)
            .align_y(Alignment::Center)
            .push(text("\u{25B3}").size(10).color(t::priority_color(priority)))
            .push(
                text(format!("{priority}"))
                    .size(12)
                    .color(t::text_secondary()),
            ),
    )
    .width(Length::Fixed(COL_PRIORITY));

    let lead_cell =
        container(lead_avatar(project.lead_id.as_deref(), members)).width(Length::Fixed(COL_LEAD));

    let trend_cell = show_trend.then(|| {
        let trend_data = progress_chart::compute(project, issues, progress_chart::today());
        container(if trend_data.snapshots.is_empty() {
            text("\u{2014}").size(12).color(t::text_muted()).into()
        } else {
            progress_chart::sparkline(trend_data, COL_TREND - 12.0)
        })
        .width(Length::Fixed(COL_TREND))
    });

    let target_color = if project.behind_schedule {
        t::danger()
    } else {
        t::text_secondary()
    };
    let target_cell = container(match project.due_date.as_deref() {
        Some(d) if !d.is_empty() => text(format_target_date(d)).size(12).color(target_color),
        _ => text("\u{2014}").size(12).color(t::text_muted()),
    })
    .width(Length::Fixed(COL_TARGET));

    let status_cell =
        container(progress_status(project.progress_percentage)).width(Length::Fixed(COL_STATUS));

    let row_content = row![]
        .spacing(12)
        .padding([10, 16])
        .align_y(Alignment::Center)
        .push(name_cell)
        .push(health_cell)
        .push(priority_cell)
        .push(lead_cell)
        .push_maybe(trend_cell)
        .push(target_cell)
        .push(status_cell);

    mouse_area(container(row_content).width(Length::Fill))
        .on_press(Message::OpenProjectDetail(project.id.clone()))
        .into()
}

/// Health dot color + label, mirroring Rails `project_health_state`.
fn project_health(project: &Project) -> (Color, String) {
    match project.status {
        Some(ProjectStatus::Completed) => (t::priority_low(), "Complete".to_string()),
        Some(ProjectStatus::Cancelled) => (t::text_muted(), "Cancelled".to_string()),
        Some(ProjectStatus::Paused) => (t::priority_high(), "Paused".to_string()),
        Some(ProjectStatus::Started) => {
            if project.progress_percentage >= 100 {
                (t::priority_low(), "Complete".to_string())
            } else if project.behind_schedule {
                (t::danger(), "At risk".to_string())
            } else {
                (t::priority_low(), "On track".to_string())
            }
        }
        Some(ProjectStatus::Backlog) | None => {
            if project
                .due_date
                .as_deref()
                .map(is_past_date)
                .unwrap_or(false)
            {
                (t::danger(), "Overdue".to_string())
            } else {
                (t::text_muted(), "No updates".to_string())
            }
        }
    }
}

/// A small circular avatar showing the lead's initials, or an empty ring.
fn lead_avatar<'a>(lead_id: Option<&str>, members: &'a [Member]) -> Element<'a, Message> {
    let name = lead_id.and_then(|id| members.iter().find(|m| m.id == id).map(|m| m.name.as_str()));
    let (initials, border, txt) = match name {
        Some(n) => (lead_initials(n), t::accent(), t::text_primary()),
        None => (String::new(), t::border(), t::text_muted()),
    };
    container(text(initials).size(10).color(txt))
        .center_x(Length::Fixed(24.0))
        .center_y(Length::Fixed(24.0))
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_card())),
            border: Border {
                color: border,
                width: 1.0,
                radius: 12.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// Status column: "NN%" over a thin progress bar filled to that percentage.
fn progress_status<'a>(pct: i64) -> Element<'a, Message> {
    let pct = pct.clamp(0, 100);
    let filled = pct as u16;
    let empty = (100 - pct) as u16;
    let bar_color = if pct >= 100 {
        t::priority_low()
    } else if pct > 0 {
        t::priority_medium()
    } else {
        t::text_muted()
    };

    let bar = row![]
        .height(Length::Fixed(4.0))
        .push(
            container(text(""))
                .width(Length::FillPortion(filled.max(1)))
                .height(Length::Fixed(4.0))
                .style(move |_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(bar_color)),
                    border: Border {
                        radius: 2.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        )
        .push(
            container(text(""))
                .width(Length::FillPortion(empty.max(1)))
                .height(Length::Fixed(4.0))
                .style(|_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(t::bg_card_focused())),
                    border: Border {
                        radius: 2.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        );

    column![]
        .spacing(4)
        .push(text(format!("{pct}%")).size(12).color(t::text_secondary()))
        .push(bar)
        .into()
}

/// First letter of up to the first two words, uppercased (e.g. "Ryan Vaznis" → "RV").
pub fn lead_initials(name: &str) -> String {
    name.split_whitespace()
        .take(2)
        .filter_map(|w| w.chars().next())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// "YYYY-MM-DD" → "Jul 6" (or "Jul 6, 2027" if not the current year). Falls back
/// to the raw string when it can't be parsed.
pub fn format_target_date(date: &str) -> String {
    match chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        Ok(d) => {
            let now_year = chrono::Utc::now().format("%Y").to_string();
            let base = format!("{} {}", d.format("%b"), d.day());
            if d.format("%Y").to_string() != now_year {
                format!("{}, {}", base, d.format("%Y"))
            } else {
                base
            }
        }
        Err(_) => date.to_string(),
    }
}

/// True when `date` ("YYYY-MM-DD") is strictly before today (UTC).
fn is_past_date(date: &str) -> bool {
    match chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        Ok(d) => d < chrono::Utc::now().date_naive(),
        Err(_) => false,
    }
}
