//! The dashboard view: a toolbar, then one colored section per group. Rows are
//! the list-mode issue rows, so the visible-property toggles apply.
//!
//! Focus uses the same `(group index, issue index in group)` pair as the list.

use iced::widget::{
    button, column, container, horizontal_rule, horizontal_space, pick_list, row, scrollable, text,
    Column,
};
use iced::{Alignment, Element, Length, Theme};

use crate::app::Message;
use crate::dashboard_eval::{Evaluation, Filter, GroupResult, Header, Sort};
use crate::db::dashboards::{Dashboard, MoveDirection};
use crate::theme as t;
use crate::views::board::CardData;
use crate::views::issue_list;

pub fn view<'a>(
    dashboard: Option<&'a Dashboard>,
    evaluation: &Evaluation<'a>,
    data: CardData<'a>,
    focused_group: usize,
    focused_issue: usize,
    header: Header,
    read_only: bool,
    is_home: bool,
) -> Element<'a, Message> {
    let Some(dashboard) = dashboard else {
        return centered("Dashboard not found");
    };

    let mut list: Column<'a, Message> = column![].spacing(8).width(Length::Fill).padding([8, 12]);
    for (group_idx, g) in evaluation.groups.iter().enumerate() {
        list = list.push(group_header(g, group_idx, evaluation.groups.len(), read_only));
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
            .push(controls(dashboard, header, read_only, is_home))
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
fn controls<'a>(
    dashboard: &'a Dashboard,
    header: Header,
    read_only: bool,
    is_home: bool,
) -> Element<'a, Message> {
    container(
        row![]
            .spacing(12)
            .align_y(Alignment::Center)
            .push(text(&dashboard.name).size(14).color(t::text_primary()))
            .push(filter_tabs(header.filter))
            .push(sort_picker(header.sort))
            .push(add_group_button(read_only))
            .push(set_home_button(dashboard, read_only, is_home)),
    )
    .padding([8, 16])
    .width(Length::Fill)
    .into()
}

/// The sort order picker.
fn sort_picker<'a>(active: Sort) -> Element<'a, Message> {
    pick_list(Sort::ALL, Some(active), Message::SetDashboardSort).text_size(13).into()
}

/// "Add group": disabled while the token is read-only.
fn add_group_button<'a>(read_only: bool) -> Element<'a, Message> {
    button(text("+ Add group").size(13))
        .on_press_maybe((!read_only).then_some(Message::OpenNewGroup))
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

/// A group's section header: color swatch, name, description and issue count.
fn group_header<'a>(
    g: &GroupResult<'a>,
    index: usize,
    count: usize,
    read_only: bool,
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
        .push(swatch)
        .push(text(g.group.name.clone()).size(14).color(t::text_primary()))
        .push(text(g.issues.len().to_string()).size(13).color(t::text_muted()));
    if !g.group.description.is_empty() {
        r = r.push(text(g.group.description.clone()).size(12).color(t::text_secondary()));
    }
    r = r.push(horizontal_space()).push(move_buttons(&g.group.id, index, count, read_only));
    container(r).padding([6, 4]).width(Length::Fill).into()
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
