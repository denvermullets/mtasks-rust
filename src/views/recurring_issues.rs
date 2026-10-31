//! Recurring issues right pane: a table of the selected team's mirrored
//! recurring issues. Modelled on the Projects table (header row, rule, rows).
//!
//! Extension points for later tickets: add a column by adding a `COL_*` width,
//! a `header_cell` in `view` and a cell in `recurring_row`; add a row action in
//! `recurring_row` (a `mouse_area` / button on the row) with its own `Message`.

use iced::widget::{column, container, horizontal_rule, row, scrollable, text};
use iced::{Alignment, Element, Length, Theme};

use crate::app::Message;
use crate::db::recurring_issues::RecurringIssueRow;
use crate::theme as t;

const COL_STATE: f32 = 110.0;
const COL_PROJECT: f32 = 140.0;

/// Full-pane table: column header, then one row per recurring issue, or an
/// empty state when the team has none.
pub fn view<'a>(rows: &'a [RecurringIssueRow]) -> Element<'a, Message> {
    let header = row![]
        .spacing(12)
        .padding([8, 16])
        .align_y(Alignment::Center)
        .push(header_cell("Title", Length::Fill))
        .push(header_cell("State", Length::Fixed(COL_STATE)))
        .push(header_cell("Project", Length::Fixed(COL_PROJECT)));

    let mut body = column![].width(Length::Fill);
    if rows.is_empty() {
        body = body.push(
            container(
                text("No recurring issues yet")
                    .size(14)
                    .color(t::text_muted()),
            )
            .center_x(Length::Fill)
            .padding(40),
        );
    } else {
        for r in rows {
            body = body.push(recurring_row(r)).push(rule());
        }
    }

    container(
        column![]
            .width(Length::Fill)
            .push(header)
            .push(rule())
            .push(scrollable(body).height(Length::Fill)),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_primary())),
        ..Default::default()
    })
    .into()
}

fn rule<'a>() -> Element<'a, Message> {
    container(horizontal_rule(1))
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::border())),
            ..Default::default()
        })
        .into()
}

fn header_cell<'a>(label: &'a str, width: Length) -> Element<'a, Message> {
    container(text(label).size(11).color(t::text_muted()))
        .width(width)
        .into()
}

/// "Active" or "Paused" label for a row's state.
pub fn state_label(active: bool) -> &'static str {
    if active {
        "Active"
    } else {
        "Paused"
    }
}

fn recurring_row<'a>(r: &'a RecurringIssueRow) -> Element<'a, Message> {
    let title = container(
        text(&r.title)
            .size(14)
            .color(if r.active {
                t::text_primary()
            } else {
                t::text_secondary()
            })
            .wrapping(text::Wrapping::None),
    )
    .width(Length::Fill);

    let dot = if r.active {
        t::priority_low()
    } else {
        t::text_muted()
    };
    let state = container(
        row![]
            .spacing(6)
            .align_y(Alignment::Center)
            .push(text("\u{25CF}").size(9).color(dot))
            .push(text(state_label(r.active)).size(12).color(t::text_secondary())),
    )
    .width(Length::Fixed(COL_STATE));

    let project = container(
        text(r.project_name.as_deref().unwrap_or(""))
            .size(12)
            .color(t::text_secondary())
            .wrapping(text::Wrapping::None),
    )
    .width(Length::Fixed(COL_PROJECT));

    container(
        row![]
            .spacing(12)
            .padding([10, 16])
            .align_y(Alignment::Center)
            .push(title)
            .push(state)
            .push(project),
    )
    .width(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_label_reads_active_or_paused() {
        assert_eq!(state_label(true), "Active");
        assert_eq!(state_label(false), "Paused");
    }
}
