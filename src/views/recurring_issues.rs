//! Recurring issues right pane: a table of the selected team's mirrored
//! recurring issues. Modelled on the Projects table (header row, rule, rows).
//!
//! Extension points for later tickets: add a column by adding a `COL_*` width,
//! a `header_cell` in `view` and a cell in `recurring_row`; add a row action in
//! `recurring_row` (a `mouse_area` / button on the row) with its own `Message`.

use iced::widget::{
    button, column, container, horizontal_rule, mouse_area, row, scrollable, text,
};
use iced::{Alignment, Element, Length, Theme};

use crate::app::Message;
use crate::db::recurring_issues::RecurringIssueRow;
use crate::theme as t;

const COL_SCHEDULE: f32 = 200.0;
const COL_NEXT_RUN: f32 = 150.0;
const COL_STATE: f32 = 110.0;
const COL_ASSIGNEE: f32 = 140.0;
const COL_PROJECT: f32 = 140.0;
const COL_LANE: f32 = 130.0;
const COL_LABELS: f32 = 200.0;
const COL_ACTION: f32 = 150.0;

/// Full-pane table: column header, then one row per recurring issue, or an
/// empty state when the team has none.
pub fn view<'a>(rows: &'a [RecurringIssueRow]) -> Element<'a, Message> {
    let header = row![]
        .spacing(12)
        .padding([8, 16])
        .align_y(Alignment::Center)
        .push(header_cell("Title", Length::Fill))
        .push(header_cell("Schedule", Length::Fixed(COL_SCHEDULE)))
        .push(header_cell("Next run", Length::Fixed(COL_NEXT_RUN)))
        .push(header_cell("State", Length::Fixed(COL_STATE)))
        .push(header_cell("Assignee", Length::Fixed(COL_ASSIGNEE)))
        .push(header_cell("Project", Length::Fixed(COL_PROJECT)))
        .push(header_cell("Lane", Length::Fixed(COL_LANE)))
        .push(header_cell("Labels", Length::Fixed(COL_LABELS)))
        .push(header_cell("", Length::Fixed(COL_ACTION)));

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
            body = body.push(recurring_row(r)).push(divider());
        }
    }

    let toolbar = row![]
        .padding([8, 16])
        .push(container(text("")).width(Length::Fill))
        .push(
            button(text("New recurring issue").size(12))
                .on_press(Message::NewRecurringIssue)
                .padding([4, 10])
                .style(button::secondary),
        );

    container(
        column![]
            .width(Length::Fill)
            .push(toolbar)
            .push(divider())
            .push(header)
            .push(divider())
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

fn divider<'a>() -> Element<'a, Message> {
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

/// Label of the row's pause/resume button: the action it performs.
pub fn toggle_label(active: bool) -> &'static str {
    if active {
        "Pause"
    } else {
        "Resume"
    }
}

/// Row actions: pause or resume, and delete (asks to confirm first).
fn toggle_action<'a>(r: &'a RecurringIssueRow) -> Element<'a, Message> {
    container(
        row![]
            .spacing(6)
            .push(
                button(text(toggle_label(r.active)).size(12))
                    .on_press(Message::ToggleRecurringActive(r.id.clone(), !r.active))
                    .padding([3, 8])
                    .style(button::secondary),
            )
            .push(
                button(text("Delete").size(12))
                    .on_press(Message::DeleteRecurring(r.id.clone()))
                    .padding([3, 8])
                    .style(button::danger),
            ),
    )
    .width(Length::Fixed(COL_ACTION))
    .into()
}

/// "2026-11-06 MT": the next run date and zone; "pending" while an unsynced
/// create or reschedule makes the date untrustworthy; "-" with no date.
pub fn next_run_label(r: &RecurringIssueRow) -> String {
    if r.next_run_pending {
        return "pending".to_string();
    }
    match (&r.next_run_on, &r.time_zone_label) {
        (Some(d), Some(z)) => format!("{d} {z}"),
        (Some(d), None) => d.clone(),
        (None, _) => "-".to_string(),
    }
}

/// Small colored pill for one label (same look as the board's label chips).
fn label_chip<'a>(name: &'a str, hex: Option<&str>) -> Element<'a, Message> {
    let color = crate::views::labels::chip_color(hex);
    container(text(name).size(11).color(color).wrapping(text::Wrapping::None))
        .padding([2, 8])
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color { a: 0.22, ..color })),
            border: iced::Border {
                color,
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        })
        .into()
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

    let schedule = container(
        text(&r.schedule)
            .size(12)
            .color(t::text_secondary())
            .wrapping(text::Wrapping::None),
    )
    .width(Length::Fixed(COL_SCHEDULE));

    let next_run = container(
        text(next_run_label(r))
            .size(12)
            .color(t::text_secondary())
            .wrapping(text::Wrapping::None),
    )
    .width(Length::Fixed(COL_NEXT_RUN));

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

    let assignee = container(
        text(r.assignee.as_deref().unwrap_or(""))
            .size(12)
            .color(t::text_secondary())
            .wrapping(text::Wrapping::None),
    )
    .width(Length::Fixed(COL_ASSIGNEE));

    let project = container(
        text(r.project_name.as_deref().unwrap_or(""))
            .size(12)
            .color(t::text_secondary())
            .wrapping(text::Wrapping::None),
    )
    .width(Length::Fixed(COL_PROJECT));

    let lane = container(
        text(&r.lane)
            .size(12)
            .color(t::text_secondary())
            .wrapping(text::Wrapping::None),
    )
    .width(Length::Fixed(COL_LANE));

    let mut chips = row![].spacing(4).align_y(Alignment::Center);
    for chip in &r.labels {
        chips = chips.push(label_chip(&chip.name, chip.color.as_deref()));
    }
    let labels = container(chips).width(Length::Fixed(COL_LABELS));

    let content = row![]
            .spacing(12)
            .padding([10, 16])
            .align_y(Alignment::Center)
            .push(title)
            .push(schedule)
            .push(next_run)
            .push(state)
            .push(assignee)
            .push(project)
            .push(lane)
            .push(labels)
            .push(toggle_action(r));

    // A click anywhere on the row opens the edit form; the action buttons
    // capture their own presses first.
    mouse_area(container(content).width(Length::Fill))
        .on_press(Message::EditRecurringIssue(r.id.clone()))
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

    fn row_with_next_run(next_run_on: Option<&str>, pending: bool) -> RecurringIssueRow {
        RecurringIssueRow {
            id: "r".into(),
            title: "T".into(),
            active: true,
            schedule: "Every day".into(),
            next_run_on: next_run_on.map(String::from),
            time_zone_label: Some("MT".into()),
            next_run_pending: pending,
            assignee: None,
            project_name: None,
            lane: "First lane".into(),
            labels: Vec::new(),
        }
    }

    #[test]
    fn next_run_shows_pending_instead_of_a_date() {
        assert_eq!(next_run_label(&row_with_next_run(Some("2026-11-06"), true)), "pending");
        assert_eq!(next_run_label(&row_with_next_run(None, true)), "pending");
        assert_eq!(next_run_label(&row_with_next_run(Some("2026-11-06"), false)), "2026-11-06 MT");
        assert_eq!(next_run_label(&row_with_next_run(None, false)), "-");
    }

    #[test]
    fn toggle_label_names_the_action() {
        assert_eq!(toggle_label(true), "Pause");
        assert_eq!(toggle_label(false), "Resume");
    }
}
