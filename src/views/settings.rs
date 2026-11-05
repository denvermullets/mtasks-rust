use iced::widget::{button, checkbox, column, container, pick_list, row, text, text_input};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::config::Config;
use crate::fonts;
use crate::theme as t;

#[derive(Debug, Clone)]
pub struct SettingsFormState {
    pub api_base_url: String,
    pub api_token: String,
    pub theme: t::ThemeChoice,
    pub font: fonts::FontChoice,
    pub terminal_font: fonts::FontChoice,
    pub native_notifications: bool,
    pub status: Option<String>,
    pub testing: bool,
}

impl SettingsFormState {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            api_base_url: cfg.api_base_url.clone(),
            api_token: cfg.api_token.clone().unwrap_or_default(),
            theme: t::choice_for_key(&cfg.theme),
            font: fonts::choice_for_key(&cfg.font),
            terminal_font: fonts::terminal_choice_for_key(&cfg.terminal_font),
            native_notifications: cfg.native_notifications,
            status: None,
            testing: false,
        }
    }
}

pub fn view<'a>(form: &'a SettingsFormState) -> Element<'a, Message> {
    let status_line: Element<'a, Message> = match form.status.as_deref() {
        Some(s) => {
            let color = if s.starts_with("Failed") || s.starts_with("Error") {
                t::danger()
            } else {
                t::text_muted()
            };
            text(s).size(12).color(color).into()
        }
        None => text("").size(12).into(),
    };

    let form_content = column![]
        .spacing(14)
        .width(Length::Fixed(460.0))
        .push(text("Settings").size(18).color(t::text_primary()))
        .push(
            column![]
                .spacing(4)
                .push(text("API base URL").size(12).color(t::text_secondary()))
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
                .push(text("API token").size(12).color(t::text_secondary()))
                .push(
                    text_input("Bearer token", &form.api_token)
                        .on_input(Message::SettingsTokenChanged)
                        .secure(true)
                        .padding(10)
                        .size(14),
                ),
        )
        .push(
            column![]
                .spacing(4)
                .push(text("Theme").size(12).color(t::text_secondary()))
                .push(
                    pick_list(
                        t::choices(),
                        Some(form.theme),
                        Message::SettingsThemeChanged,
                    )
                    .padding(10)
                    .text_size(14)
                    .width(Length::Fill),
                ),
        )
        .push(
            column![]
                .spacing(4)
                .push(text("Font").size(12).color(t::text_secondary()))
                .push(
                    pick_list(
                        fonts::choices(),
                        Some(form.font),
                        Message::SettingsFontChanged,
                    )
                    .padding(10)
                    .text_size(14)
                    .width(Length::Fill),
                )
                .push(
                    text("Applies after restart")
                        .size(11)
                        .color(t::text_muted()),
                ),
        )
        .push(
            column![]
                .spacing(4)
                .push(text("Terminal font").size(12).color(t::text_secondary()))
                .push(
                    pick_list(
                        fonts::terminal_choices(),
                        Some(form.terminal_font),
                        Message::SettingsTerminalFontChanged,
                    )
                    .padding(10)
                    .text_size(14)
                    .width(Length::Fill),
                )
                .push(
                    text("Applies to open Terminals straight away")
                        .size(11)
                        .color(t::text_muted()),
                ),
        )
        .push(
            column![]
                .spacing(4)
                .push(text("Notifications").size(12).color(t::text_secondary()))
                .push(
                    checkbox(
                        "Show native notifications when the window is unfocused",
                        form.native_notifications,
                    )
                    .on_toggle(Message::SettingsNativeNotificationsToggled)
                    .size(16)
                    .text_size(14),
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
                    button(text("Save").size(14).color(t::bg_primary()))
                        .on_press(Message::SaveSettings)
                        .padding([8, 24])
                        .style(move |_theme: &Theme, status| button::Style {
                            background: Some(iced::Background::Color(t::accent_fill(status))),
                            text_color: t::on_accent(status),
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
            background: Some(iced::Background::Color(t::bg_modal())),
            border: Border {
                color: t::border(),
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
