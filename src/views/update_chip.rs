//! The update chip in the status bar (project 320). Driven only by the
//! updater's view: download progress for a manual check, "↑ X ready" once an
//! Update is staged, hidden otherwise. Automatic downloads never reach
//! `Status`, so they stay quiet until ready.

use iced::widget::container;
use iced::{Border, Element, Theme};

use crate::app::Message;
use crate::theme as t;
use crate::updater::machine::{Status, UpdaterView};
use crate::views::text;

/// What the chip says, or `None` when it's hidden.
pub fn label(view: &UpdaterView) -> Option<String> {
    match &view.status {
        Status::Downloading { version, percent } => Some(format!("\u{2193} {version} {percent}%")),
        Status::Verifying { version } => Some(format!("\u{2193} {version} verifying")),
        _ => view
            .ready
            .as_ref()
            .map(|r| format!("\u{2191} {} ready", r.version)),
    }
}

/// The chip, or `None` when there's nothing to show.
pub fn chip<'a>(view: &UpdaterView) -> Option<Element<'a, Message>> {
    let label = label(view)?;
    Some(
        container(text(label).size(12).color(t::text_primary()))
            .padding([1, 8])
            .style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::accent())),
                border: Border {
                    radius: 8.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::machine::ReadyUpdate;
    use crate::updater::manifest::ReleaseNotes;
    use semver::Version;

    fn view(status: Status, ready: Option<&str>) -> UpdaterView {
        UpdaterView {
            status,
            ready: ready.map(|v| ReadyUpdate {
                version: Version::parse(v).unwrap(),
                notes: ReleaseNotes::Unavailable,
            }),
            last_checked: None,
            last_failure: None,
            dmg_update: None,
        }
    }

    #[test]
    fn hidden_with_nothing_to_show() {
        assert_eq!(label(&view(Status::Idle, None)), None);
        assert_eq!(label(&view(Status::Checking, None)), None);
        assert_eq!(label(&view(Status::UpToDate, None)), None);
    }

    #[test]
    fn shows_download_progress() {
        let s = Status::Downloading { version: Version::parse("0.13.0").unwrap(), percent: 42 };
        assert_eq!(label(&view(s, None)).as_deref(), Some("\u{2193} 0.13.0 42%"));
    }

    #[test]
    fn shows_ready_once_staged() {
        assert_eq!(
            label(&view(Status::Idle, Some("0.13.0"))).as_deref(),
            Some("\u{2191} 0.13.0 ready")
        );
    }
}
