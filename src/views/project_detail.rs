use iced::widget::{button, column, container, pick_list, row, scrollable, text_editor, text_input};
use crate::views::text;
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::{Message, View};
use crate::scroll::{track, ScrollKey};
use crate::db::models::{Comment, Issue, Lane, Member, Priority, Project, ProjectStatus, Team};
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::issue_detail::{new_comment_composer, push_thread, CommentAttachments, ThreadCtx};
use crate::display::{CardProperty, DisplayOptions, ProjectIssueSort};
use crate::views::util;
use crate::views::progress_chart;
use crate::views::date_picker;
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
    /// The buffered fields as last loaded or committed. Only fields that differ
    /// from this are written on commit, so an untouched buffer never pushes a
    /// stale copy over a newer pulled value.
    pub loaded: ProjectFields,
}

/// The buffered project fields in the form a commit writes them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectFields {
    pub name: String,
    pub description: Option<String>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
}

fn non_blank(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

impl ProjectDetailState {
    pub fn from_project(p: &Project) -> Self {
        let mut state = Self::buffers_for(p);
        state.loaded = state.fields();
        state
    }

    fn buffers_for(p: &Project) -> Self {
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
            loaded: ProjectFields::default(),
        }
    }

    pub fn description_text(&self) -> String {
        self.description.text()
    }

    /// The buffers as a commit would write them.
    pub fn fields(&self) -> ProjectFields {
        let body = self.description_text();
        ProjectFields {
            name: self.name.trim().to_string(),
            description: (!body.trim().is_empty()).then(|| body.trim_end().to_string()),
            start_date: non_blank(&self.start_date),
            due_date: non_blank(&self.due_date),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.fields() != self.loaded
    }

    /// Write only the fields edited since load onto `project` (the fresh DB
    /// row). Returns whether anything changed.
    pub fn apply_edits(&self, project: &mut Project) -> bool {
        let now = self.fields();
        let was = &self.loaded;
        let mut changed = false;
        if now.name != was.name {
            project.name = now.name;
            changed = true;
        }
        if now.description != was.description {
            project.description = now.description;
            changed = true;
        }
        if now.start_date != was.start_date {
            project.start_date = now.start_date;
            changed = true;
        }
        if now.due_date != was.due_date {
            project.due_date = now.due_date;
            changed = true;
        }
        changed
    }

    /// Record the buffers as committed.
    pub fn mark_committed(&mut self) {
        self.loaded = self.fields();
    }

    /// Take `p`'s values into the buffers when they hold no unsaved edits and
    /// `p` differs from what was loaded (e.g. a pull brought a newer copy).
    /// Composers and drafts are kept.
    pub fn refresh_from(&mut self, p: &Project) {
        if self.is_dirty() {
            return;
        }
        let fresh = Self::from_project(p);
        if fresh.loaded == self.loaded {
            return;
        }
        self.name = fresh.name;
        self.description = fresh.description;
        self.start_date = fresh.start_date;
        self.due_date = fresh.due_date;
        self.loaded = fresh.loaded;
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
    show_back: bool,
    detail: &'a ProjectDetailState,
    team: Option<&'a Team>,
    members: &'a [Member],
    comment_authors: &'a [Member],
    issues: &'a [Issue],
    visible: Vec<&'a Issue>,
    display: &'a DisplayOptions,
    issue_sort: ProjectIssueSort,
    lanes: &'a [Lane],
    teams: &'a [Team],
    comments: &'a [Comment],
    attachments: &'a [crate::db::attachments::Attachment],
    comment_attachments: &'a CommentAttachments,
    images: &'a crate::files::images::ImageStore,
    host: &'a str,
    pending_paths: &'a crate::views::body::PendingInlineMap,
    window_width: f32,
    properties_open: bool,
    stack_cards: bool,
    // Width a right-docked Terminal panel takes from the page (0 when none);
    // any width stacks the Burndown and Issues cards.
    terminal_width: f32,
    // The inline Properties card shows its "More" fields.
    more_properties: bool,
    // The Terminal panel is on screen: issue rows offer to send their prompt.
    terminal_showing: bool,
    // The right-docked Terminal panel, placed between the page and Properties.
    terminal: Option<Element<'a, Message>>,
    // The open date field calendar, if any.
    date_picker_state: Option<&date_picker::DatePickerState>,
) -> Element<'a, Message> {
    // ---- Breadcrumb: "← Projects › <name> [› #<rails id>]" ----
    let mut breadcrumb = row![].spacing(space::S2).align_y(Alignment::Center);
    if show_back {
        breadcrumb = breadcrumb
            .push(
                button(text("← Projects").size(type_scale::SM).color(t::text_secondary()))
                    .on_press(Message::NavigateToView(View::ProjectList))
                    .padding(0)
                    .style(button::text),
            )
            .push(text("›").size(type_scale::SM).color(t::text_muted()));
    }
    breadcrumb = breadcrumb.push(text(&project.name).size(type_scale::SM).color(t::text_secondary()));
    // The Rails id (what the JAIT MCP's `get_project` takes); absent until synced.
    if let Some(id) = project.remote_id {
        breadcrumb = breadcrumb
            .push(text("›").size(type_scale::SM).color(t::text_muted()))
            .push(text(format!("#{id}")).size(type_scale::SM).color(t::text_secondary()));
    }

    breadcrumb = breadcrumb
        .width(Length::Fill)
        .push(iced::widget::horizontal_space())
        .push(
            button(text("Dependencies").size(type_scale::XS).color(t::accent()))
                .on_press(Message::OpenProjectDependencyMap(project.id.clone()))
                .padding(0)
                .style(button::text),
        );

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
                .font(crate::fonts::bold_active())
                .padding(space::S1)
                .style(title_input_style),
        );

    // ---- Properties: down the right side, or in the header under the title ----
    let placement = util::properties_placement(properties_open, terminal_width);
    let props = properties(project, detail, members, date_picker_state);
    let (inline_props, sidebar_panel) = match placement {
        util::PropertiesPlacement::Hidden => (None, None),
        util::PropertiesPlacement::Sidebar => (None, Some(util::sidebar_properties(props))),
        util::PropertiesPlacement::Inline => (
            Some(util::inline_properties(props, more_properties, Message::ToggleMoreProperties)),
            None,
        ),
    };

    let head = column![]
        .spacing(space::S4)
        .push(breadcrumb)
        .push(header)
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
        .push(cards(
            if stack_cards || terminal_width > 0.0 {
                // A right-docked Terminal leaves the page a tall, narrow
                // column: stack whatever the leftover width.
                CardLayout::Stacked
            } else {
                card_layout(page_width(window_width, placement == util::PropertiesPlacement::Sidebar))
            },
            burndown(project, issues),
            issues_section(visible, display, issue_sort, team, lanes, terminal_showing),
        ))
        .push_maybe(SHOW_PROJECT_DISCUSSION.then(|| {
            discussion_section(
                comments, detail, members, comment_authors, teams, issues, comment_attachments, images, host,
                pending_paths,
            )
        }));
    let left = column![]
        .spacing(space::S6)
        .padding(space::S8)
        .width(Length::Fill)
        .push(head)
        .push(sections);

    let page = row![]
        .width(Length::Fill)
        .height(Length::Fill)
        .push(
            container(
                track(scrollable(left), ScrollKey::ProjectDetail),
            )
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .push_maybe(terminal);
    // Properties are hidden until `/` (or the palette) shows them.
    match sidebar_panel {
        Some(panel) => page.push(util::vertical_divider()).push(panel).into(),
        None => page.into(),
    }
}

/// The project's Properties, laid out by `util::sidebar_properties` or
/// `util::inline_properties`.
fn properties<'a>(
    project: &'a Project,
    detail: &'a ProjectDetailState,
    members: &'a [Member],
    date_picker_state: Option<&date_picker::DatePickerState>,
) -> util::Properties<'a, Message> {
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

    let fields = vec![
        (
            "Status",
            pick_list(
                STATUSES.as_slice(),
                project.status,
                Message::ProjectDetailStatusChanged,
            )
            .width(Length::Fill)
            .padding(util::SIDEBAR_CONTROL_PADDING)
            .text_size(type_scale::SM)
            .style(util::sidebar_pick_list_style)
            .menu_style(util::sidebar_menu_style)
            .into(),
        ),
        (
            "Priority",
            pick_list(
                PRIORITIES.as_slice(),
                Some(project.priority_enum()),
                Message::ProjectDetailPriorityChanged,
            )
            .width(Length::Fill)
            .padding(util::SIDEBAR_CONTROL_PADDING)
            .text_size(type_scale::SM)
            .style(util::sidebar_pick_list_style)
            .menu_style(util::sidebar_menu_style)
            .into(),
        ),
        (
            "Lead",
            pick_list(member_options, selected_lead, |o: MemberOption| {
                Message::ProjectDetailLeadChanged(o.id)
            })
            .width(Length::Fill)
            .padding(util::SIDEBAR_CONTROL_PADDING)
            .text_size(type_scale::SM)
            .style(util::sidebar_pick_list_style)
            .menu_style(util::sidebar_menu_style)
            .into(),
        ),
        // Project labels aren't wired into the data layer yet (see JAIT-181
        // notes); render read-only until the follow-up ticket lands.
        ("Labels", placeholder_value()),
        (
            "Start date",
            detail_date(
                date_picker::DateField::ProjectDetailStart,
                &detail.start_date,
                Message::ProjectDetailStartDateChanged,
                date_picker_state,
            ),
        ),
        (
            "Due date",
            detail_date(
                date_picker::DateField::ProjectDetailDue,
                &detail.due_date,
                Message::ProjectDetailDueDateChanged,
                date_picker_state,
            ),
        ),
    ];
    // Inline, the read-only Labels placeholder waits behind "More".
    let (fields, more) = fields.into_iter().partition(|(label, _)| *label != "Labels");
    util::Properties {
        fields,
        more,
        actions: Vec::new(),
        created_at: &project.created_at,
        updated_at: &project.updated_at,
    }
}

/// Width of the app's left sidebar (the literal in `App::view`).
const APP_SIDEBAR_WIDTH: f32 = 220.0;

/// Narrowest page width at which Burndown and Issues sit side by side. Below
/// it each card would get under about 300px, so they stack. The 800px window
/// minimum leaves 516px, so it always stacks; a 1100px window shows them side
/// by side until Properties opens.
const SIDE_BY_SIDE_MIN_WIDTH: f32 = 760.0;

/// How the Burndown and Issues cards sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardLayout {
    SideBySide,
    Stacked,
}

/// Width the page's content column gets: the window less the app sidebar, the
/// Properties panel and divider when open, and the page's own padding.
fn page_width(window_width: f32, properties_open: bool) -> f32 {
    let panel = if properties_open { util::SIDEBAR_WIDTH + 1.0 } else { 0.0 };
    window_width - APP_SIDEBAR_WIDTH - panel - 2.0 * space::S8
}

fn card_layout(page_width: f32) -> CardLayout {
    if page_width >= SIDE_BY_SIDE_MIN_WIDTH {
        CardLayout::SideBySide
    } else {
        CardLayout::Stacked
    }
}

/// Burndown (wider, left) and Issues (right) side by side, or stacked.
fn cards<'a>(
    layout: CardLayout,
    burndown: Element<'a, Message>,
    issues: Element<'a, Message>,
) -> Element<'a, Message> {
    match layout {
        CardLayout::SideBySide => row![]
            .spacing(space::S4)
            .align_y(Alignment::Start)
            .push(container(burndown).width(Length::FillPortion(3)))
            .push(container(issues).width(Length::FillPortion(2)))
            .into(),
        CardLayout::Stacked => column![].spacing(space::S4).push(burndown).push(issues).into(),
    }
}

fn card_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(t::bg_secondary())),
        border: Border {
            color: t::border(),
            width: 1.0,
            radius: radius::MD.into(),
        },
        ..Default::default()
    }
}

/// A Properties date field: Enter or a calendar pick commits the project.
fn detail_date<'a>(
    field: date_picker::DateField,
    value: &'a str,
    on_change: fn(String) -> Message,
    open: Option<&date_picker::DatePickerState>,
) -> Element<'a, Message> {
    date_picker::view(
        date_picker::DateInput {
            field,
            value,
            placeholder: "yyyy-mm-dd",
            on_change,
            on_commit: Some(Message::CommitProjectDetail),
            look: date_picker::Look::Field,
        },
        open,
        chrono::Local::now().date_naive(),
    )
}

#[cfg(test)]
mod buffer_tests {
    use super::*;

    fn project(desc: &str) -> Project {
        Project {
            id: "p1".into(),
            remote_id: Some(1),
            team_id: "t".into(),
            name: "Proj".into(),
            description: Some(desc.into()),
            status: None,
            priority: 0,
            lead_id: None,
            start_date: None,
            due_date: None,
            roadmap_commitment: None,
            velocity_score: 0,
            completed_issues_count: 0,
            total_issues_count: 0,
            progress_percentage: 0,
            behind_schedule: false,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn an_untouched_buffer_is_clean_and_writes_nothing() {
        let state = ProjectDetailState::from_project(&project("old"));
        assert!(!state.is_dirty());
        // A pull replaced the description; committing must not restore "old".
        let mut fresh = project("new from rails");
        assert!(!state.apply_edits(&mut fresh));
        assert_eq!(fresh.description.as_deref(), Some("new from rails"));
    }

    #[test]
    fn only_edited_fields_are_written() {
        let mut state = ProjectDetailState::from_project(&project("old"));
        state.due_date = "2026-12-01".into();
        let mut fresh = project("new from rails");
        assert!(state.apply_edits(&mut fresh));
        assert_eq!(fresh.due_date.as_deref(), Some("2026-12-01"));
        assert_eq!(fresh.description.as_deref(), Some("new from rails"));
    }

    #[test]
    fn an_edited_description_is_written() {
        let mut state = ProjectDetailState::from_project(&project("old"));
        state.description = text_editor::Content::with_text("mine");
        let mut fresh = project("old");
        assert!(state.apply_edits(&mut fresh));
        assert_eq!(fresh.description.as_deref(), Some("mine"));
        state.mark_committed();
        assert!(!state.is_dirty());
    }

    #[test]
    fn refresh_takes_pulled_values_only_when_clean() {
        let mut clean = ProjectDetailState::from_project(&project("old"));
        clean.refresh_from(&project("new"));
        assert_eq!(clean.fields().description.as_deref(), Some("new"));
        assert!(!clean.is_dirty());

        let mut dirty = ProjectDetailState::from_project(&project("old"));
        dirty.name = "Renamed".into();
        dirty.refresh_from(&project("new"));
        assert_eq!(dirty.fields().description.as_deref(), Some("old"));
        assert_eq!(dirty.name, "Renamed");
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn minimum_window_stacks_the_cards() {
        assert_eq!(card_layout(page_width(800.0, false)), CardLayout::Stacked);
        assert_eq!(card_layout(page_width(800.0, true)), CardLayout::Stacked);
    }

    #[test]
    fn a_roomy_window_puts_them_side_by_side_until_properties_open() {
        assert_eq!(card_layout(page_width(1100.0, false)), CardLayout::SideBySide);
        assert_eq!(card_layout(page_width(1100.0, true)), CardLayout::Stacked);
        assert_eq!(card_layout(page_width(1440.0, true)), CardLayout::SideBySide);
    }

    #[test]
    fn the_breakpoint_is_inclusive() {
        assert_eq!(card_layout(SIDE_BY_SIDE_MIN_WIDTH), CardLayout::SideBySide);
        assert_eq!(card_layout(SIDE_BY_SIDE_MIN_WIDTH - 0.5), CardLayout::Stacked);
    }

    #[test]
    fn page_width_subtracts_sidebars_and_padding() {
        assert_eq!(page_width(1000.0, false), 1000.0 - 220.0 - 64.0);
        assert_eq!(page_width(1000.0, true), 1000.0 - 220.0 - 321.0 - 64.0);
    }
}

/// Project discussion is unfinished, so the page doesn't show it yet. Flip this
/// to bring it back; the section and its messages are kept intact.
const SHOW_PROJECT_DISCUSSION: bool = false;

/// The project's discussion: threaded comments with indented replies, rendered
/// by the same rows and composer as issue detail. No delete on project comments
/// (the web has none).
#[allow(clippy::too_many_arguments)]
fn discussion_section<'a>(
    comments: &'a [Comment],
    detail: &'a ProjectDetailState,
    members: &'a [Member],
    comment_authors: &'a [Member],
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
        authors: comment_authors,
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
    util::with_heading("Discussion", col)
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
    .style(card_style)
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

/// This project's issues (already scoped and filtered by the caller with the
/// board's display options, and ordered by `sort`), each a clickable row that
/// opens detail. The heading carries Rails' "Sort:" chips.
fn issues_section<'a>(
    scoped: Vec<&'a Issue>,
    display: &'a DisplayOptions,
    sort: ProjectIssueSort,
    team: Option<&'a Team>,
    lanes: &'a [Lane],
    terminal_showing: bool,
) -> Element<'a, Message> {
    let heading = row![]
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push(
            text(format!("Issues ({})", scoped.len()))
                .size(type_scale::SM)
                .font(crate::fonts::bold_active())
                .color(t::text_secondary()),
        )
        .push(iced::widget::horizontal_space())
        .push(sort_chips(sort));
    let mut col = column![].spacing(space::S1);

    if scoped.is_empty() {
        col = col.push(text("No issues to show").size(type_scale::SM).color(t::text_muted()));
    } else {
        for issue in scoped {
            col = col.push(issue_row(issue, display, team, lanes, terminal_showing));
        }
    }

    container(column![].spacing(space::S4).push(heading).push(col))
        .width(Length::Fill)
        .padding(space::S4)
        .style(card_style)
        .into()
}

/// "Sort: Newest ID Status Updated Priority", the active one boxed (as on the
/// Projects table's sort row).
fn sort_chips<'a>(active_sort: ProjectIssueSort) -> Element<'a, Message> {
    let mut chips = row![]
        .spacing(space::S1)
        .align_y(Alignment::Center)
        .push(text("Sort:").size(type_scale::XS).color(t::text_muted()));
    for option in ProjectIssueSort::ALL {
        let active = option == active_sort;
        chips = chips.push(
            button(text(option.label()).size(type_scale::XS).color(if active {
                t::text_strong()
            } else {
                t::text_secondary()
            }))
            .on_press(Message::ProjectIssueSortChanged(option))
            .padding([space::S0_5, space::S2])
            .style(move |_theme: &Theme, _status| button::Style {
                background: active.then_some(iced::Background::Color(t::bg_card_focused())),
                text_color: t::text_primary(),
                border: Border {
                    color: if active { t::border() } else { iced::Color::TRANSPARENT },
                    width: 1.0,
                    radius: radius::SM.into(),
                },
                ..Default::default()
            }),
        );
    }
    chips.into()
}

fn issue_row<'a>(
    issue: &'a Issue,
    d: &DisplayOptions,
    team: Option<&'a Team>,
    lanes: &'a [Lane],
    terminal_showing: bool,
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
    // One line, cut at the card edge: the title never pushes the facts out.
    content = content.push(
        container(
            text(&issue.title)
                .size(type_scale::SM)
                .color(t::text_primary())
                .wrapping(iced::widget::text::Wrapping::None),
        )
        .width(Length::Fill)
        .clip(true),
    );
    // Right-hand facts, only the enabled ones.
    let mut facts: Vec<String> = Vec::new();
    // A project's list is about progress: status always shows, assignee never.
    if let Some(l) = lanes.iter().find(|l| l.id == issue.lane_id) {
        facts.push(l.name.clone());
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
    // Inside the row's button, so it takes the click before the row does.
    if terminal_showing {
        content = content.push(crate::views::issue_detail::claude_prompt_button(&issue.id));
    }

    button(content)
        .on_press(Message::OpenIssueDetail(issue.id.clone()))
        .padding([space::S2, space::S3])
        .width(Length::Fill)
        .style(|_theme: &Theme, status| button::Style {
            // Rows sit inside the Issues card, so no border of their own.
            background: (status == button::Status::Hovered)
                .then(|| iced::Background::Color(t::bg_primary())),
            text_color: t::text_primary(),
            border: Border {
                radius: radius::SM.into(),
                ..Default::default()
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
