use iced::widget::{column, container, row, scrollable, text, Column};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Issue, Lane, Priority, Team};
use crate::theme as t;

pub fn view<'a>(
    team: Option<&'a Team>,
    lanes: &'a [Lane],
    issues: &'a [Issue],
    focused_lane: usize,
    focused_issue: usize,
) -> Element<'a, Message> {
    let team_identifier = team.map(|t| t.identifier.as_str()).unwrap_or("---");

    let mut lane_columns = row![].spacing(8).width(Length::Fill).height(Length::Fill);

    for (lane_idx, lane) in lanes.iter().enumerate() {
        let lane_issues: Vec<&Issue> = issues.iter().filter(|i| i.lane_id == lane.id).collect();
        let is_focused_lane = lane_idx == focused_lane;

        let lane_color = lane
            .color
            .as_deref()
            .and_then(t::hex_to_color)
            .unwrap_or(t::TEXT_SECONDARY);

        let header = container(
            text(format!("{} ({})", lane.name, lane_issues.len()))
                .size(13)
                .color(lane_color),
        )
        .padding([6, 8])
        .width(Length::Fill);

        let mut issue_cards: Column<'_, Message> = column![].spacing(4).width(Length::Fill);

        for (issue_idx, issue) in lane_issues.iter().enumerate() {
            let is_focused = is_focused_lane && issue_idx == focused_issue;
            let card = issue_card(issue, team_identifier, is_focused);
            issue_cards = issue_cards.push(card);
        }

        let lane_content = column![header, scrollable(issue_cards).height(Length::Fill)]
            .spacing(4)
            .width(Length::Fill);

        let lane_container = container(lane_content)
            .padding(6)
            .width(Length::Fill)
            .style(move |_theme: &Theme| {
                container::Style {
                    background: Some(iced::Background::Color(t::BG_SECONDARY)),
                    border: Border {
                        color: if is_focused_lane {
                            t::ACCENT
                        } else {
                            t::BORDER
                        },
                        width: if is_focused_lane { 2.0 } else { 1.0 },
                        radius: 6.0.into(),
                    },
                    ..Default::default()
                }
            });

        lane_columns = lane_columns.push(lane_container);
    }

    if lanes.is_empty() {
        container(text("No lanes — select a team").size(14).color(t::TEXT_MUTED))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    } else {
        container(lane_columns)
            .padding(8)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

fn issue_card<'a>(
    issue: &'a Issue,
    team_identifier: &'a str,
    focused: bool,
) -> Element<'a, Message> {
    let priority = Priority::from_i32(issue.priority);
    let priority_color = t::priority_color(issue.priority);

    let tag = issue
        .team_number
        .map(|n| format!("{}-{}", team_identifier, n))
        .unwrap_or_default();

    let card_content = column![]
        .spacing(2)
        .push(
            text(tag)
                .size(11)
                .color(t::TEXT_MUTED),
        )
        .push(text(&issue.title).size(13).color(t::TEXT_PRIMARY))
        .push(
            row![]
                .spacing(4)
                .push(
                    text(format!("● {}", priority.label()))
                        .size(11)
                        .color(priority_color),
                ),
        );

    let bg = if focused {
        t::BG_CARD_FOCUSED
    } else {
        t::BG_CARD
    };
    let border_color = if focused { t::ACCENT } else { t::BORDER };

    container(card_content)
        .padding([8, 10])
        .width(Length::Fill)
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(bg)),
            border: Border {
                color: border_color,
                width: if focused { 2.0 } else { 1.0 },
                radius: 4.0.into(),
            },
            ..Default::default()
        })
        .into()
}
