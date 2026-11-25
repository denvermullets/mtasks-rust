use iced::widget::{button, checkbox, column, container, row, Space};
use crate::views::text;
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Issue, Label, Lane, Member, Priority, Project};
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::dropdown::dropdown;
use crate::views::{date_picker, form_page, util};

pub struct IssueFormState {
    pub title: String,
    pub description: crate::undo_editor::UndoableContent,
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
    /// The property chip whose dropdown is open.
    pub open_menu: Option<FormMenu>,
}

/// The issue form's property chips that open a dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormMenu {
    Lane,
    Priority,
    Assignee,
    Project,
    Estimate,
    Labels,
}

impl IssueFormState {
    pub fn new(default_lane_id: String) -> Self {
        Self {
            title: String::new(),
            description: crate::undo_editor::UndoableContent::new(),
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
            open_menu: None,
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
            description: crate::undo_editor::UndoableContent::with_text(&body),
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
            open_menu: None,
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

/// Web order: "No priority" first.
const PRIORITIES: [Priority; 5] = [
    Priority::None,
    Priority::Urgent,
    Priority::High,
    Priority::Medium,
    Priority::Low,
];

// Heroicons, as in the web's `issues/new` and `issues/edit`.
const ICON_LANE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 6h16M4 10h16M4 14h16M4 18h16"/></svg>"##;
const ICON_PRIORITY: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M3 6h18M3 12h18M3 18h18"/></svg>"##;
const ICON_USER: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M16 7a4 4 0 11-8 0 4 4 0 018 0zM12 14a7 7 0 00-7 7h14a7 7 0 00-7-7z"/></svg>"##;
const ICON_PROJECT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M20 7l-8-4-8 4m16 0l-8 4m8-4v10l-8 4m0-10L4 7m8 4v10M4 7v10l8 4"/></svg>"##;
const ICON_ESTIMATE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M13 10V3L4 14h7v7l9-11h-7z"/></svg>"##;
const ICON_TAG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M7 7h.01M7 3h5c.512 0 1.024.195 1.414.586l7 7a2 2 0 010 2.828l-7 7a2 2 0 01-2.828 0l-7-7A1.994 1.994 0 013 12V7a4 4 0 014-4z"/></svg>"##;
const ICON_CHAT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M7 8h10M7 12h4m1 8l-4-4H5a2 2 0 01-2-2V6a2 2 0 012-2h14a2 2 0 012 2v8a2 2 0 01-2 2h-3l-4 4z"/></svg>"##;
const ICON_LOCK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="#000" viewBox="0 0 20 20"><path fill-rule="evenodd" d="M5 9V7a5 5 0 0110 0v2a2 2 0 012 2v5a2 2 0 01-2 2H5a2 2 0 01-2-2v-5a2 2 0 012-2zm8-2v2H7V7a3 3 0 016 0z" clip-rule="evenodd"/></svg>"##;
const ICON_PAPERCLIP: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M15.172 7l-6.586 6.586a2 2 0 102.828 2.828l6.414-6.586a4 4 0 00-5.656-5.656l-6.415 6.585a6 6 0 108.486 8.486L20.5 13"/></svg>"##;

/// Web `min-w-[200px]` option menus.
const MENU_WIDTH: f32 = 200.0;
/// The label picker's width.
const LABEL_MENU_WIDTH: f32 = 320.0;

/// The description's height until it outgrows it.
const DESCRIPTION_MIN_HEIGHT: f32 = 110.0;

const TITLE_PLACEHOLDER: &str = "Issue title";

/// The title field's `Id`: a new issue's form opens with it focused.
pub fn title_input_id() -> iced::widget::Id {
    form_page::title_input_id(TITLE_PLACEHOLDER)
}

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
    date_picker_state: Option<&date_picker::DatePickerState>,
) -> Element<'a, Message> {
    let editing = form.editing_issue_id.is_some();

    let context_row = row![]
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push(context_chip(context.chip, !editing))
        .push(text("\u{203a}").size(type_scale::SM).color(t::text_muted()))
        .push(match (&context.parent, editing) {
            (_, true) => Element::from(text("Edit issue").size(type_scale::SM).color(t::text_secondary())),
            (Some((id, title)), false) => row![]
                .spacing(space::S2)
                .align_y(Alignment::Center)
                .push(text("New sub-issue of").size(type_scale::SM).color(t::text_secondary()))
                .push(parent_chip(id, title))
                .into(),
            (None, false) => text("New issue").size(type_scale::SM).color(t::text_secondary()).into(),
        });

    let open = form.open_menu;
    // A chip and, while it is the open one, its dropdown.
    let chip = |which: FormMenu, face: Element<'a, Message>, menu: &dyn Fn() -> Element<'a, Message>| {
        let is_open = open == Some(which);
        let width = if which == FormMenu::Labels { LABEL_MENU_WIDTH } else { MENU_WIDTH };
        Element::from(
            dropdown(
                form_page::chip_button(face, Message::IssueFormMenuToggled(which), is_open),
                is_open.then(menu),
                width,
                Message::IssueFormMenuClosed,
            )
            .align_left(),
        )
    };

    let lane_name = lanes
        .iter()
        .find(|l| l.id == form.lane_id)
        .map(|l| l.name.clone())
        .unwrap_or_else(|| "Backlog".to_string());
    let lane = chip(FormMenu::Lane, form_page::chip_face(ICON_LANE, lane_name), &|| {
        form_page::option_menu(
            lanes
                .iter()
                .map(|l| (l.name.clone(), l.id == form.lane_id, Message::IssueLaneChanged(l.id.clone())))
                .collect(),
        )
    });

    let priority = chip(FormMenu::Priority, form_page::chip_face(ICON_PRIORITY, form.priority.label()), &|| {
        form_page::option_menu(
            PRIORITIES
                .iter()
                .map(|p| (p.label().to_string(), *p == form.priority, Message::IssuePriorityChanged(*p)))
                .collect(),
        )
    });

    let assignee_name = choices
        .members
        .iter()
        .find(|m| Some(&m.id) == form.assignee_id.as_ref())
        .map(|m| m.name.clone())
        .unwrap_or_else(|| "No assignee".to_string());
    let members = &choices.members;
    let assignee = chip(FormMenu::Assignee, form_page::chip_face(ICON_USER, assignee_name), &|| {
        form_page::option_menu(
            std::iter::once(("No assignee".to_string(), form.assignee_id.is_none(), Message::IssueAssigneeChanged(None)))
                .chain(members.iter().map(|m| {
                    (
                        m.name.clone(),
                        form.assignee_id.as_ref() == Some(&m.id),
                        Message::IssueAssigneeChanged(Some(m.id.clone())),
                    )
                }))
                .collect(),
        )
    });

    let project_name = choices
        .projects
        .iter()
        .find(|p| Some(&p.id) == form.project_id.as_ref())
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "No project".to_string());
    let projects = &choices.projects;
    let can_create_project = choices.can_create_project;
    let project = chip(FormMenu::Project, form_page::chip_face(ICON_PROJECT, project_name), &|| {
        let mut opts: Vec<ProjectOption> = std::iter::once(ProjectOption::None)
            .chain(projects.iter().map(|p| ProjectOption::Project { id: p.id.clone(), name: p.name.clone() }))
            .collect();
        if can_create_project {
            opts.push(ProjectOption::New);
        }
        form_page::option_menu(
            opts.into_iter()
                .map(|o| {
                    let current = match (&o, &form.project_id) {
                        (ProjectOption::None, None) => true,
                        (ProjectOption::Project { id, .. }, Some(sel)) => id == sel,
                        _ => false,
                    };
                    (o.to_string(), current, project_message(o))
                })
                .collect(),
        )
    });

    let estimate = chip(
        FormMenu::Estimate,
        form_page::chip_face(ICON_ESTIMATE, EstimateOption(form.estimate).to_string()),
        &|| {
            form_page::option_menu(
                estimate_options(form.estimate)
                    .into_iter()
                    .map(|o| (o.to_string(), o.0 == form.estimate, Message::IssueEstimateChanged(o.0)))
                    .collect(),
            )
        },
    );

    // Web: a `<label>` chip around a native date field.
    let due_date = date_picker::view(
        date_picker::DateInput {
            field: date_picker::DateField::IssueDue,
            value: &form.due_date,
            placeholder: "yyyy-mm-dd",
            on_change: Message::IssueDueDateChanged,
            on_commit: None,
            look: date_picker::Look::Chip,
        },
        date_picker_state,
        chrono::Local::now().date_naive(),
    );

    let labels_chip = chip(
        FormMenu::Labels,
        form_page::chip_face(ICON_TAG, form_page::labels_summary(labels, &form.selected_label_ids)),
        &|| {
            form_page::label_menu(
                labels,
                |l| form.selected_label_ids.contains(&l.id),
                |l| Message::IssueLabelToggled(l.id.clone()),
            )
        },
    );

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

    let description = crate::undo_editor::view(
        &form.description,
        Message::IssueDescriptionChanged,
        crate::views::issue_detail::editor_binding(None, form.suggest.is_some()),
    )
    .placeholder("Add description (drop files here)...")
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
        checkbox(form.create_more).label("Create more")
            .on_toggle(Message::IssueCreateMoreToggled)
            .size(14)
            .text_size(type_scale::SM)
    });
    let actions = row![]
        .spacing(space::S4)
        .align_y(Alignment::Center)
        .push(create_more)
        // Web: Cancel on the edit page only.
        .push(editing.then(form_page::cancel_button))
        .push(form_page::submit_button(
            if editing { "Update issue" } else { "Create issue" },
            Message::SaveIssue,
        ));

    let content = column![]
        .push(context_row)
        .push(Space::new().height(space::S6))
        .push(form_page::title_input(TITLE_PLACEHOLDER, &form.title, Message::IssueTitleChanged))
        .push(form.error.as_deref().map(|e| {
            column![Space::new().height(space::S2), form_page::error_text(e)]
        }))
        .push(Space::new().height(space::S6))
        .push(description)
        .push(
            form.suggest
                .as_ref()
                .map(crate::views::issue_detail::suggestion_rows),
        )
        .push(Space::new().height(space::S6))
        .push(properties)
        .push(file_list.map(|l| column![Space::new().height(space::S6), l]))
        .push(Space::new().height(space::S8))
        .push(form_page::actions_bar(Some(attach_button()), actions));

    form_page::centered(content, form_page::WIDTH_4XL)
}

/// "Attach files" at the left of the actions bar (web paperclip link).
fn attach_button<'a>() -> Element<'a, Message> {
    button(
        row![]
            .spacing(space::S2)
            .align_y(Alignment::Center)
            .push(form_page::icon(ICON_PAPERCLIP, space::S5, t::text_muted()))
            .push(text("Attach files").size(type_scale::SM)),
    )
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

/// The read-only team/issue chip (web `px-3 py-1.5 rounded-md bg-foreground
/// border`): chat icon, identifier, and a lock on create (team is fixed).
fn context_chip<'a>(label: String, locked: bool) -> Element<'a, Message> {
    form_page::chip_frame(
        row![]
            .spacing(space::S2)
            .align_y(Alignment::Center)
            .push(form_page::icon(ICON_CHAT, space::S4, t::text_body()))
            .push(text(label).size(type_scale::SM).color(t::text_body()))
            .push(locked.then(|| form_page::icon(ICON_LOCK, space::S3, t::text_body()))),
    )
}

/// The parent issue chip on "New sub-issue of" (web `px-2 py-1`, mono
/// identifier then the title).
fn parent_chip<'a>(id: &str, title: &str) -> Element<'a, Message> {
    container(
        row![]
            .spacing(space::S1_5)
            .align_y(Alignment::Center)
            .push(text(id.to_string()).size(type_scale::XS).color(t::text_muted()))
            .push(text(title.to_string()).size(type_scale::SM).color(t::text_body())),
    )
    .padding([space::S1, space::S2])
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
