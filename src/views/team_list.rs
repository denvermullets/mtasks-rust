use std::collections::{HashMap, HashSet};

use iced::widget::{button, column, container, row, text};
use iced::{Border, Element, Length, Theme};

use crate::app::{Message, TeamWebSection, View};
use crate::db::dashboards::Dashboard;
use crate::db::home_page::HomePage;
use crate::db::models::Team;
use crate::db::saved_views::SavedView;
use crate::theme as t;

/// Everything the sidebar reads, bundled so `view` takes one argument.
pub struct SidebarState<'a> {
    pub teams: &'a [Team],
    pub selected_team_id: Option<&'a str>,
    pub expanded_teams: &'a HashSet<String>,
    pub current_view: &'a View,
    pub team_menu_open: Option<&'a str>,
    pub show_archived: bool,
    pub unread_count: i64,
    pub read_only: bool,
    pub saved_views: &'a HashMap<String, Vec<SavedView>>,
    pub open_view_id: Option<&'a str>,
    pub home: Option<&'a HomePage>,
    /// Mirrored dashboards (any order; the view sorts them).
    pub dashboards: &'a [Dashboard],
    /// Local id of the open dashboard (set once dashboards can be opened).
    pub open_dashboard_id: Option<&'a str>,
}

pub fn view<'a>(state: SidebarState<'a>) -> Element<'a, Message> {
    let SidebarState {
        teams,
        selected_team_id,
        expanded_teams,
        current_view,
        team_menu_open,
        show_archived,
        unread_count,
        read_only,
        saved_views,
        open_view_id,
        home,
        dashboards,
        open_dashboard_id,
    } = state;
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

    // Dashboards section: nothing at all while there are none.
    if !dashboards.is_empty() {
        col = col.push(section_header("DASHBOARDS"));
        for d in ordered_dashboards(dashboards) {
            let active = open_dashboard_id == Some(d.id.as_str());
            // Rows don't open anything yet (the open ticket wires this up).
            col = col.push(
                container(text(&d.name).size(13).color(if active {
                    t::text_primary()
                } else {
                    t::text_secondary()
                }))
                .padding([6, 14])
                .width(Length::Fill)
                .style(move |_theme: &Theme| selected_row_container_style(active)),
            );
        }
    }

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
            .push_maybe(is_home_team(home, team).then(home_marker))
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
            let is_home = is_home_team(home, team);
            let home_label = if is_home { "Home page \u{2713}" } else { "Set as home" };
            let can_set_home = synced && !read_only && !is_home;
            col = col
                .push(menu_row(
                    home_label,
                    can_set_home.then(|| Message::SetTeamAsHome(team.id.clone())),
                ))
                .push(menu_row(
                    "Members",
                    Some(Message::OpenTeamMembers(team.id.clone())),
                ))
                .push(web_row("Team settings \u{2197}", TeamWebSection::General))
                .push(web_row("Lane management \u{2197}", TeamWebSection::Lanes));
        }

        // Sub-rows for the expanded team.
        if is_expanded {
            let sub_rows: [(&str, View); 4] = [
                ("Issues", View::Board),
                ("Projects", View::ProjectList),
                ("Roadmap", View::Roadmap),
                ("Recurring issues", View::RecurringIssues),
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
                let active = open_view_id == Some(sv.id.as_str());
                let open_btn = button(
                    row![text(&sv.name).size(13).color(if active {
                        t::text_primary()
                    } else {
                        t::text_secondary()
                    })]
                    .spacing(6)
                    .align_y(iced::Alignment::Center)
                    .push_maybe(is_home_view(home, sv).then(home_marker)),
                )
                .on_press(Message::OpenSavedView(sv.id.clone()))
                .padding([6, 28])
                .width(Length::Fill)
                .style(move |_theme: &Theme, _status| selected_row_style(active));
                // Set-as-home needs a writable token and is a no-op on the
                // current home; disabled (muted) otherwise.
                let is_home = is_home_view(home, sv);
                let home_btn = icon_btn(
                    "\u{2302}",
                    12,
                    [6, 8],
                    (!read_only && !is_home).then(|| Message::SetSavedViewAsHome(sv.id.clone())),
                );
                // Delete needs a writable token; disabled (muted) otherwise.
                let delete_btn = icon_btn(
                    "\u{2715}",
                    11,
                    [6, 8],
                    (!read_only).then(|| Message::ConfirmDeleteSavedView(sv.id.clone())),
                );
                let rename_btn = icon_btn(
                    "\u{270E}",
                    11,
                    [6, 4],
                    (!read_only).then(|| Message::OpenRenameSavedView(sv.id.clone())),
                );
                col = col.push(
                    row![open_btn, home_btn, rename_btn, delete_btn]
                        .align_y(iced::Alignment::Center),
                );
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

/// Dashboards in sidebar order: server position, then id.
pub fn ordered_dashboards(dashboards: &[Dashboard]) -> Vec<&Dashboard> {
    let mut v: Vec<&Dashboard> = dashboards.iter().collect();
    v.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.id.cmp(&b.id)));
    v
}

/// True when `home` is this team. A team without a remote id never matches.
pub fn is_home_team(home: Option<&HomePage>, team: &Team) -> bool {
    home.is_some_and(|h| h.matches_team(team))
}

/// True when `home` is this Saved view, by remote id or (chosen before its
/// create pushed) by local id.
pub fn is_home_view(home: Option<&HomePage>, view: &SavedView) -> bool {
    home.is_some_and(|h| h.matches_view(view))
}

/// A muted glyph-only sidebar button; disabled when `msg` is `None`.
fn icon_btn<'a>(glyph: &'static str, size: u16, padding: [u16; 2], msg: Option<Message>) -> Element<'a, Message> {
    button(text(glyph).size(size).color(t::text_muted()))
        .on_press_maybe(msg)
        .padding(padding)
        .style(|_theme: &Theme, _status| button::Style {
            background: None,
            ..Default::default()
        })
        .into()
}

/// The small home glyph shown next to the Home page's row.
fn home_marker<'a>() -> Element<'a, Message> {
    text("\u{2302}").size(12).color(t::accent()).into() // ⌂
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

fn selected_row_container_style(active: bool) -> container::Style {
    let b = selected_row_style(active);
    container::Style {
        background: b.background,
        border: b.border,
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
    use super::{badge_label, is_home_team, is_home_view, ordered_dashboards};
    use crate::db::dashboards::Dashboard;
    use crate::db::home_page::HomePage;
    use crate::db::models::Team;
    use crate::db::saved_views::SavedView;

    fn home(kind: &str, id: i64) -> HomePage {
        use crate::db::home_page::HomeTarget;
        HomePage { target: HomeTarget::from_wire(kind, id).unwrap() }
    }

    fn team(remote_id: Option<i64>) -> Team {
        Team {
            id: "t".into(),
            name: "Team".into(),
            identifier: "TM".into(),
            remote_id,
            archived_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn sv(remote_id: Option<i64>) -> SavedView {
        SavedView {
            id: "x".into(),
            remote_id,
            team_id: "t".into(),
            name: "v".into(),
            query: "{}".into(),
            position: 0,
            updated_at: String::new(),
        }
    }

    #[test]
    fn team_home_marks_that_team_only() {
        let h = home("team", 5);
        assert!(is_home_team(Some(&h), &team(Some(5))));
        assert!(!is_home_team(Some(&h), &team(Some(6))));
        assert!(!is_home_team(Some(&h), &team(None)));
        assert!(!is_home_view(Some(&h), &sv(Some(5))));
    }

    #[test]
    fn view_home_marks_that_view_only() {
        let h = home("view", 7);
        assert!(is_home_view(Some(&h), &sv(Some(7))));
        assert!(!is_home_view(Some(&h), &sv(Some(8))));
        assert!(!is_home_team(Some(&h), &team(Some(7))));
    }

    #[test]
    fn dashboard_or_missing_home_marks_nothing() {
        let d = home("dashboard", 7);
        assert!(!is_home_team(Some(&d), &team(Some(7))));
        assert!(!is_home_view(Some(&d), &sv(Some(7))));
        assert!(!is_home_team(None, &team(Some(7))));
        assert!(!is_home_view(None, &sv(Some(7))));
    }

    fn dash(id: &str, position: i64) -> Dashboard {
        Dashboard {
            id: id.into(),
            remote_id: None,
            name: format!("name-{id}"),
            description: String::new(),
            position,
            updated_at: String::new(),
        }
    }

    #[test]
    fn dashboards_ordered_by_position_then_id() {
        let ds = vec![dash("c", 2), dash("b", 1), dash("a", 1), dash("z", 0)];
        let ids: Vec<&str> = ordered_dashboards(&ds).iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["z", "a", "b", "c"]);
    }

    #[test]
    fn no_dashboards_orders_to_nothing() {
        assert!(ordered_dashboards(&[]).is_empty());
    }

    #[test]
    fn badge_hidden_at_zero_and_capped() {
        assert_eq!(badge_label(0), None);
        assert_eq!(badge_label(-3), None);
        assert_eq!(badge_label(7).as_deref(), Some("7"));
        assert_eq!(badge_label(99).as_deref(), Some("99"));
        assert_eq!(badge_label(100).as_deref(), Some("99+"));
    }
}
