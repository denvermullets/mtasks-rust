//! Draws a hover card (JRST-299): a fixed-width panel in the app's top-level
//! stack, hung just under the span it describes.
//!
//! Add a field: draw it in `issue_card` (the data comes from
//! `body_hover::content_for`). Placement is `overlay`; JRST-309 refines it.

use iced::widget::{column, container, row, text};
use iced::{Border, Element, Length, Padding, Rectangle, Theme};

use crate::app::Message;
use crate::body_hover::{CardContent, IssueCard};
use crate::theme as t;

/// Card width in logical pixels.
pub const WIDTH: f32 = 320.0;

/// A transparent full-window layer holding the card at `anchor` (window
/// coordinates). Clicks outside the card pass through.
pub fn overlay<'a>(content: &CardContent, anchor: Rectangle) -> Element<'a, Message> {
    let card = match content {
        CardContent::Issue(issue) => issue_card(issue),
    };
    // The card reports the pointer so moving from the anchor onto it keeps it open.
    let card = iced::widget::mouse_area(card)
        .on_enter(Message::HoverCardEntered)
        .on_exit(Message::HoverCardLeft);
    container(card)
        .padding(Padding {
            top: anchor.y + anchor.height + t::space::S1,
            left: anchor.x.max(0.0),
            ..Padding::ZERO
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn issue_card<'a>(issue: &IssueCard) -> Element<'a, Message> {
    let mut top = row![text(issue.identifier.clone()).size(t::type_scale::XS).color(t::text_secondary())]
        .spacing(t::space::S2)
        .align_y(iced::Alignment::Center);
    if let Some(lane) = &issue.lane {
        top = top
            .push(iced::widget::horizontal_space())
            .push(crate::views::lane_icon::view(lane.icon))
            .push(text(lane.name.clone()).size(t::type_scale::XS).color(t::text_muted()));
    }
    // Groups of facts that wrap onto further lines when the card is narrow.
    let mut facts = vec![fact(vec![
        text("●").size(t::type_scale::XS).color(t::priority_color(issue.priority)).into(),
        muted(issue.priority_label),
    ])];
    if let Some(name) = &issue.assignee {
        facts.push(fact(vec![initials_badge(name), muted(name.clone())]));
    }
    if let Some(name) = &issue.project {
        facts.push(fact(vec![muted("▣"), muted(name.clone())]));
    }
    if let Some(due) = &issue.due {
        facts.push(fact(vec![muted("◷"), muted(due.clone())]));
    }
    let meta = iced::widget::Row::with_children(facts)
        .spacing(t::space::S3)
        .align_y(iced::Alignment::Center)
        .wrap();
    let body = column![
        top,
        text(issue.title.clone()).size(t::type_scale::SM).color(t::text_primary()),
        meta,
    ]
    .spacing(t::space::S1);
    container(body)
        .padding(t::space::S3)
        .width(WIDTH)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_modal())),
            border: Border { color: t::border(), width: 1.0, radius: t::radius::LG.into() },
            ..Default::default()
        })
        .into()
}

/// One icon-and-label group in the meta row.
fn fact<'a>(parts: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    iced::widget::Row::with_children(parts)
        .spacing(t::space::S1)
        .align_y(iced::Alignment::Center)
        .into()
}

fn muted<'a>(label: impl Into<String>) -> Element<'a, Message> {
    text(label.into()).size(t::type_scale::XS).color(t::text_muted()).into()
}

/// A small circle with the person's initials.
fn initials_badge<'a>(name: &str) -> Element<'a, Message> {
    container(text(crate::views::projects::lead_initials(name)).size(9).color(t::text_primary()))
        .center_x(Length::Fixed(16.0))
        .center_y(Length::Fixed(16.0))
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_card())),
            border: Border { color: t::border(), width: 1.0, radius: t::radius::FULL.into() },
            ..Default::default()
        })
        .into()
}
