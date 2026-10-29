use iced::widget::{button, column, container, row, text, text_input};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::config::Config;
use crate::theme as t;

#[derive(Debug, Clone)]
pub struct SettingsFormState {
    pub api_base_url: String,
    pub api_token: String,
    pub status: Option<String>,
    pub testing: bool,
}

impl SettingsFormState {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            api_base_url: cfg.api_base_url.clone(),
            api_token: cfg.api_token.clone().unwrap_or_default(),
            status: None,
            testing: false,
        }
    }
}

pub fn view<'a>(form: &'a SettingsFormState) -> Element<'a, Message> {
    let status_line: Element<'a, Message> = match form.status.as_deref() {
        Some(s) => {
            let color = if s.starts_with("Failed") || s.starts_with("Error") {
                t::DANGER()
            } else {
                t::TEXT_MUTED()
            };
            text(s).size(12).color(color).into()
        }
        None => text("").size(12).into(),
    };

    let form_content = column![]
        .spacing(14)
        .width(Length::Fixed(460.0))
        .push(text("Settings").size(18).color(t::TEXT_PRIMARY()))
        .push(
            column![]
                .spacing(4)
                .push(text("API base URL").size(12).color(t::TEXT_SECONDARY()))
                .push(
                    text_input("https://your-jait-instance.example.com", &form.api_base_url)
                        .on_input(Message::SettingsBaseUrlChanged)
                        .padding(10)
                        .size(14),
                ),
        )
        .push(
            column![]
                .spacing(4)
                .push(text("API token").size(12).color(t::TEXT_SECONDARY()))
                .push(
                    text_input("Bearer token", &form.api_token)
                        .on_input(Message::SettingsTokenChanged)
                        .secure(true)
                        .padding(10)
                        .size(14),
                ),
        )
        .push(status_line)
        .push(
            row![]
                .spacing(8)
                .push(
                    button(
                        text(if form.testing {
                            "Testing…"
                        } else {
                            "Test Connection"
                        })
                        .size(14),
                    )
                    .on_press_maybe((!form.testing).then_some(Message::TestConnection))
                    .padding([8, 18])
                    .style(button::secondary),
                )
                .push(iced::widget::horizontal_space().width(Length::Fill))
                .push(
                    button(text("Cancel").size(14))
                        .on_press(Message::CloseModal)
                        .padding([8, 24])
                        .style(button::secondary),
                )
                .push(
                    button(text("Save").size(14).color(t::BG_PRIMARY()))
                        .on_press(Message::SaveSettings)
                        .padding([8, 24])
                        .style(move |_theme: &Theme, _status| button::Style {
                            background: Some(iced::Background::Color(t::ACCENT())),
                            text_color: t::BG_PRIMARY(),
                            border: Border {
                                radius: 4.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                ),
        );

    let modal = container(form_content)
        .padding(28)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_MODAL())),
            border: Border {
                color: t::BORDER(),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        });

    container(modal)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.0, 0.0, 0.0, 0.7,
            ))),
            ..Default::default()
        })
        .into()
}
