use iced::widget::{button, checkbox, column, container, pick_list, row, text_editor, text_input, Space};
use crate::views::text;
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Issue, Label, Lane, Member, Priority, Project};
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::projects::MemberOption;
use crate::views::{form_page, lane_icon, util};

pub struct IssueFormState {
    pub title: String,
    pub description: text_editor::Content,
    pub priority: Priority,
    pub lane_id: String,
    pub editing_issue_id: Option<String>,
    /// Local UUIDs of labels attached to this issue, toggled via the chip grid.
    pub selected_label_ids: Vec<String>,
    /// Local UUID of the parent issue when creating a sub-issue. Ignored on
    /// edit (the existing issue's parent is preserved as-is).
    pub parent_issue_id: Option<String>,
    /// The issue's project; a new sub-issue inherits its parent's.
    pub project_id: Option<String>,
    /// Local member UUID; `None` = no assignee.
    pub assignee_id: Option<String>,
    /// Points; `None` = no estimate.
    pub estimate: Option<f64>,
    /// `YYYY-MM-DD`, or empty for none. Checked on Save.
    pub due_date: String,
    /// Files picked or dropped on the page. They are queued as Attachments
    /// on Save, once the issue row exists to own them.
    pub files: Vec<std::path::PathBuf>,
    /// "Create more" (new issue only): after a create, open a blank form for
    /// the next one instead of closing the page.
    pub create_more: bool,
    /// Inline validation message shown under the title (client-side on Save, or
    /// a server 422 routed back after a failed background push). `None` = valid.
    pub error: Option<String>,
    /// Open `@`/`#` autocomplete list under the description.
    pub suggest: Option<crate::mentions::SuggestState>,
    /// The Labels dropdown under the property chips is open.
    pub labels_open: bool,
}

impl IssueFormState {
    pub fn new(default_lane_id: String) -> Self {
        Self {
            title: String::new(),
            description: text_editor::Content::new(),
            priority: Priority::None,
            lane_id: default_lane_id,
            editing_issue_id: None,
            selected_label_ids: Vec::new(),
            parent_issue_id: None,
            project_id: None,
            assignee_id: None,
            estimate: None,
            due_date: String::new(),
            files: Vec::new(),
            create_more: false,
            error: None,
            suggest: None,
            labels_open: false,
        }
    }

    /// The blank form "Create more" opens after a create: same parent (and
    /// its project, as web's redirect keeps `parent_issue_id`), box still ticked.
    pub fn next_after_create(&self, default_lane_id: String) -> Self {
        Self {
            parent_issue_id: self.parent_issue_id.clone(),
            project_id: self.parent_issue_id.as_ref().and(self.project_id.clone()),
            create_more: true,
            ..Self::new(default_lane_id)
        }
    }

    /// Blank create form for a sub-issue of `parent`: links the parent and
    /// inherits its project so the child shows up alongside it.
    pub fn new_sub_issue(default_lane_id: String, parent: &Issue) -> Self {
        Self {
            parent_issue_id: Some(parent.id.clone()),
            project_id: parent.project_id.clone(),
            ..Self::new(default_lane_id)
        }
    }

    /// Build an edit form pre-filled from an existing issue and its attached
    /// label UUIDs. Shared by the edit shortcut and the 422-reopen path.
    pub fn from_issue(issue: &Issue, label_ids: Vec<String>) -> Self {
        let body = issue.description.clone().unwrap_or_default();
        Self {
            title: issue.title.clone(),
            description: text_editor::Content::with_text(&body),
            priority: Priority::from_i32(issue.priority),
            lane_id: issue.lane_id.clone(),
            editing_issue_id: Some(issue.id.clone()),
            selected_label_ids: label_ids,
            parent_issue_id: issue.parent_issue_id.clone(),
            project_id: issue.project_id.clone(),
            assignee_id: issue.assignee_id.clone(),
            estimate: issue.estimate,
            due_date: issue.due_date.clone().unwrap_or_default(),
            files: Vec::new(),
            create_more: false,
            error: None,
            suggest: None,
            labels_open: false,
        }
    }

    /// Toggle a label's presence in the selected set.
    pub fn toggle_label(&mut self, label_id: &str) {
        if let Some(pos) = self.selected_label_ids.iter().position(|id| id == label_id) {
            self.selected_label_ids.remove(pos);
        } else {
            self.selected_label_ids.push(label_id.to_string());
        }
    }

    pub fn description_text(&self) -> String {
        self.description.text()
    }

    /// The due date to save: `Ok(None)` when blank, else a real
    /// `YYYY-MM-DD` date or the inline error.
    pub fn parsed_due_date(&self) -> Result<Option<String>, String> {
        let due = self.due_date.trim();
        if due.is_empty() {
            return Ok(None);
        }
        chrono::NaiveDate::parse_from_str(due, "%Y-%m-%d")
            .map(|d| Some(d.format("%Y-%m-%d").to_string()))
            .map_err(|_| "Due date must be a date like 2026-12-31".to_string())
    }

    /// Add picked files, skipping ones already on the form.
    pub fn add_files(&mut self, paths: Vec<std::path::PathBuf>) {
        for p in paths {
            if !self.files.contains(&p) {
                self.files.push(p);
            }
        }
    }
}

/// The web form's estimate choices, in points.
const ESTIMATES: [f64; 6] = [1.0, 2.0, 3.0, 5.0, 8.0, 13.0];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EstimateOption(pub Option<f64>);

impl std::fmt::Display for EstimateOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            None => write!(f, "No estimate"),
            Some(1.0) => write!(f, "1 point"),
            Some(p) => write!(f, "{p} points"),
        }
    }
}

/// "No estimate", the web's points, and an issue's off-list value (say 4 from
/// the API) so editing doesn't lose it.
fn estimate_options(current: Option<f64>) -> Vec<EstimateOption> {
    let mut opts: Vec<EstimateOption> =
        std::iter::once(None).chain(ESTIMATES.iter().copied().map(Some)).map(EstimateOption).collect();
    if let Some(v) = current.filter(|v| !ESTIMATES.contains(v)) {
        opts.push(EstimateOption(Some(v)));
    }
    opts
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectOption {
    None,
    Project { id: String, name: String },
    /// The web list's "New Project" link at the bottom.
    New,
}

impl std::fmt::Display for ProjectOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectOption::None => write!(f, "No project"),
            ProjectOption::Project { name, .. } => write!(f, "{name}"),
            ProjectOption::New => write!(f, "+ New Project"),
        }
    }
}

fn project_message(opt: ProjectOption) -> Message {
    match opt {
        ProjectOption::None => Message::IssueProjectChanged(None),
        ProjectOption::Project { id, .. } => Message::IssueProjectChanged(Some(id)),
        ProjectOption::New => Message::IssueFormNewProject,
    }
}

/// The team's members and projects the form offers, and whether "New
/// Project" can make one for this issue's team.
pub struct Choices<'a> {
    pub members: Vec<&'a Member>,
    pub projects: Vec<&'a Project>,
    pub can_create_project: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneOption {
    pub id: String,
    pub name: String,
}

impl std::fmt::Display for LaneOption {
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

/// The description's height until it outgrows it.
const DESCRIPTION_MIN_HEIGHT: f32 = 110.0;

/// What the page's context row (above the title) names: the team or issue
/// chip, then "New issue" / "New sub-issue of …" / "Edit issue".
pub struct PageContext {
    /// Team identifier on create, the issue's identifier on edit.
    pub chip: String,
    /// Parent's identifier and title when creating a sub-issue.
    pub parent: Option<(String, String)>,
}

/// The full-page issue form (web `issues/new` and `issues/edit`): context
/// row, big title, description, a row of property chips, then the actions.
pub fn page<'a>(
    form: &'a IssueFormState,
    lanes: &'a [Lane],
    labels: &'a [Label],
    choices: Choices<'a>,
    context: PageContext,
) -> Element<'a, Message> {
    let editing = form.editing_issue_id.is_some();

    let context_row = row![]
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push(context_chip(context.chip))
        .push(text("\u{203a}").size(type_scale::SM).color(t::text_muted()))
        .push(match (&context.parent, editing) {
            (_, true) => Element::from(text("Edit issue").size(type_scale::SM).color(t::text_secondary())),
            (Some((id, title)), false) => row![]
                .spacing(space::S2)
                .align_y(Alignment::Center)
                .push(text("New sub-issue of").size(type_scale::SM).color(t::text_secondary()))
                .push(context_chip(format!("{id}  {title}")))
                .into(),
            (None, false) => text("New issue").size(type_scale::SM).color(t::text_secondary()).into(),
        });

    let lane_options: Vec<LaneOption> = lanes
        .iter()
        .map(|l| LaneOption {
            id: l.id.clone(),
            name: l.name.clone(),
        })
        .collect();
    let selected_lane = lane_options.iter().find(|l| l.id == form.lane_id).cloned();
    let selected_icon = lanes
        .iter()
        .find(|l| l.id == form.lane_id)
        .map(|l| crate::lane_icon::icon_for(l.category.as_deref()));

    let lane = row![]
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push_maybe(selected_icon.map(lane_icon::view))
        .push(
            pick_list(lane_options, selected_lane, |l: LaneOption| Message::IssueLaneChanged(l.id))
                .text_size(type_scale::SM)
                .padding(util::SIDEBAR_CONTROL_PADDING)
                .style(util::sidebar_pick_list_style)
                .menu_style(util::sidebar_menu_style),
        );
    let priority = pick_list(PRIORITIES.as_slice(), Some(form.priority), Message::IssuePriorityChanged)
        .text_size(type_scale::SM)
        .padding(util::SIDEBAR_CONTROL_PADDING)
        .style(util::sidebar_pick_list_style)
        .menu_style(util::sidebar_menu_style);
    let labels_chip = form_page::chip_button(
        text(form_page::labels_summary(labels, &form.selected_label_ids)).size(type_scale::SM),
        Message::IssueFormLabelsToggled,
        form.labels_open,
    );
    let assignee_options: Vec<MemberOption> = std::iter::once(MemberOption {
        id: None,
        name: "No assignee".to_string(),
    })
    .chain(choices.members.iter().map(|m| MemberOption {
        id: Some(m.id.clone()),
        name: m.name.clone(),
    }))
    .collect();
    let selected_assignee = assignee_options.iter().find(|o| o.id == form.assignee_id).cloned();
    let assignee = pick_list(assignee_options, selected_assignee, |o: MemberOption| {
        Message::IssueAssigneeChanged(o.id)
    })
    .placeholder("No assignee")
    .text_size(type_scale::SM)
    .padding(util::SIDEBAR_CONTROL_PADDING)
    .style(util::sidebar_pick_list_style)
    .menu_style(util::sidebar_menu_style);

    let mut project_options: Vec<ProjectOption> = std::iter::once(ProjectOption::None)
        .chain(choices.projects.iter().map(|p| ProjectOption::Project {
            id: p.id.clone(),
            name: p.name.clone(),
        }))
        .collect();
    if choices.can_create_project {
        project_options.push(ProjectOption::New);
    }
    let selected_project = project_options
        .iter()
        .find(|o| match (o, &form.project_id) {
            (ProjectOption::None, None) => true,
            (ProjectOption::Project { id, .. }, Some(sel)) => id == sel,
            _ => false,
        })
        .cloned();
    let project = pick_list(project_options, selected_project, project_message)
        .placeholder("No project")
        .text_size(type_scale::SM)
        .padding(util::SIDEBAR_CONTROL_PADDING)
        .style(util::sidebar_pick_list_style)
        .menu_style(util::sidebar_menu_style);

    let estimate = pick_list(
        estimate_options(form.estimate),
        Some(EstimateOption(form.estimate)),
        |o: EstimateOption| Message::IssueEstimateChanged(o.0),
    )
    .text_size(type_scale::SM)
    .padding(util::SIDEBAR_CONTROL_PADDING)
    .style(util::sidebar_pick_list_style)
    .menu_style(util::sidebar_menu_style);

    let due_date = text_input("Due date (YYYY-MM-DD)", &form.due_date)
        .on_input(Message::IssueDueDateChanged)
        .on_submit(Message::SaveIssue)
        .width(Length::Fixed(170.0))
        .padding(util::SIDEBAR_CONTROL_PADDING)
        .size(type_scale::SM)
        .style(util::sidebar_input_style);

    // Web order: lane, priority, assignee, project, estimate, due date, labels.
    // Wraps to a second line on a narrow window.
    let properties = row![]
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push(lane)
        .push(priority)
        .push(assignee)
        .push(project)
        .push(estimate)
        .push(due_date)
        .push(labels_chip)
        .wrap();

    let file_list = (!form.files.is_empty()).then(|| {
        form.files
            .iter()
            .enumerate()
            .fold(column![].spacing(space::S2), |col, (i, path)| col.push(file_row(i, path)))
    });

    let label_menu = form.labels_open.then(|| {
        form_page::label_menu(
            labels,
            |l| form.selected_label_ids.contains(&l.id),
            |l| Message::IssueLabelToggled(l.id.clone()),
        )
    });

    let description = text_editor(&form.description)
        .placeholder("Add description (drop files here)\u{2026}  @ to mention, # to link an issue")
        .on_action(Message::IssueDescriptionChanged)
        .key_binding(crate::views::issue_detail::editor_binding(None, form.suggest.is_some()))
        .padding(0)
        .size(type_scale::SM)
        .style(form_page::borderless_editor)
        // Grows with the text like the web's auto-resize textarea, from a
        // ~5-line floor (iced 0.13's editor has no min height).
        .height(if form.description.line_count() > 5 {
            Length::Shrink
        } else {
            Length::Fixed(DESCRIPTION_MIN_HEIGHT)
        });

    let create_more = (!editing).then(|| {
        checkbox("Create more", form.create_more)
            .on_toggle(Message::IssueCreateMoreToggled)
            .size(14)
            .text_size(type_scale::SM)
    });
    let actions = row![]
        .spacing(space::S4)
        .align_y(Alignment::Center)
        .push_maybe(create_more)
        .push(form_page::cancel_button())
        .push(form_page::submit_button(
            if editing { "Update issue" } else { "Create issue" },
            Message::SaveIssue,
        ));

    let content = column![]
        .push(context_row)
        .push(Space::with_height(space::S6))
        .push(form_page::title_input("Issue title", &form.title, Message::IssueTitleChanged))
        .push_maybe(form.error.as_deref().map(|e| {
            column![Space::with_height(space::S2), form_page::error_text(e)]
        }))
        .push(Space::with_height(space::S6))
        .push(description)
        .push_maybe(
            form.suggest
                .as_ref()
                .map(crate::views::issue_detail::suggestion_rows),
        )
        .push(Space::with_height(space::S6))
        .push(properties)
        .push_maybe(label_menu.map(|m| column![Space::with_height(space::S1), m]))
        .push_maybe(file_list.map(|l| column![Space::with_height(space::S6), l]))
        .push(Space::with_height(space::S8))
        .push(form_page::actions_bar(Some(attach_button()), actions));

    form_page::centered(content, form_page::WIDTH_4XL)
}

/// "Attach files" at the left of the actions bar (web paperclip link).
fn attach_button<'a>() -> Element<'a, Message> {
    button(text("\u{1f4ce}  Attach files").size(type_scale::SM))
        .on_press(Message::PickIssueFormFiles)
        .padding([space::S2, 0.0])
        .style(|_theme: &Theme, status| button::Style {
            background: None,
            text_color: if matches!(status, button::Status::Hovered) {
                t::text_secondary()
            } else {
                t::text_muted()
            },
            ..Default::default()
        })
        .into()
}

/// One staged file: its name and a Remove link.
fn file_row<'a>(index: usize, path: &'a std::path::Path) -> Element<'a, Message> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let remove = button(text("Remove").size(type_scale::XS))
        .on_press(Message::RemoveIssueFormFile(index))
        .padding(0)
        .style(|_theme: &Theme, status| button::Style {
            background: None,
            text_color: if matches!(status, button::Status::Hovered) {
                t::danger()
            } else {
                t::text_muted()
            },
            ..Default::default()
        });
    container(
        row![]
            .spacing(space::S3)
            .align_y(Alignment::Center)
            .push(text(name).size(type_scale::SM).color(t::text_body()).width(Length::Fill))
            .push(remove),
    )
    .padding(util::SIDEBAR_CONTROL_PADDING)
    .width(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_secondary())),
        border: Border { color: t::border(), width: 1.0, radius: radius::MD.into() },
        ..Default::default()
    })
    .into()
}

/// A read-only chip (web `px-3 py-1.5 rounded-md bg-foreground border`).
fn context_chip<'a>(label: String) -> Element<'a, Message> {
    container(text(label).size(type_scale::SM).color(t::text_body()))
        .padding(util::SIDEBAR_CONTROL_PADDING)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_labels_match_the_web() {
        let labels: Vec<String> = estimate_options(None).iter().map(|o| o.to_string()).collect();
        assert_eq!(
            labels,
            ["No estimate", "1 point", "2 points", "3 points", "5 points", "8 points", "13 points"]
        );
    }

    #[test]
    fn an_off_list_estimate_stays_pickable() {
        let opts = estimate_options(Some(4.0));
        assert_eq!(opts.last(), Some(&EstimateOption(Some(4.0))));
        assert_eq!(estimate_options(Some(5.0)).len(), 7);
    }

    #[test]
    fn due_date_blank_is_none_and_garbage_is_an_error() {
        let mut f = IssueFormState::new("l".into());
        assert_eq!(f.parsed_due_date(), Ok(None));
        f.due_date = " 2026-12-31 ".into();
        assert_eq!(f.parsed_due_date(), Ok(Some("2026-12-31".into())));
        f.due_date = "2026-02-30".into();
        assert!(f.parsed_due_date().is_err());
        f.due_date = "next week".into();
        assert!(f.parsed_due_date().is_err());
    }

    #[test]
    fn create_more_keeps_the_parent_and_its_project_but_clears_the_rest() {
        let mut f = IssueFormState::new("l".into());
        f.title = "one".into();
        f.assignee_id = Some("m".into());
        f.estimate = Some(3.0);
        f.files.push("a.txt".into());
        f.create_more = true;
        f.project_id = Some("p".into());
        let next = f.next_after_create("l0".into());
        assert!(next.title.is_empty() && next.assignee_id.is_none() && next.estimate.is_none());
        assert!(next.files.is_empty() && next.create_more);
        assert_eq!((next.lane_id.as_str(), next.project_id), ("l0", None));

        f.parent_issue_id = Some("parent".into());
        let sub = f.next_after_create("l0".into());
        assert_eq!(sub.parent_issue_id.as_deref(), Some("parent"));
        assert_eq!(sub.project_id.as_deref(), Some("p"));
    }

    #[test]
    fn adding_the_same_file_twice_keeps_one() {
        let mut f = IssueFormState::new("l".into());
        f.add_files(vec!["a.txt".into(), "b.txt".into()]);
        f.add_files(vec!["a.txt".into()]);
        assert_eq!(f.files.len(), 2);
    }
}
