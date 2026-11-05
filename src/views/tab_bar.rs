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
        let tab = button(text(label.clone()).size(13).color(if active {
            t::text_strong()
        } else {
            t::text_secondary()
        }))
        .padding([5, 12])
        .on_press(Message::Tab(TabMessage::Focus(i)))
        .style(move |_theme: &Theme, _status| button::Style {
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
