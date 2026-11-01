use iced::widget::{
    button, column, container, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::{Message, View};
use crate::db::models::{Comment, Issue, Lane, Member, Priority, Project, ProjectStatus, Team};
use crate::theme::{self as t, radius, space, type_scale};
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
    /// True while the description editor is open; otherwise it is drawn rendered.
    pub editing_description: bool,
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
            editing_description: false,
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
    pending_paths: &'a crate::views::body::PendingInlineMap,
) -> Element<'a, Message> {
    // ---- Breadcrumb: "← Projects › <name>" ----
    let breadcrumb = row![]
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push(
            button(text("← Projects").size(type_scale::SM).color(t::text_secondary()))
                .on_press(Message::NavigateToView(View::ProjectList))
                .padding(0)
                .style(button::text),
        )
        .push(text("›").size(type_scale::SM).color(t::text_muted()))
        .push(text(&project.name).size(type_scale::SM).color(t::text_secondary()));

    // ---- Header: status dot + buffered title ----
    let status_color = project
        .status
        .map_or(t::text_muted(), t::project_status_color);
    let header = row![]
        .spacing(space::S3)
        .align_y(Alignment::Center)
        .push(
            container(
                text("\u{25CF}").size(type_scale::LG).color(status_color),
            )
                .center_x(Length::Fixed(space::S10))
                .center_y(Length::Fixed(space::S10))
                .style(move |_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(iced::Color {
                        a: STATUS_TILE_ALPHA,
                        ..status_color
                    })),
                    border: Border {
                        radius: radius::SM.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        )
        .push(
            text_input("Project name", &detail.name)
                .on_input(Message::ProjectDetailNameChanged)
                .on_submit(Message::CommitProjectDetail)
                .size(type_scale::XXL)
                .font(crate::fonts::bold(crate::fonts::active()))
                .padding(space::S1)
                .style(title_input_style),
        );

    let head = column![]
        .spacing(space::S4)
        .push(breadcrumb)
        .push(header)
        .push_maybe(
            detail
                .error
                .as_deref()
                .map(|e| text(e).size(type_scale::XS).color(t::danger())),
        );

    // Title-to-body gap is S6; the sections below are S8 apart. The attach
    // button stays close to the description it belongs to.
    let description = column![]
        .spacing(space::S2)
        .push(description_section(detail, &crate::views::body::BodyCtx {
            teams,
            issues,
            members,
            images,
            host,
            pending_paths: Some(pending_paths),
            on_image: Some(Message::OpenInlineImage),
        }))
        .push(
            button(text("+ Attach file").size(type_scale::XS).color(t::accent()))
                .on_press(Message::PickAttachmentFiles)
                .padding(0)
                .style(button::text),
        );
    let sections = column![]
        .spacing(space::S8)
        .push(description)
        .push_maybe(crate::views::attachments::section(attachments, images))
        .push(burndown(project, issues))
        .push(issues_section(visible, display, team, lanes, members))
        .push(discussion_section(
            comments, detail, members, teams, issues, comment_attachments, images, host,
            pending_paths,
        ));
    let left = column![]
        .spacing(space::S6)
        .padding(space::S8)
        .width(Length::Fill)
        .push(head)
        .push(sections);

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
        .spacing(space::S6)
        .padding(space::S6)
        .width(Length::Fill)
        .push(util::sidebar_heading("Properties"))
        .push(util::sidebar_field(
            "Status",
            pick_list(
                STATUSES.as_slice(),
                project.status,
                Message::ProjectDetailStatusChanged,
            )
            .width(Length::Fill)
            .padding(util::SIDEBAR_CONTROL_PADDING)
            .text_size(type_scale::SM)
            .style(util::sidebar_pick_list_style),
        ))
        .push(util::sidebar_field(
            "Priority",
            pick_list(
                PRIORITIES.as_slice(),
                Some(project.priority_enum()),
                Message::ProjectDetailPriorityChanged,
            )
            .width(Length::Fill)
            .padding(util::SIDEBAR_CONTROL_PADDING)
            .text_size(type_scale::SM)
            .style(util::sidebar_pick_list_style),
        ))
        .push(util::sidebar_field(
            "Lead",
            pick_list(member_options, selected_lead, |o: MemberOption| {
                Message::ProjectDetailLeadChanged(o.id)
            })
            .width(Length::Fill)
            .padding(util::SIDEBAR_CONTROL_PADDING)
            .text_size(type_scale::SM)
            .style(util::sidebar_pick_list_style),
        ))
        // Project labels aren't wired into the data layer yet (see JAIT-181
        // notes); render read-only until the follow-up ticket lands.
        .push(util::sidebar_field("Labels", placeholder_value()))
        .push(util::sidebar_field(
            "Start date",
            text_input("YYYY-MM-DD", &detail.start_date)
                .on_input(Message::ProjectDetailStartDateChanged)
                .on_submit(Message::CommitProjectDetail)
                .padding(util::SIDEBAR_CONTROL_PADDING)
                .size(type_scale::SM)
                .style(util::sidebar_input_style),
        ))
        .push(util::sidebar_field(
            "Due date",
            text_input("YYYY-MM-DD", &detail.due_date)
                .on_input(Message::ProjectDetailDueDateChanged)
                .on_submit(Message::CommitProjectDetail)
                .padding(util::SIDEBAR_CONTROL_PADDING)
                .size(type_scale::SM)
                .style(util::sidebar_input_style),
        ))
        .push(
            column![]
                .spacing(space::S0_5)
                .push(
                    text(format!("Created {}", project.created_at))
                        .size(type_scale::XS)
                        .color(t::text_muted()),
                )
                .push(
                    text(format!("Updated {}", project.updated_at))
                        .size(type_scale::XS)
                        .color(t::text_muted()),
                ),
        );

    let sidebar_panel = util::sidebar_panel(scrollable(sidebar));

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
    teams: &'a [Team],
    issues: &'a [Issue],
    comment_attachments: &'a CommentAttachments,
    images: &'a crate::files::images::ImageStore,
    host: &'a str,
    pending_paths: &'a crate::views::body::PendingInlineMap,
) -> Element<'a, Message> {
    let mut col = column![].spacing(space::S1);
    if comments.is_empty() {
        col = col.push(text("No comments yet").size(type_scale::SM).color(t::text_muted()));
    }
    let ctx = ThreadCtx {
        members,
        teams,
        issues,
        my_member_ids: &[],
        can_reply: true,
        reply_to: detail.reply_to.as_deref(),
        reply_composer: &detail.reply_composer,
        suggest: detail.suggest.as_ref(),
        comment_attachments,
        images,
        host,
        pending_paths,
    };
    let col = push_thread(col, Comment::thread_order(comments), &ctx).push(
        container(new_comment_composer(
            &detail.comment_composer,
            detail.suggest.as_ref(),
            &detail.composer_files.files,
        ))
        .padding(iced::Padding::ZERO.top(space::S4)),
    );
    with_heading("Discussion", col)
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
        text("No data yet").size(type_scale::XS).color(t::text_muted()).into()
    } else {
        let legend = row![]
            .spacing(space::S3)
            .align_y(Alignment::Center)
            .push(legend_item("Scope", t::chart_scope()))
            .push(legend_item("Started", t::chart_started()))
            .push(legend_item("Completed", t::chart_completed()))
            .push(legend_item("Expected", t::danger()));
        column![]
            .spacing(space::S2)
            .push(legend)
            .push(progress_chart::view(data))
            .into()
    };
    container(
        column![]
            .spacing(space::S1_5)
            .push(
                row![]
                    .spacing(space::S3)
                    .align_y(Alignment::Center)
                    .push(text("Burndown").size(type_scale::SM).color(t::text_secondary()))
                    .push_maybe(behind.then(|| {
                        text("Behind schedule").size(type_scale::XS).color(t::danger())
                    })),
            )
            .push(text(summary).size(type_scale::XS).color(t::text_muted()))
            .push(body),
    )
    .width(Length::Fill)
    .padding(space::S4)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_card())),
        border: Border {
            color: t::border(),
            width: 1.0,
            radius: radius::MD.into(),
        },
        ..Default::default()
    })
    .into()
}

fn legend_item<'a>(label: &'a str, color: iced::Color) -> Element<'a, Message> {
    row![]
        .spacing(space::S1)
        .align_y(Alignment::Center)
        .push(text("\u{25CF}").size(type_scale::XS).color(color))
        .push(text(label).size(type_scale::XS).color(t::text_secondary()))
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
    let heading = format!("Issues ({})", scoped.len());
    let mut col = column![].spacing(space::S1);

    if scoped.is_empty() {
        col = col.push(text("No issues to show").size(type_scale::SM).color(t::text_muted()));
    } else {
        for issue in scoped {
            col = col.push(issue_row(issue, display, team, lanes, members));
        }
    }

    with_heading_owned(heading, col)
}

fn issue_row<'a>(
    issue: &'a Issue,
    d: &DisplayOptions,
    team: Option<&'a Team>,
    lanes: &'a [Lane],
    members: &'a [Member],
) -> Element<'a, Message> {
    let mut content = row![].spacing(space::S3).align_y(Alignment::Center);
    if d.shows(CardProperty::Id) {
        let identifier = match team {
            Some(tm) => format!("{}-{}", tm.identifier, issue.team_number.unwrap_or(0)),
            None => format!("#{}", issue.team_number.unwrap_or(0)),
        };
        content = content.push(
            text(identifier)
                .size(type_scale::XS)
                .font(crate::fonts::code())
                .color(t::text_muted())
                .width(Length::Fixed(space::S16 + space::S4)),
        );
    }
    if d.shows(CardProperty::Priority) {
        content = content.push(
            text(format!("\u{25B3} {}", issue.priority))
                .size(type_scale::XS)
                .color(t::priority_color(Priority::from_i32(issue.priority))),
        );
    }
    content = content.push(
        text(&issue.title)
            .size(type_scale::SM)
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
        content = content.push(text(f).size(type_scale::XS).color(t::text_secondary()));
    }

    button(content)
        .on_press(Message::OpenIssueDetail(issue.id.clone()))
        .padding([space::S2, space::S3])
        .width(Length::Fill)
        .style(|_theme: &Theme, _status| button::Style {
            background: Some(iced::Background::Color(t::bg_card())),
            text_color: t::text_primary(),
            border: Border {
                color: t::border(),
                width: 1.0,
                radius: radius::MD.into(),
            },
            ..Default::default()
        })
        .into()
}


fn placeholder_value<'a>() -> Element<'a, Message> {
    text("—").size(type_scale::SM).color(t::text_muted()).into()
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
        value: t::text_strong(),
        selection: t::accent(),
    }
}

/// The description: rendered markdown (click to edit), an empty prompt, or the editor.
fn description_section<'a>(
    detail: &'a ProjectDetailState,
    ctx: &crate::views::body::BodyCtx<'a>,
) -> Element<'a, Message> {
    use crate::views::issue_detail as detail_view;
    let text_now = detail.description_text();
    detail_view::description_block(
        detail_view::description_display(&text_now, detail.editing_description),
        description_editor_id(),
        Message::EditProjectDescription,
        || {
            let editor = text_editor(&detail.description)
                .placeholder("Add description…")
                .on_action(Message::ProjectDetailDescriptionChanged)
                .key_binding(detail_view::paste_image_binding(
                    text_editor::Binding::from_key_press,
                    Message::PasteImageIntoProjectDescription,
                ))
                .padding(space::S2_5)
                .size(type_scale::SM)
                .height(Length::Fixed(160.0));
            (editor.into(), None)
        },
        || crate::views::body::view(&text_now, type_scale::SM, t::text_body(), ctx),
    )
}

/// Container id used to find the project description editor for focusing.
pub(crate) fn description_editor_id() -> container::Id {
    container::Id::new("project-description-editor")
}

/// Alpha of the status tile fill (web `bg-accent/20`).
const STATUS_TILE_ALPHA: f32 = 0.2;

/// Section heading (web `text-sm font-semibold text-gray-400`) over its
/// content, `space::S4` apart.
fn with_heading<'a>(
    label: &'a str,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    with_heading_owned(label.to_string(), content)
}

fn with_heading_owned<'a>(
    label: String,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![]
        .spacing(space::S4)
        .push(
            text(label)
                .size(type_scale::SM)
                .font(crate::fonts::bold(crate::fonts::active()))
                .color(t::text_secondary()),
        )
        .push(content)
        .into()
}
