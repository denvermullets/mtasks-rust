use iced::widget::{column, container, pick_list, row, text_editor, Space};
use crate::views::text;
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Issue, Label, Lane, Priority};
use crate::theme::{self as t, radius, space, type_scale};
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
    /// Project pre-filled for a new sub-issue (inherited from its parent).
    pub project_id: Option<String>,
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
            error: None,
            suggest: None,
            labels_open: false,
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
    let properties = row![]
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push(lane)
        .push(priority)
        .push(labels_chip);

    let label_menu = form.labels_open.then(|| {
        form_page::label_menu(
            labels,
            |l| form.selected_label_ids.contains(&l.id),
            |l| Message::IssueLabelToggled(l.id.clone()),
        )
    });

    let description = text_editor(&form.description)
        .placeholder("Add description\u{2026}  (@ to mention, # to link an issue)")
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

    let actions = row![]
        .spacing(space::S4)
        .align_y(Alignment::Center)
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
        .push(Space::with_height(space::S8))
        .push(form_page::actions_bar(None, actions));

    form_page::centered(content, form_page::WIDTH_4XL)
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
