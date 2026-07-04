use iced::widget::{column, container, text};
use iced::{Element, Length};

use crate::app::Message;
use crate::theme as t;

/// A titled "Coming soon" stub for right-pane views not yet implemented
/// (Projects, Roadmap, My Issues, …). Filled in by their respective tickets.
pub fn view<'a>(title: &'a str) -> Element<'a, Message> {
    let content = column![
        text(title).size(20).color(t::TEXT_PRIMARY),
        text("Coming soon").size(14).color(t::TEXT_MUTED),
    ]
    .spacing(8)
    .align_x(iced::Alignment::Center);

    container(content)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}
