//! Command palette (Cmd+K): a searchable launcher modal. This shell has only
//! the text field; result rows are added by later tickets.

use iced::widget::{button, column, container, mouse_area, row, text, text_input};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::palette_results::{palette_results, PaletteData, PaletteNav, PaletteRow};
use crate::theme as t;

#[derive(Debug, Clone, PartialEq)]
pub struct PaletteState {
    pub query: String,
    pub rows: Vec<PaletteRow>,
    /// Index of the highlighted row; always 0 when there are no rows.
    pub selected: usize,
}

impl PaletteState {
    pub fn new() -> Self {
        let mut state = PaletteState {
            query: String::new(),
            rows: Vec::new(),
            selected: 0,
        };
        state.set_query(String::new());
        state
    }

    /// Store the query and recompute the rows.
    pub fn set_query(&mut self, query: String) {
        self.rows = palette_results(&query, &PaletteData::default());
        self.query = query;
        self.selected = 0;
    }

    /// Move the highlight by `delta` rows, wrapping at both ends.
    pub fn move_selection(&mut self, delta: i32) {
        let n = self.rows.len() as i32;
        if n > 0 {
            self.selected = (self.selected as i32 + delta).rem_euclid(n) as usize;
        }
    }

    /// Where Enter goes: the highlighted row's target, if any.
    pub fn chosen(&self) -> Option<PaletteNav> {
        self.rows.get(self.selected).map(|r| r.nav.clone())
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
            .on_submit(Message::PaletteSubmit)
            .padding(8)
            .size(14)
            .width(Length::Fill),
    );
    let mut list = column![].spacing(2);
    for (i, r) in state.rows.iter().enumerate() {
        let highlighted = i == state.selected;
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
            .style(move |_theme: &Theme, status| button::Style {
                background: (highlighted || matches!(status, button::Status::Hovered))
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
    if state.rows.is_empty() {
        content = content.push(text("No results").size(13).color(t::text_muted()));
    }

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
        assert_eq!(PaletteState::new().query, "");
    }

    #[test]
    fn typing_recomputes_rows() {
        let mut state = PaletteState::new();
        assert_eq!(state.rows.len(), 4);
        state.set_query("road".into());
        assert_eq!(state.rows.len(), 1);
    }

    #[test]
    fn first_row_highlighted_on_open_and_after_typing() {
        let mut state = PaletteState::new();
        assert_eq!(state.selected, 0);
        state.move_selection(1);
        state.set_query("o".into());
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn down_wraps_to_first_and_up_wraps_to_last() {
        let mut state = PaletteState::new();
        state.move_selection(-1);
        assert_eq!(state.selected, 3);
        state.move_selection(1);
        assert_eq!(state.selected, 0);
        state.move_selection(1);
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn move_with_no_rows_stays_put() {
        let mut state = PaletteState::new();
        state.set_query("zzz".into());
        state.move_selection(1);
        state.move_selection(-1);
        assert_eq!(state.selected, 0);
        assert_eq!(state.chosen(), None);
    }

    #[test]
    fn chosen_is_the_highlighted_rows_target() {
        let mut state = PaletteState::new();
        assert_eq!(state.chosen(), Some(PaletteNav::View(crate::app::View::Board)));
        state.move_selection(-1);
        assert_eq!(state.chosen(), Some(PaletteNav::View(crate::app::View::MyIssues)));
    }
}
