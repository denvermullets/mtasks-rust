//! Draws a hover card (JRST-299): a fixed-width panel in the app's top-level
//! stack, hung just under the span it describes.
//!
//! Add a field: draw it in `issue_card` (the data comes from
//! `body_hover::content_for`). Placement is `overlay`; JRST-309 refines it.

use iced::widget::{column, container, text};
use iced::{Border, Element, Length, Padding, Rectangle, Theme};

use crate::app::Message;
use crate::body_hover::{CardContent, IssueCard, MemberCard};
use crate::theme as t;

/// Card width in logical pixels.
pub const WIDTH: f32 = 320.0;

/// A transparent full-window layer holding the card at `anchor` (window
/// coordinates). Clicks outside the card pass through.
pub fn overlay<'a>(content: &CardContent, anchor: Rectangle) -> Element<'a, Message> {
    let card = match content {
        CardContent::Issue(issue) => issue_card(issue),
        CardContent::Member(member) => member_card(member),
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

fn member_card<'a>(member: &MemberCard) -> Element<'a, Message> {
    let body = column![
        text(member.name.clone()).size(t::type_scale::SM).color(t::text_primary()),
        text(member.email.clone()).size(t::type_scale::XS).color(t::text_secondary()),
    ]
    .spacing(t::space::S1);
    panel(body.into())
}

fn issue_card<'a>(issue: &IssueCard) -> Element<'a, Message> {
    let body = column![
        text(issue.identifier.clone()).size(t::type_scale::XS).color(t::text_secondary()),
        text(issue.title.clone()).size(t::type_scale::SM).color(t::text_primary()),
    ]
    .spacing(t::space::S1);
    panel(body.into())
}

fn panel<'a>(body: Element<'a, Message>) -> Element<'a, Message> {
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
