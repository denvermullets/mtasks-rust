use iced::widget::{column, container, horizontal_rule, row, scrollable, text, Column};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Issue, Lane, Team};
use crate::theme as t;

pub fn view<'a>(
    team: Option<&'a Team>,
    lanes: &'a [Lane],
    issues: &'a [Issue],
    focused_lane: usize,
    focused_issue: usize,
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
        let lane_issues: Vec<&Issue> = issues.iter().filter(|i| i.lane_id == lane.id).collect();
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

        let header_container = container(header)
            .padding([10, 12])
            .width(Length::Fill);

        header_row = header_row.push(header_container);
    }

    // Lane columns with cards
    let mut lane_columns = row![].spacing(1).width(Length::Fill).height(Length::Fill);

    for (lane_idx, lane) in lanes.iter().enumerate() {
        let lane_issues: Vec<&Issue> = issues.iter().filter(|i| i.lane_id == lane.id).collect();
        let is_focused_lane = lane_idx == focused_lane;

        let mut issue_cards: Column<'_, Message> =
            column![].spacing(6).width(Length::Fill).padding([4, 8]);

        for (issue_idx, issue) in lane_issues.iter().enumerate() {
            let is_focused = is_focused_lane && issue_idx == focused_issue;
            let card = issue_card(issue, team_identifier, is_focused);
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
            container(horizontal_rule(1))
                .style(|_theme: &Theme| container::Style {
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
    focused: bool,
) -> Element<'a, Message> {
    let priority_color = t::priority_color(issue.priority);

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
        .push(text(format!("{}", priority_val)).size(11).color(t::TEXT_MUTED));

    let card_content = column![]
        .spacing(4)
        .push(text(tag).size(12).color(t::TEXT_MUTED))
        .push(
            text(&issue.title)
                .size(14)
                .color(t::TEXT_PRIMARY),
        )
        .push(priority_indicator);

    let bg = if focused {
        t::BG_CARD_FOCUSED
    } else {
        t::BG_CARD
    };

    let border_color = if focused {
        t::ACCENT
    } else {
        t::BORDER
    };

    container(card_content)
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
        })
        .into()
}
