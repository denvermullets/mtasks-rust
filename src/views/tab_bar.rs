//! The tab bar across the top of the main area. Takes plain labels so other
//! Tab kinds only change what the caller passes in.

use iced::widget::{button, container, row, text};
use iced::{Border, Element, Length, Theme};

use crate::app::{Message, TabMessage};
use crate::theme as t;

pub fn view<'a>(tabs: &[(String, bool)]) -> Element<'a, Message> {
    let mut bar = row![].spacing(2).padding([4, 8]);
    for (i, (label, active)) in tabs.iter().enumerate() {
        let active = *active;
        let label_button = button(text(label.clone()).size(13).color(if active {
            t::text_strong()
        } else {
            t::text_secondary()
        }))
        .padding(iced::Padding { top: 5.0, right: 4.0, bottom: 5.0, left: 12.0 })
        .on_press(Message::Tab(TabMessage::Focus(i)))
        .style(|_theme: &Theme, _status| button::Style::default());
        let close = button(text("\u{00D7}").size(14).color(t::text_muted()))
            .padding([3, 8])
            .on_press(Message::Tab(TabMessage::Close(i)))
            .style(|_theme: &Theme, status| button::Style {
                background: matches!(status, button::Status::Hovered)
                    .then(|| iced::Background::Color(t::bg_card_focused())),
                border: Border { radius: 4.0.into(), ..Default::default() },
                ..Default::default()
            });
        let tab = container(row![label_button, close].align_y(iced::Alignment::Center))
            .padding([0, 4])
            .style(move |_theme: &Theme| container::Style {
                background: active.then(|| iced::Background::Color(t::bg_secondary())),
                border: Border { radius: 6.0.into(), ..Default::default() },
                ..Default::default()
            });
        bar = bar.push(tab);
    }
    container(bar)
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_menu())),
            border: Border { color: t::border(), width: 0.0, radius: 0.0.into() },
            ..Default::default()
        })
        .into()
}
