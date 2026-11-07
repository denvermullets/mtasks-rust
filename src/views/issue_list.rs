//! List view mode: the same `display::Column`s as the board, rendered as one
//! vertical list. Each column becomes a section header; sub-groups become
//! nested headings. Rows honor the visible-property toggles.
//!
//! Focus uses the board's `(column, flat issue index)` pair, so
//! `App::focused_issue()` and `Message::CardPressed` work unchanged. Keyboard:
//! j/k walk rows across sections, h/l jump between sections (see
//! `display::list_step` / `display::list_jump_section`). Section headers
//! collapse on click (or `c`). Rows and headers have fixed heights and cells
//! have fixed widths, so columns line up and `scroll::list_row_span` can place
//! the focused row for scroll-to-focus.

use std::collections::HashSet;

use iced::widget::{
    column, container, horizontal_rule, mouse_area, row, scrollable, text, Column, Row,
};
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::Message;
use crate::scroll::{track, ScrollKey};
use crate::db::models::Issue;
use crate::fonts;
use crate::display::{CardProperty, Column as BoardColumn, DisplayOptions};
use crate::scroll;
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::board::CardData;
use crate::views::labels as labels_view;
use crate::views::lane_icon;
use crate::views::util;

/// Horizontal padding of the list column and of each row inside it.
const LIST_PAD_X: f32 = 12.0;
const ROW_PAD_X: f32 = space::S4;

/// Fixed cell widths (px) so each property lines up across rows.
const W_PRIORITY: f32 = 16.0;
const W_ID: f32 = 70.0;
const W_STATUS: f32 = 90.0;
const W_LABELS: f32 = 170.0;
const W_PROJECT: f32 = 120.0;
const W_ASSIGNEE: f32 = 110.0;
const W_ESTIMATE: f32 = 44.0;
const W_DATE: f32 = 56.0;

/// `(top, height)` of the focused row, accounting for collapsed sections.
pub fn focused_span(
    columns: &[BoardColumn<'_>],
    collapsed: &HashSet<String>,
    lane: usize,
    issue: usize,
) -> (f32, f32) {
    let sections: Vec<scroll::ListSection> = columns
        .iter()
        .map(|c| scroll::ListSection {
            len: c.issues.len(),
            subhead_starts: c.sections.iter().map(|s| s.start).collect(),
            collapsed: collapsed.contains(&c.title),
        })
        .collect();
    scroll::list_row_span(&sections, lane, issue)
}

fn cell<'a>(content: impl Into<Element<'a, Message>>, width: f32) -> Element<'a, Message> {
    container(content)
        .width(Length::Fixed(width))
        .clip(true)
        .into()
}

fn cell_text<'a>(s: impl text::IntoFragment<'a>, width: f32) -> Element<'a, Message> {
    cell(
        text(s)
            .size(type_scale::XS)
            .color(t::text_muted())
            .wrapping(text::Wrapping::None),
        width,
    )
}

/// A body cell: `XS`, secondary text (web `text-xs text-gray-400`).
fn body_text<'a>(s: impl text::IntoFragment<'a>, width: f32) -> Element<'a, Message> {
    cell(
        text(s)
            .size(type_scale::XS)
            .color(t::text_secondary())
            .wrapping(text::Wrapping::None),
        width,
    )
}

/// Column captions, in the same order/widths as `issue_row`.
pub fn header_row<'a>(d: &DisplayOptions) -> Element<'a, Message> {
    let mut r: Row<'a, Message> = row![].spacing(space::S3).align_y(Alignment::Center);
    if d.shows(CardProperty::Priority) {
        r = r.push(cell_text("", W_PRIORITY));
    }
    if d.shows(CardProperty::Id) {
        r = r.push(cell_text("ID", W_ID));
    }
    if d.shows(CardProperty::Status) {
        r = r.push(cell_text("Status", W_STATUS));
    }
    r = r.push(container(text("Title").size(type_scale::XS).color(t::text_muted())).width(Length::Fill));
    if d.shows(CardProperty::Labels) {
        r = r.push(cell_text("Labels", W_LABELS));
    }
    if d.shows(CardProperty::Project) {
        r = r.push(cell_text("Project", W_PROJECT));
    }
    if d.shows(CardProperty::Assignee) {
        r = r.push(cell_text("Assignee", W_ASSIGNEE));
    }
    if d.shows(CardProperty::Estimate) {
        r = r.push(cell_text("Est.", W_ESTIMATE));
    }
    if d.shows(CardProperty::DueDate) {
        r = r.push(cell_text("Due", W_DATE));
    }
    if d.shows(CardProperty::Created) {
        r = r.push(cell_text("Created", W_DATE));
    }
    if d.shows(CardProperty::Updated) {
        r = r.push(cell_text("Updated", W_DATE));
    }
    // Horizontal inset = list padding + row padding + border (1).
    container(r)
        .padding([6.0, LIST_PAD_X + ROW_PAD_X + 1.0])
        .width(Length::Fill)
        .into()
}

pub fn view<'a>(
    columns: &[BoardColumn<'a>],
    data: CardData<'a>,
    collapsed: &HashSet<String>,
    focused_lane: usize,
    focused_issue: usize,
) -> Element<'a, Message> {
    if columns.is_empty() {
        let msg = if data.lanes.is_empty() {
            "No lanes — select a team"
        } else {
            "No sections to show"
        };
        return container(text(msg).size(type_scale::SM).color(t::text_muted()))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    }

    let mut list: Column<'a, Message> = column![]
        .spacing(scroll::LIST_SPACING)
        .width(Length::Fill)
        .padding([scroll::LIST_PAD_TOP, LIST_PAD_X]);

    for (col_idx, col) in columns.iter().enumerate() {
        let is_collapsed = collapsed.contains(&col.title);
        let marker = if is_collapsed { "▸" } else { "▾" };
        list = list.push(
            mouse_area(
                container(
                    row![]
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(text(marker).size(12).color(t::text_muted()))
                        .push_maybe(col.lane_icon.map(lane_icon::view))
                        .push(text(col.title.clone()).size(14).color(t::text_primary()))
                        .push(
                            text(format!("{}", col.issues.len()))
                                .size(13)
                                .color(t::text_muted()),
                        ),
                )
                .padding([0, 4])
                .center_y(Length::Fixed(scroll::LIST_HEADER_H))
                .width(Length::Fill),
            )
            .on_press(Message::ToggleSection(col.title.clone())),
        );
        if is_collapsed {
            continue;
        }

        for (issue_idx, issue) in col.issues.iter().copied().enumerate() {
            if let Some(section) = col.sections.iter().find(|s| s.start == issue_idx) {
                list = list.push(
                    container(
                        row![]
                            .spacing(6)
                            .push(
                                text(section.title.clone())
                                    .size(12)
                                    .color(t::text_secondary()),
                            )
                            .push(
                                text(format!("{}", section.end - section.start))
                                    .size(12)
                                    .color(t::text_muted()),
                            ),
                    )
                    .padding([4, 16])
                    .height(Length::Fixed(scroll::LIST_SUBHEAD_H)),
                );
            }
            let focused = col_idx == focused_lane && issue_idx == focused_issue;
            list = list.push(issue_row(issue, &data, focused, col_idx, issue_idx));
        }
    }

    let body = track(scrollable(list), ScrollKey::IssueList).height(Length::Fill);

    container(
        column![]
            .push(header_row(data.display))
            .push(horizontal_rule(1))
            .push(body),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_primary())),
        ..Default::default()
    })
    .into()
}

pub fn issue_row<'a>(
    issue: &'a Issue,
    data: &CardData<'a>,
    focused: bool,
    col_idx: usize,
    issue_idx: usize,
) -> Element<'a, Message> {
    let d = data.display;
    let mut r: Row<'a, Message> = row![].spacing(space::S3).align_y(Alignment::Center);

    if d.shows(CardProperty::Priority) {
        r = r.push(cell(
            text("△")
                .size(type_scale::XS)
                .color(t::priority_color(issue.priority_enum())),
            W_PRIORITY,
        ));
    }
    if d.shows(CardProperty::Id) {
        let ident = data
            .teams
            .iter()
            .find(|t| t.id == issue.team_id)
            .map(|t| t.identifier.as_str())
            .unwrap_or("---");
        let tag = issue
            .team_number
            .map(|n| format!("{ident}-{n}"))
            .unwrap_or_default();
        r = r.push(cell(
            text(tag)
                .size(type_scale::XS)
                .font(fonts::code())
                .color(t::text_secondary())
                .wrapping(text::Wrapping::None),
            W_ID,
        ));
    }
    if d.shows(CardProperty::Status) {
        let name = data
            .lanes
            .iter()
            .find(|l| l.id == issue.lane_id)
            .map(|l| l.name.clone())
            .unwrap_or_default();
        r = r.push(cell(
            text(name)
                .size(type_scale::XS)
                .color(t::text_secondary())
                .wrapping(text::Wrapping::None),
            W_STATUS,
        ));
    }
    r = r.push(
        container(
            text(&issue.title)
                .size(type_scale::SM)
                .color(t::text_primary())
                .wrapping(text::Wrapping::None),
        )
        .width(Length::Fill)
        .clip(true),
    );

    if d.shows(CardProperty::Labels) {
        let mut chips: Row<'a, Message> = row![].spacing(4);
        if let Some(labels) = data.issue_labels.get(&issue.id) {
            for label in labels {
                chips = chips.push(labels_view::static_chip(label));
            }
        }
        r = r.push(cell(chips, W_LABELS));
    }
    if d.shows(CardProperty::Project) {
        let name = issue
            .project_id
            .as_ref()
            .and_then(|id| data.projects.iter().find(|p| p.id == *id))
            .map(|p| p.name.clone())
            .unwrap_or_default();
        r = r.push(body_text(name, W_PROJECT));
    }
    if d.shows(CardProperty::Assignee) {
        let name = issue
            .assignee_id
            .as_ref()
            .and_then(|id| data.members.iter().find(|m| m.id == *id))
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "Unassigned".to_string());
        r = r.push(body_text(name, W_ASSIGNEE));
    }
    if d.shows(CardProperty::Estimate) {
        r = r.push(body_text(
            issue
                .estimate
                .map(util::format_estimate)
                .unwrap_or_default(),
            W_ESTIMATE,
        ));
    }
    if d.shows(CardProperty::DueDate) {
        r = r.push(body_text(
            issue
                .due_date
                .as_deref()
                .map(util::short_date)
                .unwrap_or_default(),
            W_DATE,
        ));
    }
    if d.shows(CardProperty::Created) {
        r = r.push(body_text(util::short_date(&issue.created_at), W_DATE));
    }
    if d.shows(CardProperty::Updated) {
        r = r.push(body_text(util::short_date(&issue.updated_at), W_DATE));
    }

    let hovered = data.hovered == Some(issue.id.as_str());
    let bg = if focused || hovered {
        t::bg_card_focused()
    } else {
        t::bg_primary()
    };
    // Unfocused rows keep a transparent 1px border so the row width and the
    // header inset don't change with focus.
    let border_color = if focused { t::accent() } else { iced::Color::TRANSPARENT };

    let row = mouse_area(
        container(r)
            .padding([0.0, ROW_PAD_X])
            .center_y(Length::Fixed(scroll::LIST_ROW_H))
            .width(Length::Fill)
            .style(move |_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(bg)),
                border: Border {
                    color: border_color,
                    width: 1.0,
                    radius: radius::MD.into(),
                },
                ..Default::default()
            }),
    )
    .on_press(Message::CardPressed(col_idx, issue_idx));
    crate::views::row_hover::row_hover(row, issue.id.clone(), Message::CardHover).into()
}
