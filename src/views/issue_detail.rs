use iced::widget::{button, column, container, pick_list, row, scrollable, text_editor, text_input};
use crate::views::text;
use iced::{Alignment, Border, Color, Element, Length, Theme};

use crate::app::{DepDirection, Message, View};
use crate::scroll::{track, ScrollKey};
use crate::db::attachments::Attachment;
use crate::files::images::ImageStore;
use crate::db::activity::{self, ActivityItem, ItemOrigin, TimelineItem};
use crate::db::models::{Comment, Issue, Lane, Member, Project, Team};
use crate::db::queries::{DependencyGroups, DependencyLink};
use crate::mentions::{EditorTarget, Reference, SuggestState};
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::body::BodyCtx;
use crate::views::dropdown::dropdown;
use crate::views::lane_icon;
use crate::views::truncated::truncated;
use crate::views::property_picker::{self, Field, PickerState};
use crate::views::projects::MemberOption;
use crate::views::util::{
    inline_properties, properties_placement, relative_time, sidebar_input_style,
    sidebar_menu_style, sidebar_pick_list_style, sidebar_properties, vertical_divider,
    with_heading, Properties, PropertiesPlacement, INLINE_CONTROL_WIDTH, SIDEBAR_CONTROL_PADDING, SIDEBAR_WIDTH,
};

/// Buffered edit state for the full-page issue detail view. Title, description,
/// estimate and due date live here and are committed (on Enter or on nav-away)
/// via `App::commit_issue_detail`. The sidebar pick_lists apply immediately and
/// read their current value straight from the reloaded `Issue`, so this buffer
/// is never clobbered by a `reload_data()`.
pub struct IssueDetailState {
    pub issue_id: String,
    /// An editor rather than a text input so a long title wraps.
    pub title: text_editor::Content,
    pub description: text_editor::Content,
    /// True while the description editor is open; otherwise it is drawn rendered.
    pub editing_description: bool,
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
    /// Files staged on the new-comment composer.
    pub composer_files: ComposerFiles,
    /// The buffered fields as last loaded or committed. Only fields that differ
    /// from this are written on commit, so an untouched buffer never pushes a
    /// stale copy over a newer pulled value.
    pub loaded: IssueFields,
}

/// The buffered issue fields in the form a commit writes them. `estimate` is
/// the trimmed text; it is parsed only when it was edited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueFields {
    pub title: String,
    pub description: Option<String>,
    pub due_date: Option<String>,
    pub estimate: String,
}

/// Files staged on a comment composer (issue or project detail). They are
/// queued as Pending uploads owned by `comment_id`, the id the comment will be
/// inserted with when sent, so the composer shows them before the comment exists.
pub struct ComposerFiles {
    pub comment_id: String,
    pub files: Vec<Attachment>,
    /// Id the open inline reply will be inserted with (owns its pasted images).
    pub reply_id: String,
}

impl Default for ComposerFiles {
    fn default() -> Self {
        Self {
            comment_id: uuid::Uuid::new_v4().to_string(),
            files: Vec::new(),
            reply_id: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl IssueDetailState {
    pub fn from_issue(issue: &Issue) -> Self {
        let mut state = Self::buffers_for(issue);
        state.loaded = state.fields();
        state
    }

    fn buffers_for(issue: &Issue) -> Self {
        let body = issue.description.clone().unwrap_or_default();
        Self {
            issue_id: issue.id.clone(),
            title: text_editor::Content::with_text(&issue.title),
            description: text_editor::Content::with_text(&body),
            editing_description: false,
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
            composer_files: ComposerFiles::default(),
            loaded: IssueFields::default(),
        }
    }

    pub fn description_text(&self) -> String {
        self.description.text()
    }

    /// The buffers as a commit would write them.
    pub fn fields(&self) -> IssueFields {
        let body = self.description_text();
        let due = self.due_date.trim();
        IssueFields {
            title: self.title.text().replace('\n', " ").trim().to_string(),
            description: (!body.trim().is_empty()).then(|| body.trim_end().to_string()),
            due_date: (!due.is_empty()).then(|| due.to_string()),
            estimate: self.estimate.trim().to_string(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.fields() != self.loaded
    }

    /// Write only the fields edited since load onto `issue` (the fresh DB
    /// row). Returns whether anything changed, or the estimate parse error.
    pub fn apply_edits(&self, issue: &mut Issue) -> Result<bool, String> {
        let now = self.fields();
        let was = &self.loaded;
        let mut changed = false;
        if now.estimate != was.estimate {
            issue.estimate = if now.estimate.is_empty() {
                None
            } else {
                Some(now.estimate.parse::<f64>().map_err(|_| "Estimate must be a number".to_string())?)
            };
            changed = true;
        }
        if now.title != was.title {
            issue.title = now.title;
            changed = true;
        }
        if now.description != was.description {
            issue.description = now.description;
            changed = true;
        }
        if now.due_date != was.due_date {
            issue.due_date = now.due_date;
            changed = true;
        }
        Ok(changed)
    }

    /// Record the buffers as committed.
    pub fn mark_committed(&mut self) {
        self.loaded = self.fields();
    }

    /// Take `issue`'s values into the buffers when they hold no unsaved edits
    /// and `issue` differs from what was loaded (e.g. a pull brought a newer
    /// copy). Composers and drafts are kept.
    pub fn refresh_from(&mut self, issue: &Issue) {
        if self.is_dirty() {
            return;
        }
        let fresh = Self::from_issue(issue);
        if fresh.loaded == self.loaded {
            return;
        }
        self.title = fresh.title;
        self.description = fresh.description;
        self.due_date = fresh.due_date;
        self.estimate = fresh.estimate;
        self.loaded = fresh.loaded;
    }
}

#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    issue: &'a Issue,
    detail: &'a IssueDetailState,
    team: Option<&'a Team>,
    teams: &'a [Team],
    lanes: &'a [Lane],
    projects: &'a [Project],
    members: &'a [Member],
    comment_authors: &'a [Member],
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
    pending_paths: &'a crate::views::body::PendingInlineMap,
    picker: Option<&'a PickerState>,
    properties_open: bool,
    // Width a right-docked Terminal panel takes from the page (0 when none);
    // any width moves Properties under the description.
    terminal_width: f32,
    // The inline Properties card shows its "More" fields.
    more_properties: bool,
    // A Terminal panel is open, so the Claude prompt button has somewhere to type.
    terminal_showing: bool,
) -> Element<'a, Message> {
    // ---- Breadcrumb: "← Issues › [Project ›] IDENT-#" ----
    let identifier = issue_identifier(issue, team);
    let breadcrumb = row![]
        .width(Length::Fill)
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push(
            button(text("← Issues").size(type_scale::SM).color(t::text_secondary()))
                .on_press(Message::NavigateToView(View::Board))
                .padding(0)
                .style(button::text),
        )
        .push(text("›").size(type_scale::SM).color(t::text_muted()))
        .push_maybe(project_name.map(|n| text(n).size(type_scale::SM).color(t::text_secondary())))
        .push_maybe(project_name.map(|_| text("›").size(type_scale::SM).color(t::text_muted())))
        .push(text(identifier).size(type_scale::SM).color(t::text_secondary()))
        .push_maybe(terminal_showing.then(|| claude_prompt_button(&issue.id)))
        .push(iced::widget::horizontal_space())
        .push(
            button(text("Dependencies").size(type_scale::XS).color(t::accent()))
                .on_press(Message::OpenDependencyMap(issue.id.clone()))
                .padding(0)
                .style(button::text),
        );

    // ---- Left column: title, description, activity stub ----
    // ---- Properties: down the right side, or in the header under the title ----
    let placement = properties_placement(properties_open, terminal_width);
    let (inline_props, sidebar) = match placement {
        PropertiesPlacement::Hidden => (None, None),
        PropertiesPlacement::Sidebar => {
            let props =
                properties(issue, detail, team, lanes, projects, members, deps, parent, picker, SIDEBAR_CONTROL_WIDTH);
            (None, Some(sidebar_properties(props)))
        }
        PropertiesPlacement::Inline => {
            let props =
                properties(issue, detail, team, lanes, projects, members, deps, parent, picker, INLINE_CONTROL_WIDTH);
            (Some(inline_properties(props, more_properties, Message::ToggleMoreProperties)), None)
        }
    };

    let head = column![]
        .spacing(space::S4)
        .push(breadcrumb)
        .push_maybe(recurring_origin.map(|title| origin_line(title, &issue.team_id)))
        .push(
            text_editor(&detail.title)
                .placeholder("Title")
                .on_action(Message::IssueDetailTitleChanged)
                .key_binding(title_binding)
                .size(type_scale::XXL)
                .font(crate::fonts::bold_active())
                .padding(space::S1)
                .style(title_editor_style),
        )
        .push_maybe(
            detail
                .error
                .as_deref()
                .map(|e| text(e).size(type_scale::XS).color(t::danger())),
        )
        .push_maybe(inline_props);

    // Title-to-body gap is S6; the sections below are S8 apart. The attach
    // button stays close to the description it belongs to.
    let description = column![]
        .spacing(space::S2)
        .push(description_section(
            detail,
            &BodyCtx {
                teams,
                issues,
                members,
                images,
                host,
                pending_paths: Some(pending_paths),
                on_image: Some(Message::OpenInlineImage),
            },
        ))
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
        .push_maybe(
            (!references.is_empty()).then(|| referenced_in_section(references, issues, team)),
        )
        .push(parent_block(parent, team))
        .push(sub_issues_section(issue, &sub_issues, team, lanes))
        .push(comments_section(
            comments,
            activity,
            detail,
            &ThreadCtx {
                members,
                authors: comment_authors,
                teams,
                issues,
                my_member_ids,
                can_reply: true,
                reply_to: detail.reply_to.as_deref(),
                reply_composer: &detail.reply_composer,
                suggest: detail.suggest.as_ref(),
                comment_attachments,
                images,
                host,
                pending_paths,
            },
        ));
    let left = column![]
        .spacing(space::S6)
        .padding(space::S8)
        .width(Length::Fill)
        .push(head)
        .push(sections);

    let page = row![].width(Length::Fill).height(Length::Fill).push(
        container(track(scrollable(left), ScrollKey::IssueDetail))
            .width(Length::Fill)
            .height(Length::Fill),
    );
    // `/` (or the palette) hides and shows Properties.
    match sidebar {
        Some(panel) => page.push(vertical_divider()).push(panel).into(),
        None => page.into(),
    }
}

/// The issue's Properties, laid out by `sidebar_properties` or
/// `inline_properties`. `menu_width` is the width of a field's control, which
/// its dropdown matches.
#[allow(clippy::too_many_arguments)]
fn properties<'a>(
    issue: &'a Issue,
    detail: &'a IssueDetailState,
    team: Option<&'a Team>,
    lanes: &'a [Lane],
    projects: &'a [Project],
    members: &'a [Member],
    deps: &'a DependencyGroups,
    parent: Option<&'a Issue>,
    picker: Option<&'a PickerState>,
    menu_width: f32,
) -> Properties<'a, Message> {
    let lane_name = lanes.iter().find(|l| l.id == issue.lane_id).map(|l| l.name.clone());
    let project_label = issue
        .project_id
        .as_ref()
        .and_then(|id| projects.iter().find(|p| &p.id == id))
        .map(|p| p.name.clone());

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
    let select = |field, value, empty| sidebar_select(field, value, empty, picker, menu_width);

    let mut fields: Vec<(&'a str, Element<'a, Message>)> = Vec::new();
    fields.extend(parent.map(|p| ("Parent", parent_link(p, team))));
    fields.push(("Status", select(Field::Status, lane_name, "No status")));
    fields.push((
        "Priority",
        select(Field::Priority, Some(issue.priority_enum().to_string()), "No priority"),
    ));
    fields.push((
        "Assignee",
        pick_list(member_options, selected_assignee, |o: MemberOption| {
            Message::IssueDetailAssigneeChanged(o.id)
        })
        .width(Length::Fill)
        .text_size(type_scale::SM)
        .padding(SIDEBAR_CONTROL_PADDING)
        .style(sidebar_pick_list_style)
        .menu_style(sidebar_menu_style)
        .into(),
    ));
    fields.push((
        "Creator",
        match creator_name {
            Some(name) => text(name).size(type_scale::SM).color(t::text_secondary()).into(),
            None => placeholder_value(),
        },
    ));
    fields.push((
        "Estimate",
        text_input("No estimate", &detail.estimate)
            .on_input(Message::IssueDetailEstimateChanged)
            .on_submit(Message::CommitIssueDetail)
            .padding(SIDEBAR_CONTROL_PADDING)
            .size(type_scale::SM)
            .style(sidebar_input_style)
            .into(),
    ));
    fields.push((
        "Due date",
        text_input("YYYY-MM-DD", &detail.due_date)
            .on_input(Message::IssueDetailDueDateChanged)
            .on_submit(Message::CommitIssueDetail)
            .padding(SIDEBAR_CONTROL_PADDING)
            .size(type_scale::SM)
            .style(sidebar_input_style)
            .into(),
    ));
    fields.push(("Project", {
        let select = select(Field::Project, project_label, "No project");
        // Like the web's sidebar pill: a project card while one is set.
        match &issue.project_id {
            Some(id) => crate::views::row_hover::row_hover(
                select,
                crate::body_hover::Anchor::Project(id.clone()),
                Message::RowHover,
            )
            .dismiss_on_press()
            .into(),
            None => select,
        }
    }));
    fields.push(("Labels", select(Field::Labels, None, "—")));
    fields.push(dependency_block(DepKind::Blocking, &deps.blocking));
    fields.push(dependency_block(DepKind::BlockedBy, &deps.blocked_by));
    fields.extend(optional_dependency_block(DepKind::Related, &deps.related));
    fields.extend(optional_dependency_block(DepKind::Duplicates, &deps.duplicates));
    fields.extend(optional_dependency_block(DepKind::DuplicatedBy, &deps.duplicated_by));

    // Inline, the rest wait behind "More".
    let (fields, more) = fields
        .into_iter()
        .partition(|(label, _)| ["Status", "Priority", "Assignee", "Project", "Labels"].contains(label));
    Properties {
        fields,
        more,
        actions: vec![button(text("Move to team…").size(type_scale::XS).color(t::accent()))
            .on_press(Message::OpenMoveTeamPicker(issue.id.clone()))
            .padding(0)
            .style(button::text)
            .into()],
        created_at: &issue.created_at,
        updated_at: &issue.updated_at,
    }
}

/// `IDENT-#` for an issue, or `#N` when the team isn't loaded.
const ICON_ROBOT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" viewBox="0 0 24 24"><path d="M12 8V4H8"/><rect width="16" height="12" x="4" y="8" rx="2"/><path d="M2 14h2M20 14h2M15 13v2M9 13v2"/></svg>"##;

/// Small robot button: types this issue's jait MCP prompt into the focused
/// Terminal. Used next to the issue view's identifier and on project issue rows.
pub fn claude_prompt_button<'a>(issue_id: &str) -> Element<'a, Message> {
    let tip = container(text("Type this issue's prompt into the Terminal").size(type_scale::XS).color(t::text_strong()))
        .padding([space::S1, space::S2])
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_menu())),
            border: Border { color: t::border(), width: 1.0, radius: radius::SM.into() },
            ..Default::default()
        });
    let btn = button(crate::views::form_page::icon(ICON_ROBOT, space::S4, t::text_secondary()))
        .padding(2.0)
        .on_press(Message::SendIssuePrompt(issue_id.to_string()))
        .style(|_theme: &Theme, status| button::Style {
            background: (status == button::Status::Hovered)
                .then(|| iced::Background::Color(t::bg_card_focused())),
            border: Border { radius: radius::SM.into(), ..Default::default() },
            ..Default::default()
        });
    iced::widget::tooltip(btn, tip, iced::widget::tooltip::Position::Bottom).gap(4).into()
}

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
            .spacing(space::S1)
            .push(text("From recurring issue:").size(type_scale::XS).color(t::text_muted()))
            .push(text(title).size(type_scale::XS).color(t::text_secondary())),
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
            .spacing(space::S1_5)
            .push(
                text(issue_identifier(parent, team))
                    .size(type_scale::XS)
                    .color(t::text_muted()),
            )
            .push(text(&parent.title).size(type_scale::XS).color(t::text_secondary())),
    )
    .on_press(Message::OpenIssueDetail(parent.id.clone()))
    .padding(0)
    .width(Length::Fill)
    .style(button::text)
    .into()
}

/// Children ordered like Rails' `_hierarchy`: `(lane.position, team_number)`.
/// A child whose lane is unknown sorts last.
fn ordered_sub_issues<'a>(sub_issues: &[&'a Issue], lanes: &[Lane]) -> Vec<&'a Issue> {
    let mut out: Vec<&'a Issue> = sub_issues.to_vec();
    out.sort_by_key(|c| {
        let pos = lanes
            .iter()
            .find(|l| l.id == c.lane_id)
            .map_or(i32::MAX, |l| l.position);
        (pos, c.team_number.unwrap_or(i32::MAX))
    });
    out
}

/// How many of `sub_issues` are done or canceled (`display::is_completed`).
fn done_count(sub_issues: &[&Issue]) -> usize {
    sub_issues
        .iter()
        .filter(|i| crate::display::is_completed(i))
        .count()
}

/// Thin 4px progress bar, accent fill = `done / total`.
fn sub_issue_progress_bar<'a>(done: usize, total: usize) -> Element<'a, Message> {
    let filled = (done * 100).checked_div(total).unwrap_or(0) as u16;
    let bar = |w: u16, fill: Color| {
        container(text(""))
            .width(Length::FillPortion(w))
            .height(Length::Fixed(4.0))
            .style(move |_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(fill)),
                ..Default::default()
            })
    };
    let mut r = row![].height(Length::Fixed(4.0));
    if filled > 0 {
        r = r.push(bar(filled, t::accent()));
    }
    if filled < 100 {
        r = r.push(bar(100 - filled, t::text_muted().scale_alpha(0.25)));
    }
    r.width(Length::Fill).into()
}

/// Parent line above the Sub-issues list. With a parent: "Sub-issue of
/// IDENT title" (opens it) plus Change / Remove. Without: "Set parent issue…".
fn parent_block<'a>(parent: Option<&'a Issue>, team: Option<&'a Team>) -> Element<'a, Message> {
    let link = |label: &'static str, msg: Message| {
        button(text(label).size(type_scale::XS).color(t::accent()))
            .on_press(msg)
            .padding(0)
            .style(button::text)
    };
    match parent {
        Some(p) => row![]
            .spacing(space::S3)
            .align_y(Alignment::Center)
            .push(text("Sub-issue of").size(type_scale::XS).color(t::text_muted()))
            .push(
                button(
                    row![]
                        .spacing(space::S1_5)
                        .push(
                            text(issue_identifier(p, team))
                                .size(type_scale::XS)
                                .color(t::text_muted()),
                        )
                        .push(text(&p.title).size(type_scale::XS).color(t::text_primary())),
                )
                .on_press(Message::OpenIssueDetail(p.id.clone()))
                .padding(0)
                .style(button::text),
            )
            .push(link("Change", Message::OpenParentPicker))
            .push(link("Remove", Message::RemoveParent))
            .into(),
        None => link("Set parent issue…", Message::OpenParentPicker).into(),
    }
}

/// "Sub-issues" block in the left column, shown on every issue (sub-issues can
/// nest). Header with an "N/M done" count, a progress bar, one clickable row per
/// child (closed ones dimmed), then a "+ New sub-issue" button that opens the
/// create modal pre-linked to `issue`.
///
/// Extension points: follow-up tickets add their buttons to the `actions` row
/// (JRST-436 "Add existing", JRST-444 "View tree →") and a per-row remove
/// control in `sub_issue_row` (JRST-437).
fn sub_issues_section<'a>(
    issue: &'a Issue,
    sub_issues: &[&'a Issue],
    team: Option<&'a Team>,
    lanes: &'a [Lane],
) -> Element<'a, Message> {
    let children = ordered_sub_issues(sub_issues, lanes);
    let done = done_count(&children);
    let mut header = row![]
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push(
            text("Sub-issues")
                .size(type_scale::SM)
                .font(crate::fonts::bold_active())
                .color(t::text_secondary()),
        );
    if !children.is_empty() {
        header = header.push(
            text(format!("{}/{} done", done, children.len()))
                .size(type_scale::XS)
                .color(t::text_muted()),
        );
    }
    let has_tree = issue.parent_issue_id.is_some() || !children.is_empty();
    let actions = row![]
        .spacing(space::S3)
        .align_y(Alignment::Center)
        .push_maybe(has_tree.then(|| {
            button(text("View tree →").size(type_scale::XS).color(t::accent()))
                .on_press(Message::OpenHierarchyTree(issue.id.clone()))
                .padding(0)
                .style(button::text)
        }))
        .push(
            button(text("Add existing").size(type_scale::XS).color(t::accent()))
                .on_press(Message::OpenChildPicker)
                .padding(0)
                .style(button::text),
        )
        .push(
            button(text("+ New sub-issue").size(type_scale::XS).color(t::accent()))
                .on_press(Message::CreateSubIssue(issue.id.clone()))
                .padding(0)
                .style(button::text),
        );

    let mut col = column![].spacing(space::S2);
    if children.is_empty() {
        col = col.push(
            text("No sub-issues yet.")
                .size(type_scale::XS)
                .color(t::text_muted()),
        );
    } else {
        col = col.push(sub_issue_progress_bar(done, children.len()));
        for child in &children {
            col = col.push(sub_issue_row(child, team, lanes));
        }
    }
    column![]
        .spacing(space::S4)
        .push(
            row![]
                .align_y(Alignment::Center)
                .push(header.width(Length::Fill))
                .push(actions),
        )
        .push(col)
        .into()
}

/// One sub-issue: `IDENT  title` with its lane name as a colored badge on the right.
fn sub_issue_row<'a>(
    child: &'a Issue,
    team: Option<&'a Team>,
    lanes: &'a [Lane],
) -> Element<'a, Message> {
    let lane = lanes.iter().find(|l| l.id == child.lane_id);
    // Closed (done/canceled) children are dimmed, like Rails' `opacity-60`.
    let closed = crate::display::is_completed(child);
    let dim = move |c: Color| if closed { c.scale_alpha(0.6) } else { c };
    let badge = lane.map(|l| {
        let color = l
            .color
            .as_deref()
            .and_then(t::hex_to_color)
            .unwrap_or(t::text_muted());
        container(
            row![]
                .spacing(space::S1)
                .align_y(Alignment::Center)
                .push(lane_icon::view(crate::lane_icon::icon_for(l.category.as_deref())))
                .push(text(&l.name).size(11).color(color)),
        )
            .padding([space::S0_5, space::S2])
            .style(move |_theme: &Theme| container::Style {
                border: Border {
                    color,
                    width: 1.0,
                    radius: 10.0.into(),
                },
                ..Default::default()
            })
    });

    let open = button(
        row![]
            .spacing(space::S2)
            .align_y(Alignment::Center)
            .push(
                text(issue_identifier(child, team))
                    .size(type_scale::XS)
                    .color(dim(t::text_muted())),
            )
            .push(
                text(&child.title)
                    .size(13)
                    .color(dim(t::text_primary()))
                    .width(Length::Fill),
            )
            .push_maybe(badge),
    )
    .on_press(Message::OpenIssueDetail(child.id.clone()))
    .padding([space::S1_5, space::S2])
    .width(Length::Fill)
    .style(button::text);
    // Trailing × detaches the child (it becomes top-level).
    row![]
        .spacing(space::S1_5)
        .align_y(Alignment::Center)
        .push(open)
        .push(
            button(text("×").size(type_scale::SM).color(t::text_muted()))
                .on_press(Message::DetachSubIssue(child.id.clone()))
                .padding(0)
                .style(button::text),
        )
        .into()
}

/// Width of a sidebar control: the panel minus its padding.
const SIDEBAR_CONTROL_WIDTH: f32 = SIDEBAR_WIDTH - 2.0 * space::S6;

/// A sidebar property control (Rails' `issue_sidebar` toggle button) with its
/// dropdown open below it while `picker` is open on `field`. A click toggles
/// it; `s` / `p` / `j` / `l` open it too.
fn sidebar_select<'a>(
    field: Field,
    value: Option<String>,
    empty: &'static str,
    picker: Option<&'a PickerState>,
    menu_width: f32,
) -> Element<'a, Message> {
    let open = picker.filter(|p| p.field == field);
    // One line, cut with "…" when it doesn't fit (the open menu shows it whole).
    let label = match value {
        Some(v) => truncated(v, type_scale::SM, t::text_body()),
        None => truncated(empty, type_scale::SM, t::text_muted()),
    };
    let is_open = open.is_some();
    let anchor = button(
        row![]
            .spacing(space::S1)
            .align_y(Alignment::Center)
            .push(container(label).width(Length::Fill))
            .push(text("▾").size(type_scale::SM).color(t::text_muted())),
    )
    .on_press(Message::OpenPropertyPicker(field))
    .width(Length::Fill)
    .padding(SIDEBAR_CONTROL_PADDING)
    .style(move |_: &Theme, status| button::Style {
        background: Some(t::bg_secondary().into()),
        text_color: t::text_body(),
        border: Border {
            color: if is_open || matches!(status, button::Status::Hovered) {
                t::text_muted()
            } else {
                t::border()
            },
            width: 1.0,
            radius: radius::MD.into(),
        },
        ..Default::default()
    });
    dropdown(
        anchor,
        open.map(property_picker::sidebar_menu),
        menu_width,
        Message::CloseModal,
    )
    .into()
}

/// Em-dash placeholder for sidebar sections filled in by later tickets
/// (Assignee JAIT-113, Labels JAIT-112).
fn placeholder_value<'a>() -> Element<'a, Message> {
    text("—").size(type_scale::SM).color(t::text_muted()).into()
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
fn dependency_block<'a>(
    kind: DepKind,
    links: &'a [DependencyLink],
) -> (&'static str, Element<'a, Message>) {
    (kind.title(), dependency_section(links, kind))
}

/// Like [`dependency_block`], but absent when there are no links.
fn optional_dependency_block<'a>(
    kind: DepKind,
    links: &'a [DependencyLink],
) -> Option<(&'static str, Element<'a, Message>)> {
    (!links.is_empty()).then(|| dependency_block(kind, links))
}

/// The linked issues as clickable rows, followed (for editable kinds) by a
/// text "+ Add …" button that opens the picker.
fn dependency_section<'a>(links: &'a [DependencyLink], kind: DepKind) -> Element<'a, Message> {
    let color = kind.color();
    let mut col = column![].spacing(space::S1_5);
    for link in links {
        col = col.push(dependency_row(link, color));
    }
    if let Some((add_label, direction)) = kind.add() {
        col = col.push(
            button(text(add_label).size(type_scale::XS).color(t::accent()))
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
    let line = row![]
        .spacing(space::S1_5)
        .align_y(Alignment::Center)
        .push(
            button(
                row![]
                    .spacing(space::S1_5)
                    .push(text(&link.identifier).size(type_scale::XS).color(relation_color))
                    .push(text(&link.title).size(type_scale::XS).color(t::text_secondary())),
            )
            .on_press(Message::OpenIssueDetail(link.issue_id.clone()))
            .padding(0)
            .width(Length::Fill)
            .style(button::text),
        )
        .push(
            button(text("×").size(type_scale::SM).color(t::text_muted()))
                .on_press(Message::RemoveDependency(link.dep_id.clone()))
                .padding(0)
                .style(button::text),
        );
    // The wrapper never captures events, so the × still removes the edge.
    issue_row_anchor(line, &link.issue_id)
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

/// Wrap an editor binding so Cmd+V with an image-only clipboard sends `paste`
/// (text on the clipboard keeps the default paste).
pub fn paste_image_binding(
    inner: impl Fn(text_editor::KeyPress) -> Option<text_editor::Binding<Message>>,
    paste: Message,
) -> impl Fn(text_editor::KeyPress) -> Option<text_editor::Binding<Message>> {
    move |press| {
        let is_paste = press.modifiers.command()
            && matches!(press.key.as_ref(), iced::keyboard::Key::Character("v"));
        if is_paste
            && matches!(
                crate::files::paste::read_clipboard(),
                crate::files::paste::ClipboardPaste::Image(_)
            )
        {
            return Some(text_editor::Binding::Custom(paste.clone()));
        }
        inner(press)
    }
}

/// Pending inline images by placeholder uuid -> outbox file, for `BodyCtx`.
pub fn pending_inline_paths(attachments: &[Attachment]) -> crate::views::body::PendingInlineMap {
    use crate::db::attachments::UploadState;
    attachments
        .iter()
        .filter(|a| a.upload_kind == crate::db::attachments::UploadKind::InlineImage)
        .filter_map(|a| {
            Some((
                a.placeholder_uuid.clone()?,
                crate::views::body::PendingInline {
                    id: a.id.clone(),
                    path: std::path::PathBuf::from(a.outbox_path.as_ref()?),
                    failure: (a.upload_state == UploadState::Failed)
                        .then(|| a.failure_reason.clone().unwrap_or_else(|| "unknown error".into())),
                },
            ))
        })
        .collect()
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
    let mut col = column![].spacing(space::S0_5).push(
        text("↑↓ to move · Enter/Tab or click to insert · Esc to dismiss")
            .size(type_scale::XS)
            .color(t::text_muted()),
    );
    for (i, item) in s.items.iter().enumerate() {
        let selected = i == s.selected;
        col = col.push(
            button(
                row![]
                    .spacing(space::S2)
                    .push(text(&item.label).size(type_scale::SM).color(t::accent()))
                    .push(text(&item.detail).size(type_scale::SM).color(t::text_secondary())),
            )
            .on_press(Message::PickSuggestion(i))
            .padding([space::S2, space::S3])
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
        .padding(space::S1_5)
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_secondary())),
            border: Border {
                color: t::border(),
                width: 1.0,
                radius: radius::MD.into(),
            },
            ..Default::default()
        })
        .into()
}

/// How an editable markdown description is drawn (issue and project detail).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DescriptionDisplay {
    Editor,
    Rendered,
    /// Empty and not being edited: an "Add description…" prompt.
    Empty,
}

/// Editing wins; otherwise a blank buffer shows the prompt and anything else
/// is rendered. Decided from the buffered text so unsaved edits show.
pub(crate) fn description_display(buffered: &str, editing: bool) -> DescriptionDisplay {
    if editing {
        DescriptionDisplay::Editor
    } else if buffered.trim().is_empty() {
        DescriptionDisplay::Empty
    } else {
        DescriptionDisplay::Rendered
    }
}

/// Container id used to find the description editor for focusing.
pub(crate) fn description_editor_id() -> iced::widget::container::Id {
    iced::widget::container::Id::new("issue-description-editor")
}

/// Focuses the first focusable widget inside the container `target`
/// (text_editor has no id of its own in iced 0.13) and unfocuses the rest.
pub(crate) struct FocusFirstIn {
    target: iced::advanced::widget::Id,
    done: bool,
}

impl FocusFirstIn {
    pub(crate) fn new(target: iced::widget::container::Id) -> Self {
        Self { target: target.into(), done: false }
    }
}

struct FocusFirst<'a>(&'a mut bool);

impl iced::advanced::widget::Operation<()> for FocusFirst<'_> {
    fn container(
        &mut self,
        _id: Option<&iced::advanced::widget::Id>,
        _bounds: iced::Rectangle,
        operate_on_children: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<()>),
    ) {
        operate_on_children(self);
    }

    fn focusable(
        &mut self,
        state: &mut dyn iced::advanced::widget::operation::Focusable,
        _id: Option<&iced::advanced::widget::Id>,
    ) {
        if !*self.0 {
            state.focus();
            *self.0 = true;
        }
    }
}

impl iced::advanced::widget::Operation<()> for FocusFirstIn {
    fn container(
        &mut self,
        id: Option<&iced::advanced::widget::Id>,
        _bounds: iced::Rectangle,
        operate_on_children: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<()>),
    ) {
        if id == Some(&self.target) {
            operate_on_children(&mut FocusFirst(&mut self.done));
        } else {
            operate_on_children(self);
        }
    }

    fn focusable(
        &mut self,
        state: &mut dyn iced::advanced::widget::operation::Focusable,
        _id: Option<&iced::advanced::widget::Id>,
    ) {
        // Anything focused outside the target loses focus.
        state.unfocus();
    }
}

/// Reports whether a focusable widget outside the container `target` has
/// focus. Run on Escape: the description editor is left unless the pointer's
/// keyboard focus is in another editor (the comment composer), where Escape
/// isn't about the description. iced 0.13 can't query focus directly, but an
/// operation can read it widget by widget.
pub(crate) struct FocusedOutside {
    target: iced::advanced::widget::Id,
    inside: bool,
    outside: bool,
}

impl FocusedOutside {
    pub(crate) fn new(target: iced::widget::container::Id) -> Self {
        Self { target: target.into(), inside: false, outside: false }
    }
}

impl iced::advanced::widget::Operation<bool> for FocusedOutside {
    fn container(
        &mut self,
        id: Option<&iced::advanced::widget::Id>,
        _bounds: iced::Rectangle,
        operate_on_children: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<bool>),
    ) {
        let was_inside = self.inside;
        self.inside |= id == Some(&self.target);
        operate_on_children(self);
        self.inside = was_inside;
    }

    fn focusable(
        &mut self,
        state: &mut dyn iced::advanced::widget::operation::Focusable,
        _id: Option<&iced::advanced::widget::Id>,
    ) {
        if !self.inside && state.is_focused() {
            self.outside = true;
        }
    }

    fn finish(&self) -> iced::advanced::widget::operation::Outcome<bool> {
        iced::advanced::widget::operation::Outcome::Some(self.outside)
    }
}

/// The description: rendered markdown (click to edit), an empty prompt, or the editor.
fn description_section<'a>(detail: &'a IssueDetailState, ctx: &BodyCtx<'a>) -> Element<'a, Message> {
    let text_now = detail.description_text();
    description_block(
        description_display(&text_now, detail.editing_description),
        description_editor_id(),
        Message::EditIssueDescription,
        || {
            let editor = text_editor(&detail.description)
                .placeholder("Add description…  (@ to mention, # to link an issue)")
                .on_action(Message::IssueDetailDescriptionChanged)
                .key_binding(paste_image_binding(
                    editor_binding(
                        None,
                        suggesting(detail.suggest.as_ref(), EditorTarget::Description),
                    ),
                    Message::PasteImageIntoDescription,
                ))
                .padding(space::S2_5)
                .size(type_scale::SM)
                .height(Length::Fixed(220.0));
            (editor.into(), suggestion_list(detail.suggest.as_ref(), EditorTarget::Description))
        },
        || crate::views::body::view(&text_now, type_scale::SM, t::text_body(), ctx),
    )
}

/// Shared by issue and project detail: lays out one of the three description
/// modes. `editor` is only built while editing and returns the editor plus an
/// optional suggestion list; clicking the prompt or the rendered body sends `edit`.
pub(crate) fn description_block<'a>(
    display: DescriptionDisplay,
    editor_id: iced::widget::container::Id,
    edit: Message,
    editor: impl FnOnce() -> (Element<'a, Message>, Option<Element<'a, Message>>),
    rendered: impl FnOnce() -> Element<'a, Message>,
) -> Element<'a, Message> {
    match display {
        DescriptionDisplay::Editor => {
            let (editor, suggestions) = editor();
            column![]
                .push(container(editor).id(editor_id))
                .push_maybe(suggestions)
                .into()
        }
        DescriptionDisplay::Empty => iced::widget::mouse_area(
            container(text("Add description…").size(type_scale::SM).color(t::text_muted()))
                .padding(space::S2_5)
                .width(Length::Fill),
        )
        .on_press(edit)
        .interaction(iced::mouse::Interaction::Text)
        .into(),
        DescriptionDisplay::Rendered => {
            iced::widget::mouse_area(container(rendered()).padding(space::S2_5).width(Length::Fill))
                .on_press(edit)
                .interaction(iced::mouse::Interaction::Text)
                .into()
        }
    }
}

/// Makes a row the hover-card anchor for the issue with local id `issue_id`.
/// Never captures events, so the row's buttons and focus work as before.
fn issue_row_anchor<'a>(
    row: impl Into<Element<'a, Message>>,
    issue_id: &str,
) -> Element<'a, Message> {
    crate::views::row_hover::row_hover(
        row,
        crate::body_hover::Anchor::Issue(issue_id.to_string()),
        Message::RowHover,
    )
    .into()
}

/// "Referenced in" block: one clickable row per issue whose description or
/// comments mention this one.
fn referenced_in_section<'a>(
    references: &'a [Reference],
    issues: &'a [Issue],
    team: Option<&'a Team>,
) -> Element<'a, Message> {
    let mut col = column![].spacing(space::S1);
    for r in references {
        let Some(src) = issues.iter().find(|i| i.id == r.source_issue_id) else {
            continue;
        };
        let via = match (r.in_description, r.in_comment) {
            (true, true) => "description, comment",
            (true, false) => "description",
            _ => "comment",
        };
        let row_button = button(
            row![]
                .spacing(space::S2)
                .align_y(Alignment::Center)
                .push(
                    text(issue_identifier(src, team))
                        .size(type_scale::XS)
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
        .padding([space::S1_5, space::S2])
        .width(Length::Fill)
        .style(button::text);
        col = col.push(issue_row_anchor(row_button, &src.id));
    }
    with_heading("Referenced in", col)
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
    let mut col = column![].spacing(space::S1);

    let items = activity::timeline(activity, comments);
    if items.is_empty() {
        col = col.push(text("No activity yet").size(type_scale::XS).color(t::text_muted()));
    }
    for item in items {
        match item {
            TimelineItem::Activity(item) => col = col.push(activity_row(item)),
            TimelineItem::Thread(thread) => {
                col = push_thread(col, thread, ctx);
            }
        }
    }

    let col = col.push(
        container(new_comment_composer(
            &detail.comment_composer,
            detail.suggest.as_ref(),
            &detail.composer_files.files,
        ))
        .padding(iced::Padding::ZERO.top(space::S4)),
    );
    with_heading("Activity", col)
}

/// Small action button (web `px-3 py-1.5 text-xs rounded`): the theme's
/// button style with the polished radius.
fn small_button_style(
    base: fn(&Theme, button::Status) -> button::Style,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let mut style = base(theme, status);
        style.border.radius = radius::SM.into();
        style
    }
}

/// What `push_thread` needs besides the comments themselves: lookups for
/// rendering plus the open reply composer. Shared by issue and project detail.
pub struct ThreadCtx<'a, 'b> {
    pub members: &'a [Member],
    /// Comment authors from any team, for the author name; `members` is the
    /// team's and may not include someone who has since left it.
    pub authors: &'a [Member],
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
    /// Configured JAIT base URL (resolves inline image URLs).
    pub host: &'a str,
    /// Unsent pasted images (placeholder uuid -> outbox file), comments included.
    pub pending_paths: &'a crate::views::body::PendingInlineMap,
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
    staged: &'a [Attachment],
) -> Element<'a, Message> {
    column![]
        .spacing(space::S2)
        .push(
            text_editor(content)
                .placeholder("Leave a comment…  (Cmd+Enter to send)")
                .on_action(Message::CommentComposerChanged)
                .key_binding(paste_image_binding(
                    editor_binding(
                        Some(Message::SubmitComment),
                        suggesting(suggest, EditorTarget::Comment),
                    ),
                    Message::PasteImageIntoComment,
                ))
                .padding(space::S2_5)
                .size(type_scale::SM)
                .height(Length::Fixed(80.0)),
        )
        .push_maybe(suggestion_list(suggest, EditorTarget::Comment))
        .push_maybe((!staged.is_empty()).then(|| staged_chips(staged)))
        .push(
            row![]
                .spacing(space::S2)
                .push(
                    button(text("Comment").size(type_scale::XS))
                        .on_press(Message::SubmitComment)
                        .padding([space::S1_5, space::S3])
                        .style(small_button_style(button::primary)),
                )
                .push(
                    button(text("+ Attach file").size(type_scale::XS))
                        .on_press(Message::PickCommentFiles)
                        .padding([space::S1_5, space::S3])
                        .style(small_button_style(button::secondary)),
                ),
        )
        .into()
}

/// One removable chip per file staged on the composer.
fn staged_chips<'a>(staged: &'a [Attachment]) -> Element<'a, Message> {
    let mut chips = row![].spacing(space::S1_5).align_y(Alignment::Center);
    for a in staged {
        chips = chips.push(
            button(
                text(format!(
                    "{} ({})  ✕",
                    a.filename,
                    crate::views::attachments::format_size(a.byte_size)
                ))
                .size(type_scale::XS),
            )
            .on_press(Message::RemoveStagedFile(a.id.clone()))
            .padding([space::S1, space::S2])
            .style(button::secondary),
        );
    }
    chips.wrap().into()
}

/// One change-history line: "actor description  ·  3d ago".
/// A null actor shows the text alone; a Pending item is marked as unsynced.
fn activity_row<'a>(item: &'a ActivityItem) -> Element<'a, Message> {
    let pending = item.origin == ItemOrigin::Pending;
    let line = row![]
        .spacing(space::S1_5)
        .align_y(Alignment::Center)
        .push(text("•").size(type_scale::XS).color(t::text_muted()))
        .push_maybe(
            item.actor
                .name
                .as_deref()
                .map(|n| text(n).size(type_scale::XS).color(t::text_secondary())),
        )
        .push(text(item.text.as_str()).size(type_scale::XS).color(t::text_muted()))
        .push_maybe(pending.then(|| text("(pending)").size(type_scale::XS).color(t::text_muted())))
        .push(
            text(relative_time(&item.at))
                .size(type_scale::XS)
                .color(t::text_muted()),
        );
    // Only an incoming reference with a cached source issue has a card.
    match &item.source_issue_id {
        Some(id) => {
            let clickable = iced::widget::mouse_area(line)
                .on_press(Message::OpenIssueDetail(id.clone()))
                .interaction(iced::mouse::Interaction::Pointer);
            issue_row_anchor(clickable, id)
        }
        None => line.into(),
    }
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
        .spacing(space::S1_5)
        .push(
            text_editor(content)
                .placeholder("Write a reply…  (Cmd+Enter to send)")
                .on_action(Message::ReplyComposerChanged)
                .key_binding(paste_image_binding(
                    editor_binding(
                        Some(Message::SubmitReply),
                        suggesting(suggest, EditorTarget::Reply),
                    ),
                    Message::PasteImageIntoReply,
                ))
                .padding(space::S2)
                .size(type_scale::SM)
                .height(Length::Fixed(64.0)),
        )
        .push(
            row![]
                .spacing(space::S2)
                .push(
                    button(text("Reply").size(type_scale::XS))
                        .on_press(Message::SubmitReply)
                        .padding([space::S1_5, space::S3])
                        .style(small_button_style(button::primary)),
                )
                .push(
                    button(text("Cancel").size(type_scale::XS))
                        .on_press(Message::CancelReply)
                        .padding([space::S1_5, space::S3])
                        .style(small_button_style(button::secondary)),
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
        teams,
        issues,
        my_member_ids,
        can_reply,
        ..
    } = *ctx;
    let author = comment
        .author_id
        .as_ref()
        .and_then(|aid| {
            members
                .iter()
                .chain(ctx.authors)
                .find(|m| &m.id == aid)
        })
        .map(|m| m.name.as_str())
        .unwrap_or("Unknown");

    let mut col = column![].spacing(space::S1).push(
        row![]
            .spacing(space::S2)
            .align_y(Alignment::Center)
            .push(text(author).size(type_scale::SM).color(t::text_body()))
            .push(
                text(relative_time(&comment.created_at))
                    .size(type_scale::XS)
                    .color(t::text_muted()),
            ),
    );
    col = col.push(crate::views::body::view(
        &comment.body,
        type_scale::SM,
        t::text_body(),
        &BodyCtx {
            teams,
            issues,
            members,
            images: ctx.images,
            host: ctx.host,
            pending_paths: Some(ctx.pending_paths),
            on_image: Some(Message::OpenInlineImage),
        },
    ));
    // Delete is offered on the user's own comments, synced or not. The desktop
    // does not know member roles, so team admins are not offered it on others'
    // comments (the server would 403 anyone else; the row is then restored).
    let can_delete = comment
        .author_id
        .as_ref()
        .is_some_and(|aid| my_member_ids.contains(aid));
    // Attachment delete follows the same author rule.
    col = col.push_maybe(
        ctx.comment_attachments
            .get(&comment.id)
            .and_then(|files| crate::views::attachments::section_with(files, ctx.images, can_delete)),
    );
    let mut actions = row![].spacing(space::S2_5);
    let can_reply = can_reply && Comment::can_reply_at(depth);
    if can_reply {
        actions = actions.push(
            button(text("Reply").size(type_scale::XS).color(t::text_muted()))
                .on_press(Message::StartReply(comment.id.clone()))
                .padding(0)
                .style(button::text),
        );
    }
    if can_delete {
        actions = actions.push(
            button(text("Delete").size(type_scale::XS).color(t::text_muted()))
                .on_press(Message::ConfirmDeleteComment(comment.id.clone()))
                .padding(0)
                .style(button::text),
        );
    }
    if can_reply || can_delete {
        col = col.push(actions);
    }

    // Each comment is a card; replies keep their `INDENT_PX` indent. The
    // vertical padding makes the gap between cards `space::S5`.
    container(
        container(col)
            .padding(space::S4)
            .width(Length::Fill)
            .style(|_theme: &Theme| container::Style {
                background: Some(t::bg_secondary().into()),
                border: Border {
                    radius: radius::LG.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
    )
    .padding(
        iced::Padding::from([space::S2_5, 0.0]).left(INDENT_PX * depth as f32),
    )
    .into()
}

/// Title editor keys: Enter commits instead of starting a new line.
fn title_binding(press: text_editor::KeyPress) -> Option<text_editor::Binding<Message>> {
    match press.key.as_ref() {
        iced::keyboard::Key::Named(iced::keyboard::key::Named::Enter) => {
            Some(text_editor::Binding::Custom(Message::CommitIssueDetail))
        }
        _ => text_editor::Binding::from_key_press(press),
    }
}

/// Keep the title editor to one logical line: drop new lines and flatten
/// pasted ones to spaces. `None` drops the action.
pub fn single_line(action: text_editor::Action) -> Option<text_editor::Action> {
    use text_editor::{Action, Edit};
    match action {
        Action::Edit(Edit::Enter) => None,
        Action::Edit(Edit::Paste(text)) => {
            let flat = text.replace("\r\n", " ").replace(['\n', '\r'], " ");
            Some(Action::Edit(Edit::Paste(std::sync::Arc::new(flat))))
        }
        other => Some(other),
    }
}

/// Borderless, transparent style so the title editor reads as an inline heading.
fn title_editor_style(_theme: &Theme, _status: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
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

#[cfg(test)]
mod buffer_tests {
    use super::*;

    fn issue(desc: &str) -> Issue {
        Issue {
            id: "i1".into(),
            title: "Title".into(),
            description: Some(desc.into()),
            priority: 0,
            team_number: Some(1),
            due_date: None,
            estimate: Some(2.0),
            started_at: None,
            completed_at: None,
            canceled_at: None,
            lane_id: "l".into(),
            team_id: "t".into(),
            project_id: None,
            parent_issue_id: None,
            assignee_id: None,
            creator_id: None,
            remote_id: Some(1),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn an_untouched_buffer_writes_nothing() {
        let state = IssueDetailState::from_issue(&issue("old"));
        assert!(!state.is_dirty());
        let mut fresh = issue("new from rails");
        assert_eq!(state.apply_edits(&mut fresh), Ok(false));
        assert_eq!(fresh.description.as_deref(), Some("new from rails"));
    }

    #[test]
    fn only_edited_fields_are_written() {
        let mut state = IssueDetailState::from_issue(&issue("old"));
        state.estimate = "3.5".into();
        let mut fresh = issue("new from rails");
        assert_eq!(state.apply_edits(&mut fresh), Ok(true));
        assert_eq!(fresh.estimate, Some(3.5));
        assert_eq!(fresh.description.as_deref(), Some("new from rails"));
    }

    #[test]
    fn a_bad_estimate_is_an_error() {
        let mut state = IssueDetailState::from_issue(&issue("old"));
        state.estimate = "lots".into();
        assert!(state.apply_edits(&mut issue("old")).is_err());
    }

    #[test]
    fn refresh_takes_pulled_values_only_when_clean() {
        let mut clean = IssueDetailState::from_issue(&issue("old"));
        clean.refresh_from(&issue("new"));
        assert_eq!(clean.fields().description.as_deref(), Some("new"));

        let mut dirty = IssueDetailState::from_issue(&issue("old"));
        dirty.title = text_editor::Content::with_text("Edited");
        dirty.refresh_from(&issue("new"));
        assert_eq!(dirty.fields().description.as_deref(), Some("old"));
        assert_eq!(dirty.fields().title, "Edited");
    }
}

#[cfg(test)]
mod description_tests {
    use super::*;

    #[test]
    fn description_display_table() {
        let cases = [
            ("", false, DescriptionDisplay::Empty),
            ("  \n ", false, DescriptionDisplay::Empty),
            ("hello **x**", false, DescriptionDisplay::Rendered),
            ("hello", true, DescriptionDisplay::Editor),
            ("", true, DescriptionDisplay::Editor),
        ];
        for (text, editing, want) in cases {
            assert_eq!(description_display(text, editing), want, "{text:?} {editing}");
        }
    }

    struct Fake(bool);

    impl iced::advanced::widget::operation::Focusable for Fake {
        fn is_focused(&self) -> bool {
            self.0
        }
        fn focus(&mut self) {
            self.0 = true;
        }
        fn unfocus(&mut self) {
            self.0 = false;
        }
    }

    #[test]
    fn escape_probe_sees_focus_only_outside_the_description_editor() {
        use iced::advanced::widget::Operation;
        let editor = description_editor_id();
        let editor_id: iced::advanced::widget::Id = editor.clone().into();
        // (focused inside the editor, focused outside it) -> reported "elsewhere"
        let cases = [(false, false, false), (true, false, false), (false, true, true), (true, true, true)];
        for (inside, outside, want) in cases {
            let mut op = FocusedOutside::new(editor.clone());
            op.container(None, iced::Rectangle::default(), &mut |op| {
                op.focusable(&mut Fake(outside), None);
                op.container(Some(&editor_id), iced::Rectangle::default(), &mut |op| {
                    op.focusable(&mut Fake(inside), None);
                });
                // Back outside the editor after leaving it.
                op.focusable(&mut Fake(false), None);
            });
            let got = matches!(op.finish(), iced::advanced::widget::operation::Outcome::Some(true));
            assert_eq!(got, want, "inside {inside} outside {outside}");
        }
    }
}

#[cfg(test)]
mod sub_issue_tests {
    use super::*;

    fn child(id: &str, lane: &str, num: i32) -> Issue {
        Issue {
            id: id.into(),
            title: id.into(),
            description: None,
            priority: 0,
            team_number: Some(num),
            due_date: None,
            estimate: None,
            started_at: None,
            completed_at: None,
            canceled_at: None,
            lane_id: lane.into(),
            team_id: "t".into(),
            project_id: None,
            parent_issue_id: Some("p".into()),
            assignee_id: None,
            creator_id: None,
            remote_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn lane(id: &str, position: i32) -> Lane {
        Lane {
            id: id.into(),
            name: id.into(),
            color: None,
            position,
            team_id: "t".into(),
            remote_id: None,
            category: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn sorts_by_lane_position_then_team_number() {
        let lanes = [lane("a", 0), lane("b", 1)];
        let (c1, c2, c3) = (child("c1", "b", 1), child("c2", "a", 5), child("c3", "a", 2));
        let sorted = ordered_sub_issues(&[&c1, &c2, &c3], &lanes);
        let ids: Vec<&str> = sorted.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["c3", "c2", "c1"]);
    }

    #[test]
    fn done_count_includes_completed_and_canceled() {
        let open = child("o", "a", 1);
        let mut done = child("d", "a", 2);
        done.completed_at = Some("2026-01-01T00:00:00Z".into());
        let mut canceled = child("c", "a", 3);
        canceled.canceled_at = Some("2026-01-01T00:00:00Z".into());
        assert_eq!(done_count(&[&open, &done, &canceled]), 2);
        assert_eq!(done_count(&[]), 0);
    }
}
