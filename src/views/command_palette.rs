//! Command palette (Cmd+K): a searchable launcher modal. Renders a
//! `PaletteState` (text field plus one button per result row); the state and
//! results logic live in `palette_results`.

use iced::widget::{button, column, container, mouse_area, row, scrollable, text_input};
use crate::views::text;
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::palette_results::PaletteState;
use crate::scroll::{self, ScrollKey};
use crate::theme as t;

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
    let mut list = column![].spacing(scroll::PALETTE_SPACING);
    for (i, r) in state.rows.iter().enumerate() {
        let highlighted = i == state.selected;
        list = list.push(
            button(
                row![
                    text(r.nav.label())
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
            .height(Length::Fixed(scroll::PALETTE_ROW_H))
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
    // The list scrolls past `PALETTE_VISIBLE_ROWS`, keeping the search field
    // in place; Up/Down scroll the highlighted row into view (see app.rs).
    if !state.rows.is_empty() {
        let list = scroll::track(
            scrollable(list).direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(4).scroller_width(4).spacing(4),
            )),
            ScrollKey::Palette,
        )
        .width(Length::Fill)
        .height(Length::Fixed(scroll::palette_list_height(state.rows.len())));
        content = content.push(list);
    }
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

    let backdrop = crate::views::util::modal_backdrop_top(panel);
    mouse_area(backdrop).on_press(Message::CloseModal).into()
}

