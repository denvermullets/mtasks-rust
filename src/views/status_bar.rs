use iced::widget::{container, row, text};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::theme as t;

pub fn view<'a>(pending_sync_count: i32, show_help: bool) -> Element<'a, Message> {
    let sync_text = if pending_sync_count > 0 {
        format!("{} pending sync", pending_sync_count)
    } else {
        "Synced".to_string()
    };

    let shortcuts = if show_help {
        "[n]ew  [e]dit  [m]ove  [M]ove back  [d]elete  [j/k]\u{2195}  [h/l]\u{2194}  [1-4]priority  [s]ync  [?]close help"
    } else {
        "[n]ew [e]dit [m]ove [d]elete [s]ync [?]help"
    };

    let bar = row![]
        .spacing(16)
        .push(
            text(shortcuts)
                .size(12)
                .color(t::TEXT_MUTED)
                .width(Length::Fill),
        )
        .push(text(sync_text).size(12).color(t::TEXT_MUTED));

    container(bar)
        .padding([6, 14])
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_SECONDARY)),
            border: Border {
                color: t::BORDER,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
}
