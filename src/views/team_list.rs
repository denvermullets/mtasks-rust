use iced::widget::{button, column, container, row, text};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::Team;
use crate::theme as t;

pub fn view<'a>(teams: &'a [Team], selected_team_id: Option<&'a str>) -> Element<'a, Message> {
    let mut col = column![].spacing(0).width(Length::Fill);

    // Header
    col = col.push(
        container(
            text("YOUR TEAMS")
                .size(11)
                .color(t::TEXT_MUTED),
        )
        .padding([14, 14]),
    );

    for team in teams {
        let is_selected = selected_team_id == Some(team.id.as_str());
        let team_id_clone = team.id.clone();

        let identifier_badge = container(
            text(&team.identifier)
                .size(11)
                .color(if is_selected {
                    t::ACCENT
                } else {
                    t::TEXT_MUTED
                }),
        )
        .padding([2, 4]);

        let name_text = text(&team.name)
            .size(14)
            .color(if is_selected {
                t::TEXT_PRIMARY
            } else {
                t::TEXT_SECONDARY
            });

        let row_content = row![]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .push(identifier_badge)
            .push(name_text);

        let bg = if is_selected {
            t::BG_CARD_FOCUSED
        } else {
            t::BG_MENU
        };

        let btn = button(row_content)
            .on_press(Message::SelectTeam(team_id_clone))
            .padding([8, 14])
            .width(Length::Fill)
            .style(move |_theme: &Theme, _status| {
                button::Style {
                    background: Some(iced::Background::Color(bg)),
                    text_color: t::TEXT_PRIMARY,
                    border: Border {
                        color: if is_selected {
                            t::ACCENT
                        } else {
                            iced::Color::TRANSPARENT
                        },
                        width: if is_selected { 2.0 } else { 0.0 },
                        radius: 0.0.into(),
                    },
                    ..Default::default()
                }
            });

        col = col.push(btn);
    }

    container(col)
        .padding(0)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_MENU)),
            ..Default::default()
        })
        .into()
}
