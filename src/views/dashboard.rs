//! The dashboard view: a toolbar, then one colored section per group. Rows are
//! the list-mode issue rows, so the visible-property toggles apply.
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
use crate::dashboard_eval::{Evaluation, Filter, GroupResult, Header, Sort};
use crate::db::dashboards::{Dashboard, MoveDirection};
use crate::theme as t;
use crate::views::board::CardData;
use crate::scroll;
use crate::views::issue_list;

/// Layout constants the scroll-to-focus math relies on: group headers and
/// empty-state rows have fixed heights, and the list column a fixed gap/padding.
const LIST_SPACING: f32 = 8.0;
const LIST_PAD_TOP: f32 = 8.0;
const HEADER_H: f32 = 32.0;
const EMPTY_H: f32 = 28.0;

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

pub fn view<'a>(
    dashboard: Option<&'a Dashboard>,
    evaluation: &Evaluation<'a>,
    data: CardData<'a>,
    collapsed: &HashSet<String>,
    focused_group: usize,
    focused_issue: usize,
    header: Header,
    read_only: bool,
    is_home: bool,
) -> Element<'a, Message> {
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
            list = list.push(
                container(text(empty_message(g)).size(13).color(t::text_muted()))
                    .padding([4, 16])
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
            .push(controls(dashboard, header, evaluation, read_only, is_home))
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
    read_only: bool,
    is_home: bool,
) -> Element<'a, Message> {
    container(
        row![]
            .spacing(12)
            .align_y(Alignment::Center)
            .push(text(&dashboard.name).size(14).color(t::text_primary()))
            .push(filter_tabs(header.filter))
            .push(mine_toggle(header.mine))
            .push(due_counts(evaluation))
            .push(sort_picker(header.sort))
            .push(team_picker(evaluation, header.team.as_deref()))
            .push(label_picker(evaluation, header.label.as_deref()))
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
        .push(text(format!("{} due today", e.due_today)).size(12).color(t::text_secondary()))
        .push(text(format!("{} overdue", e.overdue)).size(12).color(overdue))
        .into()
}

/// The sort order picker.
fn sort_picker<'a>(active: Sort) -> Element<'a, Message> {
    pick_list(Sort::ALL, Some(active), Message::SetDashboardSort).text_size(13).into()
}

/// The team picker, listing the dashboard's source teams. Hidden when there
/// is only one to pick; "All teams" clears the filter.
fn team_picker<'a>(e: &Evaluation<'a>, active: Option<&str>) -> Element<'a, Message> {
    if e.source_teams.len() < 2 {
        return horizontal_space().width(0).into();
    }
    let mut options = vec![TeamChoice { id: None, label: "All teams".into() }];
    options.extend(e.source_teams.iter().map(|t| TeamChoice {
        id: Some(t.id.clone()),
        label: t.name.clone(),
    }));
    let selected = options.iter().find(|o| o.id.as_deref() == active).cloned();
    pick_list(options, selected, |c| Message::SetDashboardTeam(c.id)).text_size(13).into()
}

/// The label picker, by name across the source teams. Hidden when no labels exist.
fn label_picker<'a>(e: &Evaluation<'a>, active: Option<&str>) -> Element<'a, Message> {
    if e.label_names.is_empty() {
        return horizontal_space().width(0).into();
    }
    let mut options = vec![LabelChoice(None)];
    options.extend(e.label_names.iter().map(|n| LabelChoice(Some(n.clone()))));
    let selected = options.iter().find(|o| o.0.as_deref() == active).cloned();
    pick_list(options, selected, |c| Message::SetDashboardLabel(c.0)).text_size(13).into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LabelChoice(Option<String>);

impl std::fmt::Display for LabelChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_deref().unwrap_or("All labels"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TeamChoice {
    id: Option<String>,
    label: String,
}

impl std::fmt::Display for TeamChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// "Reset": back to the default filter, sort, search and pickers.
fn reset_button<'a>() -> Element<'a, Message> {
    button(text("Reset").size(13))
        .on_press(Message::ResetDashboard)
        .padding([4, 10])
        .style(button::secondary)
        .into()
}

/// "Add group": disabled while the token is read-only.
fn add_group_button<'a>(read_only: bool) -> Element<'a, Message> {
    button(text("+ Add group").size(13))
        .on_press_maybe((!read_only).then_some(Message::OpenNewGroup))
        .padding([4, 10])
        .style(button::secondary)
        .into()
}

/// "Edit": rename the dashboard / change its description; disabled when read-only.
fn edit_button<'a>(dashboard: &Dashboard, read_only: bool) -> Element<'a, Message> {
    button(text("Edit").size(13))
        .on_press_maybe((!read_only).then(|| Message::OpenEditDashboard(dashboard.id.clone())))
        .padding([4, 10])
        .style(button::secondary)
        .into()
}

/// "Set as Home page": disabled while read-only; shows "Home page" once set.
fn set_home_button<'a>(dashboard: &Dashboard, read_only: bool, is_home: bool) -> Element<'a, Message> {
    let label = if is_home { "Home page" } else { "Set as Home page" };
    button(text(label).size(13))
        .on_press_maybe((!read_only && !is_home).then(|| Message::SetDashboardAsHome(dashboard.id.clone())))
        .padding([4, 10])
        .style(button::secondary)
        .into()
}

/// "Delete" for the dashboard (asks to confirm): disabled while read-only.
fn delete_dashboard_button<'a>(id: &str, read_only: bool) -> Element<'a, Message> {
    button(text("Delete").size(13))
        .on_press_maybe((!read_only).then(|| Message::ConfirmDeleteDashboard(id.to_string())))
        .padding([4, 10])
        .style(button::secondary)
        .into()
}

/// The Today / Week / Hot / Open tabs; the active one is highlighted.
fn filter_tabs<'a>(active: Filter) -> Element<'a, Message> {
    let mut r = row![].spacing(4).align_y(Alignment::Center);
    for f in Filter::ALL {
        let color = if f == active { t::text_primary() } else { t::text_muted() };
        r = r.push(
            button(text(f.label()).size(13).color(color))
                .padding([2, 8])
                .style(move |_theme: &Theme, _status| button::Style {
                    background: (f == active)
                        .then(|| iced::Background::Color(t::bg_card_focused())),
                    text_color: color,
                    border: iced::Border { radius: 4.0.into(), ..Default::default() },
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
    button(text("Mine").size(13).color(color))
        .padding([2, 8])
        .style(move |_theme: &Theme, _status| button::Style {
            background: on.then(|| iced::Background::Color(t::bg_card_focused())),
            text_color: color,
            border: iced::Border { radius: 4.0.into(), ..Default::default() },
            ..Default::default()
        })
        .on_press(Message::ToggleDashboardMine)
        .into()
}

/// Why a group shows no rows: no sources, unreachable sources, or no matches.
fn empty_message(g: &GroupResult<'_>) -> &'static str {
    if g.no_sources {
        "No sources yet. Edit this group to add teams, projects or labels."
    } else if g.inaccessible {
        "This group's sources are no longer reachable (left, archived or outside your access). Edit or delete the group."
    } else {
        "No issues"
    }
}

/// A group's section header: color swatch, name, description and issue count.
fn group_header<'a>(
    g: &GroupResult<'a>,
    index: usize,
    count: usize,
    read_only: bool,
    is_collapsed: bool,
) -> Element<'a, Message> {
    let color = t::hex_to_color(&g.group.color).unwrap_or_else(t::accent);
    let swatch = container(text(""))
        .width(Length::Fixed(4.0))
        .height(Length::Fixed(16.0))
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(color)),
            ..Default::default()
        });
    let mut r = row![]
        .spacing(8)
        .align_y(Alignment::Center)
        .push(text(if is_collapsed { "▸" } else { "▾" }).size(12).color(t::text_muted()))
        .push(swatch)
        .push(text(g.group.name.clone()).size(14).color(t::text_primary()))
        .push(text(g.issues.len().to_string()).size(13).color(t::text_muted()));
    if !g.group.description.is_empty() {
        r = r.push(text(g.group.description.clone()).size(12).color(t::text_secondary()));
    }
    r = r
        .push(horizontal_space())
        .push(move_buttons(&g.group.id, index, count, read_only))
        .push(delete_group_button(&g.group.id, read_only));
    mouse_area(
        container(r)
            .padding([6, 4])
            .width(Length::Fill)
            .center_y(Length::Fixed(HEADER_H)),
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

/// "Delete" on a group header (asks to confirm): disabled while read-only.
fn delete_group_button<'a>(group_id: &str, read_only: bool) -> Element<'a, Message> {
    button(text("Delete").size(12))
        .on_press_maybe((!read_only).then(|| Message::ConfirmDeleteDashboardGroup(group_id.to_string())))
        .padding([2, 8])
        .style(button::text)
        .into()
}

/// Move-up / move-down buttons; disabled at the edges and when read-only.
fn move_buttons<'a>(group_id: &str, index: usize, count: usize, read_only: bool) -> Element<'a, Message> {
    let button_for = |label: &'static str, dir: MoveDirection, allowed: bool| {
        let b = button(text(label).size(12)).padding([2, 8]).style(button::text);
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
    container(text(msg).size(14).color(t::text_muted()))
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
        assert_eq!(top, 8.0 + (32.0 + 8.0) + (34.0 + 8.0));
        // collapsed group contributes only its header; empty one header + line
        let (top, _) = focused_span(&[g(5, true), g(0, false), g(1, false)], 2, 0);
        assert_eq!(top, 8.0 + 40.0 + 40.0 + 36.0 + 40.0);
    }
}
