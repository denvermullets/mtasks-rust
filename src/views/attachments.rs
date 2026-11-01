//! Reusable file-card list for an owner's attachments (issue, project or
//! comment). Callers load the rows with `db::attachments::list_for_owner`.

use iced::widget::{button, column, container, row, text};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::attachments::{Attachment, UploadState};
use crate::theme as t;

/// The "Attachments" section: a heading plus one card per file, in the order
/// given (`list_for_owner` returns them in the order they were added).
/// Returns `None` when there are no files so callers can `push_maybe` it and
/// the section stays hidden.
pub fn section<'a>(attachments: &'a [Attachment]) -> Option<Element<'a, Message>> {
    if attachments.is_empty() {
        return None;
    }
    let cards = attachments
        .iter()
        .fold(column![].spacing(8), |col, a| col.push(card(a)));
    Some(
        column![]
            .spacing(8)
            .push(text("Attachments").size(14).color(t::text_secondary()))
            .push(cards)
            .into(),
    )
}

/// One file card: filename, then "TYPE · size". Later tickets hang the
/// thumbnail, open, delete and pending/failed states off this.
pub fn card<'a>(a: &'a Attachment) -> Element<'a, Message> {
    let meta = format!(
        "{} · {}",
        type_label(&a.content_type, &a.filename),
        format_size(a.byte_size)
    );
    // Pending marker: the file is safe in the outbox but not on the server yet.
    let pending = (a.upload_state == UploadState::Pending)
        .then(|| text("Pending upload").size(11).color(t::accent()));
    let info = column![]
        .spacing(2)
        .width(Length::Fill)
        .push(text(&a.filename).size(13).color(t::text_primary()))
        .push(
            row![]
                .spacing(8)
                .push(text(meta).size(11).color(t::text_muted()))
                .push_maybe(pending),
        );
    // Only files the server has can be deleted this way; pending/failed ones
    // are cancelled or removed by the upload tickets.
    let delete = (a.upload_state == UploadState::Synced).then(|| {
        button(text("Delete").size(11).color(t::text_muted()))
            .on_press(Message::ConfirmDeleteAttachment(a.id.clone()))
            .padding(0)
            .style(button::text)
    });
    let body = container(row![info].spacing(8).push_maybe(delete))
    .padding(10)
    .width(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_secondary())),
        border: Border {
            color: t::border(),
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    });
    // Clicking the card opens the file in its default app.
    button(body)
        .padding(0)
        .width(Length::Fill)
        .on_press(Message::OpenAttachment(a.clone()))
        .style(|_theme: &Theme, _status| button::Style::default())
        .into()
}

/// Byte count as "999 B", "1.5 KB", "1.2 MB", "3.0 GB" (1024-based, one decimal).
pub fn format_size(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    if bytes < 1024.0 {
        return format!("{} B", bytes as i64);
    }
    let mut value = bytes;
    let mut unit = "B";
    for next in ["KB", "MB", "GB"] {
        value /= 1024.0;
        unit = next;
        if value < 1024.0 {
            break;
        }
    }
    format!("{value:.1} {unit}")
}

/// Short uppercase type tag from the content type, falling back to the file
/// extension, then "FILE".
pub fn type_label(content_type: &str, filename: &str) -> String {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_uppercase())
        .filter(|e| !e.is_empty() && e.len() <= 5);
    let sub = content_type
        .split_once('/')
        .map(|(_, s)| s.split('+').next().unwrap_or(s));
    match sub {
        Some("plain") => "TXT".to_string(),
        Some("octet-stream") | None => ext.unwrap_or_else(|| "FILE".to_string()),
        Some(s) if s.len() <= 5 && !s.contains('.') => s.to_ascii_uppercase(),
        Some(_) => ext.unwrap_or_else(|| "FILE".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_in_the_largest_fitting_unit() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1_258_291), "1.2 MB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
        assert_eq!(format_size(-5), "0 B");
    }

    #[test]
    fn type_label_is_short_and_uppercase() {
        assert_eq!(type_label("application/pdf", "a.pdf"), "PDF");
        assert_eq!(type_label("image/png", "a.png"), "PNG");
        assert_eq!(type_label("image/jpeg", "a.jpg"), "JPEG");
        assert_eq!(type_label("text/plain", "n.txt"), "TXT");
        assert_eq!(type_label("application/octet-stream", "data.bin"), "BIN");
        assert_eq!(type_label("application/octet-stream", "noext"), "FILE");
        assert_eq!(type_label("", "x"), "FILE");
    }
}
