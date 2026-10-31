//! Command palette (Cmd+K): a searchable launcher modal. This shell has only
//! the text field; result rows are added by later tickets.

use iced::widget::{button, column, container, mouse_area, row, text, text_input};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::palette_results::{palette_results, PaletteData, PaletteRow};
use crate::theme as t;

#[derive(Debug, Clone, PartialEq)]
pub struct PaletteState {
    pub query: String,
    pub rows: Vec<PaletteRow>,
}

impl PaletteState {
    pub fn new(data: &PaletteData<'_>) -> Self {
        let mut state = PaletteState {
            query: String::new(),
            rows: Vec::new(),
        };
        state.set_query(String::new(), data);
        state
    }

    /// Store the query and recompute the rows.
    pub fn set_query(&mut self, query: String, data: &PaletteData<'_>) {
        self.rows = palette_results(&query, data);
        self.query = query;
    }
}

pub fn input_id() -> text_input::Id {
    text_input::Id::new("command-palette-input")
}

/// Palette modal. A click on the backdrop closes it; a click on the panel
/// itself is swallowed.
pub fn view(state: &PaletteState) -> Element<'_, Message> {
    let mut content = column![].spacing(14).width(Length::Fixed(520.0)).push(
        text_input("Search views, teams, projects, issues\u{2026}", &state.query)
            .id(input_id())
            .on_input(Message::PaletteQueryChanged)
            .padding(8)
            .size(14)
            .width(Length::Fill),
    );
    let mut list = column![].spacing(2);
    for r in &state.rows {
        list = list.push(
            button(
                row![
                    text(r.kind.label())
                        .size(11)
                        .color(t::text_muted())
                        .width(Length::Fixed(56.0)),
                    text(r.primary.as_str()).size(14).color(t::text_primary()),
                    text(r.secondary.as_str()).size(12).color(t::text_secondary()),
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center),
            )
            .on_press(Message::PaletteChoose(r.nav.clone()))
            .padding([6, 8])
            .width(Length::Fill)
            .style(|_theme: &Theme, status| button::Style {
                background: matches!(status, button::Status::Hovered)
                    .then(|| iced::Background::Color(t::bg_card_focused())),
                text_color: t::text_primary(),
                border: Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        );
    }
    content = content.push(list);

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
        assert_eq!(PaletteState::new(&PaletteData::default()).query, "");
    }

    #[test]
    fn typing_recomputes_rows() {
        let mut state = PaletteState::new(&PaletteData::default());
        assert_eq!(state.rows.len(), 4);
        state.set_query("road".into(), &PaletteData::default());
        assert_eq!(state.rows.len(), 1);
    }
}
