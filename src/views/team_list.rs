use std::collections::{HashMap, HashSet};

use iced::widget::{button, column, container, row, text, Column};
use iced::{Border, Element, Length, Theme};

use crate::app::{Message, TeamWebSection, View};
use crate::db::dashboards::Dashboard;
use crate::db::home_page::HomePage;
use crate::db::models::Team;
use crate::db::sidebar_layout::StoredSidebar;
use crate::db::saved_views::SavedView;
use crate::sidebar_layout::{sidebar_layout, SectionKind};
use crate::theme as t;
use crate::views::truncated::truncated;

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
    /// Local id of the open dashboard (`View::Dashboard`), highlighted in its row.
    pub open_dashboard_id: Option<&'a str>,
    /// Open Terminals per team id (JRST-407); teams at zero are absent.
    pub open_terminals: HashMap<String, usize>,
    /// Stored sidebar groups and team order; `None` keeps the single list.
    pub sidebar: Option<&'a StoredSidebar>,
}

/// What a row shows for its team's open Terminals: nothing at zero.
pub fn open_terminals_label(count: usize) -> Option<String> {
    (count > 0).then(|| format!("\u{2328} {count}"))
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
        open_terminals,
        home,
        dashboards,
        open_dashboard_id,
        sidebar,
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

    // Dashboards section: header with a "+" action; an empty state offers the
    // same action when there are none. Disabled in read-only mode.
    let new_dashboard = (!read_only).then_some(Message::OpenNewDashboard);
    col = col.push(
        row![]
            .align_y(iced::Alignment::Center)
            .push(section_header("DASHBOARDS"))
            .push(iced::widget::horizontal_space())
            .push(
                button(text("+").size(14).color(t::text_muted()))
                    .on_press_maybe(new_dashboard.clone())
                    .padding([2, 14])
                    .style(plain_row_style),
            ),
    );
    if dashboards.is_empty() {
        col = col.push(
            button(text("New dashboard").size(13).color(if read_only {
                t::text_muted()
            } else {
                t::text_secondary()
            }))
            .on_press_maybe(new_dashboard)
            .padding([6, 14])
            .width(Length::Fill)
            .style(plain_row_style),
        );
    }
    for d in ordered_dashboards(dashboards) {
        let active = open_dashboard_id == Some(d.id.as_str());
        col = col.push(
            button(
                row![text(&d.name).size(13).color(if active {
                    t::text_primary()
                } else {
                    t::text_secondary()
                })]
                .spacing(6)
                .align_y(iced::Alignment::Center)
                .push_maybe(is_home_dashboard(home, d).then(home_marker)),
            )
            .on_press(Message::OpenDashboard(d.id.clone()))
            .padding([6, 14])
            .width(Length::Fill)
            .style(move |_theme: &Theme, _status| selected_row_style(active)),
        );
    }

    // Teams: the stored layout's sections (groups, owned, joined, archived),
    // or one "YOUR TEAMS" list when no layout is stored. The archived toggle
    // only appears once there's something archived to reveal, on the first
    // section header.
    let has_archived = teams.iter().any(|t| t.archived_at.is_some());
    let mut archived_toggle = has_archived;
    let ctx = RowCtx {
        selected_team_id,
        expanded_teams,
        current_view,
        team_menu_open,
        saved_views,
        open_view_id,
        open_terminals: &open_terminals,
        home,
        read_only,
    };
    let layout = sidebar_layout(teams, sidebar, show_archived);
    for section in &layout.sections {
        let toggle = std::mem::take(&mut archived_toggle)
            .then(|| archived_toggle_button(show_archived));
        let collapsed = match &section.kind {
            SectionKind::Group { id, name, collapsed } => {
                col = col.push(group_header(id, name, *collapsed, toggle));
                *collapsed
            }
            kind => {
                let label = match kind {
                    SectionKind::Owned => "TEAMS YOU OWN",
                    SectionKind::Joined => "TEAMS YOU'VE JOINED",
                    SectionKind::Archived => "ARCHIVED",
                    _ => "YOUR TEAMS",
                };
                col = col.push(
                    row![]
                        .align_y(iced::Alignment::Center)
                        .push(section_header(label))
                        .push_maybe(toggle.is_some().then(iced::widget::horizontal_space))
                        .push_maybe(toggle),
                );
                false
            }
        };
        if collapsed {
            continue;
        }
        if section.teams.is_empty() {
            let msg = match section.kind {
                SectionKind::Group { .. } => Some("No teams in this group."),
                SectionKind::Owned => Some("No teams yet."),
                _ => None,
            };
            if let Some(msg) = msg {
                col = col.push(container(text(msg).size(12).color(t::text_muted())).padding([4, 14]));
            }
        }
        for team in &section.teams {
            col = team_block(col, team, &ctx);
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

/// Per-row inputs shared by every team in the sidebar.
struct RowCtx<'a, 'b> {
    selected_team_id: Option<&'a str>,
    expanded_teams: &'a HashSet<String>,
    current_view: &'a View,
    team_menu_open: Option<&'a str>,
    saved_views: &'a HashMap<String, Vec<SavedView>>,
    open_view_id: Option<&'a str>,
    open_terminals: &'b HashMap<String, usize>,
    home: Option<&'a HomePage>,
    read_only: bool,
}

/// One team's header row, menu, sub-rows and Saved views.
fn team_block<'a>(
    mut col: Column<'a, Message>,
    team: &'a Team,
    ctx: &RowCtx<'a, '_>,
) -> Column<'a, Message> {
    let RowCtx {
        selected_team_id,
        expanded_teams,
        current_view,
        team_menu_open,
        saved_views,
        open_view_id,
        open_terminals,
        home,
        read_only,
    } = *ctx;

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

        let name_color = if is_selected { t::text_primary() } else { t::text_secondary() };
        // An admin (not the owner) gets the web's small ADMIN label. The name
        // then truncates to whatever width the other items leave, so a long
        // name never pushes the badge out; without the badge the name is the
        // plain text row it always was.
        let show_admin = team.shows_admin_badge();
        let name_text: Element<'a, Message> = if show_admin {
            container(truncated(team.name.clone(), 14.0, name_color)).width(Length::Fill).into()
        } else {
            text(&team.name).size(14).color(name_color).into()
        };

        let header_content = row![]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .push(identifier_badge)
            .push(name_text)
            .push_maybe(show_admin.then(admin_badge))
            .push_maybe(is_home_team(home, team).then(home_marker))
            .push_maybe(is_archived.then(|| text("archived").size(10).color(t::text_muted())))
            .push_maybe(open_terminals_label(open_terminals.get(&team.id).copied().unwrap_or(0)).map(
                |label| text(label).size(11).color(t::text_muted()),
            ));

        // The chevron alone collapses or expands; the rest of the row expands
        // the team and focuses (or opens) its Tab.
        let chevron_button = button(chevron)
            .on_press(Message::ToggleTeamExpanded(team.id.clone()))
            .padding(iced::Padding { top: 8.0, right: 4.0, bottom: 8.0, left: 14.0 })
            .style(plain_row_style);
        let name_button = button(header_content)
            .on_press(Message::SelectTeamRow(team.id.clone()))
            .padding(iced::Padding { top: 8.0, right: 14.0, bottom: 8.0, left: 4.0 })
            .width(Length::Fill)
            .style(plain_row_style);
        let header_button = row![chevron_button, name_button].align_y(iced::Alignment::Center);

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
    col
}

/// A group's header: muted chevron and name; clicking toggles its collapsed
/// state locally. The archived toggle rides on the first header.
fn group_header<'a>(
    id: &str,
    name: &str,
    collapsed: bool,
    toggle: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let label = row![
        text(if collapsed { "\u{203A}" } else { "\u{2304}" }) // › / ⌄
            .size(13)
            .color(t::text_muted()),
        text(name.to_string()).size(13).color(t::text_muted()),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    row![]
        .align_y(iced::Alignment::Center)
        .push(
            button(label)
                .on_press(Message::ToggleSidebarGroup(id.to_string()))
                .padding([10, 14])
                .style(plain_row_style),
        )
        .push_maybe(toggle.is_some().then(iced::widget::horizontal_space))
        .push_maybe(toggle)
        .into()
}

fn archived_toggle_button<'a>(show_archived: bool) -> Element<'a, Message> {
    button(
        text(if show_archived { "Hide archived" } else { "Show archived" })
            .size(11)
            .color(t::text_muted()),
    )
    .on_press(Message::ToggleShowArchivedTeams)
    .padding([4, 14])
    .style(plain_row_style)
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

/// True when `home` is this dashboard. One without a remote id never matches.
pub fn is_home_dashboard(home: Option<&HomePage>, dashboard: &Dashboard) -> bool {
    home.is_some_and(|h| h.matches_dashboard(dashboard))
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

/// The ADMIN label after an admin's team name (about 10px, accent colour).
fn admin_badge<'a>() -> Element<'a, Message> {
    text("ADMIN").size(10).color(t::accent()).into()
}

/// The small home glyph shown next to the Home page's row.
fn home_marker<'a>() -> Element<'a, Message> {
    text("\u{2302}").size(12).color(t::accent()).into() // ⌂
}

/// Issue export isn't offered to users yet.
const SHOW_EXPORT_CSV: bool = false;

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
            // Export isn't available to users yet; `Message::ExportCsv` stays
            // wired for when it is.
            .push_maybe(SHOW_EXPORT_CSV.then(|| btn("Export CSV", Message::ExportCsv)))
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
    use super::{badge_label, is_home_dashboard, is_home_team, is_home_view, ordered_dashboards};
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
            role: None,
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
    fn dashboard_home_marks_that_dashboard_only() {
        let h = home("dashboard", 7);
        let mut d = dash("a", 0);
        d.remote_id = Some(7);
        assert!(is_home_dashboard(Some(&h), &d));
        d.remote_id = Some(8);
        assert!(!is_home_dashboard(Some(&h), &d));
        d.remote_id = None;
        assert!(!is_home_dashboard(Some(&h), &d));
        assert!(!is_home_dashboard(None, &d));
        let t = home("team", 7);
        d.remote_id = Some(7);
        assert!(!is_home_dashboard(Some(&t), &d));
    }

    #[test]
    fn dashboard_or_missing_home_marks_no_team_or_view() {
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

#[cfg(test)]
mod terminal_count_tests {
    use super::open_terminals_label;

    #[test]
    fn the_open_count_is_hidden_at_zero_and_shown_otherwise() {
        assert_eq!(open_terminals_label(0), None);
        assert_eq!(open_terminals_label(3).as_deref(), Some("\u{2328} 3"));
    }
}
