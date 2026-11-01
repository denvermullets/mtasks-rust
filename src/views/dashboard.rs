//! The dashboard view: a toolbar, then one colored block per group. Rows are
//! the list-mode issue rows with a fixed display (`DisplayOptions::dashboard_rows`),
//! so the board's visible-property toggles don't apply.
//!
//! Focus uses the same `(group index, issue index in group)` pair as the list.

use std::collections::{HashMap, HashSet};

use iced::widget::{
    button, column, container, horizontal_rule, horizontal_space, mouse_area, pick_list, row,
    scrollable, text,
    Column,
};
use iced::{Alignment, Element, Length, Theme};

use crate::app::Message;
use crate::dashboard_eval::{AssigneeFilter, Evaluation, Filter, GroupResult, Header, Sort};
use crate::db::dashboards::{Dashboard, MoveDirection};
use crate::fonts;
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::board::CardData;
use crate::scroll;
use crate::views::issue_list;

/// Layout constants the scroll-to-focus math relies on: group headers and
/// empty-state rows have fixed heights, and the list column a fixed gap/padding.
const LIST_SPACING: f32 = 8.0;
const LIST_PAD_TOP: f32 = 8.0;
const HEADER_H: f32 = 32.0;
/// An `SM` line plus the web empty message's `py-3`.
const EMPTY_H: f32 = type_scale::SM * type_scale::LH_SM + 2.0 * space::S3;

/// Scrollable id of the dashboard's group list.
pub fn scroll_id() -> scrollable::Id {
    scrollable::Id::new("dashboard-list")
}

/// What the scroll math needs to know about one group.
pub struct SpanGroup {
    pub len: usize,
    pub collapsed: bool,
}

/// `(top, height)` of the row `(group, issue)`: every group is a header, then
/// either its rows or (when expanded and empty) one empty-state line.
pub fn focused_span(groups: &[SpanGroup], group: usize, issue: usize) -> (f32, f32) {
    let mut heights: Vec<f32> = Vec::new();
    let mut target = 0;
    for (i, g) in groups.iter().enumerate() {
        heights.push(HEADER_H);
        if g.collapsed {
            continue;
        }
        if g.len == 0 {
            heights.push(EMPTY_H);
        }
        for r in 0..g.len {
            if i == group && r == issue {
                target = heights.len();
            }
            heights.push(scroll::LIST_ROW_H);
        }
    }
    (scroll::item_top(&heights, target, LIST_SPACING, LIST_PAD_TOP), scroll::LIST_ROW_H)
}

/// What the open dashboard's view needs beyond its content: which groups are
/// collapsed, where focus is, and whether writes and the Home marker apply.
pub struct ViewState<'a> {
    pub collapsed: &'a HashSet<String>,
    pub focused_group: usize,
    pub focused_issue: usize,
    pub read_only: bool,
    pub is_home: bool,
}

pub fn view<'a>(
    dashboard: Option<&'a Dashboard>,
    evaluation: &Evaluation<'a>,
    data: CardData<'a>,
    header: Header,
    state: &ViewState<'_>,
) -> Element<'a, Message> {
    let ViewState { collapsed, focused_group, focused_issue, read_only, .. } = *state;
    let Some(dashboard) = dashboard else {
        return centered("Dashboard not found");
    };

    let mut list: Column<'a, Message> = column![]
        .spacing(LIST_SPACING)
        .width(Length::Fill)
        .padding([LIST_PAD_TOP, 12.0]);
    for (group_idx, g) in evaluation.groups.iter().enumerate() {
        let is_collapsed = collapsed.contains(&g.group.id);
        list = list.push(group_header(g, group_idx, evaluation.groups.len(), read_only, is_collapsed));
        if is_collapsed {
            continue;
        }
        if g.issues.is_empty() {
            let mut line = row![]
                .spacing(8)
                .align_y(Alignment::Center)
                .push(text(empty_message(g)).size(type_scale::SM).color(t::text_muted()));
            if g.no_sources {
                line = line.push(add_sources_button(&g.group.id, read_only));
            }
            list = list.push(
                container(line)
                    .padding([space::S3, space::S4])
                    .height(Length::Fixed(EMPTY_H)),
            );
        }
        for (issue_idx, issue) in g.issues.iter().copied().enumerate() {
            let focused = group_idx == focused_group && issue_idx == focused_issue;
            list = list.push(issue_list::issue_row(issue, &data, focused, group_idx, issue_idx));
        }
    }

    container(
        column![]
            .push(controls(dashboard, header, evaluation, state))
            .push(horizontal_rule(1))
            .push(issue_list::header_row(data.display))
            .push(
                scrollable(list)
                    .id(scroll_id())
                    .on_scroll(|vp| {
                        Message::Scrolled(
                            issue_list::SCROLL_KEY,
                            vp.absolute_offset().y,
                            vp.bounds().height,
                        )
                    })
                    .height(Length::Fill),
            ),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_primary())),
        ..Default::default()
    })
    .into()
}

/// The dashboard's header controls. Later tickets push into this row: filter
/// tabs, Mine, sort, counts and the pickers. Only the "Open" lens exists now.
fn controls<'a>(
    dashboard: &'a Dashboard,
    header: Header,
    evaluation: &Evaluation<'a>,
    state: &ViewState<'_>,
) -> Element<'a, Message> {
    let ViewState { read_only, is_home, .. } = *state;
    container(
        row![]
            .spacing(12)
            .align_y(Alignment::Center)
            .push(text(&dashboard.name).size(type_scale::SM).color(t::text_primary()))
            .push(filter_tabs(header.filter))
            .push(mine_toggle(header.mine))
            .push(due_counts(evaluation))
            .push(sort_picker(header.sort))
            .push(team_picker(evaluation, header.team.as_ref()))
            .push(assignee_picker(evaluation, header.assignee))
            .push(label_picker(evaluation, header.label.as_ref()))
            .push(status_picker(evaluation, header.status.as_ref()))
            .push(reset_button())
            .push(add_group_button(read_only))
            .push(edit_button(dashboard, read_only))
            .push(set_home_button(dashboard, read_only, is_home))
            .push(delete_dashboard_button(&dashboard.id, read_only)),
    )
    .padding([8, 16])
    .width(Length::Fill)
    .into()
}

/// "N due today" and "N overdue" for the whole dashboard.
fn due_counts<'a>(e: &Evaluation<'a>) -> Element<'a, Message> {
    let overdue = if e.overdue > 0 { t::danger() } else { t::text_muted() };
    row![]
        .spacing(10)
        .push(text(format!("{} due today", e.due_today)).size(type_scale::XS).color(t::text_secondary()))
        .push(text(format!("{} overdue", e.overdue)).size(type_scale::XS).color(overdue))
        .into()
}

/// The sort order picker.
fn sort_picker<'a>(active: Sort) -> Element<'a, Message> {
    pick_list(Sort::ALL, Some(active), Message::SetDashboardSort).text_size(13).into()
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
/// the clearing choice. Hidden (zero-width) when `options` has fewer than
/// `min_options` entries, i.e. nothing real to pick.
fn picker<'a, T>(
    options: Vec<Choice<T>>,
    active: Option<&T>,
    min_options: usize,
    on_pick: impl Fn(Option<T>) -> Message + 'a,
) -> Element<'a, Message>
where
    T: Clone + PartialEq + 'a,
{
    if options.len() < min_options {
        return horizontal_space().width(0).into();
    }
    let selected = options.iter().find(|o| o.value.as_ref() == active).cloned();
    pick_list(options, selected, move |c| on_pick(c.value)).text_size(13).into()
}

fn choice<T>(value: T, label: impl Into<String>) -> Choice<T> {
    Choice { value: Some(value), label: label.into() }
}

fn clearing<T>(label: &str) -> Choice<T> {
    Choice { value: None, label: label.to_string() }
}

/// The team picker, listing the dashboard's source teams. Hidden when there
/// is only one to pick; "All teams" clears the filter.
fn team_picker<'a>(e: &Evaluation<'a>, active: Option<&String>) -> Element<'a, Message> {
    let mut options = vec![clearing("All teams")];
    options.extend(e.source_teams.iter().map(|t| choice(t.id.clone(), t.name.clone())));
    // "All teams" plus at least two teams.
    picker(options, active, 3, Message::SetDashboardTeam)
}

/// The status picker: lane names across the source teams. "Any status" clears it.
fn status_picker<'a>(e: &Evaluation<'a>, active: Option<&String>) -> Element<'a, Message> {
    let mut options = vec![clearing("Any status")];
    options.extend(e.status_options.iter().map(|s| choice(s.clone(), s.clone())));
    picker(options, active, 2, Message::SetDashboardStatus)
}

/// The assignee picker: members of the source teams plus "Unassigned".
/// Hidden when no member is known.
fn assignee_picker<'a>(e: &Evaluation<'a>, active: Option<AssigneeFilter>) -> Element<'a, Message> {
    let mut options = vec![clearing("Anyone")];
    options.extend(
        e.assignees.iter().map(|a| choice(AssigneeFilter::User(a.remote_id), a.name.clone())),
    );
    if !e.assignees.is_empty() {
        options.push(choice(AssigneeFilter::Unassigned, "Unassigned"));
    }
    picker(options, active.as_ref(), 2, Message::SetDashboardAssignee)
}

/// The label picker, by name across the source teams. Hidden when no labels exist.
fn label_picker<'a>(e: &Evaluation<'a>, active: Option<&String>) -> Element<'a, Message> {
    let mut options = vec![clearing("All labels")];
    options.extend(e.label_names.iter().map(|n| choice(n.clone(), n.clone())));
    picker(options, active, 2, Message::SetDashboardLabel)
}

/// The secondary button style with the toolbar's `rounded-md` corners.
fn toolbar_style(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::secondary(theme, status);
    style.border.radius = radius::MD.into();
    style
}

/// "Reset": back to the default filter, sort, search and pickers.
fn reset_button<'a>() -> Element<'a, Message> {
    button(text("Reset").size(type_scale::XS))
        .on_press(Message::ResetDashboard)
        .padding([space::S1_5, space::S3])
        .style(toolbar_style)
        .into()
}

/// "Add group": disabled while the token is read-only.
fn add_group_button<'a>(read_only: bool) -> Element<'a, Message> {
    button(text("+ Add group").size(type_scale::XS))
        .on_press_maybe((!read_only).then_some(Message::OpenNewGroup))
        .padding([space::S1_5, space::S3])
        .style(toolbar_style)
        .into()
}

/// "Edit": rename the dashboard / change its description; disabled when read-only.
fn edit_button<'a>(dashboard: &Dashboard, read_only: bool) -> Element<'a, Message> {
    button(text("Edit").size(type_scale::XS))
        .on_press_maybe((!read_only).then(|| Message::OpenEditDashboard(dashboard.id.clone())))
        .padding([space::S1_5, space::S3])
        .style(toolbar_style)
        .into()
}

/// "Set as Home page": disabled while read-only; shows "Home page" once set.
fn set_home_button<'a>(dashboard: &Dashboard, read_only: bool, is_home: bool) -> Element<'a, Message> {
    let label = if is_home { "Home page" } else { "Set as Home page" };
    button(text(label).size(type_scale::XS))
        .on_press_maybe((!read_only && !is_home).then(|| Message::SetDashboardAsHome(dashboard.id.clone())))
        .padding([space::S1_5, space::S3])
        .style(toolbar_style)
        .into()
}

/// "Delete" for the dashboard (asks to confirm): disabled while read-only.
fn delete_dashboard_button<'a>(id: &str, read_only: bool) -> Element<'a, Message> {
    button(text("Delete").size(type_scale::XS))
        .on_press_maybe((!read_only).then(|| Message::ConfirmDeleteDashboard(id.to_string())))
        .padding([space::S1_5, space::S3])
        .style(toolbar_style)
        .into()
}

/// The Today / Week / Hot / Open tabs; the active one is highlighted.
fn filter_tabs<'a>(active: Filter) -> Element<'a, Message> {
    let mut r = row![].spacing(4).align_y(Alignment::Center);
    for f in Filter::ALL {
        let color = if f == active { t::text_primary() } else { t::text_muted() };
        r = r.push(
            button(text(f.label()).size(type_scale::XS).color(color))
                .padding([space::S0_5, space::S2])
                .style(move |_theme: &Theme, _status| button::Style {
                    background: (f == active)
                        .then(|| iced::Background::Color(t::bg_card_focused())),
                    text_color: color,
                    border: iced::Border { radius: radius::SM.into(), ..Default::default() },
                    ..Default::default()
                })
                .on_press(Message::SetDashboardFilter(f)),
        );
    }
    r.into()
}

/// The Mine toggle; highlighted while on.
fn mine_toggle<'a>(on: bool) -> Element<'a, Message> {
    let color = if on { t::text_primary() } else { t::text_muted() };
    button(text("Mine").size(type_scale::XS).color(color))
        .padding([space::S0_5, space::S2])
        .style(move |_theme: &Theme, _status| button::Style {
            background: on.then(|| iced::Background::Color(t::bg_card_focused())),
            text_color: color,
            border: iced::Border { radius: radius::SM.into(), ..Default::default() },
            ..Default::default()
        })
        .on_press(Message::ToggleDashboardMine)
        .into()
}

/// Why a group shows no rows: no sources, unreachable sources, or no matches.
fn empty_message(g: &GroupResult<'_>) -> &'static str {
    if g.no_sources {
        "No sources yet."
    } else if g.inaccessible {
        "This group's sources are no longer reachable (left, archived or outside your access). Edit or delete the group."
    } else {
        "No issues"
    }
}

/// A group's header row: color swatch, name, description and issue count.
fn group_header<'a>(
    g: &GroupResult<'a>,
    index: usize,
    count: usize,
    read_only: bool,
    is_collapsed: bool,
) -> Element<'a, Message> {
    let color = t::hex_to_color(&g.group.color).unwrap_or_else(t::accent);
    let swatch = container(text(""))
        .width(Length::Fixed(space::S2))
        .height(Length::Fixed(space::S2))
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(color)),
            border: iced::Border { radius: radius::FULL.into(), ..Default::default() },
            ..Default::default()
        });
    let count_pill = container(
        text(g.issues.len().to_string()).size(type_scale::XS).color(t::text_muted()),
    )
    .padding([space::S0_5, space::S1_5])
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_primary())),
        border: iced::Border {
            color: t::border(),
            width: 1.0,
            radius: radius::SM.into(),
        },
        ..Default::default()
    });
    let mut r = row![]
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push(
            text(if is_collapsed { "▸" } else { "▾" })
                .size(type_scale::XS)
                .color(t::text_muted()),
        )
        .push(swatch)
        .push(
            text(g.group.name.clone())
                .size(type_scale::SM)
                .font(fonts::bold_active())
                .color(t::text_strong()),
        )
        .push(count_pill);
    if !g.group.description.is_empty() {
        r = r.push(text(g.group.description.clone()).size(type_scale::XS).color(t::text_muted()));
    }
    r = r
        .push(horizontal_space())
        .push(edit_group_button(&g.group.id, read_only))
        .push(move_buttons(&g.group.id, index, count, read_only))
        .push(delete_group_button(&g.group.id, read_only));
    mouse_area(
        container(r)
            .padding([6.0, space::S3])
            .width(Length::Fill)
            .center_y(Length::Fixed(HEADER_H))
            .style(move |_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::bg_secondary())),
                border: iced::Border {
                    // Top corners only: expanded rows below are flat. A
                    // collapsed group is just its header, so round all four.
                    radius: if is_collapsed {
                        radius::XL.into()
                    } else {
                        iced::border::top(radius::XL)
                    },
                    ..Default::default()
                },
                ..Default::default()
            }),
    )
        .on_press(Message::ToggleDashboardGroup(g.group.id.clone()))
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

/// "Add sources" on a group with none: the same edit modal; disabled while read-only.
fn add_sources_button<'a>(group_id: &str, read_only: bool) -> Element<'a, Message> {
    button(text("Add sources").size(type_scale::XS))
        .on_press_maybe((!read_only).then(|| Message::OpenEditGroup(group_id.to_string())))
        .padding([space::S0_5, space::S2])
        .style(button::secondary)
        .into()
}

/// "Edit" on a group header: opens the group modal pre-filled; disabled while read-only.
fn edit_group_button<'a>(group_id: &str, read_only: bool) -> Element<'a, Message> {
    button(text("Edit").size(type_scale::XS))
        .on_press_maybe((!read_only).then(|| Message::OpenEditGroup(group_id.to_string())))
        .padding([space::S0_5, space::S2])
        .style(button::text)
        .into()
}

/// "Delete" on a group header (asks to confirm): disabled while read-only.
fn delete_group_button<'a>(group_id: &str, read_only: bool) -> Element<'a, Message> {
    button(text("Delete").size(type_scale::XS))
        .on_press_maybe((!read_only).then(|| Message::ConfirmDeleteDashboardGroup(group_id.to_string())))
        .padding([space::S0_5, space::S2])
        .style(button::text)
        .into()
}

/// Move-up / move-down buttons; disabled at the edges and when read-only.
fn move_buttons<'a>(group_id: &str, index: usize, count: usize, read_only: bool) -> Element<'a, Message> {
    let button_for = |label: &'static str, dir: MoveDirection, allowed: bool| {
        let b = button(text(label).size(type_scale::XS)).padding([space::S0_5, space::S2]).style(button::text);
        if allowed && !read_only {
            b.on_press(Message::MoveDashboardGroup(group_id.to_string(), dir))
        } else {
            b
        }
    };
    row![]
        .spacing(4)
        .push(button_for("Up", MoveDirection::Up, index > 0))
        .push(button_for("Down", MoveDirection::Down, index + 1 < count))
        .into()
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
        assert!(c.get("d2").map_or(true, |s| s.is_empty()));
        assert!(toggle_group(&mut c, "d2", "g1"));
        assert!(!toggle_group(&mut c, "d1", "g1"));
        assert!(!c["d1"].contains("g1"));
        assert!(c["d2"].contains("g1"));
    }

    #[test]
    fn span_skips_collapsed_and_counts_empty_lines() {
        let g = |len, collapsed| SpanGroup { len, collapsed };
        // header, row 0, row 1
        let (top, h) = focused_span(&[g(2, false)], 0, 1);
        assert_eq!(h, scroll::LIST_ROW_H);
        assert_eq!(top, 8.0 + (32.0 + 8.0) + (40.0 + 8.0));
        // collapsed group contributes only its header; empty one header + line
        let (top, _) = focused_span(&[g(5, true), g(0, false), g(1, false)], 2, 0);
        // collapsed header 32+8, empty group header 32+8 and line 44+8, header 32+8
        assert_eq!(top, 8.0 + 40.0 + 40.0 + 52.0 + 40.0);
    }
}
