use iced::widget::{button, column, container, pick_list, row, text, text_editor, text_input};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Member, Priority, Project, ProjectStatus, RoadmapCommitment};
use crate::theme as t;

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
        }
    }

    pub fn from_project(p: &Project) -> Self {
        Self {
            name: p.name.clone(),
            description: match &p.description {
                Some(body) => text_editor::Content::with_text(body),
                None => text_editor::Content::new(),
            },
            status: p.status,
            priority: Priority::from_i32(p.priority),
            lead_id: p.lead_id.clone(),
            start_date: p.start_date.clone().unwrap_or_default(),
            due_date: p.due_date.clone().unwrap_or_default(),
            roadmap_commitment: p.roadmap_commitment,
            editing_project_id: Some(p.id.clone()),
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

pub fn sidebar_section<'a>(projects: &'a [Project]) -> Element<'a, Message> {
    let header = container(
        row![]
            .align_y(iced::Alignment::Center)
            .push(
                text("PROJECTS")
                    .size(11)
                    .color(t::TEXT_MUTED)
                    .width(Length::Fill),
            )
            .push(
                button(text("+").size(14).color(t::TEXT_MUTED))
                    .on_press(Message::CreateProject)
                    .padding([0, 8])
                    .style(|_theme: &Theme, _status| button::Style {
                        background: Some(iced::Background::Color(iced::Color::TRANSPARENT)),
                        text_color: t::TEXT_MUTED,
                        ..Default::default()
                    }),
            ),
    )
    .padding([14, 14]);

    let mut col = column![].spacing(0).width(Length::Fill).push(header);

    if projects.is_empty() {
        col = col.push(
            container(
                text("No projects yet")
                    .size(12)
                    .color(t::TEXT_MUTED),
            )
            .padding([4, 14]),
        );
    } else {
        for project in projects {
            col = col.push(project_row(project));
        }
    }

    container(col).padding(0).width(Length::Fill).into()
}

fn project_row<'a>(project: &'a Project) -> Element<'a, Message> {
    let status_dot = container(text(" ").size(10))
        .width(Length::Fixed(8.0))
        .height(Length::Fixed(8.0))
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::project_status_color(
                project.status,
            ))),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });

    let header_line = row![]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .push(status_dot)
        .push(
            text(&project.name)
                .size(13)
                .color(t::TEXT_PRIMARY)
                .width(Length::Fill),
        );

    let percent = if project.total_issues_count > 0 {
        (project.completed_issues_count * 100 / project.total_issues_count) as i64
    } else {
        0
    };

    let meta_line = text(format!(
        "{}  ·  {}%  ·  v: {}",
        project.status.as_str(),
        percent,
        project.velocity_score
    ))
    .size(11)
    .color(t::TEXT_MUTED);

    let content = column![]
        .spacing(2)
        .push(header_line)
        .push(meta_line);

    let project_id = project.id.clone();
    button(content)
        .on_press(Message::EditProject(project_id))
        .padding([8, 14])
        .width(Length::Fill)
        .style(|_theme: &Theme, _status| button::Style {
            background: Some(iced::Background::Color(t::BG_MENU)),
            text_color: t::TEXT_PRIMARY,
            border: Border {
                color: iced::Color::TRANSPARENT,
                width: 0.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
}

pub fn modal_view<'a>(
    form: &'a ProjectFormState,
    members: &'a [Member],
) -> Element<'a, Message> {
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
        .push(text(title).size(18).color(t::TEXT_PRIMARY))
        .push(
            text_input("Name", &form.name)
                .on_input(Message::ProjectNameChanged)
                .padding(10)
                .size(14),
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
                        .push(text("Status").size(12).color(t::TEXT_MUTED))
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
                        .push(text("Priority").size(12).color(t::TEXT_MUTED))
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
                .push(text("Lead").size(12).color(t::TEXT_MUTED))
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
                        .push(text("Start date").size(12).color(t::TEXT_MUTED))
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
                        .push(text("Due date").size(12).color(t::TEXT_MUTED))
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
                .push(text("Roadmap commitment").size(12).color(t::TEXT_MUTED))
                .push(pick_list(
                    ROADMAPS.as_slice(),
                    Some(selected_roadmap),
                    |opt: RoadmapOption| Message::ProjectRoadmapCommitmentChanged(opt.0),
                )),
        );

    let mut button_row = row![]
        .spacing(8)
        .push(
            button(text("Save").size(14).color(t::BG_PRIMARY))
                .on_press(Message::SaveProject)
                .padding([8, 24])
                .style(move |_theme: &Theme, _status| button::Style {
                    background: Some(iced::Background::Color(t::ACCENT)),
                    text_color: t::BG_PRIMARY,
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
            button(text("Delete").size(14).color(t::DANGER))
                .on_press(Message::ConfirmDeleteProject)
                .padding([8, 24])
                .style(button::secondary),
        );
    }

    form_content = form_content.push(button_row);

    let modal = container(form_content)
        .padding(28)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_MODAL)),
            border: Border {
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
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.0, 0.0, 0.0, 0.7,
            ))),
            ..Default::default()
        })
        .into()
}
