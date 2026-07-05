use chrono::{DateTime, Utc};
use iced::widget::{container, row, text};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::theme as t;

pub fn view<'a>(
    pending_sync_count: i32,
    show_help: bool,
    config_error: Option<&str>,
    last_sync_error: Option<&str>,
    sync_in_progress: bool,
    last_synced_at: Option<DateTime<Utc>>,
    token_configured: bool,
) -> Element<'a, Message> {
    let (sync_text, sync_color) = if let Some(err) = last_sync_error {
        // Transient network failures are non-fatal (sync retries) — surface them
        // as an amber warning rather than a hard red error.
        if err.contains("network") {
            (format!("Sync warning: {err}"), t::WARNING)
        } else {
            (format!("Sync error: {err}"), t::DANGER)
        }
    } else if sync_in_progress {
        ("Syncing\u{2026}".to_string(), t::TEXT_MUTED)
    } else if !token_configured {
        (
            "Sync paused \u{2014} set API token".to_string(),
            t::TEXT_MUTED,
        )
    } else if let Some(at) = last_synced_at {
        let base = format!("Synced {}", format_age(at));
        let full = if pending_sync_count > 0 {
            format!("{base} \u{00B7} {pending_sync_count} pending")
        } else {
            base
        };
        (full, t::TEXT_MUTED)
    } else if pending_sync_count > 0 {
        (
            format!("Not synced yet \u{00B7} {pending_sync_count} pending"),
            t::TEXT_MUTED,
        )
    } else {
        ("Not synced yet".to_string(), t::TEXT_MUTED)
    };

    let shortcuts = if show_help {
        "[n]ew  [e]dit  [m]ove  [M]ove back  [d]elete  [L]abels  [j/k]\u{2195}  [h/l]\u{2194}  [1-4]priority  [s]ync  [?]close help"
    } else {
        "[n]ew [e]dit [m]ove [d]elete [L]abels [s]ync [?]help"
    };

    let mut bar = row![].spacing(16).push(
        text(shortcuts)
            .size(12)
            .color(t::TEXT_MUTED)
            .width(Length::Fill),
    );

    if let Some(msg) = config_error {
        bar = bar.push(
            text(format!("\u{26A0} config: {}", msg))
                .size(12)
                .color(t::DANGER),
        );
    }

    let bar = bar.push(text(sync_text).size(12).color(sync_color));

    container(bar)
        .padding([6, 14])
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_SECONDARY)),
            border: Border {
                color: t::BORDER,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn format_age(then: DateTime<Utc>) -> String {
    let secs = (Utc::now() - then).num_seconds().max(0);
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
