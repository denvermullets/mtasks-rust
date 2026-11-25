//! The dashboard view, laid out like the web's `dashboards/show`: a centered
//! page with the title and pickers, the filter tabs, then one bordered card per
//! group. Everything scrolls together under the top bar (`header::dashboard_bar`).
//!
//! Focus uses the same `(group index, issue index in group)` pair as the list.
//! Every block has a fixed height so `focused_span` can place the focused row.

use std::collections::{HashMap, HashSet};

use iced::alignment::Horizontal;
use iced::widget::{button, column, container, horizontal_space, mouse_area, pick_list, row, scrollable, stack, svg, Column, Space};
use crate::views::text;
use iced::{Alignment, Border, Color, Element, Length, Padding, Theme};

use crate::app::Message;
use crate::scroll::{track, ScrollKey};
use crate::dashboard_eval::{self, AssigneeFilter, Evaluation, Filter, GroupResult, Header, Sort};
use crate::db::dashboards::{Dashboard, MoveDirection};
use crate::db::models::Issue;
use crate::fonts;
use crate::scroll;
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::board::CardData;
use crate::views::dropdown::dropdown;
use crate::views::header as top_bar;
use crate::views::truncated::truncated;
use crate::views::util;

// Page layout (web `max-w-6xl mx-auto p-6`, `space-y-3` between groups).
const PAGE_MAX_W: f32 = 1152.0;
const PAGE_PAD: f32 = space::S6;
const GROUP_GAP: f32 = space::S3;
const BORDER: f32 = 1.0;

/// Title and pickers: two picker rows and the counts, description clipped to fit.
const INTRO_H: f32 = 96.0;
/// Filter tabs and Mine only.
const TABS_H: f32 = 26.0;
/// Everything above the first group.
const TOOLBAR_H: f32 = PAGE_PAD + INTRO_H + space::S4 + TABS_H + space::S4;

/// Group header, without and with its one-line description (web `py-3`).
const HEADER_H: f32 = 46.0;
const HEADER_DESC_H: f32 = HEADER_H + space::S1 + type_scale::XS * type_scale::LH_XS;
/// An `SM` line plus the web empty message's `py-3`.
const EMPTY_H: f32 = type_scale::SM * type_scale::LH_SM + 2.0 * space::S3;

// Row columns (web `grid-cols-[auto_1fr_140px_160px_110px_32px_72px]`).
const W_TEAM: f32 = 140.0;
const W_PROJECT: f32 = 160.0;
const W_DUE: f32 = 110.0;
const W_ASSIGNEE: f32 = 32.0;
const W_ID: f32 = 72.0;
const DOT: f32 = space::S2;
const ICON: f32 = 14.0;
/// The keyboard focus marker sits in the row's left padding, clear of the
/// card's rounded corners.
const MARKER_W: f32 = 2.0;
const MARKER_INSET: f32 = space::S1_5;

/// The web `text-[11px]` count pill.
const COUNT_SIZE: f32 = 11.0;
/// Web `w-56` search box.
const SEARCH_W: f32 = 224.0;
/// Web `w-44` / `w-40` menus.
const DASHBOARD_MENU_W: f32 = 176.0;
const GROUP_MENU_W: f32 = 160.0;

// Heroicons, as in the web templates; `svg_icon` tints them.
const ICON_SEARCH: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-4.35-4.35m0 0A7.5 7.5 0 1010.5 18a7.5 7.5 0 006.15-1.35z"/></svg>"##;
const ICON_USER: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M16 7a4 4 0 11-8 0 4 4 0 018 0zM12 14a7 7 0 00-7 7h14a7 7 0 00-7-7z"/></svg>"##;
const ICON_TEAM: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M17 20h5v-2a3 3 0 00-5.356-1.857M17 20H7m10 0v-2c0-.656-.126-1.283-.356-1.857M7 20H2v-2a3 3 0 015.356-1.857M7 20v-2c0-.656.126-1.283.356-1.857m0 0a5.002 5.002 0 019.288 0M15 7a3 3 0 11-6 0 3 3 0 016 0z"/></svg>"##;
const ICON_PROJECT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M20 7l-8-4-8 4m16 0l-8 4m8-4v10l-8 4m0-10L4 7m8 4v10M4 7v10l8 4"/></svg>"##;
const ICON_CHEVRON_DOWN: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M19 9l-7 7-7-7"/></svg>"##;
const ICON_CHEVRON_UP: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M5 15l7-7 7 7"/></svg>"##;
const ICON_KEBAB: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="#000" viewBox="0 0 20 20"><path d="M10 6a2 2 0 110-4 2 2 0 010 4zM10 12a2 2 0 110-4 2 2 0 010 4zM10 18a2 2 0 110-4 2 2 0 010 4z"/></svg>"##;

fn svg_icon<'a>(source: &'static str, size: f32, color: Color) -> Element<'a, Message> {
    svg(svg::Handle::from_memory(source.as_bytes()))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_theme: &Theme, _status| svg::Style { color: Some(color) })
        .into()
}

/// What the scroll math needs to know about one group.
pub struct SpanGroup {
    pub len: usize,
    pub collapsed: bool,
    pub has_description: bool,
}

/// `(top, height)` of the row `(group, issue)` within the page. A card is its
/// border, the header, and when expanded a divider, then its 1px-separated rows
/// or (when empty) one empty-state line.
pub fn focused_span(groups: &[SpanGroup], group: usize, issue: usize) -> (f32, f32) {
    let row = scroll::LIST_ROW_H;
    let mut top = TOOLBAR_H;
    for (i, g) in groups.iter().enumerate() {
        top += BORDER + if g.has_description { HEADER_DESC_H } else { HEADER_H };
        if !g.collapsed {
            top += BORDER;
            if i == group {
                return (top + issue as f32 * (row + BORDER), row);
            }
            top += if g.len == 0 { EMPTY_H } else { g.len as f32 * (row + BORDER) - BORDER };
        }
        top += BORDER + GROUP_GAP;
    }
    (top, row)
}

/// Which ⋮ menu is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuTarget {
    Dashboard,
    Group(String),
}

/// What the open dashboard's view needs beyond its content: which groups are
/// collapsed, where focus is, and whether writes apply.
pub struct ViewState<'a> {
    pub collapsed: &'a HashSet<String>,
    pub focused_group: usize,
    pub focused_issue: usize,
    pub read_only: bool,
    pub search: &'a str,
    pub open_menu: Option<&'a MenuTarget>,
}

pub fn view<'a>(
    dashboard: Option<&'a Dashboard>,
    evaluation: &Evaluation<'a>,
    data: CardData<'a>,
    header: Header,
    state: &ViewState<'_>,
) -> Element<'a, Message> {
    let Some(dashboard) = dashboard else {
        return centered("Dashboard not found");
    };

    let mut groups: Column<'a, Message> = column![].spacing(GROUP_GAP).width(Length::Fill);
    if evaluation.groups.is_empty() {
        groups = groups.push(no_groups(state.read_only));
    }
    for (group_idx, g) in evaluation.groups.iter().enumerate() {
        groups = groups.push(group_card(g, group_idx, evaluation, &data, &header, state));
    }

    let page = column![]
        .spacing(space::S4)
        .width(Length::Fill)
        .max_width(PAGE_MAX_W)
        .push(intro(dashboard, &header, evaluation, state.search))
        .push(tabs(&header))
        .push(groups);

    container(
        track(
            scrollable(container(page).center_x(Length::Fill).padding(PAGE_PAD)),
            ScrollKey::Dashboard,
        )
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        background: t::repaint(t::bg_primary()),
        ..Default::default()
    })
    .into()
}

/// Title and description on the left; search, pickers and counts on the right.
fn intro<'a>(
    dashboard: &'a Dashboard,
    header: &Header,
    e: &Evaluation<'a>,
    search: &str,
) -> Element<'a, Message> {
    let mut title = column![].spacing(space::S1).width(Length::Fill).push(
        text(&dashboard.name)
            .size(type_scale::BASE)
            .font(fonts::bold_active())
            .color(t::text_primary()),
    );
    if !dashboard.description.is_empty() {
        title = title.push(
            text(&dashboard.description)
                .size(type_scale::SM)
                .color(t::text_secondary()),
        );
    }

    let first = row![]
        .spacing(space::S2)
        .push(search_box(search))
        .push(team_picker(e, header.team.as_ref()))
        .push(assignee_picker(e, header.assignee));
    let second = row![]
        .spacing(space::S2)
        .push_maybe(label_picker(e, header.label.as_ref()))
        .push(status_picker(e, header.status.as_ref()))
        .push(sort_picker(header.sort));
    let controls = column![]
        .spacing(space::S2)
        .align_x(Alignment::End)
        .push(first)
        .push(second)
        .push(due_counts(e));

    container(row![].spacing(space::S3).push(title).push(controls))
        .width(Length::Fill)
        .height(Length::Fixed(INTRO_H))
        .clip(true)
        .into()
}

/// The search box, with the search shortcut's id and the web's icon inside it.
fn search_box<'a>(query: &str) -> Element<'a, Message> {
    let input = crate::undo_input::text_input(top_bar::SEARCH_KEY, "Search issues…", query, Message::HeaderSearchChanged)
        .size(type_scale::XS)
        .padding(Padding {
            top: space::S1_5,
            right: space::S3,
            bottom: space::S1_5,
            left: space::S8,
        })
        .width(Length::Fixed(SEARCH_W))
        .style(util::sidebar_input_style);
    let icon = container(svg_icon(ICON_SEARCH, space::S4, t::text_muted()))
        .padding(Padding { left: space::S2_5, ..Padding::ZERO })
        .center_y(Length::Fill);
    stack![input, icon].into()
}

/// A small chip (web `px-1.5 py-0.5 rounded bg-foreground border`).
fn count_chip<'a>(dot: Element<'a, Message>, label: String) -> Element<'a, Message> {
    container(
        row![]
            .spacing(space::S1)
            .align_y(Alignment::Center)
            .push(dot)
            .push(text(label).size(type_scale::XS).color(t::text_secondary())),
    )
    .padding([space::S0_5, space::S1_5])
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_secondary())),
        border: Border { color: t::border(), width: BORDER, radius: radius::SM.into() },
        ..Default::default()
    })
    .into()
}

/// "N due today" (amber dot) and "N overdue" (red ring) for the whole dashboard.
fn due_counts<'a>(e: &Evaluation<'a>) -> Element<'a, Message> {
    let dot = |fill: Option<Color>, stroke: Color| -> Element<'a, Message> {
        container(Space::new(space::S1_5, space::S1_5))
            .style(move |_theme: &Theme| container::Style {
                background: fill.map(iced::Background::Color),
                border: Border { color: stroke, width: BORDER, radius: radius::FULL.into() },
                ..Default::default()
            })
            .into()
    };
    row![]
        .spacing(space::S1_5)
        .push(count_chip(dot(Some(t::AMBER_400), t::AMBER_400), format!("{} due today", e.due_today)))
        .push(count_chip(dot(None, t::RED_400), format!("{} overdue", e.overdue)))
        .into()
}

/// A header picker's closed look: the sidebar controls' frame with brighter text.
fn picker_style(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    pick_list::Style { text_color: t::text_primary(), ..util::sidebar_pick_list_style(theme, status) }
}

/// The sort order picker.
fn sort_picker<'a>(active: Sort) -> Element<'a, Message> {
    pick_list(Sort::ALL, Some(active), Message::SetDashboardSort)
        .text_size(type_scale::XS)
        .padding([space::S1_5, space::S2_5])
        .style(picker_style)
        .menu_style(util::sidebar_menu_style)
        .into()
}

/// One entry of a header picker. `value: None` is the clearing choice ("All
/// teams", "Anyone", ...).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice<T> {
    value: Option<T>,
    label: String,
}

impl<T> std::fmt::Display for Choice<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// A pick list over `options` (the clearing choice first), with the entry
/// whose value equals `active` selected. Sends the picked value, `None` for
/// the clearing choice.
fn picker<'a, T>(
    options: Vec<Choice<T>>,
    active: Option<&T>,
    on_pick: impl Fn(Option<T>) -> Message + 'a,
) -> Element<'a, Message>
where
    T: Clone + PartialEq + 'a,
{
    let selected = options.iter().find(|o| o.value.as_ref() == active).cloned();
    pick_list(options, selected, move |c| on_pick(c.value))
        .text_size(type_scale::XS)
        .padding([space::S1_5, space::S2_5])
        .style(picker_style)
        .menu_style(util::sidebar_menu_style)
        .into()
}

fn choice<T>(value: T, label: impl Into<String>) -> Choice<T> {
    Choice { value: Some(value), label: label.into() }
}

fn clearing<T>(label: &str) -> Choice<T> {
    Choice { value: None, label: label.to_string() }
}

/// The team picker, listing the dashboard's source teams.
fn team_picker<'a>(e: &Evaluation<'a>, active: Option<&String>) -> Element<'a, Message> {
    let mut options = vec![clearing("All teams")];
    options.extend(e.source_teams.iter().map(|t| choice(t.id.clone(), t.name.clone())));
    picker(options, active, Message::SetDashboardTeam)
}

/// The status picker: lane names across the source teams.
fn status_picker<'a>(e: &Evaluation<'a>, active: Option<&String>) -> Element<'a, Message> {
    let mut options = vec![clearing("Any status")];
    options.extend(e.status_options.iter().map(|s| choice(s.clone(), s.clone())));
    picker(options, active, Message::SetDashboardStatus)
}

/// The assignee picker: "Unassigned", then members of the source teams.
fn assignee_picker<'a>(e: &Evaluation<'a>, active: Option<AssigneeFilter>) -> Element<'a, Message> {
    let mut options = vec![clearing("Anyone"), choice(AssigneeFilter::Unassigned, "Unassigned")];
    options.extend(
        e.assignees.iter().map(|a| choice(AssigneeFilter::User(a.remote_id), a.name.clone())),
    );
    picker(options, active.as_ref(), Message::SetDashboardAssignee)
}

/// The label picker, by name across the source teams. Absent when no labels exist.
fn label_picker<'a>(e: &Evaluation<'a>, active: Option<&String>) -> Option<Element<'a, Message>> {
    if e.label_names.is_empty() {
        return None;
    }
    let mut options = vec![clearing("Any label")];
    options.extend(e.label_names.iter().map(|n| choice(n.clone(), n.clone())));
    Some(picker(options, active, Message::SetDashboardLabel))
}

/// The look shared by the filter tabs and Mine only (web `dashboard_control_class`).
fn control_style(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let text_color = if active {
            t::text_strong()
        } else if matches!(status, button::Status::Hovered) {
            t::text_body()
        } else {
            t::text_muted()
        };
        button::Style {
            background: active.then(|| iced::Background::Color(t::bg_secondary())),
            text_color,
            border: Border {
                color: if active { t::border() } else { Color::TRANSPARENT },
                width: BORDER,
                radius: radius::MD.into(),
            },
            ..Default::default()
        }
    }
}

/// The filter tabs on the left, Mine only on the right.
fn tabs<'a>(header: &Header) -> Element<'a, Message> {
    let mut r = row![].spacing(space::S1).align_y(Alignment::Center);
    for f in Filter::ALL {
        r = r.push(
            button(text(f.label()).size(type_scale::XS))
                .padding([space::S1, space::S2_5])
                .style(control_style(f == header.filter))
                .on_press(Message::SetDashboardFilter(f)),
        );
    }
    let mine_color = if header.mine { t::text_strong() } else { t::text_muted() };
    let mine = button(
        row![]
            .spacing(space::S1_5)
            .align_y(Alignment::Center)
            .push(svg_icon(ICON_USER, ICON, mine_color))
            .push(text("Mine only").size(type_scale::XS)),
    )
    .padding([space::S1, space::S2_5])
    .style(control_style(header.mine))
    .on_press(Message::ToggleDashboardMine);
    container(row![].align_y(Alignment::Center).push(r).push(horizontal_space()).push(mine))
        .width(Length::Fill)
        .center_y(Length::Fixed(TABS_H))
        .into()
}

/// A ⋮ button with its menu, open when `target` is the open one.
fn kebab<'a>(
    target: MenuTarget,
    open: Option<&MenuTarget>,
    width: f32,
    items: impl FnOnce() -> Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    let is_open = open == Some(&target);
    let anchor = button(svg_icon(ICON_KEBAB, space::S4, t::text_secondary()))
        .padding(space::S1)
        .style(move |_theme: &Theme, status| button::Style {
            background: (is_open || matches!(status, button::Status::Hovered))
                .then(|| iced::Background::Color(t::bg_card_focused())),
            border: Border { radius: radius::MD.into(), ..Default::default() },
            ..Default::default()
        })
        .on_press(Message::ToggleDashboardMenu(target));
    let menu = is_open.then(|| {
        container(Column::with_children(items()).width(Length::Fill))
            .width(Length::Fill)
            .style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::bg_secondary())),
                border: Border { color: t::border(), width: BORDER, radius: radius::MD.into() },
                shadow: iced::Shadow {
                    color: Color::from_rgba(0.0, 0.0, 0.0, 0.4),
                    offset: iced::Vector::new(0.0, space::S1),
                    blur_radius: space::S3,
                },
                ..Default::default()
            })
            .into()
    });
    dropdown(anchor, menu, width, Message::CloseDashboardMenu).into()
}

/// A menu item (web `px-4 py-2 text-sm`); `on_press: None` shows it disabled.
fn menu_item<'a>(label: &'static str, danger: bool, on_press: Option<Message>) -> Element<'a, Message> {
    let enabled = on_press.is_some();
    button(text(label).size(type_scale::SM))
        .on_press_maybe(on_press.map(|m| Message::DashboardMenuPick(Box::new(m))))
        .width(Length::Fill)
        .padding([space::S2, space::S4])
        .style(move |_theme: &Theme, status| button::Style {
            background: matches!(status, button::Status::Hovered)
                .then(|| iced::Background::Color(t::bg_card_focused())),
            text_color: if !enabled {
                t::text_muted()
            } else if danger {
                t::RED_400
            } else {
                t::text_body()
            },
            border: Border { radius: radius::MD.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

/// The top bar's ⋮: the web's Add group, Rename and Delete, plus the desktop's
/// Home page and Reset actions.
pub fn dashboard_menu<'a>(
    dashboard: &'a Dashboard,
    open: Option<&MenuTarget>,
    read_only: bool,
    is_home: bool,
) -> Element<'a, Message> {
    let writable = !read_only;
    kebab(MenuTarget::Dashboard, open, DASHBOARD_MENU_W, || {
        vec![
            menu_item("Add group", false, writable.then_some(Message::OpenNewGroup)),
            menu_item("Rename", false, writable.then(|| Message::OpenEditDashboard(dashboard.id.clone()))),
            menu_item(
                if is_home { "Home page" } else { "Set as Home page" },
                false,
                (writable && !is_home).then(|| Message::SetDashboardAsHome(dashboard.id.clone())),
            ),
            menu_item("Reset filters", false, Some(Message::ResetDashboard)),
            menu_item("Delete", true, writable.then(|| Message::ConfirmDeleteDashboard(dashboard.id.clone()))),
        ]
    })
}

/// A group's ⋮: Edit, Move up, Move down, Delete.
fn group_menu<'a>(group_id: &str, index: usize, count: usize, state: &ViewState<'_>) -> Element<'a, Message> {
    let writable = !state.read_only;
    let id = group_id.to_string();
    kebab(MenuTarget::Group(id.clone()), state.open_menu, GROUP_MENU_W, move || {
        vec![
            menu_item("Edit", false, writable.then(|| Message::OpenEditGroup(id.clone()))),
            menu_item(
                "Move up",
                false,
                (writable && index > 0).then(|| Message::MoveDashboardGroup(id.clone(), MoveDirection::Up)),
            ),
            menu_item(
                "Move down",
                false,
                (writable && index + 1 < count)
                    .then(|| Message::MoveDashboardGroup(id.clone(), MoveDirection::Down)),
            ),
            menu_item("Delete", true, writable.then(|| Message::ConfirmDeleteDashboardGroup(id.clone()))),
        ]
    })
}

/// The web's message for a group with nothing to show under this filter.
fn empty_message(filter: Filter, mine: bool) -> String {
    let base = match filter {
        Filter::Today => "Nothing due today",
        Filter::Week => "Nothing due this week",
        Filter::Hot => "No urgent or high-priority issues",
        Filter::Open => "No open issues",
    };
    let suffix = if mine { " assigned to you" } else { "" };
    let party = if filter == Filter::Today { " 🎉" } else { "" };
    format!("{base}{suffix}{party}")
}

/// A 1px line across the card.
fn divider<'a>() -> Element<'a, Message> {
    container(Space::with_height(BORDER))
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::border())),
            ..Default::default()
        })
        .into()
}

/// One group: a bordered card with its header and, when expanded, its rows.
fn group_card<'a>(
    g: &GroupResult<'a>,
    index: usize,
    e: &Evaluation<'a>,
    data: &CardData<'a>,
    header: &Header,
    state: &ViewState<'_>,
) -> Element<'a, Message> {
    let is_collapsed = state.collapsed.contains(&g.group.id);
    let mut card: Column<'a, Message> = column![]
        .width(Length::Fill)
        .push(group_header(g, index, e.groups.len(), is_collapsed, state));
    if !is_collapsed {
        card = card.push(divider());
        if g.issues.is_empty() {
            card = card.push(empty_line(g, header, state.read_only));
        }
        let last = g.issues.len().saturating_sub(1);
        for (issue_idx, issue) in g.issues.iter().copied().enumerate() {
            if issue_idx > 0 {
                card = card.push(divider());
            }
            let focused = index == state.focused_group && issue_idx == state.focused_issue;
            card = card.push(issue_row(issue, data, e, focused, issue_idx == last, (index, issue_idx)));
        }
    }
    container(card)
        .width(Length::Fill)
        .padding(BORDER)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_secondary())),
            border: Border { color: t::border(), width: BORDER, radius: radius::XL.into() },
            ..Default::default()
        })
        .into()
}

/// The body of a group without rows, fixed at `EMPTY_H`.
fn empty_line<'a>(g: &GroupResult<'a>, header: &Header, read_only: bool) -> Element<'a, Message> {
    let edit = |label: &'static str| {
        button(text(label).size(type_scale::SM))
            .on_press_maybe((!read_only).then(|| Message::OpenEditGroup(g.group.id.clone())))
            .padding(0)
            .style(|_theme: &Theme, _status| button::Style {
                text_color: t::accent(),
                ..Default::default()
            })
    };
    let (message, action) = if g.inaccessible {
        (
            "This group's teams, projects and labels are no longer available.".to_string(),
            Some(edit("Edit group")),
        )
    } else if g.no_sources {
        ("No sources yet.".to_string(), Some(edit("Add sources")))
    } else {
        (empty_message(header.filter, header.mine), None)
    };
    container(
        row![]
            .spacing(space::S1_5)
            .align_y(Alignment::Center)
            .push(truncated(message, type_scale::SM, t::text_muted()))
            .push_maybe(action),
    )
    .padding([0.0, space::S4])
    .center_y(Length::Fixed(EMPTY_H))
    .width(Length::Fill)
    .into()
}

/// A group's header: dot, name, count and description, with the chevron; a
/// press collapses it. The ⋮ sits beside it so opening the menu doesn't toggle.
fn group_header<'a>(
    g: &GroupResult<'a>,
    index: usize,
    count: usize,
    is_collapsed: bool,
    state: &ViewState<'_>,
) -> Element<'a, Message> {
    let color = t::hex_to_color(&g.group.color).unwrap_or_else(t::accent);
    let swatch = container(Space::new(DOT, DOT)).style(move |_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(color)),
        border: Border { radius: radius::FULL.into(), ..Default::default() },
        ..Default::default()
    });
    let count_pill = container(text(g.issues.len().to_string()).size(COUNT_SIZE).color(t::text_muted()))
        .padding([space::S0_5, space::S1_5])
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_primary())),
            border: Border { color: t::border(), width: BORDER, radius: radius::SM.into() },
            ..Default::default()
        });
    let name = row![]
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push(swatch)
        .push(
            text(g.group.name.clone())
                .size(type_scale::SM)
                .font(fonts::bold_active())
                .color(t::text_strong()),
        )
        .push(count_pill);
    let has_description = !g.group.description.is_empty();
    let height = if has_description { HEADER_DESC_H } else { HEADER_H };
    let title = column![].spacing(space::S1).width(Length::Fill).push(name).push_maybe(
        has_description.then(|| {
            truncated(g.group.description.clone(), type_scale::XS, t::text_muted())
        }),
    );
    let chevron = if is_collapsed { ICON_CHEVRON_DOWN } else { ICON_CHEVRON_UP };
    let summary = mouse_area(
        container(
            row![]
                .spacing(space::S3)
                .align_y(Alignment::Center)
                .push(title)
                .push(svg_icon(chevron, space::S4, t::text_muted())),
        )
        .padding(Padding { left: space::S4, right: space::S2, ..Padding::ZERO })
        .center_y(Length::Fill)
        .width(Length::Fill)
        .clip(true),
    )
    .on_press(Message::ToggleDashboardGroup(g.group.id.clone()))
    .interaction(iced::mouse::Interaction::Pointer);
    container(
        row![]
            .align_y(Alignment::Center)
            .push(summary)
            .push(group_menu(&g.group.id, index, count, state)),
    )
    .padding(Padding { right: space::S3, ..Padding::ZERO })
    .height(Length::Fixed(height))
    .width(Length::Fill)
    .into()
}

/// An icon and truncated label in a fixed-width cell (the team and project columns).
fn labelled_cell<'a>(icon: &'static str, label: String, width: f32) -> Element<'a, Message> {
    let color = t::text_secondary();
    container(
        row![]
            .spacing(space::S1_5)
            .align_y(Alignment::Center)
            .push(svg_icon(icon, ICON, color))
            .push(truncated(label, type_scale::XS, color)),
    )
    .width(Length::Fixed(width))
    .clip(true)
    .into()
}

/// A row (web `dashboards/_issue_row`): priority dot, title, team, project,
/// due, assignee and identifier in fixed columns. `at` is `(group, issue)`.
fn issue_row<'a>(
    issue: &'a Issue,
    data: &CardData<'a>,
    e: &Evaluation<'a>,
    focused: bool,
    last: bool,
    at: (usize, usize),
) -> Element<'a, Message> {
    let team = data.teams.iter().find(|t| t.id == issue.team_id);
    let identifier = match (team, issue.team_number) {
        (Some(team), Some(n)) => format!("{}-{n}", team.identifier),
        _ => String::new(),
    };
    let project = issue
        .project_id
        .as_ref()
        .and_then(|id| data.projects.iter().find(|p| p.id == *id));
    let assignee = issue
        .assignee_id
        .as_ref()
        .and_then(|id| data.members.iter().find(|m| m.id == *id));
    let dot_color = t::priority_dot_color(issue.priority_enum());
    let dot = container(Space::new(DOT, DOT)).style(move |_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(dot_color)),
        border: Border { radius: radius::FULL.into(), ..Default::default() },
        ..Default::default()
    });

    let project_cell: Element<'a, Message> = match project {
        Some(p) => labelled_cell(ICON_PROJECT, p.name.clone(), W_PROJECT),
        None => Space::with_width(W_PROJECT).into(),
    };
    let due = dashboard_eval::due_label(issue, data.projects, e.today);
    let due_cell = due.map(|d| {
        let font = if d.inherited { fonts::italic(fonts::active()) } else { fonts::active() };
        text(d.text)
            .size(type_scale::XS)
            .font(font)
            .color(if d.overdue { t::RED_400 } else { t::text_secondary() })
    });

    let cells = row![]
        .spacing(space::S3)
        .align_y(Alignment::Center)
        .push(dot)
        .push(
            container(truncated(issue.title.clone(), type_scale::SM, t::text_strong()))
                .width(Length::Fill),
        )
        .push(labelled_cell(
            ICON_TEAM,
            team.map(|t| t.name.clone()).unwrap_or_default(),
            W_TEAM,
        ))
        .push(project_cell)
        .push(
            container(row![].push_maybe(due_cell))
                .width(Length::Fixed(W_DUE))
                .clip(true),
        )
        .push(
            container(row![].push_maybe(assignee.map(|m| util::avatar(&m.name))))
                .center_x(Length::Fixed(W_ASSIGNEE)),
        )
        .push(
            container(
                text(identifier)
                    .size(type_scale::XS)
                    .font(fonts::code())
                    .color(t::text_secondary()),
            )
            .width(Length::Fixed(W_ID))
            .align_x(Horizontal::Right)
            .clip(true),
        );

    let marker = container(Space::new(MARKER_W, space::S4)).style(move |_theme: &Theme| container::Style {
        background: focused.then(|| iced::Background::Color(t::accent())),
        border: Border { radius: radius::XS.into(), ..Default::default() },
        ..Default::default()
    });
    let content = row![]
        .align_y(Alignment::Center)
        .push(Space::with_width(MARKER_INSET))
        .push(marker)
        .push(Space::with_width(space::S4 - MARKER_INSET - MARKER_W))
        .push(cells)
        .push(Space::with_width(space::S4))
        .height(Length::Fill);

    let (group, row_index) = at;
    button(content)
        .padding(0)
        .width(Length::Fill)
        .height(Length::Fixed(scroll::LIST_ROW_H))
        .style(move |_theme: &Theme, status| button::Style {
            background: (focused || matches!(status, button::Status::Hovered))
                .then(|| iced::Background::Color(t::bg_card_focused())),
            border: Border {
                // The last row's fill stays inside the card's rounded corners.
                radius: if last {
                    iced::border::bottom(radius::XL - BORDER)
                } else {
                    0.0.into()
                },
                ..Default::default()
            },
            ..Default::default()
        })
        .on_press(Message::CardPressed(group, row_index))
        .into()
}

/// A dashboard without groups (web's `dashboard-no-groups` card).
fn no_groups<'a>(read_only: bool) -> Element<'a, Message> {
    container(
        column![]
            .spacing(space::S1)
            .align_x(Alignment::Center)
            .push(
                text("Add a group to start pulling in issues")
                    .size(type_scale::SM)
                    .color(t::text_body()),
            )
            .push(
                text("e.g. Launch: Web, Mobile and API teams")
                    .size(type_scale::XS)
                    .color(t::text_muted()),
            )
            .push(Space::with_height(space::S3))
            .push(
                button(text("Add group").size(type_scale::XS))
                    .on_press_maybe((!read_only).then_some(Message::OpenNewGroup))
                    .padding([space::S1_5, space::S3])
                    .style(|_theme: &Theme, status| button::Style {
                        background: matches!(status, button::Status::Hovered)
                            .then(|| iced::Background::Color(t::accent())),
                        text_color: if matches!(status, button::Status::Hovered) {
                            t::on_accent(status)
                        } else {
                            t::text_primary()
                        },
                        border: Border { color: t::accent(), width: BORDER, radius: radius::MD.into() },
                        ..Default::default()
                    }),
            ),
    )
    .padding(space::S8)
    .center_x(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        border: Border { color: t::border(), width: BORDER, radius: radius::XL.into() },
        ..Default::default()
    })
    .into()
}

/// Collapsed group ids, kept per dashboard (local ids) for the session.
pub type CollapsedGroups = HashMap<String, HashSet<String>>;

/// Flip a group's collapsed state on a dashboard. Returns true if it is now collapsed.
pub fn toggle_group(collapsed: &mut CollapsedGroups, dashboard_id: &str, group_id: &str) -> bool {
    let set = collapsed.entry(dashboard_id.to_string()).or_default();
    if set.remove(group_id) {
        false
    } else {
        set.insert(group_id.to_string());
        true
    }
}

fn centered<'a>(msg: &'static str) -> Element<'a, Message> {
    container(text(msg).size(type_scale::SM).color(t::text_muted()))
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_is_per_dashboard_and_reversible() {
        let mut c = CollapsedGroups::new();
        assert!(toggle_group(&mut c, "d1", "g1"));
        assert!(c["d1"].contains("g1"));
        assert!(c.get("d2").is_none_or(|s| s.is_empty()));
        assert!(toggle_group(&mut c, "d2", "g1"));
        assert!(!toggle_group(&mut c, "d1", "g1"));
        assert!(!c["d1"].contains("g1"));
        assert!(c["d2"].contains("g1"));
    }

    #[test]
    fn span_counts_the_toolbar_card_borders_and_row_dividers() {
        let g = |len, collapsed, has_description| SpanGroup { len, collapsed, has_description };
        // Toolbar, top border, header, divider, then two rows and their divider.
        let (top, h) = focused_span(&[g(3, false, false)], 0, 2);
        assert_eq!(h, scroll::LIST_ROW_H);
        assert_eq!(top, 178.0 + 1.0 + 46.0 + 1.0 + 2.0 * (40.0 + 1.0));
        // A description makes only its own header taller.
        let (top, _) = focused_span(&[g(1, false, true)], 0, 0);
        assert_eq!(top, 178.0 + 1.0 + 66.0 + 1.0);
    }

    #[test]
    fn span_skips_collapsed_and_counts_empty_lines() {
        let g = |len, collapsed| SpanGroup { len, collapsed, has_description: false };
        let (top, _) = focused_span(&[g(5, true), g(0, false), g(1, false)], 2, 0);
        let collapsed = 1.0 + 46.0 + 1.0 + 12.0;
        let empty = 1.0 + 46.0 + 1.0 + 44.0 + 1.0 + 12.0;
        assert_eq!(top, 178.0 + collapsed + empty + 1.0 + 46.0 + 1.0);
    }

    #[test]
    fn empty_messages_match_the_web() {
        assert_eq!(empty_message(Filter::Today, false), "Nothing due today 🎉");
        assert_eq!(empty_message(Filter::Today, true), "Nothing due today assigned to you 🎉");
        assert_eq!(empty_message(Filter::Open, true), "No open issues assigned to you");
    }
}
