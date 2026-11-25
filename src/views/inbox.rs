//! Inbox right pane: one flat list of Notifications, newest first. Unread rows
//! are bold with a dot; the selected row is highlighted. Selection and the
//! filter live in `crate::inbox::InboxState`.

use iced::widget::{button, column, container, mouse_area, row, scrollable, Column};
use crate::views::text;
use iced::widget::text::LineHeight;
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::Message;
use crate::scroll::{track, ScrollKey};
use crate::db::notifications::{Filter, Notification};
use crate::inbox::{empty_message, InboxState};
use crate::fonts;
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::util::relative_time;

/// Height of one row: a `LH_SM` headline line (20), `space::S1`, a `LH_XS` issue
/// line (16) and `space::S3` padding above and below. Keyboard navigation and
/// scroll-to-selected multiply by it; `row_height_matches_its_parts` pins it.
pub const ROW_HEIGHT: f32 = 64.0;

fn filter_tab<'a>(label: &'a str, filter: Filter, active: Filter) -> Element<'a, Message> {
    let on = filter == active;
    let color = if on { t::text_primary() } else { t::text_muted() };
    button(text(label).size(type_scale::XS).color(color))
        .padding([space::S1_5, space::S3])
        .style(move |_theme: &Theme, _status| button::Style {
            background: on.then(|| iced::Background::Color(t::bg_card_focused())),
            text_color: color,
            border: Border {
                radius: radius::SM.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .on_press(Message::InboxSetFilter(filter))
        .into()
}

pub fn view<'a>(state: &'a InboxState) -> Element<'a, Message> {
    let tabs = row![
        filter_tab("Unread", Filter::Unread, state.filter),
        filter_tab("All", Filter::All, state.filter),
        text("[f] switch").size(type_scale::XS).color(t::text_muted()),
    ]
    .spacing(space::S1_5)
    .align_y(Alignment::Center)
    .padding([space::S2, space::S4]);
    column![tabs, list(state)].into()
}

fn list<'a>(state: &'a InboxState) -> Element<'a, Message> {
    if state.items.is_empty() {
        return container(
            text(empty_message(state.filter))
                .size(type_scale::SM)
                .color(t::text_muted()),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into();
    }
    let rows: Vec<Element<'a, Message>> = state
        .items
        .iter()
        .enumerate()
        .map(|(i, n)| notification_row(i, n, i == state.selected))
        .collect();
    track(
        scrollable(Column::with_children(rows).width(Length::Fill)),
        ScrollKey::Inbox,
    )
    .height(Length::Fill)
        .into()
}

fn notification_row<'a>(index: usize, n: &'a Notification, selected: bool) -> Element<'a, Message> {
    let unread = n.is_unread();
    let (font, primary) = if unread {
        (fonts::bold_active(), t::text_body())
    } else {
        let body = t::text_body();
        (fonts::active(), iced::Color { a: body.a * 0.6, ..body })
    };

    let dot = container(text(""))
        .width(Length::Fixed(space::S3))
        .height(Length::Fixed(space::S3))
        .style(move |_theme: &Theme| container::Style {
            background: unread.then(|| iced::Background::Color(t::accent())),
            border: Border {
                radius: radius::FULL.into(),
                ..Default::default()
            },
            ..Default::default()
        });

    // Rails builds `message` with the actor's name already at the front
    // ("Ann commented on JRST-12"), so it is shown as-is.
    let headline = n.message.as_str();
    let issue = match (n.issue_identifier.as_deref(), n.issue_title.as_deref()) {
        (Some(id), Some(title)) => format!("{id}  {title}"),
        (Some(id), None) => id.to_string(),
        (None, Some(title)) => title.to_string(),
        (None, None) => String::new(),
    };

    let body = column![
        text(headline)
            .size(type_scale::SM)
            .line_height(LineHeight::Relative(type_scale::LH_SM))
            .font(font)
            .color(primary)
            .wrapping(text::Wrapping::None),
        text(issue)
            .size(type_scale::XS)
            .line_height(LineHeight::Relative(type_scale::LH_XS))
            .color(t::text_muted())
            .wrapping(text::Wrapping::None),
    ]
    .spacing(space::S1)
    .width(Length::Fill);

    let line = row![
        dot,
        body,
        text(relative_time(&n.created_at))
            .size(type_scale::XS)
            .color(t::text_muted()),
    ]
    .spacing(space::S3)
    .align_y(Alignment::Center);

    let card = container(line)
        .padding([space::S3, space::S4])
        .height(Length::Fixed(ROW_HEIGHT))
        .center_y(Length::Fixed(ROW_HEIGHT))
        .width(Length::Fill)
        .style(move |_theme: &Theme| container::Style {
            background: selected.then(|| iced::Background::Color(t::bg_card_focused())),
            border: Border {
                color: t::border(),
                width: 0.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        });

    mouse_area(card)
        .on_press(Message::InboxOpenAt(index))
        .interaction(iced::mouse::Interaction::Pointer)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_height_matches_its_parts() {
        let parts = type_scale::LH_SM * type_scale::SM
            + space::S1
            + type_scale::LH_XS * type_scale::XS
            + 2.0 * space::S3;
        assert!((ROW_HEIGHT - parts).abs() < 0.5, "{ROW_HEIGHT} vs {parts}");
        assert_eq!(ROW_HEIGHT, 64.0);
    }
}
