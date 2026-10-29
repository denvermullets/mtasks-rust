use iced::widget::{button, column, container, row, text, text_input, Row};
use iced::{Border, Color, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::Label;
use crate::theme as t;

/// Palette mirrored from Rails `Label.random_color` so desktop-created labels
/// use the same six swatches the server would pick from.
pub const PALETTE: [&str; 6] = [
    "#ef4444", "#f59e0b", "#10b981", "#3b82f6", "#8b5cf6", "#ec4899",
];

/// State for the label modal. The modal is scoped to a single focused issue:
/// team labels are toggled on/off for that issue, and new labels can be created
/// inline. `selected_label_ids` is a working copy applied on save.
pub struct LabelFormState {
    /// New-label name input.
    pub name: String,
    /// New-label color (hex, always one of [`PALETTE`]).
    pub color: String,
    /// Local UUID of the issue being labeled, if the modal was opened on a card.
    pub issue_id: Option<String>,
    /// Working set of attached label UUIDs, toggled in the UI, applied on save.
    pub selected_label_ids: Vec<String>,
    /// Inline validation message shown under the name input. `None` = valid.
    pub error: Option<String>,
}

impl LabelFormState {
    pub fn new(issue_id: Option<String>, selected_label_ids: Vec<String>) -> Self {
        Self {
            name: String::new(),
            color: PALETTE[0].to_string(),
            issue_id,
            selected_label_ids,
            error: None,
        }
    }
}

/// Resolve a label's stored hex to a color, falling back to a muted grey when
/// the color is missing or unparseable so a chip always renders.
pub fn chip_color(hex: Option<&str>) -> Color {
    hex.and_then(t::hex_to_color).unwrap_or(t::PRIORITY_NONE())
}

/// A filled colored pill used to render a label. `selected` controls whether it
/// reads as "on" (filled) or "off" (outline only) — the board passes `true`.
fn pill<'a>(label: &'a Label, selected: bool) -> Element<'a, Message> {
    let color = chip_color(label.color.as_deref());
    let (bg, text_color, border_w) = if selected {
        (Color { a: 0.22, ..color }, color, 1.0)
    } else {
        (Color::TRANSPARENT, t::TEXT_MUTED(), 1.0)
    };
    let border_color = if selected { color } else { t::BORDER() };

    container(text(&label.name).size(11).color(text_color))
        .padding([2, 8])
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(bg)),
            border: Border {
                color: border_color,
                width: border_w,
                radius: 8.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// Non-interactive chip for the board card.
pub fn static_chip<'a>(label: &'a Label) -> Element<'a, Message> {
    pill(label, true)
}

/// Clickable chip that emits `msg` when pressed. Used for the multi-select in
/// both the issue form and the label modal.
pub fn toggle_chip<'a>(label: &'a Label, selected: bool, msg: Message) -> Element<'a, Message> {
    button(pill(label, selected))
        .on_press(msg)
        .padding(0)
        .style(|_theme: &Theme, _status| button::Style {
            background: Some(iced::Background::Color(Color::TRANSPARENT)),
            ..Default::default()
        })
        .into()
}

/// Wrap chips so they flow onto multiple lines within a fixed width. iced 0.13
/// has no flex-wrap, so we pack a fixed number per row.
pub fn chip_grid<'a>(
    labels: &'a [Label],
    is_selected: impl Fn(&Label) -> bool,
    make_msg: impl Fn(&Label) -> Message,
    per_row: usize,
) -> Element<'a, Message> {
    let mut col = column![].spacing(6);
    let mut current: Row<'a, Message> = row![].spacing(6);
    let mut count = 0;
    for label in labels {
        current = current.push(toggle_chip(label, is_selected(label), make_msg(label)));
        count += 1;
        if count % per_row == 0 {
            col = col.push(current);
            current = row![].spacing(6);
        }
    }
    if count % per_row != 0 {
        col = col.push(current);
    }
    col.into()
}

fn color_swatches<'a>(selected_hex: &str) -> Element<'a, Message> {
    let mut r = row![].spacing(6);
    for hex in PALETTE {
        let color = t::hex_to_color(hex).unwrap_or(t::PRIORITY_NONE());
        let is_selected = selected_hex.eq_ignore_ascii_case(hex);
        let border_color = if is_selected {
            t::TEXT_PRIMARY()
        } else {
            t::BORDER()
        };
        let swatch = container(text(" ").size(10))
            .width(Length::Fixed(20.0))
            .height(Length::Fixed(20.0))
            .style(move |_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(color)),
                border: Border {
                    color: border_color,
                    width: if is_selected { 2.0 } else { 1.0 },
                    radius: 4.0.into(),
                },
                ..Default::default()
            });
        r = r.push(
            button(swatch)
                .padding(0)
                .on_press(Message::LabelColorChanged(hex.to_string()))
                .style(|_theme: &Theme, _status| button::Style {
                    background: Some(iced::Background::Color(Color::TRANSPARENT)),
                    ..Default::default()
                }),
        );
    }
    r.into()
}

pub fn modal_view<'a>(form: &'a LabelFormState, labels: &'a [Label]) -> Element<'a, Message> {
    let has_issue = form.issue_id.is_some();

    // ── Attach section (only meaningful when opened on a card) ──
    let attach_section: Element<'a, Message> = if labels.is_empty() {
        text("No labels yet — create one below.")
            .size(12)
            .color(t::TEXT_MUTED())
            .into()
    } else if has_issue {
        chip_grid(
            labels,
            |l| form.selected_label_ids.contains(&l.id),
            |l| Message::ToggleIssueLabel(l.id.clone()),
            4,
        )
    } else {
        // No focused issue: show labels read-only (all filled) as a reference.
        let mut col = column![].spacing(6);
        let mut current: Row<'a, Message> = row![].spacing(6);
        for (i, label) in labels.iter().enumerate() {
            current = current.push(static_chip(label));
            if (i + 1) % 4 == 0 {
                col = col.push(current);
                current = row![].spacing(6);
            }
        }
        if labels.len() % 4 != 0 {
            col = col.push(current);
        }
        col.into()
    };

    let attach_header = if has_issue {
        "Toggle labels for this issue"
    } else {
        "Team labels"
    };

    // ── Create-new section ──
    let create_row = row![]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .push(
            text_input("New label name", &form.name)
                .on_input(Message::LabelNameChanged)
                .on_submit(Message::CreateLabel)
                .padding(8)
                .size(13)
                .width(Length::Fill),
        )
        .push(color_swatches(&form.color))
        .push(
            button(text("Add").size(13).color(t::BG_PRIMARY()))
                .on_press(Message::CreateLabel)
                .padding([8, 16])
                .style(move |_theme: &Theme, _status| button::Style {
                    background: Some(iced::Background::Color(t::ACCENT())),
                    text_color: t::BG_PRIMARY(),
                    border: Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        );

    let mut form_content = column![]
        .spacing(14)
        .width(Length::Fixed(440.0))
        .push(text("Labels").size(18).color(t::TEXT_PRIMARY()))
        .push(text(attach_header).size(12).color(t::TEXT_MUTED()))
        .push(attach_section)
        .push(
            container(text(""))
                .height(Length::Fixed(1.0))
                .width(Length::Fill)
                .style(|_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(t::BORDER())),
                    ..Default::default()
                }),
        )
        .push(text("Create a new label").size(12).color(t::TEXT_MUTED()))
        .push(create_row)
        .push_maybe(
            form.error
                .as_deref()
                .map(|e| text(e).size(12).color(t::DANGER())),
        );

    // Footer: Done applies the working set to the issue; Close just dismisses.
    let done_label = if has_issue { "Done" } else { "Close" };
    let done_msg = if has_issue {
        Message::SaveIssueLabels
    } else {
        Message::CloseModal
    };
    form_content = form_content.push(
        row![]
            .spacing(8)
            .push(
                button(text(done_label).size(14).color(t::BG_PRIMARY()))
                    .on_press(done_msg)
                    .padding([8, 24])
                    .style(move |_theme: &Theme, _status| button::Style {
                        background: Some(iced::Background::Color(t::ACCENT())),
                        text_color: t::BG_PRIMARY(),
                        border: Border {
                            radius: 4.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
            )
            .push(
                button(text("Cancel").size(14))
                    .on_press(Message::CloseModal)
                    .padding([8, 24])
                    .style(button::secondary),
            ),
    );

    let modal = container(form_content)
        .padding(28)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_MODAL())),
            border: Border {
                color: t::BORDER(),
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
