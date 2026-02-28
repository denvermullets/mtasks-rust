use iced::widget::{button, column, container, text};
use iced::{Element, Length};

use crate::app::Message;
use crate::db::models::Team;
use crate::theme;

pub fn view<'a>(teams: &'a [Team], selected_team_id: Option<&'a str>) -> Element<'a, Message> {
    let mut col = column![].spacing(4).width(Length::Fill);

    for team in teams {
        let is_selected = selected_team_id == Some(team.id.as_str());
        let label = format!("{} ({})", team.name, team.identifier);

        let btn = button(
            text(label)
                .size(14)
                .color(if is_selected {
                    theme::ACCENT
                } else {
                    theme::TEXT_PRIMARY
                }),
        )
        .on_press(Message::SelectTeam(team.id.clone()))
        .padding([6, 12])
        .width(Length::Fill)
        .style(if is_selected {
            button::secondary
        } else {
            button::text
        });

        col = col.push(btn);
    }

    container(col).padding(8).into()
}
