//! The "Add group" modal of a dashboard: name, description, color swatch and a
//! source picker. State is a `GroupDraft` (see `db::dashboards`), so every
//! source kind selects and flags sources through the same two methods.
//!
//! A new source kind joins by adding a `*_sources` block to `source_picker`
//! whose rows send `Message::GroupToggleSource` and `Message::GroupSetIncludeAll`.

use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_input, Column};
use iced::{Element, Length, Theme};

use crate::app::Message;
use crate::db::dashboards::{GroupDraft, GroupSource, SourceType, GROUP_SWATCHES};
use crate::db::models::{Label, Project, ProjectStatus, Team};
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
    labels: &'a [Label],
    error: Option<&'static str>,
    editing: bool,
) -> Element<'a, Message> {
    let (title, submit_label) = if editing { ("Edit group", "Save") } else { ("Add group", "Add group") };
    let mut content = column![]
        .spacing(12)
        .push(text(title).size(18).color(t::text_primary()))
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
        .push(scrollable(source_picker(draft, teams, projects, labels)).height(Length::Fixed(200.0)))
        .push(
            row![]
                .spacing(8)
                .push(
                    button(text(submit_label).size(14))
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
    labels: &'a [Label],
) -> Element<'a, Message> {
    let mut col: Column<'a, Message> = column![].spacing(6);
    col = col.push(team_sources(draft, teams));
    col = col.push(project_sources(draft, teams, projects));
    col = col.push(label_sources(draft, teams, labels));
    col = col.push(unavailable_section(draft, teams, projects, labels));
    col.into()
}

/// Sources of the draft that the pickers above don't offer (not mirrored here,
/// team archived or unsynced, project or label gone). They stay in the group
/// until removed, so they are listed apart. Same rules as the pickers.
pub fn unavailable_sources(
    draft: &GroupDraft,
    teams: &[Team],
    projects: &[Project],
    labels: &[Label],
) -> Vec<GroupSource> {
    let pickable = pickable_teams(teams);
    let offered_team = |remote: i64| pickable.iter().any(|t| t.remote_id == Some(remote));
    let offered_project = |remote: i64| {
        pickable.iter().any(|t| {
            pickable_projects(projects, &t.id, draft).iter().any(|p| p.remote_id == Some(remote))
        })
    };
    let offered_label = |remote: i64| {
        pickable
            .iter()
            .any(|t| pickable_labels(labels, &t.id).iter().any(|l| l.remote_id == Some(remote)))
    };
    draft
        .sources
        .iter()
        .copied()
        .filter(|s| {
            !match s.source_type {
                SourceType::Team => offered_team(s.remote_id),
                SourceType::Project => offered_project(s.remote_id),
                SourceType::Label => offered_label(s.remote_id),
            }
        })
        .collect()
}

fn unavailable_section<'a>(
    draft: &'a GroupDraft,
    teams: &'a [Team],
    projects: &'a [Project],
    labels: &'a [Label],
) -> Element<'a, Message> {
    let missing = unavailable_sources(draft, teams, projects, labels);
    let mut col: Column<'a, Message> = column![].spacing(4);
    if missing.is_empty() {
        return col.into();
    }
    col = col.push(text("Unavailable").size(12).color(t::text_muted()));
    for s in missing {
        let kind = match s.source_type {
            SourceType::Team => "Team",
            SourceType::Project => "Project",
            SourceType::Label => "Label",
        };
        col = col.push(
            row![]
                .spacing(12)
                .align_y(iced::Alignment::Center)
                .push(
                    text(format!("{kind} #{} (not available here)", s.remote_id))
                        .size(13)
                        .color(t::text_secondary()),
                )
                .push(
                    button(text("Remove").size(12))
                        .on_press(Message::GroupToggleSource(s.source_type, s.remote_id))
                        .padding([2, 8])
                        .style(button::text),
                ),
        );
    }
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

/// Labels the picker offers under one team: those Rails knows (a remote id),
/// sorted by name. Labels have no "all issues" switch.
pub fn pickable_labels<'a>(labels: &'a [Label], team_id: &str) -> Vec<&'a Label> {
    let mut v: Vec<&Label> =
        labels.iter().filter(|l| l.team_id == team_id && l.remote_id.is_some()).collect();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.name.cmp(&b.name)));
    v
}

fn label_sources<'a>(
    draft: &'a GroupDraft,
    teams: &'a [Team],
    labels: &'a [Label],
) -> Element<'a, Message> {
    let mut col: Column<'a, Message> = column![].spacing(4);
    let mut any = false;
    for team in pickable_teams(teams) {
        let offered = pickable_labels(labels, &team.id);
        if offered.is_empty() {
            continue;
        }
        if !any {
            col = col.push(text("Labels").size(12).color(t::text_muted()));
            col = col.push(
                text(LABEL_HINT).size(11).color(t::text_muted()),
            );
            any = true;
        }
        col = col.push(text(team.name.as_str()).size(12).color(t::text_secondary()));
        for label in offered {
            let remote_id = label.remote_id.expect("filtered to labels with a remote id");
            col = col.push(
                checkbox(label.name.as_str(), draft.is_selected(SourceType::Label, remote_id))
                    .on_toggle(move |_| Message::GroupToggleSource(SourceType::Label, remote_id))
                    .size(16)
                    .text_size(13),
            );
        }
    }
    col.into()
}

const LABEL_HINT: &str = "With no team or project selected, a label pulls in every matching issue. Alongside them, it narrows the group.";

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
            role: None,
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
    fn labels_are_synced_per_team_and_sorted_by_name() {
        let l = |n: &str, t: &str, r: Option<i64>| Label {
            id: n.into(),
            remote_id: r,
            team_id: t.into(),
            name: n.into(),
            color: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let ls = [l("bug", "t", Some(1)), l("Aardvark", "t", Some(2)), l("local", "t", None), l("x", "u", Some(3))];
        let names: Vec<_> = pickable_labels(&ls, "t").iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Aardvark", "bug"]);
    }

    #[test]
    fn sources_the_pickers_dont_offer_are_listed_as_unavailable() {
        let teams = [team("t", Some(1), false), team("old", Some(2), true)];
        let ps = [project("p", "t", Some(10), None)];
        let l = |n: &str, r: i64| Label {
            id: n.into(),
            remote_id: Some(r),
            team_id: "t".into(),
            name: n.into(),
            color: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let ls = [l("bug", 20)];
        let mut draft = GroupDraft::default();
        for (ty, id) in [
            (SourceType::Team, 1),
            (SourceType::Team, 2),
            (SourceType::Team, 99),
            (SourceType::Project, 10),
            (SourceType::Project, 98),
            (SourceType::Label, 20),
            (SourceType::Label, 97),
        ] {
            draft.toggle_source(ty, id);
        }
        let got: Vec<_> = unavailable_sources(&draft, &teams, &ps, &ls)
            .iter()
            .map(|s| (s.source_type, s.remote_id))
            .collect();
        assert_eq!(
            got,
            [
                (SourceType::Team, 2),
                (SourceType::Team, 99),
                (SourceType::Project, 98),
                (SourceType::Label, 97)
            ]
        );
        // Removing one is the same toggle the checkboxes use.
        draft.toggle_source(SourceType::Team, 99);
        assert_eq!(unavailable_sources(&draft, &teams, &ps, &ls).len(), 3);
    }

    #[test]
    fn the_picker_offers_only_synced_unarchived_teams() {
        let teams = [team("A", Some(1), false), team("B", None, false), team("C", Some(3), true)];
        let names: Vec<_> = pickable_teams(&teams).iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["A"]);
    }
}
