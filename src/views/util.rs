//! Small view-layer formatting helpers shared across views.

use chrono::{DateTime, Utc};
use iced::widget::{column, container, pick_list, text_input};
use crate::views::text;
use iced::{Border, Element, Length, Theme};

use crate::theme::{self as t, radius, space, type_scale};

/// Coarse "N{s,m,h,d} ago" label for a past instant. Clamps future instants to
/// `0s ago`.
pub fn format_age(then: DateTime<Utc>) -> String {
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

/// Relative label for an RFC3339 timestamp string (as stored on models). Falls
/// back to the raw string if it can't be parsed.
pub fn relative_time(rfc3339: &str) -> String {
    match DateTime::parse_from_rfc3339(rfc3339) {
        Ok(dt) => format_age(dt.with_timezone(&Utc)),
        Err(_) => rfc3339.to_string(),
    }
}

/// "Jan 05" from an RFC 3339 timestamp or `YYYY-MM-DD` date (Rails list-row
/// format); falls back to the raw date prefix when unparseable.
pub fn short_date(raw: &str) -> String {
    let day = raw.get(..10).unwrap_or(raw);
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| d.format("%b %d").to_string())
        .unwrap_or_else(|_| day.to_string())
}

/// "3pt" / "2.5pt" estimate label.
pub fn format_estimate(e: f64) -> String {
    if e.fract() == 0.0 {
        format!("{}pt", e as i64)
    } else {
        format!("{e:.1}pt")
    }
}

// ---- Detail-view sidebar (shared by issue and project detail) ----

/// Sidebar width (web `w-80`).
pub const SIDEBAR_WIDTH: f32 = space::S16 * 5.0;

/// The "Properties" heading text: the web uppercases it.
pub fn sidebar_heading_text(label: &str) -> String {
    label.to_uppercase()
}

/// Section heading (web `text-sm font-semibold text-gray-400`) over its
/// content, `space::S4` apart. Shared by the issue and project detail views.
pub fn with_heading<'a, M: 'a>(
    label: impl Into<String>,
    content: impl Into<Element<'a, M>>,
) -> Element<'a, M> {
    column![]
        .spacing(space::S4)
        .push(
            text(label.into())
                .size(type_scale::SM)
                .font(crate::fonts::bold_active())
                .color(t::text_secondary()),
        )
        .push(content)
        .into()
}

/// Sidebar group heading: small, bold, uppercase, muted (web
/// `text-xs font-semibold text-gray-500 uppercase`).
pub fn sidebar_heading<'a, M: 'a>(label: &str) -> Element<'a, M> {
    text(sidebar_heading_text(label))
        .size(type_scale::XS)
        .font(crate::fonts::bold_active())
        .color(t::text_muted())
        .into()
}

/// A labelled sidebar field: small muted label above its control (web
/// `text-xs font-medium text-gray-500 mb-2`).
pub fn sidebar_field<'a, M: 'a>(
    label: &'a str,
    control: impl Into<Element<'a, M>>,
) -> Element<'a, M> {
    column![]
        .spacing(space::S2)
        .push(text(label).size(type_scale::XS).color(t::text_muted()))
        .push(control.into())
        .into()
}

/// The sidebar column's frame: fixed width, page background, scrolling is the
/// caller's job. Pair the content with `.spacing(space::S6).padding(space::S6)`.
pub fn sidebar_panel<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    container(content)
        .width(Length::Fixed(SIDEBAR_WIDTH))
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_primary())),
            ..Default::default()
        })
        .into()
}

/// Padding for sidebar controls (web `px-3 py-1.5`).
pub const SIDEBAR_CONTROL_PADDING: [f32; 2] = [space::S1_5, space::S3];

fn control_border(hovered_or_open: bool) -> Border {
    Border {
        color: if hovered_or_open { t::text_muted() } else { t::border() },
        width: 1.0,
        radius: radius::MD.into(),
    }
}

/// `pick_list` style for sidebar controls: foreground fill, stroke border,
/// `radius::MD`. Use as `.style(util::sidebar_pick_list_style)`.
pub fn sidebar_pick_list_style(_theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    pick_list::Style {
        text_color: t::text_body(),
        placeholder_color: t::text_muted(),
        handle_color: t::text_muted(),
        background: iced::Background::Color(t::bg_secondary()),
        border: control_border(!matches!(status, pick_list::Status::Active)),
    }
}

/// Dropdown menu style to pair with `sidebar_pick_list_style`: same fill, stroke
/// border and `radius::MD` as the closed control, accent for the highlighted
/// option. Use as `.menu_style(util::sidebar_menu_style)`.
pub fn sidebar_menu_style(_theme: &Theme) -> iced::overlay::menu::Style {
    iced::overlay::menu::Style {
        background: iced::Background::Color(t::bg_secondary()),
        border: control_border(false),
        text_color: t::text_body(),
        selected_text_color: t::on_accent(iced::widget::button::Status::Active),
        selected_background: iced::Background::Color(t::accent()),
    }
}

/// `text_input` style for sidebar controls; the focused border is the accent.
/// Use as `.style(util::sidebar_input_style)`.
pub fn sidebar_input_style(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut border = control_border(matches!(status, text_input::Status::Hovered));
    if matches!(status, text_input::Status::Focused) {
        border.color = t::accent();
    }
    text_input::Style {
        background: iced::Background::Color(t::bg_secondary()),
        border,
        icon: t::text_muted(),
        placeholder: t::text_muted(),
        value: t::text_body(),
        selection: iced::Color { a: 0.35, ..t::accent() },
    }
}

#[cfg(test)]
mod sidebar_tests {
    use super::*;

    #[test]
    fn sidebar_is_w_80_wide() {
        assert_eq!(SIDEBAR_WIDTH, 320.0);
    }

    #[test]
    fn heading_is_uppercased() {
        assert_eq!(sidebar_heading_text("Properties"), "PROPERTIES");
    }
}

#[cfg(test)]
mod date_tests {
    use super::*;

    #[test]
    fn short_date_formats_and_falls_back() {
        assert_eq!(short_date("2026-01-05T10:00:00Z"), "Jan 05");
        assert_eq!(short_date("2026-03-09"), "Mar 09");
        assert_eq!(short_date("bad"), "bad");
    }

    #[test]
    fn estimate_label() {
        assert_eq!(format_estimate(3.0), "3pt");
        assert_eq!(format_estimate(2.5), "2.5pt");
    }
}
