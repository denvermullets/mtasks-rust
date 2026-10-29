//! PROTOTYPE (JRST-4) — throwaway theme switcher pill. Deliberately fixed
//! black-on-white so it never reads as part of the theme being judged.
//! `[` / `]` or the arrows cycle themes; the font is fixed at launch (JAIT_FONT).

use iced::widget::{button, container, row, text, Row};
use iced::{Background, Border, Color, Element, Length, Shadow, Theme, Vector};

use crate::app::Message;
use crate::theme as t;

const INK: Color = Color::from_rgb(0.05, 0.05, 0.05);
const PAPER: Color = Color::from_rgb(0.98, 0.98, 0.96);

fn swatch<'a>(color: Color) -> Element<'a, Message> {
    container(text(""))
        .width(Length::Fixed(12.0))
        .height(Length::Fixed(12.0))
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(color)),
            border: Border {
                color: INK,
                width: 1.0,
                radius: 2.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn arrow<'a>(label: &'a str, delta: isize) -> Element<'a, Message> {
    button(text(label).size(13).color(INK))
        .on_press(Message::ProtoCycleTheme(delta))
        .padding([2, 8])
        .style(|_: &Theme, _| button::Style {
            background: None,
            text_color: INK,
            ..Default::default()
        })
        .into()
}

pub fn overlay<'a>() -> Element<'a, Message> {
    let tk = t::tok();
    let label = format!(
        "{}/{}  {} ({})  ·  font: {} (JAIT_FONT, restart)",
        t::current_index() + 1,
        t::THEMES.len(),
        tk.name,
        if tk.light { "light" } else { "dark" },
        t::startup_font().0,
    );

    // The tokens this theme resolves to, then the semantic colors.
    let swatches: Row<'a, Message> = [
        tk.menu,
        tk.background,
        tk.foreground,
        tk.context_foreground,
        tk.hover_highlight,
        tk.stroke,
        tk.highlight,
        tk.accent,
        t::TEXT_PRIMARY(),
        t::TEXT_SECONDARY(),
        t::TEXT_MUTED(),
        t::PRIORITY_URGENT(),
        t::PRIORITY_HIGH(),
        t::PRIORITY_MEDIUM(),
        t::PRIORITY_LOW(),
        tk.dep[0],
        tk.dep[1],
        tk.dep[2],
        tk.dep[3],
    ]
    .into_iter()
    .fold(row![].spacing(3), |r, c| r.push(swatch(c)));

    let pill = container(
        row![]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center)
            .push(arrow("◀", -1))
            .push(text(label).size(12).color(INK))
            .push(swatches)
            .push(arrow("▶", 1)),
    )
    .padding([4, 10])
    .style(|_: &Theme| container::Style {
        background: Some(Background::Color(PAPER)),
        border: Border {
            color: INK,
            width: 1.5,
            radius: 14.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.5),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 8.0,
        },
        ..Default::default()
    });

    container(pill)
        .padding(iced::Padding {
            top: 0.0,
            right: 0.0,
            bottom: 40.0,
            left: 0.0,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Center)
        .align_y(iced::alignment::Vertical::Bottom)
        .into()
}
