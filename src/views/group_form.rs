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
use crate::db::models::{Project, ProjectStatus, Team};
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
    projects: &'a [Project],
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
        .push(scrollable(source_picker(draft, teams, projects)).height(Length::Fixed(200.0)))
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

/// All source sections. Teams and projects today; labels add their own.
fn source_picker<'a>(
    draft: &'a GroupDraft,
    teams: &'a [Team],
    projects: &'a [Project],
) -> Element<'a, Message> {
    let mut col: Column<'a, Message> = column![].spacing(6);
    col = col.push(team_sources(draft, teams));
    col = col.push(project_sources(draft, teams, projects));
    col.into()
}

/// Projects the picker offers under one team: synced (remote id) and not
/// completed, plus any completed project the draft already uses so it can be
/// unchecked. A null status counts as open.
pub fn pickable_projects<'a>(
    projects: &'a [Project],
    team_id: &str,
    draft: &GroupDraft,
) -> Vec<&'a Project> {
    projects
        .iter()
        .filter(|p| p.team_id == team_id)
        .filter(|p| {
            let Some(rid) = p.remote_id else { return false };
            p.status != Some(ProjectStatus::Completed)
                || draft.is_selected(SourceType::Project, rid)
        })
        .collect()
}

fn project_sources<'a>(
    draft: &'a GroupDraft,
    teams: &'a [Team],
    projects: &'a [Project],
) -> Element<'a, Message> {
    let mut col: Column<'a, Message> = column![].spacing(4);
    let mut any = false;
    for team in pickable_teams(teams) {
        let offered = pickable_projects(projects, &team.id, draft);
        if offered.is_empty() {
            continue;
        }
        if !any {
            col = col.push(text("Projects").size(12).color(t::text_muted()));
            any = true;
        }
        col = col.push(text(team.name.as_str()).size(12).color(t::text_secondary()));
        for project in offered {
            let remote_id = project.remote_id.expect("filtered to projects with a remote id");
            let selected = draft.is_selected(SourceType::Project, remote_id);
            let mut line = row![].spacing(12).push(
                checkbox(project.name.as_str(), selected)
                    .on_toggle(move |_| Message::GroupToggleSource(SourceType::Project, remote_id))
                    .size(16)
                    .text_size(13),
            );
            if selected {
                line = line.push(
                    checkbox("All issues", draft.include_all(SourceType::Project, remote_id))
                        .on_toggle(move |on| {
                            Message::GroupSetIncludeAll(SourceType::Project, remote_id, on)
                        })
                        .size(14)
                        .text_size(12),
                );
            }
            col = col.push(line);
        }
    }
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

    fn project(id: &str, team: &str, remote: Option<i64>, st: Option<ProjectStatus>) -> Project {
        Project {
            id: id.into(),
            remote_id: remote,
            team_id: team.into(),
            name: id.into(),
            description: None,
            status: st,
            priority: 0,
            lead_id: None,
            start_date: None,
            due_date: None,
            roadmap_commitment: None,
            velocity_score: 0,
            completed_issues_count: 0,
            total_issues_count: 0,
            progress_percentage: 0,
            behind_schedule: false,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn projects_are_synced_open_and_a_used_completed_one_stays() {
        let ps = [
            project("open", "t", Some(1), Some(ProjectStatus::Started)),
            project("nostatus", "t", Some(2), None),
            project("unsynced", "t", None, Some(ProjectStatus::Started)),
            project("done", "t", Some(3), Some(ProjectStatus::Completed)),
            project("done_used", "t", Some(4), Some(ProjectStatus::Completed)),
            project("other_team", "u", Some(5), None),
        ];
        let mut draft = GroupDraft::default();
        draft.toggle_source(SourceType::Project, 4);
        let names: Vec<_> =
            pickable_projects(&ps, "t", &draft).iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["open", "nostatus", "done_used"]);
    }

    #[test]
    fn the_picker_offers_only_synced_unarchived_teams() {
        let teams = [team("A", Some(1), false), team("B", None, false), team("C", Some(3), true)];
        let names: Vec<_> = pickable_teams(&teams).iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["A"]);
    }
}
