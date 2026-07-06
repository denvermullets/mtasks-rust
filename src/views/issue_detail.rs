use iced::widget::{
    button, column, container, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::{Message, View};
use crate::db::models::{Issue, Lane, Member, Priority, Project, Team};
use crate::theme as t;
use crate::views::issue_form::LaneOption;
use crate::views::projects::MemberOption;

/// Buffered edit state for the full-page issue detail view. Title, description,
/// estimate and due date live here and are committed (on Enter or on nav-away)
/// via `App::commit_issue_detail`. The sidebar pick_lists apply immediately and
/// read their current value straight from the reloaded `Issue`, so this buffer
/// is never clobbered by a `reload_data()`.
pub struct IssueDetailState {
    pub issue_id: String,
    pub title: String,
    pub description: text_editor::Content,
    /// "YYYY-MM-DD" or "".
    pub due_date: String,
    /// Numeric text (e.g. "3", "2.5") or "".
    pub estimate: String,
    /// Inline validation message (blank title / non-numeric estimate). `None` = valid.
    pub error: Option<String>,
}

impl IssueDetailState {
    pub fn from_issue(issue: &Issue) -> Self {
        let body = issue.description.clone().unwrap_or_default();
        Self {
            issue_id: issue.id.clone(),
            title: issue.title.clone(),
            description: text_editor::Content::with_text(&body),
            due_date: issue.due_date.clone().unwrap_or_default(),
            estimate: issue
                .estimate
                .map(|e| {
                    if e.fract() == 0.0 {
                        format!("{}", e as i64)
                    } else {
                        format!("{e}")
                    }
                })
                .unwrap_or_default(),
            error: None,
        }
    }

    pub fn description_text(&self) -> String {
        self.description.text()
    }
}

/// Project pick_list option; the leading `{ id: None }` entry is "No project".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectOption {
    pub id: Option<String>,
    pub name: String,
}

impl std::fmt::Display for ProjectOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

const PRIORITIES: [Priority; 5] = [
    Priority::Urgent,
    Priority::High,
    Priority::Medium,
    Priority::Low,
    Priority::None,
];

pub fn view<'a>(
    issue: &'a Issue,
    detail: &'a IssueDetailState,
    team: Option<&'a Team>,
    lanes: &'a [Lane],
    projects: &'a [Project],
    members: &'a [Member],
    project_name: Option<&'a str>,
) -> Element<'a, Message> {
    // ---- Breadcrumb: "← Issues › [Project ›] IDENT-#" ----
    let identifier = match team {
        Some(tm) => format!("{}-{}", tm.identifier, issue.team_number.unwrap_or(0)),
        None => format!("#{}", issue.team_number.unwrap_or(0)),
    };
    let breadcrumb = row![]
        .spacing(6)
        .align_y(Alignment::Center)
        .push(
            button(text("← Issues").size(13).color(t::TEXT_SECONDARY))
                .on_press(Message::NavigateToView(View::Board))
                .padding(0)
                .style(button::text),
        )
        .push(text("›").size(13).color(t::TEXT_MUTED))
        .push_maybe(project_name.map(|n| text(n).size(13).color(t::TEXT_SECONDARY)))
        .push_maybe(project_name.map(|_| text("›").size(13).color(t::TEXT_MUTED)))
        .push(text(identifier).size(13).color(t::TEXT_MUTED));

    // ---- Left column: title, description, activity stub ----
    let left = column![]
        .spacing(16)
        .padding(24)
        .width(Length::Fill)
        .push(breadcrumb)
        .push(
            text_input("Title", &detail.title)
                .on_input(Message::IssueDetailTitleChanged)
                .on_submit(Message::CommitIssueDetail)
                .size(22)
                .padding(4)
                .style(title_input_style),
        )
        .push_maybe(
            detail
                .error
                .as_deref()
                .map(|e| text(e).size(12).color(t::DANGER)),
        )
        .push(
            text_editor(&detail.description)
                .placeholder("Add description…")
                .on_action(Message::IssueDetailDescriptionChanged)
                .padding(10)
                .size(14)
                .height(Length::Fixed(220.0)),
        )
        .push(
            column![]
                .spacing(8)
                .push(text("Activity").size(13).color(t::TEXT_SECONDARY))
                .push(
                    text("Activity feed coming soon")
                        .size(12)
                        .color(t::TEXT_MUTED),
                ),
        );

    // ---- Right sidebar: properties ----
    let lane_options: Vec<LaneOption> = lanes
        .iter()
        .map(|l| LaneOption {
            id: l.id.clone(),
            name: l.name.clone(),
        })
        .collect();
    let selected_lane = lane_options.iter().find(|l| l.id == issue.lane_id).cloned();

    let project_options: Vec<ProjectOption> = std::iter::once(ProjectOption {
        id: None,
        name: "No project".to_string(),
    })
    .chain(projects.iter().map(|p| ProjectOption {
        id: Some(p.id.clone()),
        name: p.name.clone(),
    }))
    .collect();
    let selected_project = project_options
        .iter()
        .find(|o| o.id == issue.project_id)
        .cloned();

    let member_options: Vec<MemberOption> = std::iter::once(MemberOption {
        id: None,
        name: "Unassigned".to_string(),
    })
    .chain(members.iter().map(|m| MemberOption {
        id: Some(m.id.clone()),
        name: m.name.clone(),
    }))
    .collect();
    let selected_assignee = member_options
        .iter()
        .find(|o| o.id == issue.assignee_id)
        .cloned();

    // Creator is read-only (set on create); resolve the local member's name.
    let creator_name = issue.creator_id.as_ref().and_then(|cid| {
        members
            .iter()
            .find(|m| &m.id == cid)
            .map(|m| m.name.as_str())
    });

    let sidebar = column![]
        .spacing(18)
        .padding(20)
        .width(Length::Fill)
        .push(text("Properties").size(11).color(t::TEXT_MUTED))
        .push(section(
            "Status",
            pick_list(lane_options, selected_lane, |o: LaneOption| {
                Message::IssueDetailStatusChanged(o.id)
            })
            .width(Length::Fill),
        ))
        .push(section(
            "Priority",
            pick_list(
                PRIORITIES.as_slice(),
                Some(issue.priority_enum()),
                Message::IssueDetailPriorityChanged,
            )
            .width(Length::Fill),
        ))
        .push(section(
            "Assignee",
            pick_list(member_options, selected_assignee, |o: MemberOption| {
                Message::IssueDetailAssigneeChanged(o.id)
            })
            .width(Length::Fill),
        ))
        .push(section(
            "Creator",
            match creator_name {
                Some(name) => text(name).size(13).color(t::TEXT_SECONDARY).into(),
                None => placeholder_value(),
            },
        ))
        .push(section(
            "Estimate",
            text_input("No estimate", &detail.estimate)
                .on_input(Message::IssueDetailEstimateChanged)
                .on_submit(Message::CommitIssueDetail)
                .padding(8)
                .size(13),
        ))
        .push(section(
            "Due date",
            text_input("YYYY-MM-DD", &detail.due_date)
                .on_input(Message::IssueDetailDueDateChanged)
                .on_submit(Message::CommitIssueDetail)
                .padding(8)
                .size(13),
        ))
        .push(section(
            "Project",
            pick_list(project_options, selected_project, |o: ProjectOption| {
                Message::IssueDetailProjectChanged(o.id)
            })
            .width(Length::Fill),
        ))
        .push(section("Labels", placeholder_value()))
        .push(section("Blocking", placeholder_value()))
        .push(section("Blocked by", placeholder_value()))
        .push(
            column![]
                .spacing(2)
                .push(
                    text(format!("Created {}", issue.created_at))
                        .size(11)
                        .color(t::TEXT_MUTED),
                )
                .push(
                    text(format!("Updated {}", issue.updated_at))
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

    // Thin vertical rule standing in for a `border-l` between panes.
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
        .push(container(scrollable(left)).width(Length::Fill).height(Length::Fill))
        .push(divider)
        .push(sidebar_panel)
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

/// Em-dash placeholder for sidebar sections filled in by later tickets
/// (Assignee JAIT-113, Labels JAIT-112, Blocking/Blocked-by JAIT-114).
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
