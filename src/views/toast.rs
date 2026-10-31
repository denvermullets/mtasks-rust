//! Generic transient toast surface, renderable over any view.
//!
//! Toasts stack in the bottom-right corner above the base UI (and any open
//! modal) via `iced::widget::stack!`. Non-error toasts auto-expire on a timer
//! (see `App::update`'s `ToastTick` arm); error toasts persist until the user
//! dismisses them or hits "Retry now". The overlay container is transparent and
//! only its cards are interactive, so clicks elsewhere pass through to the UI
//! beneath it.

use iced::widget::{button, column, container, row, text};
use iced::{Border, Color, Element, Length, Theme};

use crate::app::Message;
use crate::theme as t;

#[derive(Debug, Clone, PartialEq)]
pub enum ToastKind {
    Error,
    Info,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub id: u64,
    pub message: String,
    pub kind: ToastKind,
    /// Whether to show a "Retry now" affordance (immediate re-sync).
    pub retry: bool,
    /// Creation time, for age-based auto-dismiss of non-error toasts.
    pub created: std::time::Instant,
}

impl ToastKind {
    fn accent(&self) -> Color {
        match self {
            ToastKind::Error => t::danger(),
            ToastKind::Info => t::accent(),
        }
    }
}

/// Build the bottom-right toast stack. Returns a full-screen, transparent
/// overlay element positioning the cards; callers layer it with `stack!`.
pub fn overlay(toasts: &[Toast]) -> Element<'_, Message> {
    let mut stack = column![].spacing(8).width(Length::Shrink);

    for toast in toasts {
        stack = stack.push(card(toast));
    }

    container(stack)
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(iced::alignment::Horizontal::Right)
        .align_y(iced::alignment::Vertical::Bottom)
        .into()
}

fn card(toast: &Toast) -> Element<'_, Message> {
    let accent = toast.kind.accent();

    let dismiss = button(text("×").size(16).color(t::text_secondary()))
        .on_press(Message::DismissToast(toast.id))
        .padding([0, 6])
        .style(button::text);

    let header = row![]
        .spacing(10)
        .align_y(iced::alignment::Vertical::Center)
        .push(
            text(&toast.message)
                .size(13)
                .color(t::text_primary())
                .width(Length::Shrink),
        )
        .push(iced::widget::horizontal_space().width(Length::Fill))
        .push(dismiss);

    let mut body = column![].spacing(8).push(header);

    if toast.retry {
        body = body.push(
            row![]
                .push(iced::widget::horizontal_space().width(Length::Fill))
                .push(
                    button(text("Retry now").size(12).color(accent))
                        .on_press(Message::RetrySyncFromToast(toast.id))
                        .padding([4, 10])
                        .style(button::text),
                ),
        );
    }

    container(body)
        .padding(12)
        .max_width(360.0)
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_modal())),
            border: Border {
                color: accent,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .into()
}
