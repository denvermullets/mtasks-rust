use iced::widget::{button, column, container, pick_list, row, text, text_editor, text_input};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Issue, Label, Lane, Priority};
use crate::theme as t;
use crate::views::labels as labels_view;

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

pub fn view<'a>(
    form: &'a IssueFormState,
    lanes: &'a [Lane],
    labels: &'a [Label],
) -> Element<'a, Message> {
    let title = if form.editing_issue_id.is_some() {
        "Edit Issue"
    } else if form.parent_issue_id.is_some() {
        "New Sub-issue"
    } else {
        "New Issue"
    };

    let lane_options: Vec<LaneOption> = lanes
        .iter()
        .map(|l| LaneOption {
            id: l.id.clone(),
            name: l.name.clone(),
        })
        .collect();
    let selected_lane = lane_options.iter().find(|l| l.id == form.lane_id).cloned();

    let form_content = column![]
        .spacing(14)
        .width(Length::Fixed(420.0))
        .push(text(title).size(18).color(t::TEXT_PRIMARY()))
        .push(
            text_input("Title", &form.title)
                .on_input(Message::IssueTitleChanged)
                .padding(10)
                .size(14),
        )
        .push_maybe(
            form.error
                .as_deref()
                .map(|e| text(e).size(12).color(t::DANGER())),
        )
        .push(
            text_editor(&form.description)
                .placeholder("Description  (@ to mention, # to link an issue)")
                .on_action(Message::IssueDescriptionChanged)
                .key_binding(crate::views::issue_detail::editor_binding(
                    None,
                    form.suggest.is_some(),
                ))
                .padding(10)
                .size(14)
                .height(Length::Fixed(160.0)),
        )
        .push_maybe(
            form.suggest
                .as_ref()
                .map(crate::views::issue_detail::suggestion_rows),
        )
        .push(
            row![]
                .spacing(12)
                .push(
                    column![]
                        .spacing(4)
                        .width(Length::Fill)
                        .push(text("Priority").size(12).color(t::TEXT_MUTED()))
                        .push(pick_list(
                            PRIORITIES.as_slice(),
                            Some(form.priority),
                            Message::IssuePriorityChanged,
                        )),
                )
                .push(
                    column![]
                        .spacing(4)
                        .width(Length::Fill)
                        .push(text("Lane").size(12).color(t::TEXT_MUTED()))
                        .push(pick_list(lane_options, selected_lane, |l: LaneOption| {
                            Message::IssueLaneChanged(l.id)
                        })),
                ),
        )
        .push(labels_section(form, labels))
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text("Save").size(14).color(t::BG_PRIMARY()))
                        .on_press(Message::SaveIssue)
                        .padding([8, 24])
                        .style(move |_theme: &Theme, _status| button::Style {
                            background: Some(iced::Background::Color(t::ACCENT())),
                            text_color: t::BG_PRIMARY(),
                            border: Border {
                                radius: 4.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                )
                .push(
                    button(text("Cancel").size(14))
                        .on_press(Message::CloseModal)
                        .padding([8, 24])
                        .style(button::secondary),
                ),
        );

    let modal = container(form_content)
        .padding(28)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_MODAL())),
            border: Border {
                color: t::BORDER(),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        });

    container(modal)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.0, 0.0, 0.0, 0.7,
            ))),
            ..Default::default()
        })
        .into()
}

fn labels_section<'a>(form: &'a IssueFormState, labels: &'a [Label]) -> Element<'a, Message> {
    let content: Element<'a, Message> = if labels.is_empty() {
        text("No labels yet — press L on a card to create one.")
            .size(12)
            .color(t::TEXT_MUTED())
            .into()
    } else {
        labels_view::chip_grid(
            labels,
            |l| form.selected_label_ids.contains(&l.id),
            |l| Message::IssueLabelToggled(l.id.clone()),
            4,
        )
    };

    column![]
        .spacing(6)
        .push(text("Labels").size(12).color(t::TEXT_MUTED()))
        .push(content)
        .into()
}
