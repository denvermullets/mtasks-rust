//! Full-window image viewer: dark backdrop, the image scaled to fit, and the
//! filename. Clicking anywhere (or Esc) closes it.

use iced::widget::{button, column, container, image, text};
use iced::{Alignment, ContentFit, Element, Length, Theme};

use crate::app::Message;
use crate::files::images::{ImageState, ImageStore};
use crate::files::lightbox::LightboxState;
use crate::theme as t;

pub fn view<'a>(state: &'a LightboxState, images: &'a ImageStore) -> Element<'a, Message> {
    let current = state.current();
    let key = current.key();
    let picture: Element<'a, Message> = match key.as_deref().and_then(|k| images.path(k)) {
        Some(path) => image(path)
            .content_fit(ContentFit::Contain)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        None => {
            let failed = key.is_none()
                || key.as_deref().and_then(|k| images.state(k)) == Some(&ImageState::Failed);
            let label = if failed { "Couldn't load this image" } else { "Loading…" };
            container(text(label).size(14).color(iced::Color::WHITE))
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into()
        }
    };
    let content = column![]
        .spacing(12)
        .align_x(Alignment::Center)
        .push(picture)
        .push(
            text(match state.position_label() {
                Some(pos) => format!(
                    "{pos}  ·  {}  ·  ←/→: step  ·  o: open in Preview",
                    current.filename()
                ),
                None => format!("{}  ·  o: open in Preview", current.filename()),
            })
                .size(13)
                .color(t::text_muted()),
        );
    // The whole backdrop is one button so a click anywhere dismisses.
    button(
        container(content)
            .padding(32)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(iced::Color::from_rgba(
                    0.0, 0.0, 0.0, 0.88,
                ))),
                ..Default::default()
            }),
    )
    .padding(0)
    .width(Length::Fill)
    .height(Length::Fill)
    .on_press(Message::CloseModal)
    .style(|_theme: &Theme, _status| button::Style::default())
    .into()
}
