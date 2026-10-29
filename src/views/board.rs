use std::collections::HashMap;

use iced::widget::{
    column, container, horizontal_rule, mouse_area, row, scrollable, text, Column, Row,
};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Issue, Label, Lane, Team};
use crate::theme as t;
use crate::views::header;
use crate::views::labels as labels_view;

pub fn view<'a>(
    team: Option<&'a Team>,
    lanes: &'a [Lane],
    issues: &'a [Issue],
    issue_labels: &'a HashMap<String, Vec<Label>>,
    focused_lane: usize,
    focused_issue: usize,
    needle: &str,
) -> Element<'a, Message> {
    let team_identifier = team.map(|t| t.identifier.as_str()).unwrap_or("---");

    if lanes.is_empty() {
        return container(
            text("No lanes — select a team")
                .size(14)
                .color(t::TEXT_MUTED),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into();
    }

    // Lane headers row
    let mut header_row = row![].spacing(0).width(Length::Fill);
    for (lane_idx, lane) in lanes.iter().enumerate() {
        let lane_issues: Vec<&Issue> = issues
            .iter()
            .filter(|i| i.lane_id == lane.id && header::issue_matches(i, team, needle))
            .collect();
        let is_focused_lane = lane_idx == focused_lane;

        let name_color = if is_focused_lane {
            t::TEXT_PRIMARY
        } else {
            t::TEXT_SECONDARY
        };

        let header = row![]
            .spacing(6)
            .push(text(&lane.name).size(14).color(name_color))
            .push(
                text(format!("{}", lane_issues.len()))
                    .size(13)
                    .color(t::TEXT_MUTED),
            );

        let header_container = container(header).padding([10, 12]).width(Length::Fill);

        header_row = header_row.push(header_container);
    }

    // Lane columns with cards
    let mut lane_columns = row![].spacing(1).width(Length::Fill).height(Length::Fill);

    for (lane_idx, lane) in lanes.iter().enumerate() {
        let lane_issues: Vec<&Issue> = issues
            .iter()
            .filter(|i| i.lane_id == lane.id && header::issue_matches(i, team, needle))
            .collect();
        let is_focused_lane = lane_idx == focused_lane;

        let mut issue_cards: Column<'_, Message> =
            column![].spacing(6).width(Length::Fill).padding([4, 8]);

        for (issue_idx, issue) in lane_issues.iter().enumerate() {
            let is_focused = is_focused_lane && issue_idx == focused_issue;
            let card_labels = issue_labels
                .get(&issue.id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let card = issue_card(
                issue,
                team_identifier,
                card_labels,
                is_focused,
                lane_idx,
                issue_idx,
            );
            issue_cards = issue_cards.push(card);
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
    team_identifier: &'a str,
    labels: &'a [Label],
    focused: bool,
    lane_idx: usize,
    issue_idx: usize,
) -> Element<'a, Message> {
    let priority_color = t::priority_color(issue.priority_enum());

    let tag = issue
        .team_number
        .map(|n| format!("{}-{}", team_identifier, n))
        .unwrap_or_default();

    // Priority indicator: triangle + number like the Rails app
    let priority_val = issue.priority;
    let priority_indicator = row![]
        .spacing(4)
        .align_y(iced::Alignment::Center)
        .push(text("\u{25B3}").size(11).color(priority_color))
        .push(
            text(format!("{}", priority_val))
                .size(11)
                .color(t::TEXT_MUTED),
        );

    let mut card_content = column![]
        .spacing(4)
        .push(text(tag).size(12).color(t::TEXT_MUTED))
        .push(text(&issue.title).size(14).color(t::TEXT_PRIMARY));

    if !labels.is_empty() {
        let mut chips: Row<'a, Message> = row![].spacing(4);
        for label in labels {
            chips = chips.push(labels_view::static_chip(label));
        }
        card_content = card_content.push(chips);
    }

    let card_content = card_content.push(priority_indicator);

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
