//! The update chip in the status bar (project 320). Driven only by the
//! updater's view: download progress for a manual check, "↑ X ready" once an
//! Update is staged, hidden otherwise. Automatic downloads never reach
//! `Status`, so they stay quiet until ready.

use iced::widget::{button, column, container, mouse_area, row, scrollable};
use iced::{Alignment, Border, Element, Length, Theme};
use semver::Version;

use crate::app::Message;
use crate::theme as t;
use crate::updater::machine::{ReadyUpdate, Status, UpdaterView};
use crate::updater::manifest::{ReleaseEntry, ReleaseNotes};
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
    let pill = container(text(label).size(12).color(t::text_primary()))
        .padding([1, 8])
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::accent())),
            border: Border {
                radius: 8.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
    // Only the ready chip opens the popover; download progress is passive.
    let ready = view.ready.is_some() && !matches!(view.status, Status::Downloading { .. } | Status::Verifying { .. });
    Some(if ready {
        mouse_area(pill).on_press(Message::OpenUpdatePopover).into()
    } else {
        pill.into()
    })
}

/// The entries worth showing: those with notes. A heading with nothing under
/// it is skipped; an empty result means "Release notes unavailable".
fn shown_entries(notes: &ReleaseNotes) -> Vec<&ReleaseEntry> {
    match notes {
        ReleaseNotes::Available(entries) => entries.iter().filter(|e| !e.notes.trim().is_empty()).collect(),
        ReleaseNotes::Unavailable => Vec::new(),
    }
}

/// The Release notes of every Release newer than the installed version,
/// newest first, rendered as markdown; or "Release notes unavailable".
fn notes_section<'a>(
    notes: &ReleaseNotes,
    ctx: &crate::views::body::BodyCtx<'a>,
) -> Element<'a, Message> {
    let entries = shown_entries(notes);
    if entries.is_empty() {
        return text("Release notes unavailable").size(12).color(t::text_muted()).into();
    }
    let mut col = column![].spacing(10);
    for e in entries {
        col = col
            .push(text(e.version.to_string()).size(13).color(t::text_primary()))
            .push(crate::views::body::view(&e.notes, 12.0, t::text_secondary(), ctx));
    }
    col.into()
}

/// The popover over the status bar: the new version, the installed one, the
/// notes, and Restart now. A click outside closes it.
pub fn popover<'a>(
    ready: &ReadyUpdate,
    installed: &Version,
    ctx: &crate::views::body::BodyCtx<'a>,
) -> Element<'a, Message> {
    let panel = column![]
        .spacing(8)
        .push(text(format!("JAIT {} is ready", ready.version)).size(14).color(t::text_primary()))
        .push(text(format!("You have {installed}. It installs when you quit.")).size(12).color(t::text_muted()))
        .push(scrollable(notes_section(&ready.notes, ctx)).height(Length::Shrink))
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text("Restart now").size(13))
                        .on_press(Message::RestartNowClicked)
                        .padding([6, 14])
                        .style(button::primary),
                )
                .push(
                    button(text("Close").size(13))
                        .on_press(Message::CloseModal)
                        .padding([6, 14])
                        .style(button::secondary),
                ),
        );
    let panel = mouse_area(
        container(panel)
            .padding(14)
            .width(Length::Fixed(380.0))
            .max_height(420.0)
            .style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::bg_modal())),
                border: Border {
                    color: t::border(),
                    width: 1.0,
                    radius: 8.0.into(),
                },
                ..Default::default()
            }),
    )
    .on_press(Message::Noop);
    let anchored = container(panel)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::End)
        .align_y(iced::alignment::Vertical::Bottom)
        .padding(iced::Padding { top: 0.0, right: 14.0, bottom: 34.0, left: 0.0 });
    mouse_area(anchored).on_press(Message::CloseModal).into()
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
            failure_shown: false,
        }
    }

    fn entry(v: &str, notes: &str) -> ReleaseEntry {
        ReleaseEntry {
            version: Version::parse(v).unwrap(),
            published_at: chrono::DateTime::parse_from_rfc3339("2026-11-17T10:00:00Z").unwrap(),
            notes: notes.into(),
        }
    }

    #[test]
    fn entries_with_empty_notes_are_skipped() {
        let n = ReleaseNotes::Available(vec![entry("0.14.0", "  \n"), entry("0.13.0", "- fix")]);
        let shown = shown_entries(&n);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].version, Version::parse("0.13.0").unwrap());
        let all_empty = ReleaseNotes::Available(vec![entry("0.14.0", "")]);
        assert!(shown_entries(&all_empty).is_empty());
        assert!(shown_entries(&ReleaseNotes::Unavailable).is_empty());
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
