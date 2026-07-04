use std::collections::HashSet;

use iced::widget::{button, column, container, row, text};
use iced::{Border, Element, Length, Theme};

use crate::app::{Message, View};
use crate::db::models::Team;
use crate::theme as t;

pub fn view<'a>(
    teams: &'a [Team],
    selected_team_id: Option<&'a str>,
    expanded_teams: &'a HashSet<String>,
    current_view: &'a View,
) -> Element<'a, Message> {
    let mut col = column![].spacing(0).width(Length::Fill);

    // Workspace section
    col = col.push(section_header("WORKSPACE"));
    col = col.push(nav_row(
        "My Issues",
        *current_view == View::MyIssues,
        Message::NavigateToView(View::MyIssues),
        14,
    ));

    // Teams section
    col = col.push(section_header("YOUR TEAMS"));

    for team in teams {
        let is_selected = selected_team_id == Some(team.id.as_str());
        let is_expanded = expanded_teams.contains(&team.id);

        // Collapsible team header (chevron + identifier badge + name).
        let chevron = text(if is_expanded {
            "\u{25BE}" // ▾
        } else {
            "\u{25B8}" // ▸
        })
        .size(10)
        .color(t::TEXT_MUTED);

        let identifier_badge = container(
            text(&team.identifier)
                .size(11)
                .color(if is_selected { t::ACCENT } else { t::TEXT_MUTED }),
        )
        .padding([2, 4]);

        let name_text = text(&team.name).size(14).color(if is_selected {
            t::TEXT_PRIMARY
        } else {
            t::TEXT_SECONDARY
        });

        let header_content = row![]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .push(chevron)
            .push(identifier_badge)
            .push(name_text);

        col = col.push(
            button(header_content)
                .on_press(Message::ToggleTeamExpanded(team.id.clone()))
                .padding([8, 14])
                .width(Length::Fill)
                .style(plain_row_style),
        );

        // Sub-rows for the expanded team.
        if is_expanded {
            let sub_rows: [(&str, View); 3] = [
                ("Issues", View::Board),
                ("Projects", View::ProjectList),
                ("Roadmap", View::Roadmap),
            ];
            for (label, view) in sub_rows {
                let active = is_selected && *current_view == view;
                col = col.push(nav_row(
                    label,
                    active,
                    Message::SelectTeamView(team.id.clone(), view),
                    28,
                ));
            }
        }
    }

    container(col)
        .padding(0)
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_MENU)),
            ..Default::default()
        })
        .into()
}

/// A muted uppercase section label.
fn section_header<'a>(label: &'a str) -> Element<'a, Message> {
    container(text(label).size(11).color(t::TEXT_MUTED))
        .padding([14, 14])
        .into()
}

/// A clickable navigation row with optional active (selected) highlight.
/// `left_pad` controls indentation (top-level vs. nested sub-row).
fn nav_row<'a>(
    label: &'a str,
    active: bool,
    on_press: Message,
    left_pad: u16,
) -> Element<'a, Message> {
    button(text(label).size(14).color(if active {
        t::TEXT_PRIMARY
    } else {
        t::TEXT_SECONDARY
    }))
    .on_press(on_press)
    .padding([6, left_pad])
    .width(Length::Fill)
    .style(move |_theme: &Theme, _status| selected_row_style(active))
    .into()
}

fn selected_row_style(active: bool) -> button::Style {
    button::Style {
        background: Some(iced::Background::Color(if active {
            t::BG_CARD_FOCUSED
        } else {
            t::BG_MENU
        })),
        text_color: t::TEXT_PRIMARY,
        border: Border {
            color: if active { t::ACCENT } else { iced::Color::TRANSPARENT },
            width: if active { 2.0 } else { 0.0 },
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

fn plain_row_style(_theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: Some(iced::Background::Color(t::BG_MENU)),
        text_color: t::TEXT_PRIMARY,
        border: Border {
            color: iced::Color::TRANSPARENT,
            width: 0.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}
