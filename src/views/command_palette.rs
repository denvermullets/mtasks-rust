//! Command palette (Cmd+K): a searchable launcher modal. This shell has only
//! the text field; result rows are added by later tickets.

use iced::widget::{column, container, mouse_area, text_input};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::theme as t;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaletteState {
    pub query: String,
}

pub fn input_id() -> text_input::Id {
    text_input::Id::new("command-palette-input")
}

/// Palette modal. A click on the backdrop closes it; a click on the panel
/// itself is swallowed.
pub fn view(state: &PaletteState) -> Element<'_, Message> {
    let content = column![].spacing(14).width(Length::Fixed(520.0)).push(
        text_input("Search views, teams, projects, issues\u{2026}", &state.query)
            .id(input_id())
            .on_input(Message::PaletteQueryChanged)
            .padding(8)
            .size(14)
            .width(Length::Fill),
    );

    let panel = container(content)
        .padding(20)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_modal())),
            border: Border {
                color: t::border(),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        });
    let panel = mouse_area(panel).on_press(Message::Noop);

    let backdrop = container(panel)
        .center_x(Length::Fill)
        .align_top(Length::Fill)
        .padding([96, 0])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.0, 0.0, 0.0, 0.7,
            ))),
            ..Default::default()
        });
    mouse_area(backdrop).on_press(Message::CloseModal).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_starts_with_empty_query() {
        assert_eq!(PaletteState::default().query, "");
    }
}
