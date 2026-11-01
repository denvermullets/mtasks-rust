//! The "Add group" modal of a dashboard: name, description, color swatch and a
//! source picker. State is a `GroupDraft` (see `db::dashboards`), so every
//! source kind selects and flags sources through the same two methods.
//!
//! A new source kind joins by adding a `*_sources` section to `source_picker`
//! whose rows send `Message::GroupToggleSource` and `Message::GroupSetIncludeAll`.

use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_input, Column};
use iced::{Element, Length, Theme};

use crate::app::Message;
use crate::db::dashboards::{GroupDraft, SourceType, GROUP_SWATCHES};
use crate::db::models::Team;
use crate::theme as t;

pub fn name_input_id() -> text_input::Id {
    text_input::Id::new("dashboard-group-name")
}

/// Teams the picker offers: only those Rails knows (a remote id), not archived.
pub fn pickable_teams(teams: &[Team]) -> Vec<&Team> {
    teams.iter().filter(|t| t.remote_id.is_some() && t.archived_at.is_none()).collect()
}

pub fn modal<'a>(
    draft: &'a GroupDraft,
    teams: &'a [Team],
    error: Option<&'static str>,
) -> Element<'a, Message> {
    let mut content = column![]
        .spacing(12)
        .push(text("Add group").size(18).color(t::text_primary()))
        .push(
            text_input("Name", &draft.name)
                .id(name_input_id())
                .on_input(Message::GroupNameChanged)
                .on_submit(Message::GroupSubmit)
                .padding([6, 10])
                .size(14)
                .width(Length::Fixed(400.0)),
        );
    if let Some(e) = error {
        content = content.push(text(e).size(12).color(t::danger()));
    }
    content = content
        .push(
            text_input("Description (optional)", &draft.description)
                .on_input(Message::GroupDescriptionChanged)
                .on_submit(Message::GroupSubmit)
                .padding([6, 10])
                .size(14)
                .width(Length::Fixed(400.0)),
        )
        .push(swatches(&draft.color))
        .push(text("Sources").size(13).color(t::text_secondary()))
        .push(scrollable(source_picker(draft, teams)).height(Length::Fixed(200.0)))
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text("Add group").size(14))
                        .on_press(Message::GroupSubmit)
                        .padding([8, 24])
                        .style(button::primary),
                )
                .push(
                    button(text("Cancel").size(14))
                        .on_press(Message::CloseModal)
                        .padding([8, 24])
                        .style(button::secondary),
                ),
        );

    let card = container(content).padding(28).style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_modal())),
        border: iced::Border { color: t::border(), width: 1.0, radius: 8.0.into() },
        ..Default::default()
    });
    container(card)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(0.0, 0.0, 0.0, 0.7))),
            ..Default::default()
        })
        .into()
}

fn swatches<'a>(selected: &str) -> Element<'a, Message> {
    let mut r = row![].spacing(8);
    for hex in GROUP_SWATCHES {
        let color = t::hex_to_color(hex).unwrap_or_else(t::accent);
        let on = hex == selected;
        r = r.push(
            button(text("").size(1))
                .width(Length::Fixed(22.0))
                .height(Length::Fixed(22.0))
                .on_press(Message::GroupColorPicked(hex.to_string()))
                .style(move |_theme: &Theme, _status| button::Style {
                    background: Some(iced::Background::Color(color)),
                    border: iced::Border {
                        color: if on { t::text_primary() } else { iced::Color::TRANSPARENT },
                        width: 2.0,
                        radius: 11.0.into(),
                    },
                    ..Default::default()
                }),
        );
    }
    r.into()
}

/// All source sections. Teams today; projects and labels add their own.
fn source_picker<'a>(draft: &'a GroupDraft, teams: &'a [Team]) -> Element<'a, Message> {
    let mut col: Column<'a, Message> = column![].spacing(6);
    col = col.push(team_sources(draft, teams));
    col.into()
}

fn team_sources<'a>(draft: &'a GroupDraft, teams: &'a [Team]) -> Element<'a, Message> {
    let mut col: Column<'a, Message> = column![].spacing(6);
    let pickable = pickable_teams(teams);
    if pickable.is_empty() {
        return text("No synced teams to pick from").size(12).color(t::text_muted()).into();
    }
    for team in pickable {
        let remote_id = team.remote_id.expect("filtered to teams with a remote id");
        let selected = draft.is_selected(SourceType::Team, remote_id);
        let mut line = row![]
            .spacing(12)
            .push(
                checkbox(format!("{} ({})", team.name, team.identifier), selected)
                    .on_toggle(move |_| Message::GroupToggleSource(SourceType::Team, remote_id))
                    .size(16)
                    .text_size(13),
            );
        if selected {
            line = line.push(
                checkbox("All issues", draft.include_all(SourceType::Team, remote_id))
                    .on_toggle(move |on| {
                        Message::GroupSetIncludeAll(SourceType::Team, remote_id, on)
                    })
                    .size(14)
                    .text_size(12),
            );
        }
        col = col.push(line);
    }
    col.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(name: &str, remote_id: Option<i64>, archived: bool) -> Team {
        Team {
            id: name.into(),
            name: name.into(),
            identifier: "T".into(),
            remote_id,
            archived_at: archived.then(|| "2026-01-01".to_string()),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn the_picker_offers_only_synced_unarchived_teams() {
        let teams = [team("A", Some(1), false), team("B", None, false), team("C", Some(3), true)];
        let names: Vec<_> = pickable_teams(&teams).iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["A"]);
    }
}
