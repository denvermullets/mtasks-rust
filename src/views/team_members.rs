use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Border, Element, Length, Theme};

use crate::app::{Message, TeamWebSection};
use crate::db::models::{Member, Team};
use crate::theme as t;

/// Read-only members panel for a team. Members are Rails-managed (invites,
/// removals), so this lists the local mirror and links out to the web app.
pub fn modal_view<'a>(team: &'a Team, members: &'a [Member]) -> Element<'a, Message> {
    let list: Element<'a, Message> = if members.is_empty() {
        text(if team.remote_id.is_some() {
            "No members synced yet."
        } else {
            "This team only exists locally, so it has no members."
        })
        .size(12)
        .color(t::TEXT_MUTED)
        .into()
    } else {
        let mut col = column![].spacing(10);
        for member in members {
            col = col.push(
                row![]
                    .spacing(12)
                    .align_y(iced::Alignment::Center)
                    .push(
                        text(&member.name)
                            .size(14)
                            .color(t::TEXT_PRIMARY)
                            .width(Length::Fill),
                    )
                    .push(text(&member.email).size(12).color(t::TEXT_MUTED)),
            );
        }
        scrollable(col).height(Length::Shrink).into()
    };

    let mut footer = row![].spacing(8).push(
        button(text("Close").size(14))
            .on_press(Message::CloseModal)
            .padding([8, 24])
            .style(button::secondary),
    );
    if team.remote_id.is_some() {
        footer = footer.push(
            button(
                text("Manage in web app \u{2197}")
                    .size(14)
                    .color(t::BG_PRIMARY),
            )
            .on_press(Message::OpenTeamWebSettings(
                team.id.clone(),
                TeamWebSection::Members,
            ))
            .padding([8, 16])
            .style(move |_theme: &Theme, _status| button::Style {
                background: Some(iced::Background::Color(t::ACCENT)),
                text_color: t::BG_PRIMARY,
                border: Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        );
    }

    let content = column![]
        .spacing(14)
        .width(Length::Fixed(440.0))
        .push(
            text(format!("{} members", team.name))
                .size(18)
                .color(t::TEXT_PRIMARY),
        )
        .push(
            text("Members are managed in the web app.")
                .size(12)
                .color(t::TEXT_MUTED),
        )
        .push(container(list).max_height(360.0))
        .push(footer);

    let modal = container(content)
        .padding(28)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_MODAL)),
            border: Border {
                color: t::BORDER,
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
