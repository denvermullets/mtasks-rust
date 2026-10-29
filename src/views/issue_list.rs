//! List view mode: the same `display::Column`s as the board, rendered as one
//! vertical list. Each column becomes a section header; sub-groups become
//! nested headings. Rows honor the visible-property toggles.
//!
//! Focus uses the board's `(column, flat issue index)` pair, so
//! `App::focused_issue()` and `Message::CardPressed` work unchanged. Keyboard:
//! j/k walk rows across sections, h/l jump between sections (see
//! `display::list_step` / `display::list_jump_section`).

use iced::widget::{column, container, mouse_area, row, scrollable, text, Column, Row};
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::Issue;
use crate::display::{CardProperty, Column as BoardColumn};
use crate::theme as t;
use crate::views::board::CardData;
use crate::views::labels as labels_view;

pub fn view<'a>(
    columns: &[BoardColumn<'a>],
    data: CardData<'a>,
    focused_lane: usize,
    focused_issue: usize,
) -> Element<'a, Message> {
    if columns.is_empty() {
        let msg = if data.lanes.is_empty() {
            "No lanes — select a team"
        } else {
            "No sections to show"
        };
        return container(text(msg).size(14).color(t::TEXT_MUTED))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    }

    let mut list: Column<'a, Message> = column![].spacing(2).width(Length::Fill).padding([4, 12]);

    for (col_idx, col) in columns.iter().enumerate() {
        list = list.push(
            container(
                row![]
                    .spacing(8)
                    .push(text(col.title.clone()).size(14).color(t::TEXT_PRIMARY))
                    .push(
                        text(format!("{}", col.issues.len()))
                            .size(13)
                            .color(t::TEXT_MUTED),
                    ),
            )
            .padding([10, 4])
            .width(Length::Fill)
            .style(|_theme: &Theme| container::Style {
                border: Border {
                    color: t::BORDER,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            }),
        );

        for (issue_idx, issue) in col.issues.iter().copied().enumerate() {
            if let Some(section) = col.sections.iter().find(|s| s.start == issue_idx) {
                list = list.push(
                    row![]
                        .spacing(6)
                        .padding([4, 16])
                        .push(
                            text(section.title.clone())
                                .size(12)
                                .color(t::TEXT_SECONDARY),
                        )
                        .push(
                            text(format!("{}", section.end - section.start))
                                .size(12)
                                .color(t::TEXT_MUTED),
                        ),
                );
            }
            let focused = col_idx == focused_lane && issue_idx == focused_issue;
            list = list.push(issue_row(issue, &data, focused, col_idx, issue_idx));
        }
    }

    container(scrollable(list).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_PRIMARY)),
            ..Default::default()
        })
        .into()
}

fn issue_row<'a>(
    issue: &'a Issue,
    data: &CardData<'a>,
    focused: bool,
    col_idx: usize,
    issue_idx: usize,
) -> Element<'a, Message> {
    let d = data.display;
    let mut r: Row<'a, Message> = row![].spacing(12).align_y(Alignment::Center);

    if d.shows(CardProperty::Priority) {
        r = r.push(
            text("△")
                .size(12)
                .color(t::priority_color(issue.priority_enum())),
        );
    }
    if d.shows(CardProperty::Id) {
        let tag = issue
            .team_number
            .map(|n| format!("{}-{}", data.team_identifier, n))
            .unwrap_or_default();
        r = r.push(container(text(tag).size(12).color(t::TEXT_MUTED)).width(Length::Fixed(70.0)));
    }
    if d.shows(CardProperty::Status) {
        let name = data
            .lanes
            .iter()
            .find(|l| l.id == issue.lane_id)
            .map(|l| l.name.clone())
            .unwrap_or_default();
        r = r.push(
            container(text(name).size(12).color(t::TEXT_SECONDARY)).width(Length::Fixed(90.0)),
        );
    }
    r = r.push(container(text(&issue.title).size(14).color(t::TEXT_PRIMARY)).width(Length::Fill));

    if d.shows(CardProperty::Labels) {
        if let Some(labels) = data.issue_labels.get(&issue.id) {
            let mut chips: Row<'a, Message> = row![].spacing(4);
            for label in labels {
                chips = chips.push(labels_view::static_chip(label));
            }
            r = r.push(chips);
        }
    }
    if d.shows(CardProperty::Project) {
        if let Some(p) = issue
            .project_id
            .as_ref()
            .and_then(|id| data.projects.iter().find(|p| p.id == *id))
        {
            r = r.push(text(p.name.clone()).size(12).color(t::TEXT_MUTED));
        }
    }
    if d.shows(CardProperty::Assignee) {
        let name = issue
            .assignee_id
            .as_ref()
            .and_then(|id| data.members.iter().find(|m| m.id == *id))
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "Unassigned".to_string());
        r = r.push(text(name).size(12).color(t::TEXT_MUTED));
    }
    if d.shows(CardProperty::DueDate) {
        if let Some(due) = issue.due_date.as_deref() {
            r = r.push(
                text(due.get(..10).unwrap_or(due).to_string())
                    .size(12)
                    .color(t::TEXT_MUTED),
            );
        }
    }

    let bg = if focused {
        t::BG_CARD_FOCUSED
    } else {
        t::BG_PRIMARY
    };
    let border_color = if focused { t::ACCENT } else { t::BORDER };

    mouse_area(
        container(r)
            .padding([7, 12])
            .width(Length::Fill)
            .style(move |_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(bg)),
                border: Border {
                    color: border_color,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }),
    )
    .on_press(Message::CardPressed(col_idx, issue_idx))
    .into()
}
