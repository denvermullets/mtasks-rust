use iced::widget::{
    button, column, container, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::{Message, View};
use crate::db::models::{Issue, Lane, Member, Priority, Project, ProjectStatus, Team};
use crate::theme as t;
use crate::views::projects::MemberOption;

/// Buffered edit state for the full-page project detail view. Name, description,
/// and the two dates live here and are committed (on Enter or on nav-away) via
/// `App::commit_project_detail`. The sidebar pick_lists (Status/Priority/Lead)
/// apply immediately and read straight from the reloaded `Project`, so this
/// buffer is never clobbered by a `reload_data()`.
pub struct ProjectDetailState {
    pub project_id: String,
    pub name: String,
    pub description: text_editor::Content,
    /// "YYYY-MM-DD" or "".
    pub start_date: String,
    pub due_date: String,
    /// Inline validation (blank name). `None` = valid.
    pub error: Option<String>,
}

impl ProjectDetailState {
    pub fn from_project(p: &Project) -> Self {
        let body = p.description.clone().unwrap_or_default();
        Self {
            project_id: p.id.clone(),
            name: p.name.clone(),
            description: text_editor::Content::with_text(&body),
            start_date: p.start_date.clone().unwrap_or_default(),
            due_date: p.due_date.clone().unwrap_or_default(),
            error: None,
        }
    }

    pub fn description_text(&self) -> String {
        self.description.text()
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

pub fn view<'a>(
    project: &'a Project,
    detail: &'a ProjectDetailState,
    team: Option<&'a Team>,
    members: &'a [Member],
    issues: &'a [Issue],
    lanes: &'a [Lane],
) -> Element<'a, Message> {
    // ---- Breadcrumb: "← Projects › <name>" ----
    let breadcrumb = row![]
        .spacing(6)
        .align_y(Alignment::Center)
        .push(
            button(text("← Projects").size(13).color(t::TEXT_SECONDARY))
                .on_press(Message::NavigateToView(View::ProjectList))
                .padding(0)
                .style(button::text),
        )
        .push(text("›").size(13).color(t::TEXT_MUTED))
        .push(text(&project.name).size(13).color(t::TEXT_MUTED));

    // ---- Header: status dot + buffered title ----
    let header = row![]
        .spacing(10)
        .align_y(Alignment::Center)
        .push(
            container(text("\u{25CF}").size(14).color(t::project_status_color(project.status)))
                .center_x(Length::Fixed(28.0))
                .center_y(Length::Fixed(28.0))
                .style(|_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(t::BG_CARD)),
                    border: Border {
                        color: t::BORDER,
                        width: 1.0,
                        radius: 6.0.into(),
                    },
                    ..Default::default()
                }),
        )
        .push(
            text_input("Project name", &detail.name)
                .on_input(Message::ProjectDetailNameChanged)
                .on_submit(Message::CommitProjectDetail)
                .size(22)
                .padding(4)
                .style(title_input_style),
        );

    let left = column![]
        .spacing(16)
        .padding(24)
        .width(Length::Fill)
        .push(breadcrumb)
        .push(header)
        .push_maybe(
            detail
                .error
                .as_deref()
                .map(|e| text(e).size(12).color(t::DANGER)),
        )
        .push(
            text_editor(&detail.description)
                .placeholder("Add description…")
                .on_action(Message::ProjectDetailDescriptionChanged)
                .padding(10)
                .size(14)
                .height(Length::Fixed(160.0)),
        )
        .push(burndown_stub(project))
        .push(issues_section(project, issues, team, lanes, members));

    // ---- Right sidebar: properties ----
    let member_options: Vec<MemberOption> = std::iter::once(MemberOption {
        id: None,
        name: "No lead".to_string(),
    })
    .chain(members.iter().map(|m| MemberOption {
        id: Some(m.id.clone()),
        name: m.name.clone(),
    }))
    .collect();
    let selected_lead = member_options
        .iter()
        .find(|o| o.id == project.lead_id)
        .cloned();

    let sidebar = column![]
        .spacing(18)
        .padding(20)
        .width(Length::Fill)
        .push(text("Properties").size(11).color(t::TEXT_MUTED))
        .push(section(
            "Status",
            pick_list(
                STATUSES.as_slice(),
                Some(project.status),
                Message::ProjectDetailStatusChanged,
            )
            .width(Length::Fill),
        ))
        .push(section(
            "Priority",
            pick_list(
                PRIORITIES.as_slice(),
                Some(project.priority_enum()),
                Message::ProjectDetailPriorityChanged,
            )
            .width(Length::Fill),
        ))
        .push(section(
            "Lead",
            pick_list(member_options, selected_lead, |o: MemberOption| {
                Message::ProjectDetailLeadChanged(o.id)
            })
            .width(Length::Fill),
        ))
        // Project labels aren't wired into the data layer yet (see JAIT-181
        // notes); render read-only until the follow-up ticket lands.
        .push(section("Labels", placeholder_value()))
        .push(section(
            "Start date",
            text_input("YYYY-MM-DD", &detail.start_date)
                .on_input(Message::ProjectDetailStartDateChanged)
                .on_submit(Message::CommitProjectDetail)
                .padding(8)
                .size(13),
        ))
        .push(section(
            "Due date",
            text_input("YYYY-MM-DD", &detail.due_date)
                .on_input(Message::ProjectDetailDueDateChanged)
                .on_submit(Message::CommitProjectDetail)
                .padding(8)
                .size(13),
        ))
        .push(
            column![]
                .spacing(2)
                .push(
                    text(format!("Created {}", project.created_at))
                        .size(11)
                        .color(t::TEXT_MUTED),
                )
                .push(
                    text(format!("Updated {}", project.updated_at))
                        .size(11)
                        .color(t::TEXT_MUTED),
                ),
        );

    let sidebar_panel = container(scrollable(sidebar))
        .width(Length::Fixed(300.0))
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_SECONDARY)),
            ..Default::default()
        });

    let divider = container(column![])
        .width(Length::Fixed(1.0))
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BORDER)),
            ..Default::default()
        });

    row![]
        .width(Length::Fill)
        .height(Length::Fill)
        .push(
            container(scrollable(left))
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .push(divider)
        .push(sidebar_panel)
        .into()
}

/// Placeholder for the real burndown chart (JAIT-217). Shows a compact progress
/// summary in the meantime so the panel isn't empty.
fn burndown_stub<'a>(project: &'a Project) -> Element<'a, Message> {
    let summary = format!(
        "{} of {} issues complete · {}% · velocity {}",
        project.completed_issues_count,
        project.total_issues_count,
        project.progress_percentage,
        project.velocity_score,
    );
    container(
        column![]
            .spacing(6)
            .push(text("Burndown").size(12).color(t::TEXT_SECONDARY))
            .push(text(summary).size(12).color(t::TEXT_MUTED))
            .push(
                text("Chart coming soon")
                    .size(11)
                    .color(t::TEXT_MUTED),
            ),
    )
    .width(Length::Fill)
    .padding(16)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::BG_CARD)),
        border: Border {
            color: t::BORDER,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    })
    .into()
}

/// Issues scoped to this project, each a clickable row that opens issue detail.
fn issues_section<'a>(
    project: &'a Project,
    issues: &'a [Issue],
    team: Option<&'a Team>,
    lanes: &'a [Lane],
    members: &'a [Member],
) -> Element<'a, Message> {
    let scoped: Vec<&Issue> = issues
        .iter()
        .filter(|i| i.project_id.as_deref() == Some(project.id.as_str()))
        .collect();

    let mut col = column![].spacing(8).push(
        text(format!("Issues ({})", scoped.len()))
            .size(13)
            .color(t::TEXT_SECONDARY),
    );

    if scoped.is_empty() {
        col = col.push(
            text("No issues in this project")
                .size(12)
                .color(t::TEXT_MUTED),
        );
    } else {
        for issue in scoped {
            col = col.push(issue_row(issue, team, lanes, members));
        }
    }

    col.into()
}

fn issue_row<'a>(
    issue: &'a Issue,
    team: Option<&'a Team>,
    lanes: &'a [Lane],
    members: &'a [Member],
) -> Element<'a, Message> {
    let identifier = match team {
        Some(tm) => format!("{}-{}", tm.identifier, issue.team_number.unwrap_or(0)),
        None => format!("#{}", issue.team_number.unwrap_or(0)),
    };
    let lane_name = lanes
        .iter()
        .find(|l| l.id == issue.lane_id)
        .map(|l| l.name.as_str())
        .unwrap_or("");
    let assignee = issue
        .assignee_id
        .as_ref()
        .and_then(|aid| members.iter().find(|m| &m.id == aid))
        .map(|m| m.name.as_str())
        .unwrap_or("Unassigned");

    let content = row![]
        .spacing(10)
        .align_y(Alignment::Center)
        .push(text(identifier).size(12).color(t::TEXT_MUTED).width(Length::Fixed(90.0)))
        .push(
            text(&issue.title)
                .size(13)
                .color(t::TEXT_PRIMARY)
                .width(Length::Fill),
        )
        .push(text(lane_name).size(12).color(t::TEXT_SECONDARY))
        .push(text(assignee).size(12).color(t::TEXT_MUTED));

    button(content)
        .on_press(Message::OpenIssueDetail(issue.id.clone()))
        .padding([8, 10])
        .width(Length::Fill)
        .style(|_theme: &Theme, _status| button::Style {
            background: Some(iced::Background::Color(t::BG_CARD)),
            text_color: t::TEXT_PRIMARY,
            border: Border {
                color: t::BORDER,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// A labelled sidebar row: small muted label above its control.
fn section<'a>(label: &'a str, control: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![]
        .spacing(6)
        .push(text(label).size(12).color(t::TEXT_MUTED))
        .push(control.into())
        .into()
}

fn placeholder_value<'a>() -> Element<'a, Message> {
    text("—").size(13).color(t::TEXT_MUTED).into()
}

/// Borderless, transparent style so the title input reads as an inline heading.
fn title_input_style(_theme: &Theme, _status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: iced::Background::Color(iced::Color::TRANSPARENT),
        border: Border {
            color: iced::Color::TRANSPARENT,
            width: 0.0,
            radius: 0.0.into(),
        },
        icon: t::TEXT_MUTED,
        placeholder: t::TEXT_MUTED,
        value: t::TEXT_PRIMARY,
        selection: t::ACCENT,
    }
}
