use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use iced::advanced::svg::Handle;
use iced::widget::{container, row, svg, tooltip};
use crate::views::text;
use iced::{Alignment, Border, Color, Element, Length, Theme};

use crate::app::Message;
use crate::theme as t;
use crate::views::util::format_age;

/// The shortcut hint on the left of the bar.
fn shortcuts(show_help: bool, in_inbox: bool, on_project: bool) -> &'static str {
    if show_help {
        "[c]reate  [e]dit  [s]tatus  [p]riority  [l]abels  [j] project  [m]ove  [M]ove back  [t]eam move  [D]elete  [\u{2191}\u{2193}\u{2190}\u{2192}] move focus  [v]iew board/list  [C]ollapse section  [1-4]priority  [S]ync  [P] new project  [L]abels page  [I]nbox  [\u{2318}K]command palette  [\u{2318}B]sidebar  [\u{2318}\u{2325}\u{2190}\u{2192}]switch tab  [\u{2318}\u{21e7}F]search  Pickers: [\u{2191}\u{2193}] move  [Enter] pick  Issue detail: [s]tatus  [p]riority  [l]abels  [j] project  [a]ttach file  [e] edit description  [/] properties  Project detail: [e] edit description  [/] properties  Inbox: [j/k] move  [f] unread/all  [Enter] open  [r] read  [R] read all  [?]close help"
    } else if in_inbox {
        "[j/k] move  [f] unread/all  [Enter] open  [r] read  [R] read all  [?]help"
    } else if on_project {
        "[e]dit description [/]properties [S]ync [?]help"
    } else {
        "[c]reate [e]dit [s]tatus [p]riority [l]abels [j] project [D]elete [v]iew [S]ync [?]help"
    }
}

#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    pending_sync_count: i32,
    show_help: bool,
    config_error: Option<&str>,
    last_sync_error: Option<&str>,
    sync_in_progress: bool,
    last_synced_at: Option<DateTime<Utc>>,
    token_configured: bool,
    inaccessible_teams: &[String],
    read_only: bool,
    in_inbox: bool,
    on_project: bool,
    update_chip: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let status = sync_status(
        pending_sync_count,
        last_sync_error,
        sync_in_progress,
        last_synced_at,
        token_configured,
    );

    let mut bar = row![].spacing(16).push(
        text(shortcuts(show_help, in_inbox, on_project))
            .size(12)
            .color(t::text_muted())
            .width(Length::Fill),
    );

    if let Some(msg) = config_error {
        bar = bar.push(
            text(format!("\u{26A0} config: {}", msg))
                .size(12)
                .color(t::danger()),
        );
    }

    let bar = if read_only {
        bar.push(
            text("Read-only token \u{2014} edits are saved locally and will sync once a write token is set")
                .size(12)
                .color(t::warning()),
        )
    } else {
        bar
    };

    let bar = if inaccessible_teams.is_empty() {
        bar
    } else {
        bar.push(
            text(format!(
                "\u{26A0} No access to team: {} (checking\u{2026})",
                inaccessible_teams.join(", ")
            ))
            .size(12)
            .color(t::warning()),
        )
    };

    let bar = bar
        .push_maybe(update_chip)
        .push(sync_indicator(status, last_synced_at))
        .align_y(Alignment::Center);

    container(bar)
        .padding([6, 14])
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::base(t::bg_secondary()))),
            border: Border {
                color: t::border(),
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
}

const TEXT_SIZE: f32 = 12.0;
const ICON_SIZE: f32 = 14.0;
/// The time slot holds `34s ago`-style labels: 7 characters of mono text.
const TIME_CHARS: f32 = 7.0;
/// A mono glyph's advance as a fraction of the font size. The bundled mono
/// families sit at 0.6; the margin keeps a slightly wider face from clipping.
const MONO_ADVANCE_EM: f32 = 0.62;

// Heroicons (outline), as elsewhere in the app; tinted at draw time.
const ICON_ARROW_PATH: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0 3.181 3.183a8.25 8.25 0 0 0 13.803-3.7M4.031 9.865a8.25 8.25 0 0 1 13.803-3.7l3.181 3.182m0-4.991v4.99"/></svg>"##;
const ICON_CHECK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="m4.5 12.75 6 6 9-13.5"/></svg>"##;
const ICON_WARNING: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M12 9v3.75m-9.303 3.376c-.866 1.5.217 3.374 1.948 3.374h14.71c1.73 0 2.813-1.874 1.948-3.374L13.949 3.378c-.866-1.5-3.032-1.5-3.898 0L2.697 16.126ZM12 15.75h.007v.008H12v-.008Z"/></svg>"##;

static ARROW_PATH: LazyLock<Handle> = LazyLock::new(|| Handle::from_memory(ICON_ARROW_PATH.as_bytes()));
static CHECK: LazyLock<Handle> = LazyLock::new(|| Handle::from_memory(ICON_CHECK.as_bytes()));
static WARNING: LazyLock<Handle> = LazyLock::new(|| Handle::from_memory(ICON_WARNING.as_bytes()));

/// Which icon the sync indicator shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncIcon {
    /// Spinning arrows.
    Syncing,
    /// Check mark.
    Synced,
    /// Still arrows: nothing has synced yet.
    NotYet,
    /// Warning triangle: failed, offline or paused.
    Warning,
}

/// The sync indicator's icon, its tint, and the full status shown on hover.
#[derive(Debug, Clone, PartialEq)]
struct SyncStatus {
    icon: SyncIcon,
    color: Color,
    detail: String,
}

fn sync_status(
    pending_sync_count: i32,
    last_sync_error: Option<&str>,
    sync_in_progress: bool,
    last_synced_at: Option<DateTime<Utc>>,
    token_configured: bool,
) -> SyncStatus {
    let pending = |base: String| {
        if pending_sync_count > 0 {
            format!("{base} \u{00B7} {pending_sync_count} pending")
        } else {
            base
        }
    };
    let status = |icon, color, detail| SyncStatus { icon, color, detail };
    if let Some(err) = last_sync_error {
        // Transient network failures are non-fatal (sync retries) — surface them
        // as an amber warning rather than a hard red error.
        if err.contains("network") {
            status(SyncIcon::Warning, t::warning(), pending(format!("Sync warning: {err}")))
        } else {
            status(SyncIcon::Warning, t::danger(), pending(format!("Sync error: {err}")))
        }
    } else if sync_in_progress {
        status(SyncIcon::Syncing, t::text_muted(), pending("Syncing\u{2026}".to_string()))
    } else if !token_configured {
        status(
            SyncIcon::Warning,
            t::text_muted(),
            "Sync paused \u{2014} set API token".to_string(),
        )
    } else if let Some(at) = last_synced_at {
        status(SyncIcon::Synced, t::text_muted(), pending(format!("Synced {}", format_age(at))))
    } else {
        status(SyncIcon::NotYet, t::text_muted(), pending("Not synced yet".to_string()))
    }
}

fn icon<'a>(handle: &Handle, color: Color) -> Element<'a, Message> {
    svg(handle.clone())
        .width(Length::Fixed(ICON_SIZE))
        .height(Length::Fixed(ICON_SIZE))
        .style(move |_theme: &Theme, _status| svg::Style { color: Some(color) })
        .into()
}

/// The icon, then the time since the last sync in mono at a fixed width, so
/// nothing to its left moves as the time ticks or the state changes. Hovering
/// shows the full status (the error reason, pending count).
fn sync_indicator<'a>(status: SyncStatus, last_synced_at: Option<DateTime<Utc>>) -> Element<'a, Message> {
    let glyph = match status.icon {
        SyncIcon::Syncing => crate::views::spinner::spinner(ARROW_PATH.clone(), ICON_SIZE, status.color).into(),
        SyncIcon::Synced => icon(&CHECK, status.color),
        SyncIcon::NotYet => icon(&ARROW_PATH, status.color),
        SyncIcon::Warning => icon(&WARNING, status.color),
    };
    let age = last_synced_at.map(format_age).unwrap_or_default();
    let time = text(age)
        .size(TEXT_SIZE)
        .font(crate::fonts::code())
        .color(t::text_muted())
        .wrapping(text::Wrapping::None)
        .width(Length::Fixed(TIME_CHARS * MONO_ADVANCE_EM * TEXT_SIZE));
    let content = row![glyph, time].spacing(6).align_y(Alignment::Center);
    tooltip(
        content,
        container(text(status.detail).size(TEXT_SIZE).color(t::text_strong()))
            .padding([4, 8])
            .style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::bg_menu())),
                border: Border { color: t::border(), width: 1.0, radius: 4.0.into() },
                ..Default::default()
            }),
        tooltip::Position::Top,
    )
    .gap(6)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(err: Option<&str>, syncing: bool, at: bool, token: bool, pending: i32) -> SyncStatus {
        let at = at.then(|| Utc::now() - chrono::Duration::seconds(34));
        sync_status(pending, err, syncing, at, token)
    }

    #[test]
    fn each_sync_state_picks_its_icon() {
        assert_eq!(status(None, true, true, true, 0).icon, SyncIcon::Syncing);
        assert_eq!(status(None, false, true, true, 0).icon, SyncIcon::Synced);
        assert_eq!(status(None, false, false, true, 0).icon, SyncIcon::NotYet);
        assert_eq!(status(Some("boom"), false, true, true, 0).icon, SyncIcon::Warning);
        assert_eq!(status(None, false, true, false, 0).icon, SyncIcon::Warning);
    }

    #[test]
    fn the_hover_carries_the_reason_and_pending_count() {
        let failed = status(Some("network unreachable"), false, true, true, 2);
        assert_eq!(failed.detail, "Sync warning: network unreachable \u{00B7} 2 pending");
        assert_eq!(failed.color, t::warning());
        assert_eq!(status(Some("401"), false, true, true, 0).color, t::danger());
        assert_eq!(status(None, false, true, true, 3).detail, "Synced 34s ago \u{00B7} 3 pending");
        assert_eq!(status(None, false, true, false, 0).detail, "Sync paused \u{2014} set API token");
    }

    #[test]
    fn the_time_slot_fits_seven_mono_characters() {
        assert!(TIME_CHARS * MONO_ADVANCE_EM >= 7.0 * 0.6);
    }

    #[test]
    fn project_page_hint_shows_the_properties_key_and_keeps_help() {
        let hint = shortcuts(false, false, true);
        assert!(hint.contains("[/]properties"));
        assert!(hint.contains("[?]help"));
    }

    #[test]
    fn other_pages_do_not_advertise_properties() {
        assert!(!shortcuts(false, false, false).contains("[/]properties"));
        assert!(!shortcuts(false, true, false).contains("[/]properties"));
    }

    #[test]
    fn help_text_lists_the_project_shortcut() {
        assert!(shortcuts(true, false, true).contains("[/] properties"));
    }
}
