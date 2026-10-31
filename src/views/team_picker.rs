use iced::widget::{button, column, container, scrollable, text};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::Team;
use crate::theme as t;

/// Teams an issue in `source_team_id` can move to: every other live team.
/// Archived teams are the ones Rails stopped listing for the token, so what
/// remains is the user's other teams the token covers. Teams without a
/// `remote_id` are skipped: a move to one would wait forever in prep.
pub fn move_targets<'a>(teams: &'a [Team], source_team_id: &str) -> Vec<&'a Team> {
    teams
        .iter()
        .filter(|t| t.id != source_team_id && t.archived_at.is_none() && t.remote_id.is_some())
        .collect()
}

/// "Move to team" modal: one row per target team. Esc / Cancel dismisses.
pub fn view<'a>(teams: &[&'a Team]) -> Element<'a, Message> {
    let mut list = column![].spacing(2);
    for team in teams {
        list = list.push(
            button(
                text(format!("{}  {}", team.identifier, team.name))
                    .size(13)
                    .color(t::text_primary()),
            )
            .on_press(Message::MoveTeamSelected(team.id.clone()))
            .padding([6, 8])
            .width(Length::Fill)
            .style(button::text),
        );
    }
    let results: Element<'a, Message> = if teams.is_empty() {
        text("No other teams to move to")
            .size(13)
            .color(t::text_muted())
            .into()
    } else {
        scrollable(list).height(Length::Shrink).into()
    };

    let content = column![]
        .spacing(14)
        .width(Length::Fixed(360.0))
        .push(text("Move to team").size(18).color(t::text_primary()))
        .push(results)
        .push(
            button(text("Cancel").size(14))
                .on_press(Message::CloseModal)
                .padding([8, 24])
                .style(button::secondary),
        );

    let modal = container(content)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn team(id: &str, archived: bool) -> Team {
        Team {
            id: id.into(),
            name: id.into(),
            identifier: id.to_uppercase(),
            remote_id: Some(1),
            archived_at: archived.then(|| "2026-01-01T00:00:00.000Z".to_string()),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn move_targets_exclude_teams_without_a_remote_id() {
        let mut local = team("b", false);
        local.remote_id = None;
        let teams = [team("a", false), local, team("c", false)];
        let ids: Vec<&str> = move_targets(&teams, "a").iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["c"]);
    }

    #[test]
    fn move_targets_exclude_the_source_and_archived_teams() {
        let teams = [team("a", false), team("b", false), team("c", true)];
        let ids: Vec<&str> = move_targets(&teams, "a").iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["b"]);
    }
}
