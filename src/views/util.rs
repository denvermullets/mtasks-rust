//! Small view-layer formatting helpers shared across views.

use chrono::{DateTime, Utc};
use iced::widget::{column, container, pick_list, scrollable, text_input};
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
            background: t::repaint(t::bg_primary()),
            ..Default::default()
        })
        .into()
}

/// Where a detail page (issue or project) draws its Properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertiesPlacement {
    /// Hidden (`/` shows them).
    Hidden,
    /// The fixed-width panel down the page's right side.
    Sidebar,
    /// A card under the description, its fields wrapping to the page width.
    Inline,
}

/// Properties go under the description while a right-docked Terminal panel
/// shares the page, which would leave too little room for the panel too.
pub fn properties_placement(open: bool, terminal_width: f32) -> PropertiesPlacement {
    match (open, terminal_width > 0.0) {
        (false, _) => PropertiesPlacement::Hidden,
        (true, false) => PropertiesPlacement::Sidebar,
        (true, true) => PropertiesPlacement::Inline,
    }
}

/// A detail page's Properties, for `sidebar_properties` or
/// `inline_properties` to lay out.
pub struct Properties<'a, M> {
    /// Each field's label and control: the ones always shown inline.
    pub fields: Vec<(&'a str, Element<'a, M>)>,
    /// Fields the inline card shows only once "More" is open. The sidebar
    /// shows them after `fields`.
    pub more: Vec<(&'a str, Element<'a, M>)>,
    /// Text buttons after the fields (e.g. "Move to team…"). Inline, under "More".
    pub actions: Vec<Element<'a, M>>,
    /// RFC3339 timestamps for the "Created" / "Updated" lines.
    pub created_at: &'a str,
    pub updated_at: &'a str,
}

/// Width of one label + control cell in the inline Properties card.
pub const INLINE_FIELD_WIDTH: f32 = 272.0;

/// Width of the label column inside an inline cell.
const INLINE_LABEL_WIDTH: f32 = 76.0;

/// Width of an inline cell's control: what its dropdown menu should match.
pub const INLINE_CONTROL_WIDTH: f32 = INLINE_FIELD_WIDTH - INLINE_LABEL_WIDTH - space::S2;

/// The inline Properties card, kept short: each label sits left of its
/// control and the cells wrap to the page width. Only `fields` show until
/// `expanded`; then `more` follows, and the actions and timestamps share one
/// closing line. A trailing "More" / "Less" cell sends `on_toggle`. No
/// heading; the card is the grouping.
pub fn inline_properties<'a, M: Clone + 'a>(
    props: Properties<'a, M>,
    expanded: bool,
    on_toggle: M,
) -> Element<'a, M> {
    let fields = if expanded {
        props.fields.into_iter().chain(props.more).collect()
    } else {
        props.fields
    };
    let toggle = iced::widget::button(
        text(if expanded { "Less \u{25B4}" } else { "More \u{25BE}" })
            .size(type_scale::XS)
            .color(t::accent()),
    )
    .on_press(on_toggle)
    .padding(0)
    .style(iced::widget::button::text);
    let cells = fields
        .into_iter()
        .map(|(label, control)| {
            iced::widget::row![]
                .spacing(space::S2)
                .align_y(iced::Alignment::Center)
                .push(text(label).size(type_scale::XS).color(t::text_muted()).width(Length::Fixed(INLINE_LABEL_WIDTH)))
                .push(container(control).width(Length::Fill))
                .width(Length::Fixed(INLINE_FIELD_WIDTH))
                .into()
        })
        .chain(std::iter::once(toggle.into()));
    let footer = expanded.then(|| {
        iced::widget::Row::with_children(props.actions)
            .spacing(space::S4)
            .align_y(iced::Alignment::Center)
            .push(iced::widget::space::horizontal())
            .push(
                text(format!(
                    "Created {} \u{00B7} Updated {}",
                    relative_time(props.created_at),
                    relative_time(props.updated_at)
                ))
                .size(type_scale::XS)
                .color(t::text_muted()),
            )
    });
    container(
        column![]
            .spacing(space::S3)
            .push(
                // Centered per wrapped line, so a cell shorter than a control
                // (Creator's text, an empty "+ Add …") lines up with its neighbours.
                iced::widget::Row::with_children(cells)
                    .spacing(space::S3)
                    .align_y(iced::Alignment::Center)
                    .wrap(),
            )
            .push(footer),
    )
    .padding([space::S3, space::S4])
    .width(Length::Fill)
    .style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_secondary())),
        border: Border { color: t::border(), width: 1.0, radius: radius::MD.into() },
        ..Default::default()
    })
    .into()
}

/// The Properties panel for `PropertiesPlacement::Sidebar`: heading, each
/// label above its control, actions, then the timestamps, down a scrolling
/// fixed-width column.
pub fn sidebar_properties<'a, M: 'a>(props: Properties<'a, M>) -> Element<'a, M> {
    let timestamps = column![]
        .spacing(space::S0_5)
        .push(text(format!("Created {}", props.created_at)).size(type_scale::XS).color(t::text_muted()))
        .push(text(format!("Updated {}", props.updated_at)).size(type_scale::XS).color(t::text_muted()));
    let content = column![]
        .spacing(space::S6)
        .padding(space::S6)
        .width(Length::Fill)
        .push(sidebar_heading("Properties"))
        .extend(
            props
                .fields
                .into_iter()
                .chain(props.more)
                .map(|(label, control)| sidebar_field(label, control)),
        )
        .extend(props.actions)
        .push(timestamps);
    sidebar_panel(scrollable(content))
}

/// Thin full-height rule standing in for a `border-l` between panes.
pub fn vertical_divider<'a, M: 'a>() -> Element<'a, M> {
    container(column![])
        .width(Length::Fixed(1.0))
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::border())),
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
        shadow: iced::Shadow::default(),
    }
}

/// `text_input` style for sidebar controls; the focused border is the accent.
/// Use as `.style(util::sidebar_input_style)`.
pub fn sidebar_input_style(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut border = control_border(matches!(status, text_input::Status::Hovered));
    if matches!(status, text_input::Status::Focused { .. }) {
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
    fn properties_go_inline_beside_a_right_docked_terminal() {
        assert_eq!(properties_placement(false, 0.0), PropertiesPlacement::Hidden);
        assert_eq!(properties_placement(false, 480.0), PropertiesPlacement::Hidden);
        assert_eq!(properties_placement(true, 0.0), PropertiesPlacement::Sidebar);
        assert_eq!(properties_placement(true, 480.0), PropertiesPlacement::Inline);
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

/// Gap kept between a modal panel and the window edge.
const MODAL_GUTTER: f32 = 24.0;

/// The dimmed full-window backdrop behind a centered modal panel. The panel
/// sits in a vertical scrollable, so a panel taller than the window scrolls
/// instead of being clipped. It stays vertically centered while it fits.
pub fn modal_backdrop<'a, M: 'a>(panel: impl Into<Element<'a, M>>) -> Element<'a, M> {
    modal_layer(panel, false)
}

/// `modal_backdrop` with the panel pinned near the top of the window instead
/// of centered (the command palette, whose height changes as results filter).
pub fn modal_backdrop_top<'a, M: 'a>(panel: impl Into<Element<'a, M>>) -> Element<'a, M> {
    modal_layer(panel, true)
}

fn modal_layer<'a, M: 'a>(panel: impl Into<Element<'a, M>>, top: bool) -> Element<'a, M> {
    let pad = if top { iced::Padding::from([96.0, MODAL_GUTTER]) } else { iced::Padding::from(MODAL_GUTTER) };
    // Shrink height: the scrollable's content must not fill its scroll axis,
    // and `container` would otherwise inherit a Fill height from the panel.
    let inner = container(panel).center_x(Length::Fill).height(Length::Shrink).padding(pad);
    let scroll = scrollable(inner).width(Length::Fill).height(Length::Shrink);
    let outer = container(scroll).width(Length::Fill).height(Length::Fill);
    let outer = if top { outer.align_top(Length::Fill) } else { outer.center_y(Length::Fill) };
    outer
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(0.0, 0.0, 0.0, 0.7))),
            ..Default::default()
        })
        .into()
}

/// The assignee's initial in an accent circle (web `user_avatar size: "sm"`).
pub fn avatar<'a, Message: 'a>(name: &str) -> Element<'a, Message> {
    let initial: String = name.chars().next().into_iter().flat_map(char::to_uppercase).collect();
    container(text(initial).size(type_scale::XS).font(crate::fonts::bold_active()).color(t::on_fill(t::accent())))
        .center_x(Length::Fixed(space::S5))
        .center_y(Length::Fixed(space::S5))
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::accent())),
            border: Border { radius: radius::FULL.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}
