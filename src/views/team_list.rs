use std::collections::{HashMap, HashSet};

use iced::widget::{button, column, container, row, text};
use iced::{Border, Element, Length, Theme};

use crate::app::{Message, TeamWebSection, View};
use crate::db::models::Team;
use crate::db::saved_views::SavedView;
use crate::theme as t;

#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    teams: &'a [Team],
    selected_team_id: Option<&'a str>,
    expanded_teams: &'a HashSet<String>,
    current_view: &'a View,
    team_menu_open: Option<&'a str>,
    show_archived: bool,
    unread_count: i64,
    saved_views: &'a HashMap<String, Vec<SavedView>>,
    open_view_id: Option<&'a str>,
)-> Element<'a, Message> {
    let mut col = column![].spacing(0).width(Length::Fill);

    // Workspace section
    col = col.push(section_header("WORKSPACE"));
    col = col.push(nav_row(
        "My Issues",
        *current_view == View::MyIssues,
        Message::NavigateToView(View::MyIssues),
        14,
    ));
    col = col.push(inbox_row(*current_view == View::Inbox, unread_count));

    // Teams section. The archived toggle only appears once there's something
    // archived to reveal.
    let has_archived = teams.iter().any(|t| t.archived_at.is_some());
    if has_archived {
        col = col.push(
            row![]
                .align_y(iced::Alignment::Center)
                .push(section_header("YOUR TEAMS"))
                .push(iced::widget::horizontal_space())
                .push(
                    button(
                        text(if show_archived {
                            "Hide archived"
                        } else {
                            "Show archived"
                        })
                        .size(11)
                        .color(t::text_muted()),
                    )
                    .on_press(Message::ToggleShowArchivedTeams)
                    .padding([4, 14])
                    .style(plain_row_style),
                ),
        );
    } else {
        col = col.push(section_header("YOUR TEAMS"));
    }

    for team in teams
        .iter()
        .filter(|t| show_archived || t.archived_at.is_none())
    {
        let is_archived = team.archived_at.is_some();
        let is_selected = selected_team_id == Some(team.id.as_str());
        let is_expanded = expanded_teams.contains(&team.id);

        // Collapsible team header (chevron + identifier badge + name).
        let chevron = text(if is_expanded {
            "\u{25BE}" // ▾
        } else {
            "\u{25B8}" // ▸
        })
        .size(10)
        .color(t::text_muted());

        let identifier_badge = container(text(&team.identifier).size(11).color(if is_selected {
            t::accent()
        } else {
            t::text_muted()
        }))
        .padding([2, 4]);

        let name_text = text(&team.name).size(14).color(if is_selected {
            t::text_primary()
        } else {
            t::text_secondary()
        });

        let header_content = row![]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .push(chevron)
            .push(identifier_badge)
            .push(name_text)
            .push_maybe(is_archived.then(|| text("archived").size(10).color(t::text_muted())));

        let header_button = button(header_content)
            .on_press(Message::ToggleTeamExpanded(team.id.clone()))
            .padding([8, 14])
            .width(Length::Fill)
            .style(plain_row_style);

        // The kebab only shows on the expanded team header (iced 0.13 has no
        // cheap hover state for a sidebar row).
        let menu_open = team_menu_open == Some(team.id.as_str());
        if is_expanded {
            col = col.push(
                row![]
                    .align_y(iced::Alignment::Center)
                    .push(header_button)
                    .push(
                        button(text("\u{22EF}").size(14).color(if menu_open {
                            t::text_primary()
                        } else {
                            t::text_muted()
                        }))
                        .on_press(Message::ToggleTeamMenu(team.id.clone()))
                        .padding([6, 10])
                        .style(plain_row_style),
                    ),
            );
        } else {
            col = col.push(header_button);
        }

        // Team menu. Members is read from the local mirror; settings and lanes
        // are Rails-managed (no API write endpoints), so they open the web app.
        if is_expanded && menu_open {
            let synced = team.remote_id.is_some();
            let web_row = |label: &'a str, section: TeamWebSection| {
                menu_row(
                    label,
                    synced.then(|| Message::OpenTeamWebSettings(team.id.clone(), section)),
                )
            };
            col = col
                .push(menu_row(
                    "Members",
                    Some(Message::OpenTeamMembers(team.id.clone())),
                ))
                .push(web_row("Team settings \u{2197}", TeamWebSection::General))
                .push(web_row("Lane management \u{2197}", TeamWebSection::Lanes));
        }

        // Sub-rows for the expanded team.
        if is_expanded {
            let sub_rows: [(&str, View); 3] = [
                ("Issues", View::Board),
                ("Projects", View::ProjectList),
                ("Roadmap", View::Roadmap),
            ];
            for (label, view) in sub_rows {
                let active = is_selected && *current_view == view && open_view_id.is_none();
                col = col.push(nav_row(
                    label,
                    active,
                    Message::SelectTeamView(team.id.clone(), view),
                    28,
                ));
            }
            for sv in saved_views.get(&team.id).into_iter().flatten() {
                col = col.push(nav_row(
                    &sv.name,
                    open_view_id == Some(sv.id.as_str()),
                    Message::OpenSavedView(sv.id.clone()),
                    28,
                ));
            }
        }
    }

    container(col)
        .padding(0)
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_menu())),
            ..Default::default()
        })
        .into()
}

/// Sidebar footer: per-team CSV export / import (disabled without a team).
pub fn footer<'a>(has_team: bool) -> Element<'a, Message> {
    let btn = |label: &'a str, msg: Message| {
        button(text(label).size(13).color(if has_team {
            t::text_secondary()
        } else {
            t::text_muted()
        }))
        .on_press_maybe(has_team.then_some(msg))
        .padding([6, 14])
        .width(Length::Fill)
        .style(plain_row_style)
    };
    container(
        column![]
            .push(btn("Export CSV", Message::ExportCsv))
            .push(btn("Import CSV", Message::PickCsvImport)),
    )
    .padding([6, 0])
    .width(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_menu())),
        border: Border {
            color: t::border(),
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    })
    .into()
}

/// A muted uppercase section label.
fn section_header<'a>(label: &'a str) -> Element<'a, Message> {
    container(text(label).size(11).color(t::text_muted()))
        .padding([14, 14])
        .into()
}

/// A team-menu item. `on_press = None` renders it disabled (muted, inert),
/// e.g. web links for a team that only exists locally.
fn menu_row<'a>(label: &'a str, on_press: Option<Message>) -> Element<'a, Message> {
    let enabled = on_press.is_some();
    button(text(label).size(12).color(if enabled {
        t::text_secondary()
    } else {
        t::text_muted()
    }))
    .on_press_maybe(on_press)
    .padding([5, 28])
    .width(Length::Fill)
    .style(plain_row_style)
    .into()
}

/// A clickable navigation row with optional active (selected) highlight.
/// `left_pad` controls indentation (top-level vs. nested sub-row).
/// Unread badge text: hidden at zero (or below), capped at "99+".
pub fn badge_label(count: i64) -> Option<String> {
    match count {
        i64::MIN..=0 => None,
        1..=99 => Some(count.to_string()),
        _ => Some("99+".into()),
    }
}

/// The Inbox sidebar entry: label plus an unread badge when there is any.
fn inbox_row<'a>(active: bool, unread_count: i64) -> Element<'a, Message> {
    let content = row![text("Inbox").size(14).color(if active {
        t::text_primary()
    } else {
        t::text_secondary()
    })]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .push(iced::widget::horizontal_space())
    .push_maybe(badge_label(unread_count).map(|label| {
        container(text(label).size(11).color(t::text_primary()))
            .padding([1, 6])
            .style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::accent())),
                border: Border {
                    radius: 8.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
    }));
    button(content)
        .on_press(Message::NavigateToView(View::Inbox))
        .padding([6, 14])
        .width(Length::Fill)
        .style(move |_theme: &Theme, _status| selected_row_style(active))
        .into()
}

fn nav_row<'a>(
    label: &'a str,
    active: bool,
    on_press: Message,
    left_pad: u16,
) -> Element<'a, Message> {
    button(text(label).size(14).color(if active {
        t::text_primary()
    } else {
        t::text_secondary()
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
            t::bg_card_focused()
        } else {
            t::bg_menu()
        })),
        text_color: t::text_primary(),
        border: Border {
            color: if active {
                t::accent()
            } else {
                iced::Color::TRANSPARENT
            },
            width: if active { 2.0 } else { 0.0 },
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

fn plain_row_style(_theme: &Theme, _status: button::Status) -> button::Style {
    button::Style {
        background: Some(iced::Background::Color(t::bg_menu())),
        text_color: t::text_primary(),
        border: Border {
            color: iced::Color::TRANSPARENT,
            width: 0.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::badge_label;

    #[test]
    fn badge_hidden_at_zero_and_capped() {
        assert_eq!(badge_label(0), None);
        assert_eq!(badge_label(-3), None);
        assert_eq!(badge_label(7).as_deref(), Some("7"));
        assert_eq!(badge_label(99).as_deref(), Some("99"));
        assert_eq!(badge_label(100).as_deref(), Some("99+"));
    }
}
