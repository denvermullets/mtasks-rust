//! Building blocks for the full-page create/edit forms (issue, project,
//! recurring issue), which mirror the web's `new`/`edit` pages instead of
//! floating in a modal (JRST-454). The app header supplies the top bar
//! (`‹ Back` + crumbs); these pieces lay out the page below it.

use iced::widget::{button, column, container, row, scrollable, svg, text_editor, text_input, Column, Space};
use iced::{Alignment, Border, Color, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::Label;
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::text;
use crate::views::util;

/// Web `max-w-4xl`: the issue page's column.
pub const WIDTH_4XL: f32 = 896.0;
/// Web `max-w-3xl`: the project and recurring pages' column.
pub const WIDTH_3XL: f32 = 768.0;

/// The page body: scrolls, `p-8`, content capped at `max_width` and centered.
pub fn centered<'a>(content: impl Into<Element<'a, Message>>, max_width: f32) -> Element<'a, Message> {
    scrollable(
        container(container(content).max_width(max_width).width(Length::Fill))
            .center_x(Length::Fill)
            .padding(space::S8),
    )
    .height(Length::Fill)
    .into()
}

/// Same padding and scrolling as `centered`, left-aligned (the project page's
/// left column sits against its Properties sidebar).
pub fn left<'a>(content: impl Into<Element<'a, Message>>, max_width: f32) -> Element<'a, Message> {
    scrollable(
        container(container(content).max_width(max_width).width(Length::Fill))
            .width(Length::Fill)
            .padding(space::S8),
    )
    .height(Length::Fill)
    .into()
}

/// The big borderless title field (web `text-2xl font-semibold`, `p-0`).
pub fn title_input<'a>(
    placeholder: &str,
    value: &str,
    on_input: fn(String) -> Message,
) -> Element<'a, Message> {
    crate::undo_input::text_input(format!("title-{placeholder}"), placeholder, value, on_input)
        .padding(0)
        .size(type_scale::XXL)
        .style(borderless_input)
        .into()
}

fn borderless_input(_theme: &Theme, _status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: iced::Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        icon: t::text_muted(),
        placeholder: t::text_muted(),
        value: t::text_strong(),
        selection: Color { a: 0.35, ..t::accent() },
    }
}

/// A borderless text field inside a chip (the due date), in chip text color.
pub fn borderless_chip_input(_theme: &Theme, _status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: iced::Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        icon: t::text_muted(),
        placeholder: t::text_muted(),
        value: t::text_body(),
        selection: Color { a: 0.35, ..t::accent() },
    }
}

/// Borderless text editor style: the description reads as page text (web
/// `bg-transparent border-0 p-0`).
pub fn borderless_editor(_theme: &Theme, _status: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: iced::Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        icon: t::text_muted(),
        placeholder: t::text_muted(),
        value: t::text_body(),
        selection: Color { a: 0.35, ..t::accent() },
    }
}

/// A field caption (web `text-sm font-medium text-gray-400 mb-…`).
pub fn caption<'a>(label: &str) -> Element<'a, Message> {
    text(label.to_string()).size(type_scale::SM).color(t::text_secondary()).into()
}

/// A small caption over a control (web `text-xs font-medium text-gray-500`).
pub fn small_caption<'a>(label: &str) -> Element<'a, Message> {
    text(label.to_string()).size(type_scale::XS).color(t::text_muted()).into()
}

/// A section heading (web `text-lg font-medium text-gray-100`).
pub fn heading<'a>(label: &str) -> Element<'a, Message> {
    text(label.to_string()).size(type_scale::LG).color(t::text_strong()).into()
}

/// Inline validation text under a field.
pub fn error_text<'a>(msg: &str) -> Element<'a, Message> {
    text(msg.to_string()).size(type_scale::XS).color(t::danger()).into()
}

/// A 1px stroke-colored rule (web `border-t border-stroke`).
pub fn rule<'a>() -> Element<'a, Message> {
    container(Space::new(Length::Fill, Length::Fixed(1.0)))
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::border())),
            ..Default::default()
        })
        .into()
}

/// The bottom bar: a rule, then `left` and the right-aligned `actions`
/// (web `flex justify-between pt-6 border-t`).
pub fn actions_bar<'a>(
    left: Option<Element<'a, Message>>,
    actions: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![]
        .spacing(space::S6)
        .push(rule())
        .push(
            row![]
                .align_y(Alignment::Center)
                .push_maybe(left)
                .push(Space::with_width(Length::Fill))
                .push(actions),
        )
        .into()
}

/// The submit button (web `px-4 py-2 rounded-md border border-accent
/// text-accent hover:bg-accent`).
pub fn submit_button<'a>(label: &str, on_press: Message) -> button::Button<'a, Message> {
    button(text(label.to_string()).size(type_scale::SM))
        .on_press(on_press)
        .padding([space::S2, space::S4])
        .style(|_theme: &Theme, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: hovered.then(|| iced::Background::Color(t::accent())),
                text_color: if hovered { t::on_accent(status) } else { t::accent() },
                border: Border { color: t::accent(), width: 1.0, radius: radius::MD.into() },
                ..Default::default()
            }
        })
}

/// The quiet Cancel link (web `px-4 py-2 text-gray-400 hover:text-gray-300`).
pub fn cancel_button<'a>() -> button::Button<'a, Message> {
    button(text("Cancel").size(type_scale::SM))
        .on_press(Message::CloseModal)
        .padding([space::S2, space::S4])
        .style(|_theme: &Theme, status| button::Style {
            background: None,
            text_color: if matches!(status, button::Status::Hovered) {
                t::text_body()
            } else {
                t::text_secondary()
            },
            ..Default::default()
        })
}

/// A property chip button (web `px-3 py-1.5 rounded-md bg-foreground border
/// border-stroke text-sm`), sized like the pick lists beside it.
pub fn chip_button<'a>(
    content: impl Into<Element<'a, Message>>,
    on_press: Message,
    open: bool,
) -> Element<'a, Message> {
    button(content)
        .on_press(on_press)
        .padding(util::SIDEBAR_CONTROL_PADDING)
        .style(move |_theme: &Theme, status| button::Style {
            background: Some(iced::Background::Color(if matches!(status, button::Status::Hovered) {
                t::bg_card_focused()
            } else {
                t::bg_secondary()
            })),
            text_color: t::text_body(),
            border: Border {
                color: if open || matches!(status, button::Status::Hovered) {
                    t::text_muted()
                } else {
                    t::border()
                },
                width: 1.0,
                radius: radius::MD.into(),
            },
            ..Default::default()
        })
        .into()
}

/// A heroicon from the web templates, tinted `color` (`currentColor`).
pub fn icon<'a>(source: &'static str, size: f32, color: Color) -> Element<'a, Message> {
    svg(svg::Handle::from_memory(source.as_bytes()))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_theme: &Theme, _status| svg::Style { color: Some(color) })
        .into()
}

/// A property chip's face: icon then label (web `inline-flex gap-2`).
pub fn chip_face<'a>(icon_src: &'static str, label: impl Into<String>) -> Element<'a, Message> {
    row![]
        .spacing(space::S2)
        .align_y(Alignment::Center)
        .push(icon(icon_src, space::S4, t::text_body()))
        .push(text(label.into()).size(type_scale::SM).color(t::text_body()))
        .into()
}

/// The chip frame without a button (web `<label>` chips such as the due
/// date): same padding, fill and stroke as `chip_button`.
pub fn chip_frame<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(util::SIDEBAR_CONTROL_PADDING)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_secondary())),
            border: Border { color: t::border(), width: 1.0, radius: radius::MD.into() },
            ..Default::default()
        })
        .into()
}

/// A chip's open dropdown (web `min-w-[200px] bg-foreground border
/// rounded-md shadow-lg`): one `px-3 py-2` row per option, the current one
/// in the strong text color. Scrolls past ~10 rows.
pub fn option_menu<'a>(options: Vec<(String, bool, Message)>) -> Element<'a, Message> {
    let rows = options.into_iter().map(|(label, current, msg)| {
        button(text(label).size(type_scale::SM))
            .on_press(msg)
            .width(Length::Fill)
            .padding([space::S2, space::S3])
            .style(move |_theme: &Theme, status| button::Style {
                background: matches!(status, button::Status::Hovered)
                    .then(|| iced::Background::Color(t::bg_card_focused())),
                text_color: if current { t::text_strong() } else { t::text_body() },
                border: Border { radius: radius::MD.into(), ..Default::default() },
                ..Default::default()
            })
            .into()
    });
    container(scrollable(Column::with_children(rows).width(Length::Fill)).height(Length::Shrink))
        .max_height(320.0)
        .width(Length::Fill)
        .style(menu_style)
        .into()
}

fn menu_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(t::bg_secondary())),
        border: Border { color: t::border(), width: 1.0, radius: radius::MD.into() },
        shadow: iced::Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.4),
            offset: iced::Vector::new(0.0, 4.0),
            blur_radius: 12.0,
        },
        ..Default::default()
    }
}

/// The Labels chip's text: "Labels", or the picked names.
pub fn labels_summary(labels: &[Label], selected: &[String]) -> String {
    let names: Vec<&str> = labels
        .iter()
        .filter(|l| selected.contains(&l.id))
        .map(|l| l.name.as_str())
        .collect();
    match names.len() {
        0 => "Labels".to_string(),
        1..=2 => names.join(", "),
        n => format!("{n} labels"),
    }
}

/// The open Labels dropdown: one row per label (check, color dot, name), the
/// web's label picker list. `None` labels → an empty-state line.
pub fn label_menu<'a>(
    labels: &'a [Label],
    is_selected: impl Fn(&Label) -> bool,
    on_toggle: impl Fn(&Label) -> Message,
) -> Element<'a, Message> {
    let mut list = column![].spacing(0).width(Length::Fill);
    if labels.is_empty() {
        list = list.push(
            container(text("No labels available").size(type_scale::SM).color(t::text_muted()))
                .padding([space::S4, space::S3])
                .center_x(Length::Fill),
        );
    }
    for label in labels {
        let selected = is_selected(label);
        let dot_color = crate::views::labels::chip_color(label.color.as_deref());
        let check = text(if selected { "\u{2713}" } else { "" })
            .size(type_scale::SM)
            .color(t::accent())
            .width(Length::Fixed(14.0));
        let dot = container(Space::new(Length::Fixed(8.0), Length::Fixed(8.0))).style(move |_theme: &Theme| {
            container::Style {
                background: Some(iced::Background::Color(dot_color)),
                border: Border { radius: radius::FULL.into(), ..Default::default() },
                ..Default::default()
            }
        });
        let line = row![]
            .spacing(space::S2)
            .align_y(Alignment::Center)
            .push(check)
            .push(dot)
            .push(text(label.name.as_str()).size(type_scale::SM).color(t::text_body()));
        list = list.push(
            button(line)
                .on_press(on_toggle(label))
                .width(Length::Fill)
                .padding([space::S2, space::S3])
                .style(|_theme: &Theme, status| button::Style {
                    background: matches!(status, button::Status::Hovered)
                        .then(|| iced::Background::Color(t::bg_card_focused())),
                    text_color: t::text_body(),
                    ..Default::default()
                }),
        );
    }
    container(scrollable(list).height(Length::Shrink))
        .max_height(240.0)
        .width(Length::Fixed(320.0))
        .padding([space::S1, 0.0])
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_card())),
            border: Border { color: t::border(), width: 1.0, radius: radius::MD.into() },
            ..Default::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(id: &str, name: &str) -> Label {
        Label {
            id: id.into(),
            remote_id: None,
            team_id: "t".into(),
            name: name.into(),
            color: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn labels_summary_names_up_to_two_then_counts() {
        let labels = vec![label("a", "bug"), label("b", "ui"), label("c", "p1")];
        assert_eq!(labels_summary(&labels, &[]), "Labels");
        assert_eq!(labels_summary(&labels, &["b".into()]), "ui");
        assert_eq!(labels_summary(&labels, &["a".into(), "b".into()]), "bug, ui");
        assert_eq!(labels_summary(&labels, &["a".into(), "b".into(), "c".into()]), "3 labels");
    }
}
