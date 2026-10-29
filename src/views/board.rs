use std::collections::HashMap;

use iced::widget::{
    column, container, horizontal_rule, mouse_area, row, scrollable, text, Column, Row,
};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Issue, Label, Lane, Member, Project};
use crate::display::{CardProperty, Column as BoardColumn, DisplayOptions};
use crate::theme as t;
use crate::views::labels as labels_view;

/// Lookup data cards need for their optional properties.
pub struct CardData<'a> {
    pub team_identifier: &'a str,
    pub lanes: &'a [Lane],
    pub members: &'a [Member],
    pub projects: &'a [Project],
    pub issue_labels: &'a HashMap<String, Vec<Label>>,
    pub display: &'a DisplayOptions,
}

/// Render the board. Columns come from `display::build_columns`, so they may be
/// lanes, priorities, assignees, etc.; `focused_lane` indexes `columns` and
/// `focused_issue` is the flat index within the column (across sub-groups).
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
            "No columns to show"
        };
        return container(text(msg).size(14).color(t::TEXT_MUTED))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    }

    // Column headers row
    let mut header_row = row![].spacing(0).width(Length::Fill);
    for (col_idx, col) in columns.iter().enumerate() {
        let name_color = if col_idx == focused_lane {
            t::TEXT_PRIMARY
        } else {
            t::TEXT_SECONDARY
        };

        let header = row![]
            .spacing(6)
            .push(text(col.title.clone()).size(14).color(name_color))
            .push(
                text(format!("{}", col.issues.len()))
                    .size(13)
                    .color(t::TEXT_MUTED),
            );

        header_row = header_row.push(container(header).padding([10, 12]).width(Length::Fill));
    }

    // Columns with cards
    let mut lane_columns = row![].spacing(1).width(Length::Fill).height(Length::Fill);

    for (col_idx, col) in columns.iter().enumerate() {
        let is_focused_lane = col_idx == focused_lane;

        let mut issue_cards: Column<'_, Message> =
            column![].spacing(6).width(Length::Fill).padding([4, 8]);

        for (issue_idx, issue) in col.issues.iter().copied().enumerate() {
            // Sub-group heading before the first card of each section.
            if let Some(section) = col.sections.iter().find(|s| s.start == issue_idx) {
                issue_cards = issue_cards.push(
                    row![]
                        .spacing(6)
                        .padding([4, 2])
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
            let is_focused = is_focused_lane && issue_idx == focused_issue;
            issue_cards =
                issue_cards.push(issue_card(issue, &data, is_focused, col_idx, issue_idx));
        }

        let lane_content = scrollable(issue_cards).height(Length::Fill);

        let lane_container = container(lane_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::BG_PRIMARY)),
                border: Border {
                    color: t::BORDER,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            });

        lane_columns = lane_columns.push(lane_container);
    }

    let board = column![]
        .push(header_row)
        .push(
            container(horizontal_rule(1)).style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::BORDER)),
                ..Default::default()
            }),
        )
        .push(lane_columns)
        .width(Length::Fill)
        .height(Length::Fill);

    container(board)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_PRIMARY)),
            ..Default::default()
        })
        .into()
}

fn issue_card<'a>(
    issue: &'a Issue,
    data: &CardData<'a>,
    focused: bool,
    lane_idx: usize,
    issue_idx: usize,
) -> Element<'a, Message> {
    let d = data.display;
    let priority_color = t::priority_color(issue.priority_enum());

    let mut card_content = column![].spacing(4);

    if d.shows(CardProperty::Id) {
        let tag = issue
            .team_number
            .map(|n| format!("{}-{}", data.team_identifier, n))
            .unwrap_or_default();
        card_content = card_content.push(text(tag).size(12).color(t::TEXT_MUTED));
    }
    card_content = card_content.push(text(&issue.title).size(14).color(t::TEXT_PRIMARY));

    let labels = data
        .issue_labels
        .get(&issue.id)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    if d.shows(CardProperty::Labels) && !labels.is_empty() {
        let mut chips: Row<'a, Message> = row![].spacing(4);
        for label in labels {
            chips = chips.push(labels_view::static_chip(label));
        }
        card_content = card_content.push(chips);
    }

    // Meta line: small muted facts, only the enabled ones.
    let mut facts: Vec<String> = Vec::new();
    if d.shows(CardProperty::Assignee) {
        let name = issue
            .assignee_id
            .as_ref()
            .and_then(|id| data.members.iter().find(|m| m.id == *id))
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "Unassigned".to_string());
        facts.push(name);
    }
    if d.shows(CardProperty::Project) {
        if let Some(p) = issue
            .project_id
            .as_ref()
            .and_then(|id| data.projects.iter().find(|p| p.id == *id))
        {
            facts.push(p.name.clone());
        }
    }
    if d.shows(CardProperty::DueDate) {
        if let Some(due) = issue.due_date.as_deref() {
            facts.push(format!("Due {}", due.get(..10).unwrap_or(due)));
        }
    }
    if d.shows(CardProperty::Status) {
        if let Some(l) = data.lanes.iter().find(|l| l.id == issue.lane_id) {
            facts.push(l.name.clone());
        }
    }

    let mut meta: Row<'a, Message> = row![].spacing(10).align_y(iced::Alignment::Center);
    if d.shows(CardProperty::Priority) {
        // Priority indicator: triangle + number like the Rails app
        meta = meta.push(
            row![]
                .spacing(4)
                .align_y(iced::Alignment::Center)
                .push(text("△").size(11).color(priority_color))
                .push(
                    text(format!("{}", issue.priority))
                        .size(11)
                        .color(t::TEXT_MUTED),
                ),
        );
    }
    let has_meta = d.shows(CardProperty::Priority) || !facts.is_empty();
    for f in facts {
        meta = meta.push(text(f).size(11).color(t::TEXT_MUTED));
    }
    if has_meta {
        card_content = card_content.push(meta);
    }

    let bg = if focused {
        t::BG_CARD_FOCUSED
    } else {
        t::BG_CARD
    };

    let border_color = if focused { t::ACCENT } else { t::BORDER };

    let card = container(card_content)
        .padding([10, 12])
        .width(Length::Fill)
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(bg)),
            border: Border {
                color: border_color,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        });

    mouse_area(card)
        .on_press(Message::CardPressed(lane_idx, issue_idx))
        .into()
}
