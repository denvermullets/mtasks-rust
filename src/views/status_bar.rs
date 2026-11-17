use chrono::{DateTime, Utc};
use iced::widget::{container, row};
use crate::views::text;
use iced::{Border, Element, Length, Theme};

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
    let (sync_text, sync_color) = if let Some(err) = last_sync_error {
        // Transient network failures are non-fatal (sync retries) — surface them
        // as an amber warning rather than a hard red error.
        if err.contains("network") {
            (format!("Sync warning: {err}"), t::warning())
        } else {
            (format!("Sync error: {err}"), t::danger())
        }
    } else if sync_in_progress {
        ("Syncing\u{2026}".to_string(), t::text_muted())
    } else if !token_configured {
        (
            "Sync paused \u{2014} set API token".to_string(),
            t::text_muted(),
        )
    } else if let Some(at) = last_synced_at {
        let base = format!("Synced {}", format_age(at));
        let full = if pending_sync_count > 0 {
            format!("{base} \u{00B7} {pending_sync_count} pending")
        } else {
            base
        };
        (full, t::text_muted())
    } else if pending_sync_count > 0 {
        (
            format!("Not synced yet \u{00B7} {pending_sync_count} pending"),
            t::text_muted(),
        )
    } else {
        ("Not synced yet".to_string(), t::text_muted())
    };

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
        .push(text(sync_text).size(12).color(sync_color));

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

#[cfg(test)]
mod tests {
    use super::*;

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
