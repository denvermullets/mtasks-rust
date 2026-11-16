use iced::widget::{button, checkbox, column, container, pick_list, row, slider, text_input};
use crate::views::text;
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::config::Config;
use crate::fonts;
use crate::theme as t;
use crate::window_effects;

#[derive(Debug, Clone)]
pub struct SettingsFormState {
    pub api_base_url: String,
    pub api_token: String,
    pub theme: t::ThemeChoice,
    pub font: fonts::FontChoice,
    pub terminal_font: fonts::FontChoice,
    pub terminal_size: u16,
    pub native_notifications: bool,
    /// Window opacity in percent.
    pub window_opacity: u8,
    /// Background blur radius.
    pub window_blur: u8,
    pub status: Option<String>,
    pub testing: bool,
    /// Explains why Check for updates did nothing (dev builds).
    pub update_note: Option<String>,
}

impl SettingsFormState {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            api_base_url: cfg.api_base_url.clone(),
            api_token: cfg.api_token.clone().unwrap_or_default(),
            theme: t::choice_for_key(&cfg.theme),
            font: fonts::choice_for_key(&cfg.font),
            terminal_font: fonts::terminal_choice_for_key(&cfg.terminal_font),
            terminal_size: fonts::clamp_terminal_size(cfg.terminal_font_size).round() as u16,
            native_notifications: cfg.native_notifications,
            window_opacity: cfg.window_opacity.clamp(t::MIN_WINDOW_OPACITY, 100),
            window_blur: cfg.window_blur.min(window_effects::MAX_BLUR_RADIUS),
            status: None,
            testing: false,
            update_note: None,
        }
    }
}

pub fn view<'a>(form: &'a SettingsFormState, updates: &UpdatesInfo) -> Element<'a, Message> {
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
        .push(window_section(form))
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
                .push(text("Terminal font size").size(12).color(t::text_secondary()))
                .push(
                    pick_list(
                        fonts::TERMINAL_SIZES.to_vec(),
                        Some(form.terminal_size),
                        Message::SettingsTerminalSizeChanged,
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
        .push(updates_section(updates, form.update_note.as_deref()))
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

    crate::views::util::modal_backdrop(modal)
        .into()
}

/// What Settings → Updates shows, read from the updater state.
pub struct UpdatesInfo {
    pub installed: semver::Version,
    pub view: crate::updater::machine::UpdaterView,
    /// A check or download is running: the Check button waits.
    pub busy: bool,
}

/// The one line for the current step, with whether it is an error. None when
/// nothing is going on. Download progress only reaches here for manual checks
/// (the machine's view decides).
fn status_text(status: &crate::updater::machine::Status) -> Option<(String, bool)> {
    use crate::updater::machine::Status;
    match status {
        Status::Idle => None,
        Status::Checking => Some(("Checking…".into(), false)),
        Status::UpToDate => Some(("Up to date ✓".into(), false)),
        Status::Downloading { version, percent: 0 } => {
            Some((format!("Update available: {version}"), false))
        }
        Status::Downloading { version, percent } => {
            Some((format!("Update available: {version} (downloading {percent}%)"), false))
        }
        Status::Verifying { version } => Some((format!("Verifying {version}…"), false)),
        Status::Failed { message } => Some((format!("Failed: {message}"), true)),
    }
}

/// When the last check ran, in local time, or "Never".
fn last_check_label(last: Option<chrono::DateTime<chrono::Utc>>) -> String {
    match last {
        Some(at) => at
            .with_timezone(&chrono::Local)
            .format("%b %-d, %Y at %-I:%M %p")
            .to_string(),
        None => "Never".to_string(),
    }
}

/// Settings → Updates: the installed version, when the last check ran, and the
/// last failure (so a quiet one can be found).
fn updates_section<'a>(updates: &UpdatesInfo, note: Option<&str>) -> Element<'a, Message> {
    let line = |name: &'static str, value: String| {
        row![]
            .push(text(name).size(12).color(t::text_secondary()))
            .push(iced::widget::horizontal_space().width(Length::Fill))
            .push(text(value).size(12).color(t::text_muted()))
    };
    let mut col = column![]
        .spacing(6)
        .push(text("Updates").size(12).color(t::text_secondary()))
        .push(line("Installed version", updates.installed.to_string()))
        .push(line("Last checked", last_check_label(updates.view.last_checked)))
        .push(
            row![]
                .spacing(10)
                .align_y(iced::Alignment::Center)
                .push(
                    button(text("Check for updates").size(13))
                        .on_press_maybe((!updates.busy).then_some(Message::UpdateCheckClicked))
                        .padding([6, 14])
                        .style(button::secondary),
                )
                .push(match status_text(&updates.view.status) {
                    Some((s, is_error)) => text(s).size(12).color(if is_error {
                        t::danger()
                    } else {
                        t::text_muted()
                    }),
                    None => text("").size(12),
                }),
        );
    if let Some(offer) = &updates.view.dmg_update {
        col = col.push(dmg_offer(offer));
    }
    if let Some(note) = note {
        col = col.push(text(note.to_string()).size(11).color(t::text_muted()));
    }
    if let Some(f) = &updates.view.last_failure {
        col = col.push(
            text(format!(
                "Last failure ({}): {}",
                last_check_label(Some(f.at)),
                f.message
            ))
            .size(11)
            .color(t::danger()),
        );
    }
    col.into()
}

/// The DMG offer for an Update this install can't apply itself: the version,
/// the Release notes when they parse, and a button opening the manifest's URL.
fn dmg_offer<'a>(offer: &crate::updater::machine::DmgUpdate) -> Element<'a, Message> {
    use crate::updater::manifest::ReleaseNotes;
    let mut col = column![].spacing(4).push(
        text(format!("JAIT {} is available but can't install itself here.", offer.version))
            .size(12)
            .color(t::text_secondary()),
    );
    if let ReleaseNotes::Available(entries) = &offer.notes {
        for e in entries {
            col = col.push(
                text(format!("{}: {}", e.version, e.notes.trim()))
                    .size(11)
                    .color(t::text_muted()),
            );
        }
    }
    col.push(
        button(text(format!("Download JAIT {}", offer.version)).size(13))
            .on_press(Message::UpdateDmgClicked)
            .padding([6, 14])
            .style(button::primary),
    )
    .into()
}

/// Window opacity and blur sliders; both apply live and save on release.
fn window_section<'a>(form: &'a SettingsFormState) -> Element<'a, Message> {
    let label = |name: &'static str, value: String| {
        row![]
            .push(text(name).size(12).color(t::text_secondary()))
            .push(iced::widget::horizontal_space().width(Length::Fill))
            .push(text(value).size(12).color(t::text_muted()))
    };
    let blur_value = if form.window_blur == 0 {
        "Off".to_string()
    } else {
        form.window_blur.to_string()
    };
    let blur_hint = if form.window_opacity >= 100 {
        "Blur shows once the window is below 100% opacity"
    } else {
        "Blurs what's behind the window"
    };
    column![]
        .spacing(6)
        .push(label("Window opacity", format!("{}%", form.window_opacity)))
        .push(
            slider(
                t::MIN_WINDOW_OPACITY..=100,
                form.window_opacity,
                Message::SettingsWindowOpacityChanged,
            )
            .on_release(Message::SettingsWindowEffectsReleased),
        )
        .push(label("Background blur", blur_value))
        .push(
            slider(
                0..=window_effects::MAX_BLUR_RADIUS,
                form.window_blur,
                Message::SettingsWindowBlurChanged,
            )
            .on_release(Message::SettingsWindowEffectsReleased),
        )
        .push(text(blur_hint).size(11).color(t::text_muted()))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_text_matches_the_ui_rules() {
        use crate::updater::machine::Status;
        assert_eq!(status_text(&Status::Idle), None);
        assert_eq!(status_text(&Status::Checking), Some(("Checking…".into(), false)));
        assert_eq!(status_text(&Status::UpToDate), Some(("Up to date ✓".into(), false)));
        let failed = status_text(&Status::Failed { message: "offline".into() }).unwrap();
        assert_eq!(failed, ("Failed: offline".to_string(), true));
    }

    #[test]
    fn last_check_reads_never_until_one_ran() {
        assert_eq!(last_check_label(None), "Never");
        assert_ne!(last_check_label(Some(chrono::Utc::now())), "Never");
    }
}
