//! Draws a hover card (JRST-299): a fixed-width panel in the app's top-level
//! stack, hung just under the span it describes.
//!
//! Add a field: draw it in `issue_card` (the data comes from
//! `body_hover::content_for`). Placement is `card_placement::place`, called from
//! `overlay`; keep `estimated_height` in step with the card's rows.

use iced::widget::{column, container, text};
use iced::{alignment, Border, Element, Length, Padding, Rectangle, Size, Theme};

use crate::app::Message;
use crate::body_hover::{CardContent, IssueCard, MemberCard};
use crate::card_placement::{self, Vertical};
use crate::theme as t;

/// Card width in logical pixels.
pub const WIDTH: f32 = 320.0;

/// A transparent full-window layer holding the card at `anchor` (window
/// coordinates). Clicks outside the card pass through.
pub fn overlay<'a>(content: &CardContent, anchor: Rectangle) -> Element<'a, Message> {
    let content = content.clone();
    let height = estimated_height(&content);
    // `responsive` supplies the window size at layout time, with no app message.
    iced::widget::responsive(move |window| {
        let placed = card_placement::place(anchor, Size::new(WIDTH, height), window);
        let (top, bottom, valign) = match placed.vertical {
            Vertical::Top(y) => (y, 0.0, alignment::Vertical::Top),
            Vertical::Bottom(gap) => (0.0, gap, alignment::Vertical::Bottom),
        };
        let card = match &content {
            CardContent::Issue(issue) => issue_card(issue),
            CardContent::Member(member) => member_card(member),
        };
        // The card reports the pointer so moving from the anchor onto it keeps it open.
        let card = iced::widget::mouse_area(card)
            .on_enter(Message::HoverCardEntered)
            .on_exit(Message::HoverCardLeft);
        container(card)
            .padding(Padding { top, bottom, left: placed.left, right: 0.0 })
            .align_y(valign)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    })
    .into()
}

/// Rough card height for the flip decision: padding plus the identifier line
/// and the title wrapped at about 0.55 em per character. Update when
/// `issue_card` gains rows (JRST-301 to 305).
fn estimated_height(content: &CardContent) -> f32 {
    match content {
        CardContent::Issue(issue) => {
            let per_line = ((WIDTH - 2.0 * t::space::S3) / (t::type_scale::SM * 0.55)).floor();
            let title_lines = (issue.title.chars().count() as f32 / per_line).ceil().max(1.0);
            2.0 * t::space::S3
                + t::type_scale::XS * t::type_scale::LH_XS
                + t::space::S1
                + title_lines * t::type_scale::SM * t::type_scale::LH_SM
        }
        // Name, email and count: three single lines.
        CardContent::Member(_) => {
            2.0 * t::space::S3
                + t::type_scale::SM * t::type_scale::LH_SM
                + 2.0 * (t::space::S1 + t::type_scale::XS * t::type_scale::LH_XS)
        }
    }
}

fn member_card<'a>(member: &MemberCard) -> Element<'a, Message> {
    let body = column![
        text(member.name.clone()).size(t::type_scale::SM).color(t::text_primary()),
        text(member.email.clone()).size(t::type_scale::XS).color(t::text_secondary()),
        text(match member.open_issues {
            1 => "1 open issue".to_string(),
            n => format!("{n} open issues"),
        })
        .size(t::type_scale::XS)
        .color(t::text_secondary()),
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
