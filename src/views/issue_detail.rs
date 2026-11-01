use iced::widget::{
    button, column, container, pick_list, rich_text, row, scrollable, span, text, text_editor,
    text_input,
};
use iced::{Alignment, Border, Color, Element, Length, Theme};

use crate::app::{DepDirection, Message, View};
use crate::db::attachments::Attachment;
use crate::files::images::ImageStore;
use crate::db::activity::{self, ActivityItem, ItemOrigin, TimelineItem};
use crate::db::models::{Comment, Issue, Lane, Member, Priority, Project, Team};
use crate::db::queries::{DependencyGroups, DependencyLink};
use crate::mentions::{self, EditorTarget, Reference, Segment, SuggestState};
use crate::theme as t;
use crate::views::body::BodyCtx;
use crate::views::issue_form::LaneOption;
use crate::views::lane_icon;
use crate::views::projects::MemberOption;
use crate::views::util::relative_time;

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
    /// New-comment composer. Cleared on submit.
    pub comment_composer: text_editor::Content,
    /// Local id of the comment currently being replied to (inline composer open).
    pub reply_to: Option<String>,
    /// Inline reply composer for `reply_to`. Reset on open/cancel/submit.
    pub reply_composer: text_editor::Content,
    /// Open `@`/`#` autocomplete list (under the editor named by `target`).
    pub suggest: Option<SuggestState>,
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
            comment_composer: text_editor::Content::new(),
            reply_to: None,
            reply_composer: text_editor::Content::new(),
            suggest: None,
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
    teams: &'a [Team],
    lanes: &'a [Lane],
    projects: &'a [Project],
    members: &'a [Member],
    project_name: Option<&'a str>,
    deps: &'a DependencyGroups,
    comments: &'a [Comment],
    activity: &'a [ActivityItem],
    references: &'a [Reference],
    attachments: &'a [Attachment],
    comment_attachments: &'a CommentAttachments,
    images: &'a ImageStore,
    issues: &'a [Issue],
    parent: Option<&'a Issue>,
    sub_issues: Vec<&'a Issue>,
    recurring_origin: Option<&'a str>,
    my_member_ids: &[String],
    host: &'a str,
) -> Element<'a, Message> {
    // ---- Breadcrumb: "← Issues › [Project ›] IDENT-#" ----
    let identifier = issue_identifier(issue, team);
    let breadcrumb = row![]
        .spacing(6)
        .align_y(Alignment::Center)
        .push(
            button(text("← Issues").size(13).color(t::text_secondary()))
                .on_press(Message::NavigateToView(View::Board))
                .padding(0)
                .style(button::text),
        )
        .push(text("›").size(13).color(t::text_muted()))
        .push_maybe(project_name.map(|n| text(n).size(13).color(t::text_secondary())))
        .push_maybe(project_name.map(|_| text("›").size(13).color(t::text_muted())))
        .push(text(identifier).size(13).color(t::text_muted()));

    // ---- Left column: title, description, activity stub ----
    let left = column![]
        .spacing(16)
        .padding(24)
        .width(Length::Fill)
        .push(breadcrumb)
        .push_maybe(recurring_origin.map(|title| origin_line(title, &issue.team_id)))
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
                .map(|e| text(e).size(12).color(t::danger())),
        )
        .push(
            text_editor(&detail.description)
                .placeholder("Add description…  (@ to mention, # to link an issue)")
                .on_action(Message::IssueDetailDescriptionChanged)
                .key_binding(editor_binding(
                    None,
                    suggesting(detail.suggest.as_ref(), EditorTarget::Description),
                ))
                .padding(10)
                .size(14)
                .height(Length::Fixed(220.0)),
        )
        .push_maybe(suggestion_list(detail.suggest.as_ref(), EditorTarget::Description))
        .push_maybe(description_preview(
            &detail.description_text(),
            &BodyCtx {
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
        .push_maybe(
            (!references.is_empty()).then(|| referenced_in_section(references, issues, team)),
        )
        // Rails nests one level only, so a sub-issue gets no section of its own.
        .push_maybe(
            issue
                .parent_issue_id
                .is_none()
                .then(|| sub_issues_section(issue, &sub_issues, team, lanes)),
        )
        .push(comments_section(
            comments,
            activity,
            detail,
            &ThreadCtx {
                members,
                team,
                teams,
                issues,
                my_member_ids,
                can_reply: true,
                reply_to: detail.reply_to.as_deref(),
                reply_composer: &detail.reply_composer,
                suggest: detail.suggest.as_ref(),
                comment_attachments,
                images,
            },
        ));

    // ---- Right sidebar: properties ----
    let lane_options: Vec<LaneOption> = lanes
        .iter()
        .map(|l| LaneOption {
            id: l.id.clone(),
            name: l.name.clone(),
        })
        .collect();
    let selected_lane = lane_options.iter().find(|l| l.id == issue.lane_id).cloned();
    let selected_icon = lanes
        .iter()
        .find(|l| l.id == issue.lane_id)
        .map(|l| crate::lane_icon::icon_for(l.category.as_deref()));

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
        .push(text("Properties").size(11).color(t::text_muted()))
        .push_maybe(parent.map(|p| section("Parent", parent_link(p, team))))
        .push(section(
            "Status",
            row![]
                .spacing(8)
                .align_y(Alignment::Center)
                .push_maybe(selected_icon.map(lane_icon::view))
                .push(
                    pick_list(lane_options, selected_lane, |o: LaneOption| {
                        Message::IssueDetailStatusChanged(o.id)
                    })
                    .width(Length::Fill),
                ),
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
                Some(name) => text(name).size(13).color(t::text_secondary()).into(),
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
        .push(dependency_block(DepKind::Blocking, &deps.blocking))
        .push(dependency_block(DepKind::BlockedBy, &deps.blocked_by))
        .push_maybe(optional_dependency_block(DepKind::Related, &deps.related))
        .push_maybe(optional_dependency_block(DepKind::Duplicates, &deps.duplicates))
        .push_maybe(optional_dependency_block(DepKind::DuplicatedBy, &deps.duplicated_by))
        .push(
            button(text("Move to team…").size(12).color(t::accent()))
                .on_press(Message::OpenMoveTeamPicker(issue.id.clone()))
                .padding(0)
                .style(button::text),
        )
        .push(
            column![]
                .spacing(2)
                .push(
                    text(format!("Created {}", issue.created_at))
                        .size(11)
                        .color(t::text_muted()),
                )
                .push(
                    text(format!("Updated {}", issue.updated_at))
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

    // Thin vertical rule standing in for a `border-l` between panes.
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
        .push(container(scrollable(left)).width(Length::Fill).height(Length::Fill))
        .push(divider)
        .push(sidebar_panel)
        .into()
}

/// `IDENT-#` for an issue, or `#N` when the team isn't loaded.
fn issue_identifier(issue: &Issue, team: Option<&Team>) -> String {
    match team {
        Some(tm) => format!("{}-{}", tm.identifier, issue.team_number.unwrap_or(0)),
        None => format!("#{}", issue.team_number.unwrap_or(0)),
    }
}

/// "From recurring issue: <title>" line; opens the team's Recurring issues view.
fn origin_line<'a>(title: &'a str, team_id: &str) -> Element<'a, Message> {
    button(
        row![]
            .spacing(4)
            .push(text("From recurring issue:").size(12).color(t::text_muted()))
            .push(text(title).size(12).color(t::text_secondary())),
    )
    .on_press(Message::SelectTeamView(
        team_id.to_string(),
        View::RecurringIssues,
    ))
    .padding(0)
    .style(button::text)
    .into()
}

/// Sidebar "Parent" value: a clickable `IDENT — title` that opens the parent.
fn parent_link<'a>(parent: &'a Issue, team: Option<&'a Team>) -> Element<'a, Message> {
    button(
        row![]
            .spacing(6)
            .push(
                text(issue_identifier(parent, team))
                    .size(12)
                    .color(t::text_muted()),
            )
            .push(text(&parent.title).size(12).color(t::text_secondary())),
    )
    .on_press(Message::OpenIssueDetail(parent.id.clone()))
    .padding(0)
    .width(Length::Fill)
    .style(button::text)
    .into()
}

/// "Sub-issues" block in the left column: one clickable row per child, then a
/// "+ Add sub-issue" button that opens the create modal pre-linked to `issue`.
fn sub_issues_section<'a>(
    issue: &'a Issue,
    sub_issues: &[&'a Issue],
    team: Option<&'a Team>,
    lanes: &'a [Lane],
) -> Element<'a, Message> {
    let mut col = column![]
        .spacing(8)
        .push(text("Sub-issues").size(13).color(t::text_secondary()));
    for child in sub_issues {
        col = col.push(sub_issue_row(child, team, lanes));
    }
    col.push(
        button(text("+ Add sub-issue").size(12).color(t::accent()))
            .on_press(Message::CreateSubIssue(issue.id.clone()))
            .padding(0)
            .style(button::text),
    )
    .into()
}

/// One sub-issue: `IDENT  title` with its lane name as a colored badge on the right.
fn sub_issue_row<'a>(
    child: &'a Issue,
    team: Option<&'a Team>,
    lanes: &'a [Lane],
) -> Element<'a, Message> {
    let lane = lanes.iter().find(|l| l.id == child.lane_id);
    let badge = lane.map(|l| {
        let color = l
            .color
            .as_deref()
            .and_then(t::hex_to_color)
            .unwrap_or(t::text_muted());
        container(
            row![]
                .spacing(4)
                .align_y(Alignment::Center)
                .push(lane_icon::view(crate::lane_icon::icon_for(l.category.as_deref())))
                .push(text(&l.name).size(11).color(color)),
        )
            .padding([2, 8])
            .style(move |_theme: &Theme| container::Style {
                border: Border {
                    color,
                    width: 1.0,
                    radius: 10.0.into(),
                },
                ..Default::default()
            })
    });

    button(
        row![]
            .spacing(8)
            .align_y(Alignment::Center)
            .push(
                text(issue_identifier(child, team))
                    .size(12)
                    .color(t::text_muted()),
            )
            .push(
                text(&child.title)
                    .size(13)
                    .color(t::text_primary())
                    .width(Length::Fill),
            )
            .push_maybe(badge),
    )
    .on_press(Message::OpenIssueDetail(child.id.clone()))
    .padding([6, 8])
    .width(Length::Fill)
    .style(button::text)
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

/// Em-dash placeholder for sidebar sections filled in by later tickets
/// (Assignee JAIT-113, Labels JAIT-112).
fn placeholder_value<'a>() -> Element<'a, Message> {
    text("—").size(13).color(t::text_muted()).into()
}

/// The dependency sidebar sections. Blocking / blocked-by are editable (they
/// get a "+ Add" button); the rest are read-only and hidden when empty.
#[derive(Clone, Copy)]
enum DepKind {
    Blocking,
    BlockedBy,
    Related,
    Duplicates,
    DuplicatedBy,
}

impl DepKind {
    fn title(self) -> &'static str {
        match self {
            DepKind::Blocking => "Blocking",
            DepKind::BlockedBy => "Blocked by",
            DepKind::Related => "Related",
            DepKind::Duplicates => "Duplicates",
            DepKind::DuplicatedBy => "Duplicated by",
        }
    }

    fn color(self) -> Color {
        match self {
            DepKind::Blocking => t::dep_blocks(),
            DepKind::BlockedBy => t::dep_blocked_by(),
            DepKind::Related => t::dep_relates(),
            DepKind::Duplicates | DepKind::DuplicatedBy => t::dep_duplicates(),
        }
    }

    fn add(self) -> Option<(&'static str, DepDirection)> {
        match self {
            DepKind::Blocking => Some(("+ Add blocking issue…", DepDirection::Blocking)),
            DepKind::BlockedBy => Some(("+ Add blocked by issue…", DepDirection::BlockedBy)),
            _ => None,
        }
    }
}

/// A titled sidebar section for `kind`.
fn dependency_block<'a>(kind: DepKind, links: &'a [DependencyLink]) -> Element<'a, Message> {
    section(kind.title(), dependency_section(links, kind))
}

/// Like [`dependency_block`], but absent when there are no links.
fn optional_dependency_block<'a>(
    kind: DepKind,
    links: &'a [DependencyLink],
) -> Option<Element<'a, Message>> {
    (!links.is_empty()).then(|| dependency_block(kind, links))
}

/// The linked issues as clickable rows, followed (for editable kinds) by a
/// text "+ Add …" button that opens the picker.
fn dependency_section<'a>(links: &'a [DependencyLink], kind: DepKind) -> Element<'a, Message> {
    let color = kind.color();
    let mut col = column![].spacing(6);
    for link in links {
        col = col.push(dependency_row(link, color));
    }
    if let Some((add_label, direction)) = kind.add() {
        col = col.push(
            button(text(add_label).size(12).color(t::accent()))
                .on_press(Message::OpenDependencyPicker(direction))
                .padding(0)
                .style(button::text),
        );
    }
    col.into()
}

/// One dependency edge: a clickable `IDENT — title` that navigates to that
/// issue, plus a trailing `×` that removes the relationship. The identifier is
/// tinted with the section's dependency color.
fn dependency_row<'a>(link: &'a DependencyLink, relation_color: Color) -> Element<'a, Message> {
    row![]
        .spacing(6)
        .align_y(Alignment::Center)
        .push(
            button(
                row![]
                    .spacing(6)
                    .push(text(&link.identifier).size(12).color(relation_color))
                    .push(text(&link.title).size(12).color(t::text_secondary())),
            )
            .on_press(Message::OpenIssueDetail(link.issue_id.clone()))
            .padding(0)
            .width(Length::Fill)
            .style(button::text),
        )
        .push(
            button(text("×").size(14).color(t::text_muted()))
                .on_press(Message::RemoveDependency(link.dep_id.clone()))
                .padding(0)
                .style(button::text),
        )
        .into()
}

/// Editor key bindings: Cmd/Ctrl+Enter submits (when `on_submit` is given).
/// While the autocomplete list is open, Up/Down (Ctrl-p/n) move the highlight,
/// Enter/Tab accept it (so Enter never inserts a newline) and Esc is left
/// uncaptured so the app closes the list. Everything else keeps the editor's
/// defaults.
pub fn editor_binding(
    on_submit: Option<Message>,
    suggesting: bool,
) -> impl Fn(text_editor::KeyPress) -> Option<text_editor::Binding<Message>> {
    use iced::keyboard::key::Named;
    use text_editor::Binding::Custom;
    move |press| {
        if suggesting {
            let ctrl = press.modifiers.control() && !press.modifiers.command();
            match press.key.as_ref() {
                iced::keyboard::Key::Named(Named::ArrowDown) => {
                    return Some(Custom(Message::MoveSuggestion(1)));
                }
                iced::keyboard::Key::Named(Named::ArrowUp) => {
                    return Some(Custom(Message::MoveSuggestion(-1)));
                }
                iced::keyboard::Key::Character("n") if ctrl => {
                    return Some(Custom(Message::MoveSuggestion(1)));
                }
                iced::keyboard::Key::Character("p") if ctrl => {
                    return Some(Custom(Message::MoveSuggestion(-1)));
                }
                iced::keyboard::Key::Named(Named::Tab) => {
                    return Some(Custom(Message::AcceptSuggestion));
                }
                iced::keyboard::Key::Named(Named::Enter) if !press.modifiers.command() => {
                    return Some(Custom(Message::AcceptSuggestion));
                }
                // Not bound: the key reaches the app, which dismisses the list.
                iced::keyboard::Key::Named(Named::Escape) => return None,
                _ => {}
            }
        }
        match press.key.as_ref() {
            iced::keyboard::Key::Named(Named::Enter)
                if press.modifiers.command() && on_submit.is_some() =>
            {
                on_submit.clone().map(Custom)
            }
            _ => text_editor::Binding::from_key_press(press),
        }
    }
}

fn suggesting(suggest: Option<&SuggestState>, target: EditorTarget) -> bool {
    suggest.is_some_and(|s| s.target == target)
}

/// Autocomplete rows under the editor `target`, if its list is open.
fn suggestion_list<'a>(
    suggest: Option<&'a SuggestState>,
    target: EditorTarget,
) -> Option<Element<'a, Message>> {
    suggest
        .filter(|s| s.target == target)
        .map(suggestion_rows)
}

/// The list itself: arrow keys move the highlight, Enter/Tab or a click
/// replaces the partial `@query`/`#query` with the full token. Rendered inline
/// under the editor (not a floating popover).
pub fn suggestion_rows<'a>(s: &'a crate::mentions::SuggestState) -> Element<'a, Message> {
    let mut col = column![].spacing(2).push(
        text("↑↓ to move · Enter/Tab or click to insert · Esc to dismiss")
            .size(10)
            .color(t::text_muted()),
    );
    for (i, item) in s.items.iter().enumerate() {
        let selected = i == s.selected;
        col = col.push(
            button(
                row![]
                    .spacing(8)
                    .push(text(&item.label).size(12).color(t::accent()))
                    .push(text(&item.detail).size(12).color(t::text_secondary())),
            )
            .on_press(Message::PickSuggestion(i))
            .padding([3, 8])
            .width(Length::Fill)
            .style(move |theme: &Theme, status| {
                let mut st = button::text(theme, status);
                if selected {
                    st.background = Some(iced::Background::Color(t::bg_card_focused()));
                }
                st
            }),
        );
    }
    container(col)
        .padding(6)
        .width(Length::Fill)
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

/// Body text with `@mentions` and `IDENT-n` references highlighted. Own-team
/// references that resolve to a local issue are clickable. Spans own their
/// text so this also works for live editor content.
pub(crate) fn rich_body<'a>(
    body: &str,
    size: f32,
    plain: iced::Color,
    team: Option<&Team>,
    teams: &[Team],
    issues: &[Issue],
    members: &[Member],
) -> Element<'a, Message> {
    let ident = team.map(|t| t.identifier.as_str()).unwrap_or("");
    let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
    let spans: Vec<text::Span<'a, String, iced::Font>> = mentions::segments(body, ident, &names)
        .into_iter()
        .map(|seg| match seg {
            Segment::Text(s) => span(s.to_string()).color(plain),
            Segment::Mention(s) => span(s.to_string()).color(t::highlight()),
            Segment::Issue { text, .. } => {
                // Resolves own-team and cross-team identifiers alike.
                let target = mentions::parse_ident(text)
                    .and_then(|(p, n)| mentions::resolve_issue(issues, teams, p, n));
                match target {
                    Some(i) => span(text.to_string())
                        .color(t::accent())
                        .underline(true)
                        .link(i.id.clone()),
                    None => span(text.to_string()).color(t::accent()),
                }
            }
        })
        .collect();
    let el: Element<'a, String> = rich_text(spans).size(size).into();
    el.map(Message::OpenIssueDetail)
}

/// Rendered (highlighted, clickable) view of the description, shown only when
/// it contains at least one token; the editor itself is plain text.
fn description_preview<'a>(body: &str, ctx: &BodyCtx<'a>) -> Option<Element<'a, Message>> {
    let ident = ctx.team.map(|t| t.identifier.as_str()).unwrap_or("");
    let names: Vec<&str> = ctx.members.iter().map(|m| m.name.as_str()).collect();
    let has_token = mentions::segments(body, ident, &names)
        .iter()
        .any(|s| !matches!(s, Segment::Text(_)))
        || crate::views::body::has_images(body, ctx.host);
    if !has_token {
        return None;
    }
    Some(
        container(
            column![]
                .spacing(4)
                .push(text("Preview").size(10).color(t::text_muted()))
                .push(crate::views::body::view(body, 12.0, t::text_secondary(), ctx)),
        )
        .padding([6, 10])
        .into(),
    )
}

/// "Referenced in" block: one clickable row per issue whose description or
/// comments mention this one.
fn referenced_in_section<'a>(
    references: &'a [Reference],
    issues: &'a [Issue],
    team: Option<&'a Team>,
) -> Element<'a, Message> {
    let mut col = column![]
        .spacing(4)
        .push(text("Referenced in").size(13).color(t::text_secondary()));
    for r in references {
        let Some(src) = issues.iter().find(|i| i.id == r.source_issue_id) else {
            continue;
        };
        let via = match (r.in_description, r.in_comment) {
            (true, true) => "description, comment",
            (true, false) => "description",
            _ => "comment",
        };
        col = col.push(
            button(
                row![]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(
                        text(issue_identifier(src, team))
                            .size(12)
                            .color(t::text_muted()),
                    )
                    .push(
                        text(&src.title)
                            .size(13)
                            .color(t::text_primary())
                            .width(Length::Fill),
                    )
                    .push(text(via).size(11).color(t::text_muted())),
            )
            .on_press(Message::OpenIssueDetail(src.id.clone()))
            .padding([6, 8])
            .width(Length::Fill)
            .style(button::text),
        );
    }
    col.into()
}

/// Activity timeline: change events interleaved with threaded comments
/// (oldest first), plus the top-level composer, appended to the bottom of the
/// left column. Own comments can be deleted.
fn comments_section<'a>(
    comments: &'a [Comment],
    activity: &'a [ActivityItem],
    detail: &'a IssueDetailState,
    ctx: &ThreadCtx<'a, '_>,
) -> Element<'a, Message> {
    let mut col = column![]
        .spacing(12)
        .push(text("Activity").size(13).color(t::text_secondary()));

    let items = activity::timeline(activity, comments);
    if items.is_empty() {
        col = col.push(text("No activity yet").size(12).color(t::text_muted()));
    }
    for item in items {
        match item {
            TimelineItem::Activity(item) => col = col.push(activity_row(item)),
            TimelineItem::Thread(thread) => {
                col = push_thread(col, thread, ctx);
            }
        }
    }

    col.push(new_comment_composer(
        &detail.comment_composer,
        detail.suggest.as_ref(),
    ))
    .into()
}

/// What `push_thread` needs besides the comments themselves: lookups for
/// rendering plus the open reply composer. Shared by issue and project detail.
pub struct ThreadCtx<'a, 'b> {
    pub members: &'a [Member],
    pub team: Option<&'a Team>,
    pub teams: &'a [Team],
    pub issues: &'a [Issue],
    /// Local ids of the current user's members; gates the Delete action.
    pub my_member_ids: &'b [String],
    /// Offer Reply on comments under the depth cap.
    pub can_reply: bool,
    pub reply_to: Option<&'a str>,
    pub reply_composer: &'a text_editor::Content,
    pub suggest: Option<&'a SuggestState>,
    /// Files per comment, keyed by the comment's local id.
    pub comment_attachments: &'a CommentAttachments,
    /// Image fetch state, for attachment thumbnails.
    pub images: &'a ImageStore,
}

/// Comment local id -> its files (absent when it has none).
pub type CommentAttachments = std::collections::HashMap<String, Vec<Attachment>>;

/// Append a threaded run of comments (indented replies, inline reply composer
/// under the comment being replied to).
pub fn push_thread<'a>(
    mut col: iced::widget::Column<'a, Message>,
    thread: Vec<(usize, &'a Comment)>,
    ctx: &ThreadCtx<'a, '_>,
) -> iced::widget::Column<'a, Message> {
    for (depth, comment) in thread {
        col = col.push(comment_row(depth, comment, ctx));
        if ctx.reply_to == Some(comment.id.as_str()) {
            col = col.push(reply_composer(depth + 1, ctx.reply_composer, ctx.suggest));
            col = col.push_maybe(
                suggestion_list(ctx.suggest, EditorTarget::Reply).map(|e| indent(depth + 1, e)),
            );
        }
    }
    col
}

/// Composer: multiline editor + "Comment" button.
pub fn new_comment_composer<'a>(
    content: &'a text_editor::Content,
    suggest: Option<&'a SuggestState>,
) -> Element<'a, Message> {
    column![]
        .spacing(8)
        .push(
            text_editor(content)
                .placeholder("Leave a comment…  (Cmd+Enter to send)")
                .on_action(Message::CommentComposerChanged)
                .key_binding(editor_binding(
                    Some(Message::SubmitComment),
                    suggesting(suggest, EditorTarget::Comment),
                ))
                .padding(10)
                .size(14)
                .height(Length::Fixed(80.0)),
        )
        .push_maybe(suggestion_list(suggest, EditorTarget::Comment))
        .push(
            button(text("Comment").size(13))
                .on_press(Message::SubmitComment)
                .padding([6, 14])
                .style(button::primary),
        )
        .into()
}

/// One change-history line: "actor description  ·  3d ago".
/// A null actor shows the text alone; a Pending item is marked as unsynced.
fn activity_row<'a>(item: &'a ActivityItem) -> Element<'a, Message> {
    let pending = item.origin == ItemOrigin::Pending;
    row![]
        .spacing(6)
        .align_y(Alignment::Center)
        .push(text("•").size(11).color(t::text_muted()))
        .push_maybe(
            item.actor
                .name
                .as_deref()
                .map(|n| text(n).size(12).color(t::text_secondary())),
        )
        .push(text(item.text.as_str()).size(12).color(t::text_muted()))
        .push_maybe(pending.then(|| text("(pending)").size(11).color(t::text_muted())))
        .push(
            text(relative_time(&item.at))
                .size(11)
                .color(t::text_muted()),
        )
        .into()
}

/// Left indent (px) per nesting level.
const INDENT_PX: f32 = 18.0;

fn indent<'a>(depth: usize, e: Element<'a, Message>) -> Element<'a, Message> {
    container(e)
        .padding(iced::Padding::ZERO.left(INDENT_PX * depth as f32))
        .into()
}

/// Inline composer shown under the comment being replied to.
fn reply_composer<'a>(
    depth: usize,
    content: &'a text_editor::Content,
    suggest: Option<&'a SuggestState>,
) -> Element<'a, Message> {
    let body = column![]
        .spacing(6)
        .push(
            text_editor(content)
                .placeholder("Write a reply…  (Cmd+Enter to send)")
                .on_action(Message::ReplyComposerChanged)
                .key_binding(editor_binding(
                    Some(Message::SubmitReply),
                    suggesting(suggest, EditorTarget::Reply),
                ))
                .padding(8)
                .size(13)
                .height(Length::Fixed(64.0)),
        )
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text("Reply").size(12))
                        .on_press(Message::SubmitReply)
                        .padding([4, 12])
                        .style(button::primary),
                )
                .push(
                    button(text("Cancel").size(12))
                        .on_press(Message::CancelReply)
                        .padding([4, 12])
                        .style(button::secondary),
                ),
        );
    container(body)
        .padding(iced::Padding::ZERO.left(INDENT_PX * depth as f32))
        .into()
}

/// One comment: author name + relative timestamp on a header row, body below,
/// then a "Reply" link while the thread is under the depth cap. Replies are
/// indented per level inside a bordered container.
fn comment_row<'a>(
    depth: usize,
    comment: &'a Comment,
    ctx: &ThreadCtx<'a, '_>,
) -> Element<'a, Message> {
    let ThreadCtx {
        members,
        team,
        teams,
        issues,
        my_member_ids,
        can_reply,
        ..
    } = *ctx;
    let author = comment
        .author_id
        .as_ref()
        .and_then(|aid| members.iter().find(|m| &m.id == aid))
        .map(|m| m.name.as_str())
        .unwrap_or("Unknown");

    let mut col = column![].spacing(4).push(
        row![]
            .spacing(8)
            .align_y(Alignment::Center)
            .push(text(author).size(12).color(t::text_secondary()))
            .push(
                text(relative_time(&comment.created_at))
                    .size(11)
                    .color(t::text_muted()),
            ),
    );
    col = col.push(rich_body(
        &comment.body,
        13.0,
        t::text_primary(),
        team,
        teams,
        issues,
        members,
    ));
    col = col.push_maybe(
        ctx.comment_attachments
            .get(&comment.id)
            .and_then(|files| crate::views::attachments::section(files, ctx.images)),
    );
    // Delete is offered on the user's own comments, synced or not.
    let can_delete = comment
        .author_id
        .as_ref()
        .is_some_and(|aid| my_member_ids.contains(aid));
    let mut actions = row![].spacing(10);
    let can_reply = can_reply && Comment::can_reply_at(depth);
    if can_reply {
        actions = actions.push(
            button(text("Reply").size(11).color(t::text_muted()))
                .on_press(Message::StartReply(comment.id.clone()))
                .padding(0)
                .style(button::text),
        );
    }
    if can_delete {
        actions = actions.push(
            button(text("Delete").size(11).color(t::text_muted()))
                .on_press(Message::ConfirmDeleteComment(comment.id.clone()))
                .padding(0)
                .style(button::text),
        );
    }
    if can_reply || can_delete {
        col = col.push(actions);
    }

    if depth == 0 {
        return col.into();
    }
    container(
        container(col)
            .padding([4, 10])
            .style(|_theme: &Theme| container::Style {
                border: Border {
                    color: t::text_muted(),
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..Default::default()
            }),
    )
    .padding(iced::Padding::ZERO.left(INDENT_PX * depth as f32))
    .into()
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
