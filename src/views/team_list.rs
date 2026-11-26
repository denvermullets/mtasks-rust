use std::collections::{HashMap, HashSet};

use iced::widget::{button, column, container, row, Column};
use crate::views::text;
use iced::{Border, Element, Length, Theme};

use crate::app::{Message, TeamWebSection, View};
use crate::db::dashboards::Dashboard;
use crate::db::home_page::HomePage;
use crate::db::models::Team;
use crate::db::sidebar_layout::StoredSidebar;
use crate::db::saved_views::SavedView;
use crate::fonts;
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
    /// What a team's Terminals wait on the person for, per team id; teams
    /// with nothing waiting are absent.
    pub terminal_needs: HashMap<String, crate::terminal::Need>,
    /// Stored sidebar groups and team order; `None` keeps the single list.
    pub sidebar: Option<&'a StoredSidebar>,
}

/// What a row shows for its team's open Terminals: nothing at zero, else the
/// ⌨ glyph (in the symbol font) and the count (in the UI font), and a dot
/// while one of them waits on the person.
fn open_terminals_badge<'a>(count: usize, need: Option<crate::terminal::Need>) -> Option<Element<'a, Message>> {
    (count > 0).then(|| {
        row![
            text("\u{2328}").size(11).font(fonts::SYMBOL).color(t::text_muted()),
            text(count.to_string()).size(11).color(t::text_muted()),
        ]
        .push(need.map(crate::views::terminal_panel::attention_dot))
        .spacing(4)
        .align_y(iced::Alignment::Center)
        .into()
    })
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
        terminal_needs,
        home,
        dashboards,
        open_dashboard_id,
        sidebar,
    } = state;
    let mut col = column![].spacing(0).width(Length::Fill);

    // Workspace section, with the collapse button at the column's right.
    col = col.push(
        row![]
            .align_y(iced::Alignment::Center)
            .push(section_header("WORKSPACE"))
            .push(iced::widget::space::horizontal())
            .push(sidebar_toggle(false, false)),
    );
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
            .push(iced::widget::space::horizontal())
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
                .push(is_home_dashboard(home, d).then(home_marker)),
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
        terminal_needs: &terminal_needs,
        home,
        read_only,
    };
    let layout = sidebar_layout(teams, sidebar, show_archived);
    for section in &layout.sections {
        let toggle = std::mem::take(&mut archived_toggle)
            .then(|| archived_toggle_button(show_archived));
        // Breathing room between sections, like the web's `p-2 mt-1` blocks.
        col = col.push(iced::widget::space::vertical().height(SECTION_GAP));
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
                        .push(toggle.is_some().then(iced::widget::space::horizontal))
                        .push(toggle),
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
            background: t::repaint(t::bg_menu()),
            ..Default::default()
        })
        .into()
}

/// Vertical gap above each team section (group, owned, joined, archived).
const SECTION_GAP: f32 = 12.0;

/// Per-row inputs shared by every team in the sidebar.
struct RowCtx<'a, 'b> {
    selected_team_id: Option<&'a str>,
    expanded_teams: &'a HashSet<String>,
    current_view: &'a View,
    team_menu_open: Option<&'a str>,
    saved_views: &'a HashMap<String, Vec<SavedView>>,
    open_view_id: Option<&'a str>,
    open_terminals: &'b HashMap<String, usize>,
    terminal_needs: &'b HashMap<String, crate::terminal::Need>,
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
        terminal_needs,
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
        .font(fonts::SYMBOL)
        .color(t::text_muted());

        let identifier_badge = container(text(&team.identifier).size(11).color(if is_selected {
            t::accent()
        } else {
            t::text_muted()
        }))
        .padding([2, 4]);

        let name_color = if is_selected { t::text_primary() } else { t::text_secondary() };
        // The name stays on one line and truncates to whatever width the
        // other items leave (web `truncate`), so every team row is the same
        // height and a long name never pushes the badges out.
        let show_admin = team.shows_admin_badge();
        let name_text: Element<'a, Message> =
            container(truncated(team.name.clone(), 14.0, name_color)).width(Length::Fill).into();

        let header_content = row![]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .push(identifier_badge)
            .push(name_text)
            .push(show_admin.then(admin_badge))
            .push(is_home_team(home, team).then(home_marker))
            .push(is_archived.then(|| text("archived").size(10).color(t::text_muted())))
            .push(open_terminals_badge(
                open_terminals.get(&team.id).copied().unwrap_or(0),
                terminal_needs.get(&team.id).copied(),
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
                        button(text("\u{22EF}").size(14).font(fonts::SYMBOL).color(if menu_open {
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
                .push(menu_row(
                    "Import CSV",
                    Some(Message::PickCsvImport(team.id.clone())),
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
                let active =
                    is_selected && sidebar_row_view(current_view) == Some(view.clone()) && open_view_id.is_none();
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
                    .push(is_home_view(home, sv).then(home_marker)),
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
            .font(fonts::SYMBOL)
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
        .push(toggle.is_some().then(iced::widget::space::horizontal))
        .push(toggle)
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

/// Chevrons-left (collapse) and chevrons-right (expand), heroicons outline.
const ICON_COLLAPSE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M18.75 19.5l-7.5-7.5 7.5-7.5m-6 15L5.25 12l7.5-7.5"/></svg>"##;
const ICON_EXPAND: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="1.5" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M5.25 4.5l7.5 7.5-7.5 7.5m6-15l7.5 7.5-7.5 7.5"/></svg>"##;

/// Width the sidebar toggle takes, padding included. The Tab bar leads with
/// it while the sidebar is collapsed, and its layout math counts it.
pub const TOGGLE_WIDTH: f32 = 32.0;

/// Narrowest and widest the sidebar drags to.
pub const MIN_SIDEBAR_WIDTH: f32 = 160.0;
pub const MAX_SIDEBAR_WIDTH: f32 = 480.0;
/// Width of the grab strip on the sidebar's right edge.
pub const RESIZE_HANDLE_WIDTH: f32 = 4.0;

/// The sidebar's width for a stored (or dragged-to) `width`: the default
/// when unset, otherwise kept within the drag limits.
pub fn sidebar_width(width: Option<f32>) -> f32 {
    width
        .unwrap_or(crate::terminal::panel::SIDEBAR_WIDTH)
        .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
}

/// The grab strip on the sidebar's right edge: drag to resize, double-click
/// to go back to the default width.
pub fn resize_handle<'a>() -> Element<'a, Message> {
    iced::widget::mouse_area(
        container(iced::widget::Space::new())
            .width(Length::Fixed(RESIZE_HANDLE_WIDTH))
            .height(Length::Fill),
    )
    .on_press(Message::SidebarDragStart)
    .on_double_click(Message::SidebarWidthReset)
    .interaction(iced::mouse::Interaction::ResizingHorizontally)
    .into()
}

/// The button that collapses the sidebar (at the right of its top row) or,
/// once `collapsed`, expands it again (leading the Tab bar). Cmd+B does both.
pub fn sidebar_toggle<'a>(collapsed: bool, unread: bool) -> Element<'a, Message> {
    let (icon, tip) = if collapsed {
        (ICON_EXPAND, "Show sidebar  \u{2318}B")
    } else {
        (ICON_COLLAPSE, "Hide sidebar  \u{2318}B")
    };
    let glyph = container(crate::views::form_page::icon(icon, 14.0, t::text_muted()))
        .center_x(Length::Fill)
        .center_y(Length::Shrink);
    // Unread Notifications while collapsed (JRST-551): a red dot at the icon's
    // top right, standing in for the hidden Inbox badge.
    let glyph: Element<'a, Message> = if unread {
        iced::widget::stack![glyph, unread_corner_dot()].into()
    } else {
        glyph.into()
    };
    let btn = button(glyph)
    .width(Length::Fixed(TOGGLE_WIDTH))
    .padding([5, 0])
    .on_press(Message::ToggleSidebar)
    .style(|_theme: &Theme, status| button::Style {
        background: matches!(status, button::Status::Hovered)
            .then(|| iced::Background::Color(t::bg_card_focused())),
        border: Border { radius: 4.0.into(), ..Default::default() },
        ..Default::default()
    });
    iced::widget::tooltip(
        btn,
        container(text(tip).size(12).color(t::text_strong()))
            .padding([4, 8])
            .style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::bg_menu())),
                border: Border { color: t::border(), width: 1.0, radius: 4.0.into() },
                ..Default::default()
            }),
        iced::widget::tooltip::Position::Bottom,
    )
    .gap(4)
    .into()
}

/// Diameter of the expand button's unread dot.
const UNREAD_DOT: f32 = 6.0;

/// The red unread dot, pinned to the top right of the expand icon.
fn unread_corner_dot<'a>() -> Element<'a, Message> {
    let dot = container(iced::widget::Space::new().width(UNREAD_DOT).height(UNREAD_DOT)).style(
        |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::danger())),
            border: Border { radius: (UNREAD_DOT / 2.0).into(), ..Default::default() },
            ..Default::default()
        },
    );
    // The icon is 14px, centred in the button: sit the dot on its corner.
    container(dot)
        .width(Length::Fill)
        .align_x(iced::alignment::Horizontal::Center)
        .padding(iced::Padding { left: 14.0, ..Default::default() })
        .into()
}

/// A muted glyph-only sidebar button; disabled when `msg` is `None`.
fn icon_btn<'a>(glyph: &'static str, size: u16, padding: [u16; 2], msg: Option<Message>) -> Element<'a, Message> {
    button(text(glyph).size(f32::from(size)).font(fonts::SYMBOL).color(t::text_muted()))
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
    text("\u{2302}").size(12).font(fonts::SYMBOL).color(t::accent()).into() // ⌂
}

/// Issue export isn't offered to users yet.
const SHOW_EXPORT_CSV: bool = false;

/// Sidebar footer: What's new and Settings (plus CSV export, once offered,
/// disabled without a team). CSV import lives in each team's menu.
pub fn footer<'a>(has_team: bool, settings_dot: bool) -> Element<'a, Message> {
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
            .push(SHOW_EXPORT_CSV.then(|| btn("Export CSV", Message::ExportCsv)))
            .push(
                button(text("What's new").size(13).color(t::text_secondary()))
                    .on_press(Message::OpenWhatsNew)
                    .padding([6, 14])
                    .width(Length::Fill)
                    .style(plain_row_style),
            )
            .push(settings_row(settings_dot)),
    )
    .padding([6, 0])
    .width(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        background: t::repaint(t::bg_menu()),
        border: Border {
            color: t::border(),
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    })
    .into()
}

/// The Settings sidebar entry, with a dot while an Update is ready or a shown
/// failure is active (project 320).
fn settings_row<'a>(dot: bool) -> Element<'a, Message> {
    let content = row![text("Settings").size(13).color(t::text_secondary())]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .push(dot.then(|| {
            container(iced::widget::Space::new().width(8).height(8)).style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::accent())),
                border: Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
        }));
    button(content)
        .on_press(Message::OpenSettings)
        .padding([6, 14])
        .width(Length::Fill)
        .style(plain_row_style)
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
    .push(iced::widget::space::horizontal())
    .push(badge_label(unread_count).map(|label| {
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

/// The team sub-row a view lights up (JRST-453): a list view lights its own
/// row, and a detail view lights the list it belongs to, so the focused team
/// stays outlined while an issue or project is open. Cross-team views (My
/// issues, Inbox, dashboards) light no team row.
pub fn sidebar_row_view(view: &View) -> Option<View> {
    match view {
        View::Board | View::IssueDetail(_) | View::DependencyMap { .. } => Some(View::Board),
        View::ProjectList | View::ProjectDetail(_) | View::ProjectDependencyMap { .. } => {
            Some(View::ProjectList)
        }
        View::Roadmap => Some(View::Roadmap),
        View::RecurringIssues => Some(View::RecurringIssues),
        View::MyIssues | View::Inbox | View::Dashboard(_) => None,
    }
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
        background: if active {
            Some(iced::Background::Color(t::bg_card_focused()))
        } else {
            t::repaint(t::bg_menu())
        },
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
        background: t::repaint(t::bg_menu()),
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
    #[test]
    fn sidebar_width_defaults_and_clamps() {
        use super::{sidebar_width, MAX_SIDEBAR_WIDTH, MIN_SIDEBAR_WIDTH};
        assert_eq!(sidebar_width(None), crate::terminal::panel::SIDEBAR_WIDTH);
        assert_eq!(sidebar_width(Some(300.0)), 300.0);
        assert_eq!(sidebar_width(Some(10.0)), MIN_SIDEBAR_WIDTH);
        assert_eq!(sidebar_width(Some(9000.0)), MAX_SIDEBAR_WIDTH);
    }

    use super::{
        badge_label, is_home_dashboard, is_home_team, is_home_view, ordered_dashboards, sidebar_row_view,
    };
    use crate::app::{DependencyMapTab, View};

    #[test]
    fn detail_views_light_their_parent_row() {
        assert_eq!(sidebar_row_view(&View::IssueDetail("i".into())), Some(View::Board));
        assert_eq!(
            sidebar_row_view(&View::DependencyMap {
                issue_id: "i".into(),
                tab: DependencyMapTab::Dependencies,
                depth: 1
            }),
            Some(View::Board)
        );
        assert_eq!(sidebar_row_view(&View::ProjectDetail("p".into())), Some(View::ProjectList));
        assert_eq!(
            sidebar_row_view(&View::ProjectDependencyMap { project_id: "p".into(), active_only: true }),
            Some(View::ProjectList)
        );
    }

    #[test]
    fn list_views_light_themselves_and_cross_team_views_light_none() {
        for v in [View::Board, View::ProjectList, View::Roadmap, View::RecurringIssues] {
            assert_eq!(sidebar_row_view(&v), Some(v.clone()));
        }
        for v in [View::MyIssues, View::Inbox, View::Dashboard("d".into())] {
            assert_eq!(sidebar_row_view(&v), None);
        }
    }
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
    use super::open_terminals_badge;

    #[test]
    fn the_open_count_is_hidden_at_zero_and_shown_otherwise() {
        assert!(open_terminals_badge(0, None).is_none());
        assert!(open_terminals_badge(3, None).is_some());
        assert!(open_terminals_badge(3, Some(crate::terminal::Need::Done)).is_some());
    }
}
