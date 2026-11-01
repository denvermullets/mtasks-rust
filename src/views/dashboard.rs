//! The dashboard view: a toolbar, then one colored section per group. Rows are
//! the list-mode issue rows, so the visible-property toggles apply.
//!
//! Focus uses the same `(group index, issue index in group)` pair as the list.

use std::collections::{HashMap, HashSet};

use iced::widget::{column, container, horizontal_rule, mouse_area, row, scrollable, text, Column};
use iced::{Alignment, Element, Length, Theme};

use crate::app::Message;
use crate::dashboard_eval::{Evaluation, GroupResult};
use crate::db::dashboards::Dashboard;
use crate::theme as t;
use crate::views::board::CardData;
use crate::views::issue_list;

pub fn view<'a>(
    dashboard: Option<&'a Dashboard>,
    evaluation: &Evaluation<'a>,
    data: CardData<'a>,
    collapsed: &HashSet<String>,
    focused_group: usize,
    focused_issue: usize,
) -> Element<'a, Message> {
    let Some(dashboard) = dashboard else {
        return centered("Dashboard not found");
    };

    let mut list: Column<'a, Message> = column![].spacing(8).width(Length::Fill).padding([8, 12]);
    for (group_idx, g) in evaluation.groups.iter().enumerate() {
        let is_collapsed = collapsed.contains(&g.group.id);
        list = list.push(group_header(g, is_collapsed));
        if is_collapsed {
            continue;
        }
        if g.issues.is_empty() {
            list = list.push(
                container(text("No issues").size(13).color(t::text_muted())).padding([4, 16]),
            );
        }
        for (issue_idx, issue) in g.issues.iter().copied().enumerate() {
            let focused = group_idx == focused_group && issue_idx == focused_issue;
            list = list.push(issue_list::issue_row(issue, &data, focused, group_idx, issue_idx));
        }
    }

    container(
        column![]
            .push(controls(dashboard))
            .push(horizontal_rule(1))
            .push(issue_list::header_row(data.display))
            .push(scrollable(list).height(Length::Fill)),
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
fn controls<'a>(dashboard: &'a Dashboard) -> Element<'a, Message> {
    container(
        row![]
            .spacing(12)
            .align_y(Alignment::Center)
            .push(text(&dashboard.name).size(14).color(t::text_primary()))
            .push(text("Open").size(13).color(t::text_muted())),
    )
    .padding([8, 16])
    .width(Length::Fill)
    .into()
}

/// A group's section header: color swatch, name, description and issue count.
fn group_header<'a>(g: &GroupResult<'a>, is_collapsed: bool) -> Element<'a, Message> {
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
    mouse_area(container(r).padding([6, 4]).width(Length::Fill))
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
}
