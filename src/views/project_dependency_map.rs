//! A project's dependency map (`View::ProjectDependencyMap`, JRST-445): the
//! web's ProjectDependencyMapsController page. Header (title, explainer,
//! Active / All switch, legend) over the map canvas, or an empty state.
//! The breadcrumb is built in `app::breadcrumb`.

use std::collections::HashMap;

use iced::widget::{button, column, container, row};
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Issue, IssueDependency, Label, Lane, Member, Project};
use crate::dep_connectors::{ConnectorKind, DrawOptions, EdgeSpec};
use crate::dep_graph::project_graph;
use crate::theme::{self as t, space, type_scale};
use crate::views::dep_map_canvas::{self, NodeCard};
use crate::views::text;

/// What the view draws, built once per data change (the canvas borrows it).
#[derive(Debug, Clone, Default)]
pub struct ProjectMapData {
    pub columns: Vec<Vec<NodeCard>>,
    pub edges: Vec<EdgeSpec>,
}

impl ProjectMapData {
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

fn kind_for(kind: &str) -> ConnectorKind {
    match kind {
        "relates" => ConnectorKind::Relates,
        "duplicates" => ConnectorKind::Duplicates,
        _ => ConnectorKind::Blocks,
    }
}

/// Everything [`build`] reads: the project and the loaded mirror data.
pub struct MapInput<'a> {
    pub project: &'a Project,
    pub deps: &'a [IssueDependency],
    pub issues: &'a [Issue],
    pub lanes: &'a [Lane],
    pub projects: &'a [Project],
    pub members: &'a [Member],
    pub issue_labels: &'a HashMap<String, Vec<Label>>,
    pub active_only: bool,
    pub team_identifier: &'a str,
}

/// Run `dep_graph::project_graph` and map its output onto canvas cards.
pub fn build(input: MapInput<'_>) -> ProjectMapData {
    let MapInput {
        project,
        deps,
        issues,
        lanes,
        projects,
        members,
        issue_labels,
        active_only,
        team_identifier,
    } = input;
    let graph = project_graph(project, deps, issues, lanes, projects, active_only);
    let columns = graph
        .columns
        .iter()
        .map(|col| {
            col.iter()
                .map(|node| {
                    let issue = &node.issue;
                    let category = lanes
                        .iter()
                        .find(|l| l.id == issue.lane_id)
                        .and_then(|l| l.category.as_deref());
                    let closed = issue.completed_at.is_some()
                        || issue.canceled_at.is_some()
                        || matches!(category, Some("completed" | "canceled"));
                    NodeCard {
                        id: issue.id.clone(),
                        identifier: format!(
                            "{team_identifier}-{}",
                            issue.team_number.unwrap_or(0)
                        ),
                        title: issue.title.clone(),
                        lane_icon: crate::lane_icon::icon_for(category),
                        assignee_initials: issue
                            .assignee_id
                            .as_deref()
                            .and_then(|id| members.iter().find(|m| m.id == id))
                            .map(|m| crate::views::projects::lead_initials(&m.name)),
                        label_colors: issue_labels
                            .get(&issue.id)
                            .map(|ls| {
                                ls.iter()
                                    .filter_map(|l| l.color.as_deref().and_then(t::hex_to_color))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        closed,
                        tag: node.outside_project.clone(),
                    }
                })
                .collect()
        })
        .collect();
    let edges = graph
        .edges
        .iter()
        .map(|e| EdgeSpec { from_id: e.from.clone(), to_id: e.to.clone(), kind: kind_for(&e.kind) })
        .collect();
    ProjectMapData { columns, edges }
}

pub fn empty_message(active_only: bool) -> &'static str {
    if active_only {
        "No active issues in this project."
    } else {
        "No issues in this project yet."
    }
}

pub fn view<'a>(
    project: &'a Project,
    data: &'a ProjectMapData,
    active_only: bool,
) -> Element<'a, Message> {
    let header = row![]
        .spacing(space::S6)
        .align_y(Alignment::Start)
        .push(
            column![]
                .spacing(space::S1)
                .width(Length::Fill)
                .push(
                    text(format!("{} dependencies", project.name))
                        .size(type_scale::LG)
                        .color(t::text_primary()),
                )
                .push(
                    text(
                        "Work flows left to right: each issue sits to the right of everything \
                         blocking it, and issues with no blockers start in the first column. \
                         Click an issue to open its dependency map.",
                    )
                    .size(type_scale::SM)
                    .color(t::text_secondary()),
                ),
        )
        .push(
            row![scope_switch(active_only), dep_map_canvas::legend()]
                .spacing(space::S4)
                .align_y(Alignment::Center),
        );

    let body: Element<'a, Message> = if data.is_empty() {
        container(
            column![]
                .spacing(space::S2)
                .align_x(Alignment::Center)
                .push(text(empty_message(active_only)).size(type_scale::SM).color(t::text_muted()))
                .push(
                    button(text("Back to the project.").size(type_scale::SM).color(t::accent()))
                        .on_press(Message::CloseProjectDependencyMap)
                        .padding(0)
                        .style(button::text),
                ),
        )
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    } else {
        dep_map_canvas::view(
            &data.columns,
            &data.edges,
            None,
            &DrawOptions::default(),
            |id| Message::OpenDependencyMap(id.to_string()),
        )
    };

    column![]
        .spacing(space::S6)
        .padding(space::S8)
        .width(Length::Fill)
        .height(Length::Fill)
        .push(header)
        .push(body)
        .into()
}

/// The Active / All segmented switch.
fn scope_switch<'a>(active_only: bool) -> Element<'a, Message> {
    let segment = |label: &'static str, on: bool, value: bool| {
        button(text(label).size(type_scale::XS))
            .on_press(Message::SetProjectMapActiveOnly(value))
            .padding([space::S1, space::S3])
            .style(move |_theme: &Theme, status| button::Style {
                background: (on || matches!(status, button::Status::Hovered))
                    .then(|| iced::Background::Color(t::bg_card_focused())),
                text_color: if on { t::text_primary() } else { t::text_secondary() },
                border: Border { radius: 4.0.into(), ..Default::default() },
                ..Default::default()
            })
    };
    row![segment("Active", active_only, true), segment("All", !active_only, false)]
        .spacing(space::S1)
        .align_y(Alignment::Center)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_copy_matches_the_web() {
        assert_eq!(empty_message(true), "No active issues in this project.");
        assert_eq!(empty_message(false), "No issues in this project yet.");
    }

    #[test]
    fn kinds_map_to_connector_kinds() {
        assert_eq!(kind_for("blocks"), ConnectorKind::Blocks);
        assert_eq!(kind_for("relates"), ConnectorKind::Relates);
        assert_eq!(kind_for("duplicates"), ConnectorKind::Duplicates);
    }
}
