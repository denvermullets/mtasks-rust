use iced::widget::{button, column, container, row, Row};
use crate::views::text;
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
    /// Local UUID of the label being edited, if edit mode is open.
    pub editing_id: Option<String>,
    /// Edit-mode name and color working copies.
    pub edit_name: String,
    pub edit_color: String,
    /// Inline validation message for edit mode.
    pub edit_error: Option<String>,
}

impl LabelFormState {
    pub fn new(issue_id: Option<String>, selected_label_ids: Vec<String>) -> Self {
        Self {
            name: String::new(),
            color: PALETTE[0].to_string(),
            issue_id,
            selected_label_ids,
            error: None,
            editing_id: None,
            edit_name: String::new(),
            edit_color: PALETTE[0].to_string(),
            edit_error: None,
        }
    }
}

/// Resolve a label's stored hex to a color, falling back to a muted grey when
/// the color is missing or unparseable so a chip always renders.
pub fn chip_color(hex: Option<&str>) -> Color {
    hex.and_then(t::hex_to_color).unwrap_or(t::priority_none())
}

/// A filled colored pill used to render a label. `selected` controls whether it
/// reads as "on" (filled) or "off" (outline only) — the board passes `true`.
fn pill<'a>(label: &'a Label, selected: bool) -> Element<'a, Message> {
    let color = chip_color(label.color.as_deref());
    let (bg, text_color, border_w) = if selected {
        (Color { a: 0.22, ..color }, color, 1.0)
    } else {
        (Color::TRANSPARENT, t::text_muted(), 1.0)
    };
    let border_color = if selected { color } else { t::border() };

    // XS text on the XS line height is 16px; with S0_5 padding the pill is 20px tall,
    // which is `scroll::CARD_CHIP_ROW_H`.
    container(
        text(&label.name)
            .size(t::type_scale::XS)
            .line_height(iced::widget::text::LineHeight::Relative(t::type_scale::LH_XS))
            .color(text_color),
    )
    .padding([t::space::S0_5, t::space::S2])
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(bg)),
            border: Border {
                color: border_color,
                width: border_w,
                radius: t::radius::SM.into(),
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

fn color_swatches<'a>(
    selected_hex: &str,
    on_pick: fn(String) -> Message,
) -> Element<'a, Message> {
    let mut r = row![].spacing(6);
    for hex in PALETTE {
        let color = t::hex_to_color(hex).unwrap_or(t::priority_none());
        let is_selected = selected_hex.eq_ignore_ascii_case(hex);
        let border_color = if is_selected {
            t::text_primary()
        } else {
            t::border()
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
                .on_press(on_pick(hex.to_string()))
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
            .color(t::text_muted())
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
        if !labels.len().is_multiple_of(4) {
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
            crate::undo_input::text_input("label-new-name", "New label name", &form.name, Message::LabelNameChanged)
                .on_submit(Message::CreateLabel)
                .padding(8)
                .size(13)
                .width(Length::Fill),
        )
        .push(color_swatches(&form.color, Message::LabelColorChanged))
        .push(
            button(text("Add").size(13).color(t::bg_primary()))
                .on_press(Message::CreateLabel)
                .padding([8, 16])
                .style(move |_theme: &Theme, status| button::Style {
                    background: Some(iced::Background::Color(t::accent_fill(status))),
                    text_color: t::on_accent(status),
                    border: Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        );

    // ── Edit section: pick a label, then rename/recolor it ──
    let edit_section: Element<'a, Message> = if labels.is_empty() {
        column![].into()
    } else {
        let mut col = column![]
            .spacing(8)
            .push(text("Edit a label").size(12).color(t::text_muted()))
            .push(chip_grid(
                labels,
                |l| form.editing_id.as_deref() == Some(l.id.as_str()),
                |l| Message::SelectLabelForEdit(l.id.clone()),
                4,
            ));
        if form.editing_id.is_some() {
            col = col
                .push(
                    row![]
                        .spacing(8)
                        .align_y(iced::Alignment::Center)
                        .push(
                            crate::undo_input::text_input(
                                format!("label-name-{}", form.editing_id.as_deref().unwrap_or_default()),
                                "Label name",
                                &form.edit_name,
                                Message::EditLabelNameChanged,
                            )
                                .on_submit(Message::SaveLabelEdit)
                                .padding(8)
                                .size(13)
                                .width(Length::Fill),
                        )
                        .push(color_swatches(&form.edit_color, Message::EditLabelColorChanged))
                        .push(
                            button(text("Save").size(13).color(t::bg_primary()))
                                .on_press(Message::SaveLabelEdit)
                                .padding([8, 16])
                                .style(move |_theme: &Theme, status| button::Style {
                                    background: Some(iced::Background::Color(t::accent_fill(
                                        status,
                                    ))),
                                    text_color: t::on_accent(status),
                                    border: Border {
                                        radius: 4.0.into(),
                                        ..Default::default()
                                    },
                                    ..Default::default()
                                }),
                        )
                        .push(
                            button(text("Delete").size(13).color(iced::Color::WHITE))
                                .on_press(Message::ConfirmDeleteLabel)
                                .padding([8, 16])
                                .style(move |_theme: &Theme, _status| button::Style {
                                    background: Some(iced::Background::Color(t::danger())),
                                    text_color: iced::Color::WHITE,
                                    border: Border {
                                        radius: 4.0.into(),
                                        ..Default::default()
                                    },
                                    ..Default::default()
                                }),
                        ),
                )
                .push(
                    form.edit_error
                        .as_deref()
                        .map(|e| text(e).size(12).color(t::danger())),
                );
        }
        col.into()
    };

    let mut form_content = column![]
        .spacing(14)
        .width(Length::Fixed(440.0))
        .push(text("Labels").size(18).color(t::text_primary()))
        .push(text(attach_header).size(12).color(t::text_muted()))
        .push(attach_section)
        .push(edit_section)
        .push(
            container(text(""))
                .height(Length::Fixed(1.0))
                .width(Length::Fill)
                .style(|_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(t::border())),
                    ..Default::default()
                }),
        )
        .push(text("Create a new label").size(12).color(t::text_muted()))
        .push(create_row)
        .push(
            form.error
                .as_deref()
                .map(|e| text(e).size(12).color(t::danger())),
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
                button(text(done_label).size(14).color(t::bg_primary()))
                    .on_press(done_msg)
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
