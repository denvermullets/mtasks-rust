//! Inbox right pane: one flat list of Notifications, newest first. Unread rows
//! are bold with a dot; the selected row is highlighted. Selection and the
//! filter live in `crate::inbox::InboxState`.

use iced::font::Weight;
use iced::widget::{column, container, mouse_area, row, scrollable, text, Column};
use iced::{Alignment, Border, Element, Font, Length, Theme};

use crate::app::Message;
use crate::db::notifications::Notification;
use crate::inbox::{empty_message, InboxState};
use crate::theme as t;
use crate::views::util::relative_time;

const ROW_HEIGHT: f32 = 52.0;

pub fn view<'a>(state: &'a InboxState) -> Element<'a, Message> {
    if state.items.is_empty() {
        return container(
            text(empty_message(state.filter))
                .size(14)
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
    scrollable(Column::with_children(rows).width(Length::Fill))
        .height(Length::Fill)
        .into()
}

fn notification_row<'a>(index: usize, n: &'a Notification, selected: bool) -> Element<'a, Message> {
    let unread = n.is_unread();
    let font = if unread {
        Font {
            weight: Weight::Bold,
            ..Font::DEFAULT
        }
    } else {
        Font::DEFAULT
    };
    let primary = if unread { t::text_primary() } else { t::text_secondary() };

    let dot = text(if unread { "\u{25CF}" } else { "" })
        .size(10)
        .color(t::accent())
        .width(Length::Fixed(14.0));

    let headline = match n.actor_name.as_deref() {
        Some(actor) if !actor.is_empty() => format!("{actor} {}", n.message),
        _ => n.message.clone(),
    };
    let issue = match (n.issue_identifier.as_deref(), n.issue_title.as_deref()) {
        (Some(id), Some(title)) => format!("{id}  {title}"),
        (Some(id), None) => id.to_string(),
        (None, Some(title)) => title.to_string(),
        (None, None) => String::new(),
    };

    let body = column![
        text(headline)
            .size(13)
            .font(font)
            .color(primary)
            .wrapping(text::Wrapping::None),
        text(issue)
            .size(12)
            .color(t::text_muted())
            .wrapping(text::Wrapping::None),
    ]
    .spacing(3)
    .width(Length::Fill);

    let line = row![
        dot,
        body,
        text(relative_time(&n.created_at))
            .size(12)
            .color(t::text_muted()),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let card = container(line)
        .padding([0, 14])
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

    mouse_area(card).on_press(Message::InboxSelect(index)).into()
}
