use iced::widget::{button, column, container, pick_list, row, text, text_input};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Lane, Priority};
use crate::theme as t;

#[derive(Debug, Clone)]
pub struct IssueFormState {
    pub title: String,
    pub description: String,
    pub priority: Priority,
    pub lane_id: String,
    pub editing_issue_id: Option<String>,
}

impl IssueFormState {
    pub fn new(default_lane_id: String) -> Self {
        Self {
            title: String::new(),
            description: String::new(),
            priority: Priority::None,
            lane_id: default_lane_id,
            editing_issue_id: None,
        }
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

pub fn view<'a>(form: &'a IssueFormState, lanes: &'a [Lane]) -> Element<'a, Message> {
    let title = if form.editing_issue_id.is_some() {
        "Edit Issue"
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
        .spacing(12)
        .width(Length::Fixed(400.0))
        .push(text(title).size(18).color(t::TEXT_PRIMARY))
        .push(
            text_input("Title", &form.title)
                .on_input(Message::IssueTitleChanged)
                .padding(8)
                .size(14),
        )
        .push(
            text_input("Description", &form.description)
                .on_input(Message::IssueDescriptionChanged)
                .padding(8)
                .size(14),
        )
        .push(
            row![]
                .spacing(12)
                .push(
                    column![]
                        .spacing(4)
                        .push(text("Priority").size(12).color(t::TEXT_SECONDARY))
                        .push(pick_list(
                            PRIORITIES.as_slice(),
                            Some(form.priority),
                            |p| Message::IssuePriorityChanged(p),
                        )),
                )
                .push(
                    column![]
                        .spacing(4)
                        .push(text("Lane").size(12).color(t::TEXT_SECONDARY))
                        .push(pick_list(lane_options, selected_lane, |l: LaneOption| {
                            Message::IssueLaneChanged(l.id)
                        })),
                ),
        )
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text("Save").size(14))
                        .on_press(Message::SaveIssue)
                        .padding([8, 20])
                        .style(button::primary),
                )
                .push(
                    button(text("Cancel").size(14))
                        .on_press(Message::CloseModal)
                        .padding([8, 20])
                        .style(button::secondary),
                ),
        );

    let modal = container(form_content)
        .padding(24)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_MODAL)),
            border: Border {
                color: t::BORDER,
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
                0.0, 0.0, 0.0, 0.6,
            ))),
            ..Default::default()
        })
        .into()
}
