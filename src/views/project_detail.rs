use iced::widget::{
    button, column, container, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::{Message, View};
use crate::db::models::{Comment, Issue, Lane, Member, Priority, Project, ProjectStatus, Team};
use crate::theme as t;
use crate::views::issue_detail::{new_comment_composer, push_thread, CommentAttachments, ThreadCtx};
use crate::display::{CardProperty, DisplayOptions};
use crate::views::util;
use crate::views::progress_chart;
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
    /// New-comment composer, cleared on submit.
    pub comment_composer: text_editor::Content,
    /// Reply composer state and the mention list (as on issue detail).
    pub reply_to: Option<String>,
    pub reply_composer: text_editor::Content,
    pub suggest: Option<crate::mentions::SuggestState>,
    /// Files staged on the new-comment composer.
    pub composer_files: crate::views::issue_detail::ComposerFiles,
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
            comment_composer: text_editor::Content::new(),
            reply_to: None,
            reply_composer: text_editor::Content::new(),
            suggest: None,
            composer_files: Default::default(),
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

#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    project: &'a Project,
    detail: &'a ProjectDetailState,
    team: Option<&'a Team>,
    members: &'a [Member],
    issues: &'a [Issue],
    visible: Vec<&'a Issue>,
    display: &'a DisplayOptions,
    lanes: &'a [Lane],
    teams: &'a [Team],
    comments: &'a [Comment],
    attachments: &'a [crate::db::attachments::Attachment],
    comment_attachments: &'a CommentAttachments,
    images: &'a crate::files::images::ImageStore,
    host: &'a str,
) -> Element<'a, Message> {
    // ---- Breadcrumb: "← Projects › <name>" ----
    let breadcrumb = row![]
        .spacing(6)
        .align_y(Alignment::Center)
        .push(
            button(text("← Projects").size(13).color(t::text_secondary()))
                .on_press(Message::NavigateToView(View::ProjectList))
                .padding(0)
                .style(button::text),
        )
        .push(text("›").size(13).color(t::text_muted()))
        .push(text(&project.name).size(13).color(t::text_muted()));

    // ---- Header: status dot + buffered title ----
    let header = row![]
        .spacing(10)
        .align_y(Alignment::Center)
        .push(
            container(
                text("\u{25CF}").size(14).color(
                    project
                        .status
                        .map_or(t::text_muted(), t::project_status_color),
                ),
            )
                .center_x(Length::Fixed(28.0))
                .center_y(Length::Fixed(28.0))
                .style(|_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(t::bg_card())),
                    border: Border {
                        color: t::border(),
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
                .map(|e| text(e).size(12).color(t::danger())),
        )
        .push(
            text_editor(&detail.description)
                .placeholder("Add description…")
                .on_action(Message::ProjectDetailDescriptionChanged)
                .padding(10)
                .size(14)
                .height(Length::Fixed(160.0)),
        )
        .push_maybe(crate::views::issue_detail::description_preview(
            &detail.description_text(),
            &crate::views::body::BodyCtx {
                team,
                teams,
                issues,
                members,
                images,
                host,
                pending_paths: None,
                on_image: None,
            },
        ))
        .push_maybe(crate::views::attachments::section(attachments, images))
        .push(burndown(project, issues))
        .push(issues_section(visible, display, team, lanes, members))
        .push(discussion_section(
            comments, detail, members, team, teams, issues, comment_attachments, images,
        ));

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
        .push(text("Properties").size(11).color(t::text_muted()))
        .push(section(
            "Status",
            pick_list(
                STATUSES.as_slice(),
                project.status,
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
                        .color(t::text_muted()),
                )
                .push(
                    text(format!("Updated {}", project.updated_at))
                        .size(11)
                        .color(t::text_muted()),
                ),
        );

    let sidebar_panel = container(scrollable(sidebar))
        .width(Length::Fixed(300.0))
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_secondary())),
            ..Default::default()
        });

    let divider = container(column![])
        .width(Length::Fixed(1.0))
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::border())),
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

/// The project's discussion: threaded comments with indented replies, rendered
/// by the same rows and composer as issue detail. No delete on project comments
/// (the web has none).
#[allow(clippy::too_many_arguments)]
fn discussion_section<'a>(
    comments: &'a [Comment],
    detail: &'a ProjectDetailState,
    members: &'a [Member],
    team: Option<&'a Team>,
    teams: &'a [Team],
    issues: &'a [Issue],
    comment_attachments: &'a CommentAttachments,
    images: &'a crate::files::images::ImageStore,
) -> Element<'a, Message> {
    let mut col = column![]
        .spacing(12)
        .push(text("Discussion").size(13).color(t::text_secondary()));
    if comments.is_empty() {
        col = col.push(text("No comments yet").size(12).color(t::text_muted()));
    }
    let ctx = ThreadCtx {
        members,
        team,
        teams,
        issues,
        my_member_ids: &[],
        can_reply: true,
        reply_to: detail.reply_to.as_deref(),
        reply_composer: &detail.reply_composer,
        suggest: detail.suggest.as_ref(),
        comment_attachments,
        images,
    };
    push_thread(col, Comment::thread_order(comments), &ctx)
        .push(new_comment_composer(
            &detail.comment_composer,
            detail.suggest.as_ref(),
            &detail.composer_files.files,
        ))
        .into()
}

/// Burndown / progress chart: scope, started, completed over time against the
/// expected-progress line (JAIT-217). Red "Behind schedule" badge when trailing.
fn burndown<'a>(project: &'a Project, issues: &'a [Issue]) -> Element<'a, Message> {
    let summary = format!(
        "{} of {} issues complete · {}% · velocity {}",
        project.completed_issues_count,
        project.total_issues_count,
        project.progress_percentage,
        project.velocity_score,
    );
    let data = progress_chart::compute(project, issues, progress_chart::today());
    let behind = data.behind;
    let body: Element<'a, Message> = if data.snapshots.is_empty() {
        text("No data yet").size(12).color(t::text_muted()).into()
    } else {
        let legend = row![]
            .spacing(14)
            .align_y(Alignment::Center)
            .push(legend_item("Scope", t::chart_scope()))
            .push(legend_item("Started", t::chart_started()))
            .push(legend_item("Completed", t::chart_completed()))
            .push(legend_item("Expected", t::danger()));
        column![]
            .spacing(8)
            .push(legend)
            .push(progress_chart::view(data))
            .into()
    };
    container(
        column![]
            .spacing(6)
            .push(
                row![]
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .push(text("Burndown").size(12).color(t::text_secondary()))
                    .push_maybe(
                        behind.then(|| text("Behind schedule").size(11).color(t::danger())),
                    ),
            )
            .push(text(summary).size(12).color(t::text_muted()))
            .push(body),
    )
    .width(Length::Fill)
    .padding(16)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_card())),
        border: Border {
            color: t::border(),
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn legend_item<'a>(label: &'a str, color: iced::Color) -> Element<'a, Message> {
    row![]
        .spacing(5)
        .align_y(Alignment::Center)
        .push(text("\u{25CF}").size(9).color(color))
        .push(text(label).size(11).color(t::text_secondary()))
        .into()
}

/// This project's issues (already scoped, filtered and ordered by the caller
/// with the board's display options), each a clickable row that opens detail.
fn issues_section<'a>(
    scoped: Vec<&'a Issue>,
    display: &'a DisplayOptions,
    team: Option<&'a Team>,
    lanes: &'a [Lane],
    members: &'a [Member],
) -> Element<'a, Message> {
    let mut col = column![].spacing(8).push(
        text(format!("Issues ({})", scoped.len()))
            .size(13)
            .color(t::text_secondary()),
    );

    if scoped.is_empty() {
        col = col.push(text("No issues to show").size(12).color(t::text_muted()));
    } else {
        for issue in scoped {
            col = col.push(issue_row(issue, display, team, lanes, members));
        }
    }

    col.into()
}

fn issue_row<'a>(
    issue: &'a Issue,
    d: &DisplayOptions,
    team: Option<&'a Team>,
    lanes: &'a [Lane],
    members: &'a [Member],
) -> Element<'a, Message> {
    let mut content = row![].spacing(10).align_y(Alignment::Center);
    if d.shows(CardProperty::Id) {
        let identifier = match team {
            Some(tm) => format!("{}-{}", tm.identifier, issue.team_number.unwrap_or(0)),
            None => format!("#{}", issue.team_number.unwrap_or(0)),
        };
        content = content.push(
            text(identifier)
                .size(12)
                .color(t::text_muted())
                .width(Length::Fixed(90.0)),
        );
    }
    if d.shows(CardProperty::Priority) {
        content = content.push(
            text(format!("\u{25B3} {}", issue.priority))
                .size(12)
                .color(t::priority_color(Priority::from_i32(issue.priority))),
        );
    }
    content = content.push(
        text(&issue.title)
            .size(13)
            .color(t::text_primary())
            .width(Length::Fill),
    );
    // Right-hand facts, only the enabled ones.
    let mut facts: Vec<String> = Vec::new();
    if d.shows(CardProperty::Status) {
        if let Some(l) = lanes.iter().find(|l| l.id == issue.lane_id) {
            facts.push(l.name.clone());
        }
    }
    if d.shows(CardProperty::Assignee) {
        facts.push(
            issue
                .assignee_id
                .as_ref()
                .and_then(|aid| members.iter().find(|m| &m.id == aid))
                .map(|m| m.name.clone())
                .unwrap_or_else(|| "Unassigned".to_string()),
        );
    }
    if d.shows(CardProperty::DueDate) {
        if let Some(due) = issue.due_date.as_deref() {
            facts.push(format!("Due {}", due.get(..10).unwrap_or(due)));
        }
    }
    if d.shows(CardProperty::Estimate) {
        if let Some(e) = issue.estimate {
            facts.push(util::format_estimate(e));
        }
    }
    if d.shows(CardProperty::Created) {
        facts.push(format!("Created {}", util::short_date(&issue.created_at)));
    }
    if d.shows(CardProperty::Updated) {
        facts.push(format!("Updated {}", util::short_date(&issue.updated_at)));
    }
    for f in facts {
        content = content.push(text(f).size(12).color(t::text_secondary()));
    }

    button(content)
        .on_press(Message::OpenIssueDetail(issue.id.clone()))
        .padding([8, 10])
        .width(Length::Fill)
        .style(|_theme: &Theme, _status| button::Style {
            background: Some(iced::Background::Color(t::bg_card())),
            text_color: t::text_primary(),
            border: Border {
                color: t::border(),
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
        .push(text(label).size(12).color(t::text_muted()))
        .push(control.into())
        .into()
}

fn placeholder_value<'a>() -> Element<'a, Message> {
    text("—").size(13).color(t::text_muted()).into()
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
        icon: t::text_muted(),
        placeholder: t::text_muted(),
        value: t::text_primary(),
        selection: t::accent(),
    }
}
