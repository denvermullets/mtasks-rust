//! One issue's dependency map (`View::DependencyMap`, JRST-442): a Dependencies
//! / Hierarchy segmented switch over a tab body. The header's breadcrumb is
//! built in `app::breadcrumb`.
//!
//! The Dependencies tab (JRST-443) is the graph canvas, a depth selector and
//! three lists. [`hierarchy_body`] (JRST-444) is still a placeholder.

use std::collections::{HashMap, HashSet};

use iced::widget::{button, column, container, horizontal_space, row, scrollable};
use iced::{Alignment, Border, Color, Element, Length, Theme};

use crate::app::{DependencyMapTab, Message, MAX_DEPENDENCY_DEPTH};
use crate::db::models::{Issue, IssueDependency, Label, Lane, Member, Team};
use crate::dep_connectors::{ConnectorKind, DrawOptions, EdgeSpec};
use crate::dep_graph::{self, IssueGraph};
use crate::hierarchy::{Hierarchy, Tree, MAX_DEPTH};
use crate::scroll::{track, ScrollKey};
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::dep_map_canvas::{self, NodeCard};
use crate::views::text;
use crate::views::truncated::truncated;

/// Everything the map reads from the app (borrowed for one render).
pub struct MapData<'a> {
    pub issues: &'a [Issue],
    pub lanes: &'a [Lane],
    pub teams: &'a [Team],
    pub members: &'a [Member],
    pub issue_labels: &'a HashMap<String, Vec<Label>>,
    /// Every dependency row (the graph query filters by team itself).
    pub deps: &'a [IssueDependency],
    /// Hierarchy outline nodes the user toggled away from their default.
    pub flipped: &'a HashSet<String>,
}

pub fn view<'a>(
    issue: &'a Issue,
    tab: DependencyMapTab,
    depth: u8,
    data: &MapData<'a>,
) -> Element<'a, Message> {
    let body = match tab {
        DependencyMapTab::Dependencies => dependencies_body(issue, depth, data),
        DependencyMapTab::Hierarchy => hierarchy_body(issue, data),
    };
    let header = row![tab_switch(tab), horizontal_space()]
        .push_maybe((tab == DependencyMapTab::Dependencies).then(dep_map_canvas::legend))
        .align_y(Alignment::Center);
    track(
        scrollable(
            column![]
                .spacing(space::S6)
                .padding(space::S8)
                .width(Length::Fill)
                .push(header)
                .push(body),
        )
        .width(Length::Fill)
        .height(Length::Fill),
        ScrollKey::DependencyMap,
    )
    .into()
}

/// The Dependencies / Hierarchy segmented switch.
fn tab_switch<'a>(active: DependencyMapTab) -> Element<'a, Message> {
    let segment = |tab: DependencyMapTab| {
        let on = tab == active;
        button(text(tab.label()).size(type_scale::SM))
            .on_press(Message::SetDependencyMapTab(tab))
            .padding([space::S1, space::S4])
            .style(move |_theme: &Theme, status| button::Style {
                background: (on || matches!(status, button::Status::Hovered))
                    .then(|| iced::Background::Color(t::bg_card_focused())),
                text_color: if on { t::text_primary() } else { t::text_secondary() },
                border: Border { radius: 4.0.into(), ..Default::default() },
                ..Default::default()
            })
    };
    row![segment(DependencyMapTab::Dependencies), segment(DependencyMapTab::Hierarchy)]
        .spacing(space::S1)
        .align_y(Alignment::Center)
        .into()
}

/// The Dependencies tab: depth selector, graph, truncation note, three lists.
fn dependencies_body<'a>(issue: &'a Issue, depth: u8, data: &MapData<'a>) -> Element<'a, Message> {
    let Some(graph) =
        dep_graph::issue_graph(&issue.id, data.deps, data.issues, data.lanes, depth as usize)
    else {
        return placeholder("Issue not found".to_string());
    };
    if is_empty(&graph) {
        return empty_state();
    }

    let model = MapModel::build(&graph, data);    let opts = DrawOptions { focus_id: Some(issue.id.clone()), ..Default::default() };
    let graph_el: Element<'a, Message> = container(dep_map_canvas::view(
        &model.columns,
        &model.edges,
        Some(issue.id.as_str()),
        &opts,
        |id| Message::FocusDependencyMap(id.to_string()),
    ))
    .height(Length::Fixed(GRAPH_HEIGHT))
    .width(Length::Fill)
    .style(|_: &Theme| container::Style {
        border: Border { color: t::border(), width: 1.0, radius: radius::LG.into() },
        ..Default::default()
    })
    .into();

    let mut out = column![depth_selector(depth), model.header_row, graph_el].spacing(space::S3);
    if graph.truncated {
        out = out.push(truncation_note(depth));
    }

    let row_of = |i: &Issue, kind: &'static str, tag: Option<&'static str>| ListRow {
        issue: i.clone(),
        kind,
        tag,
        closed: is_closed(i, data.lanes),
        labels: data.issue_labels.get(&i.id).cloned().unwrap_or_default(),
        identifier: identifier(i, data.teams),
    };
    let blocked_by: Vec<ListRow> = graph
        .upstream
        .get(&1)
        .map(|v| v.iter().map(|i| row_of(i, "blocked_by", None)).collect())
        .unwrap_or_default();
    let blocks: Vec<ListRow> = graph
        .downstream
        .get(&1)
        .map(|v| v.iter().map(|i| row_of(i, "blocks", None)).collect())
        .unwrap_or_default();
    let related: Vec<ListRow> = graph
        .related
        .iter()
        .map(|e| row_of(&e.issue, "relates", related_tag(&e.direction)))
        .collect();

    out = out.push(
        row![
            list_card(
                "Blocked by",
                t::dep_blocked_by(),
                "These issues must be completed before this issue can be started.",
                blocked_by
            ),
            list_card("Blocks", t::dep_blocks(), "These issues are blocked by this issue.", blocks),
            list_card(
                "Related",
                t::dep_relates(),
                "These issues are related but not blocking.",
                related
            ),
        ]
        .spacing(space::S4),
    );
    out.into()
}

const GRAPH_HEIGHT: f32 = 460.0;

/// Web `dependency_map_empty?`.
pub fn is_empty(graph: &IssueGraph) -> bool {
    graph.upstream.is_empty() && graph.downstream.is_empty() && graph.related.is_empty()
}

/// Web `RELATED_TAGS`.
pub fn related_tag(direction: &str) -> Option<&'static str> {
    match direction {
        "duplicates" => Some("Duplicates"),
        "duplicated_by" => Some("Duplicated by"),
        _ => None,
    }
}

pub fn is_closed(issue: &Issue, lanes: &[Lane]) -> bool {
    lanes
        .iter()
        .find(|l| l.id == issue.lane_id)
        .and_then(|l| l.category.as_deref())
        .is_some_and(|c| matches!(c, "completed" | "canceled"))
}

fn identifier(issue: &Issue, teams: &[Team]) -> String {
    let n = issue.team_number.unwrap_or(0);
    match teams.iter().find(|t| t.id == issue.team_id) {
        Some(tm) => format!("{}-{n}", tm.identifier),
        None => format!("#{n}"),
    }
}

fn connector_kind(kind: &str) -> ConnectorKind {
    match kind {
        "relates" => ConnectorKind::Relates,
        "duplicates" => ConnectorKind::Duplicates,
        _ => ConnectorKind::Blocks,
    }
}

/// Edges the connector layer draws: relates/duplicates links touching the focus
/// are the related cards' job, so they are dropped (web `dependency_map_edges`).
pub fn drawn_edges(graph: &IssueGraph) -> Vec<EdgeSpec> {
    graph
        .edges
        .iter()
        .filter(|e| e.kind == "blocks" || (e.from != graph.focus.id && e.to != graph.focus.id))
        .map(|e| EdgeSpec {
            from_id: e.from.clone(),
            to_id: e.to.clone(),
            kind: connector_kind(&e.kind),
        })
        .collect()
}

/// Column header and issues, left to right: upstream deepest first, the focus
/// column (focus, then related), then downstream nearest first.
pub fn column_plan(graph: &IssueGraph) -> Vec<(String, Vec<Issue>)> {
    let head = |up: bool, depth: usize| {
        if depth == 1 {
            if up { "Upstream: blocked by" } else { "Downstream: blocks" }.to_string()
        } else {
            format!("Depth {depth}")
        }
    };
    let mut cols: Vec<(String, Vec<Issue>)> =
        graph.upstream.iter().rev().map(|(d, v)| (head(true, *d), v.clone())).collect();
    let mut focus_col = vec![graph.focus.clone()];
    focus_col.extend(graph.related.iter().map(|e| e.issue.clone()));
    cols.push(("Selected issue".to_string(), focus_col));
    cols.extend(graph.downstream.iter().map(|(d, v)| (head(false, *d), v.clone())));
    cols
}

struct MapModel<'a> {
    columns: Vec<Vec<NodeCard>>,
    edges: Vec<EdgeSpec>,
    header_row: Element<'a, Message>,
}

impl<'a> MapModel<'a> {
    fn build(graph: &IssueGraph, data: &MapData<'_>) -> Self {
        let mut headers =
            row![].spacing(dep_map_canvas::COLUMN_GAP).padding([0.0, dep_map_canvas::PAD]);
        let mut columns = Vec::new();
        for (title, issues) in column_plan(graph) {
            headers = headers.push(
                container(text(title).size(type_scale::XS).color(t::text_muted()))
                    .width(Length::Fixed(dep_map_canvas::CARD_W)),
            );
            columns.push(issues.iter().map(|i| node_card(i, data)).collect());
        }
        MapModel { columns, edges: drawn_edges(graph), header_row: headers.into() }
    }
}

fn node_card(issue: &Issue, data: &MapData<'_>) -> NodeCard {
    let category =
        data.lanes.iter().find(|l| l.id == issue.lane_id).and_then(|l| l.category.as_deref());
    NodeCard {
        id: issue.id.clone(),
        identifier: identifier(issue, data.teams),
        title: issue.title.clone(),
        lane_icon: crate::lane_icon::icon_for(category),
        assignee_initials: issue.assignee_id.as_ref().and_then(|a| {
            data.members
                .iter()
                .find(|m| &m.id == a)
                .map(|m| crate::views::projects::lead_initials(&m.name))
        }),
        label_colors: data
            .issue_labels
            .get(&issue.id)
            .map(|ls| {
                ls.iter()
                    .filter_map(|l| l.color.as_deref().and_then(t::hex_to_color))
                    .take(3)
                    .collect()
            })
            .unwrap_or_default(),
        closed: is_closed(issue, data.lanes),
        tag: None,
    }
}

fn depth_selector<'a>(depth: u8) -> Element<'a, Message> {
    let mut r = row![text("Depth").size(type_scale::XS).color(t::text_muted())]
        .spacing(space::S1)
        .align_y(Alignment::Center);
    for n in 1..=MAX_DEPENDENCY_DEPTH {
        let on = n == depth;
        r = r.push(
            button(text(n.to_string()).size(type_scale::SM))
                .on_press(Message::SetDependencyMapDepth(n))
                .padding([space::S1, space::S3])
                .style(move |_: &Theme, status| button::Style {
                    background: (on || matches!(status, button::Status::Hovered))
                        .then(|| iced::Background::Color(t::bg_card_focused())),
                    text_color: if on { t::text_primary() } else { t::text_secondary() },
                    border: Border { radius: 4.0.into(), ..Default::default() },
                    ..Default::default()
                }),
        );
    }
    r.into()
}

fn truncation_note<'a>(depth: u8) -> Element<'a, Message> {
    let mut r = row![text(format!("More beyond depth {depth}"))
        .size(type_scale::XS)
        .color(t::text_muted())]
    .spacing(space::S2);
    if depth < MAX_DEPENDENCY_DEPTH {
        r = r.push(
            button(text("Show more").size(type_scale::XS).color(t::accent()))
                .on_press(Message::SetDependencyMapDepth(depth + 1))
                .padding(0)
                .style(button::text),
        );
    }
    r.into()
}

fn empty_state<'a>() -> Element<'a, Message> {
    container(
        row![
            text("No dependencies yet.").size(type_scale::SM).color(t::text_muted()),
            button(
                text("Link issues from the issue's sidebar.")
                    .size(type_scale::SM)
                    .color(t::text_secondary())
            )
            .on_press(Message::CloseDependencyMap)
            .padding(0)
            .style(button::text),
        ]
        .spacing(space::S2),
    )
    .center_x(Length::Fill)
    .padding(space::S8 * 2.0)
    .into()
}

struct ListRow {
    issue: Issue,
    identifier: String,
    kind: &'static str,
    tag: Option<&'static str>,
    closed: bool,
    labels: Vec<Label>,
}

fn kind_color(kind: &str) -> Color {
    match kind {
        "blocked_by" => t::dep_blocked_by(),
        "blocks" => t::dep_blocks(),
        _ => t::dep_relates(),
    }
}

fn dot<'a>(c: Color, size: f32) -> Element<'a, Message> {
    container(text(""))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_: &Theme| container::Style {
            background: Some(iced::Background::Color(c)),
            border: Border { radius: radius::FULL.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

fn list_card<'a>(
    title: &'static str,
    color: Color,
    subtitle: &'static str,
    rows: Vec<ListRow>,
) -> Element<'a, Message> {
    let mut col = column![
        row![
            dot(color, 10.0),
            text(title).size(type_scale::SM).color(t::text_primary()),
            text(rows.len().to_string()).size(type_scale::XS).color(t::text_muted()),
        ]
        .spacing(space::S2)
        .align_y(Alignment::Center),
        text(subtitle).size(type_scale::XS).color(t::text_muted()),
    ]
    .spacing(space::S2);
    if rows.is_empty() {
        col = col.push(text("None").size(type_scale::XS).color(t::text_muted()));
    }
    for r in rows {
        let alpha = if r.closed { 0.6 } else { 1.0 };
        let dim = move |c: Color| Color { a: c.a * alpha, ..c };
        let mut meta = row![].spacing(space::S1_5).align_y(Alignment::Center);
        if let Some(tag) = r.tag {
            meta = meta.push(text(tag).size(type_scale::XS).color(dim(t::dep_duplicates())));
        }
        for l in r.labels.iter().take(2) {
            meta = meta
                .push(text(l.name.clone()).size(type_scale::XS).color(dim(t::text_secondary())));
        }
        let body = row![
            dot(dim(kind_color(r.kind)), 8.0),
            text(r.identifier).size(type_scale::XS).color(dim(t::text_muted())),
            column![
                text(r.issue.title.clone()).size(type_scale::XS).color(dim(t::text_primary())),
                meta
            ]
            .spacing(space::S1)
            .width(Length::Fill),
        ]
        .spacing(space::S2);
        col = col.push(
            button(body)
                .on_press(Message::OpenIssueDetail(r.issue.id.clone()))
                .width(Length::Fill)
                .padding([space::S2, space::S3])
                .style(|_: &Theme, status| button::Style {
                    background: matches!(status, button::Status::Hovered)
                        .then(|| iced::Background::Color(t::bg_card_focused())),
                    text_color: t::text_primary(),
                    border: Border { color: t::border(), width: 1.0, radius: radius::LG.into() },
                    ..Default::default()
                }),
        );
    }
    container(col)
        .padding(space::S4)
        .width(Length::FillPortion(1))
        .style(|_: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_card())),
            border: Border { color: t::border(), width: 1.0, radius: radius::LG.into() },
            ..Default::default()
        })
        .into()
}

/// Levels at or past this start collapsed (web `collapse_depth`).
pub const COLLAPSE_DEPTH: usize = 3;

/// One visible line of the outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineRow {
    pub id: String,
    pub depth: usize,
    pub has_children: bool,
    pub expanded: bool,
}

/// Flatten `tree` into the rows that are showing. A node is open by default
/// above [`COLLAPSE_DEPTH`] or on the path to `focus`; `flipped` ids are the
/// ones the user toggled away from that default. Pure.
pub fn outline_rows(
    tree: &Tree,
    focus: &str,
    flipped: &HashSet<String>,
) -> Vec<OutlineRow> {
    let path: HashSet<&str> =
        tree.ancestors.iter().map(String::as_str).chain(std::iter::once(focus)).collect();
    let mut out = Vec::new();
    // Explicit stack so a deep tree can't overflow; children pushed reversed.
    let mut stack: Vec<(&str, usize)> = vec![(tree.root.as_str(), 0)];
    while let Some((id, depth)) = stack.pop() {
        let kids = tree.children_of(id);
        let default_open = depth < COLLAPSE_DEPTH || path.contains(id);
        let expanded = default_open != flipped.contains(id);
        out.push(OutlineRow {
            id: id.to_string(),
            depth,
            has_children: !kids.is_empty(),
            expanded: expanded && !kids.is_empty(),
        });
        if expanded {
            for k in kids.iter().rev() {
                stack.push((k.as_str(), depth + 1));
            }
        }
    }
    out
}

/// Open blockers inside the tree, keyed by the blocked issue: `blocks` links
/// whose both ends are tree nodes and whose blocker is not closed.
pub fn tree_blockers<'a>(
    tree: &Tree,
    deps: &[IssueDependency],
    by_id: &HashMap<&'a str, &'a Issue>,
    lanes: &[Lane],
) -> HashMap<String, Vec<&'a Issue>> {
    let mut nodes: HashSet<&str> = HashSet::from([tree.root.as_str()]);
    nodes.extend(tree.children.values().flatten().map(String::as_str));
    let mut out: HashMap<String, Vec<&Issue>> = HashMap::new();
    for d in deps.iter().filter(|d| d.kind == "blocks") {
        if !nodes.contains(d.blocking_issue_id.as_str())
            || !nodes.contains(d.blocked_issue_id.as_str())
        {
            continue;
        }
        if let Some(blocker) = by_id.get(d.blocking_issue_id.as_str()) {
            if !is_closed(blocker, lanes) {
                out.entry(d.blocked_issue_id.clone()).or_default().push(blocker);
            }
        }
    }
    out
}

/// The Hierarchy tab: the sub-issue tree this issue belongs to, as an outline.
fn hierarchy_body<'a>(issue: &'a Issue, data: &MapData<'a>) -> Element<'a, Message> {
    let hierarchy = Hierarchy::new(data.issues, data.lanes);
    let Some(tree) = hierarchy.tree(&issue.id, MAX_DEPTH) else {
        return placeholder("Issue not found".to_string());
    };
    if tree.ancestors.is_empty() && tree.children_of(&issue.id).is_empty() {
        return container(
            row![
                text("No parent or sub-issues yet.").size(type_scale::SM).color(t::text_muted()),
                button(
                    text("Add sub-issues from the issue page.")
                        .size(type_scale::SM)
                        .color(t::text_secondary())
                )
                .on_press(Message::CloseDependencyMap)
                .padding(0)
                .style(button::text),
            ]
            .spacing(space::S2),
        )
        .center_x(Length::Fill)
        .padding(space::S8 * 2.0)
        .into();
    }

    let by_id: HashMap<&str, &Issue> = data.issues.iter().map(|i| (i.id.as_str(), i)).collect();
    let blockers = tree_blockers(&tree, data.deps, &by_id, data.lanes);
    let mut list = column![].spacing(space::S1);
    for r in outline_rows(&tree, &issue.id, data.flipped) {
        if let Some(node) = by_id.get(r.id.as_str()) {
            let kids: Vec<&Issue> = tree
                .children_of(&r.id)
                .iter()
                .filter_map(|c| by_id.get(c.as_str()).copied())
                .collect();
            let node_blockers = blockers.get(&r.id).map(Vec::as_slice).unwrap_or(&[]);
            list = list.push(outline_row(node, &r, &kids, node_blockers, node.id == issue.id, data));
        }
    }
    let mut out = column![
        text("The full sub-issue tree this issue belongs to, from the top-level issue down.")
            .size(type_scale::SM)
            .color(t::text_secondary()),
        container(list).padding(space::S2).width(Length::Fill).style(|_: &Theme| {
            container::Style {
                border: Border { color: t::border(), width: 1.0, radius: radius::LG.into() },
                ..Default::default()
            }
        }),
    ]
    .spacing(space::S3);
    if tree.truncated {
        out = out.push(
            text(format!(
                "Showing the first {MAX_DEPTH} levels. Open a deeper issue to see the rest of its tree."
            ))
            .size(type_scale::XS)
            .color(t::text_muted()),
        );
    }
    out.into()
}

fn outline_row<'a>(
    node: &Issue,
    r: &OutlineRow,
    kids: &[&Issue],
    blockers: &[&Issue],
    focus: bool,
    data: &MapData<'_>,
) -> Element<'a, Message> {
    let closed = is_closed(node, data.lanes);
    let alpha = if closed { 0.6 } else { 1.0 };
    let dim = move |c: Color| Color { a: c.a * alpha, ..c };
    let category =
        data.lanes.iter().find(|l| l.id == node.lane_id).and_then(|l| l.category.as_deref());

    let mut line = row![
        crate::views::lane_icon::view(crate::lane_icon::icon_for(category)),
        text(identifier(node, data.teams))
            .size(type_scale::XS)
            .color(if focus { t::accent() } else { dim(t::text_muted()) }),
        container(truncated(
            node.title.clone(),
            type_scale::SM,
            if focus { t::text_primary() } else { dim(t::text_primary()) },
        ))
        .width(Length::Fill),
    ]
    .spacing(space::S2)
    .align_y(Alignment::Center);
    for b in blockers.iter().take(2) {
        line = line.push(
            row![dot(dim(t::dep_blocked_by()), 6.0), text(identifier(b, data.teams))
                .size(type_scale::XS)
                .color(dim(t::text_secondary()))]
            .spacing(space::S1)
            .align_y(Alignment::Center),
        );
    }
    if !kids.is_empty() {
        let done = kids.iter().filter(|k| is_closed(k, data.lanes)).count();
        line = line
            .push(text(format!("{done}/{}", kids.len())).size(type_scale::XS).color(t::text_muted()));
    }
    if let Some(initials) = node.assignee_id.as_ref().and_then(|a| {
        data.members
            .iter()
            .find(|m| &m.id == a)
            .map(|m| crate::views::projects::lead_initials(&m.name))
    }) {
        line = line.push(text(initials).size(9).color(dim(t::text_primary())));
    }

    let row_btn = button(line)
        .on_press(Message::OpenIssueDetail(node.id.clone()))
        .width(Length::Fill)
        .padding([space::S1_5, space::S2])
        .style(move |_: &Theme, status| button::Style {
            background: (focus || matches!(status, button::Status::Hovered))
                .then(|| iced::Background::Color(t::bg_card_focused())),
            text_color: t::text_primary(),
            border: Border {
                color: if focus { t::accent() } else { Color::TRANSPARENT },
                width: if focus { 1.0 } else { 0.0 },
                radius: radius::MD.into(),
            },
            ..Default::default()
        });

    let chevron: Element<'a, Message> = if r.has_children {
        button(text(if r.expanded { "v" } else { ">" }).size(type_scale::XS).color(t::text_muted()))
            .on_press(Message::ToggleHierarchyNode(r.id.clone()))
            .padding(0)
            .width(Length::Fixed(14.0))
            .style(button::text)
            .into()
    } else {
        iced::widget::Space::with_width(Length::Fixed(14.0)).into()
    };
    row![
        iced::widget::Space::with_width(Length::Fixed(r.depth as f32 * 20.0)),
        chevron,
        row_btn
    ]
    .spacing(space::S1)
    .align_y(Alignment::Center)
    .into()
}

fn placeholder<'a>(label: String) -> Element<'a, Message> {
    container(text(label).size(type_scale::SM).color(t::text_muted()))
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dep_graph::Edge;
    use std::collections::BTreeMap;

    fn issue(id: &str) -> Issue {
        Issue {
            id: id.into(),
            title: id.into(),
            description: None,
            priority: 0,
            team_number: Some(1),
            due_date: None,
            estimate: None,
            started_at: None,
            completed_at: None,
            canceled_at: None,
            lane_id: "l".into(),
            team_id: "t".into(),
            project_id: None,
            parent_issue_id: None,
            assignee_id: None,
            creator_id: None,
            remote_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn graph() -> IssueGraph {
        let mut upstream = BTreeMap::new();
        upstream.insert(1, vec![issue("u1")]);
        upstream.insert(2, vec![issue("u2")]);
        let mut downstream = BTreeMap::new();
        downstream.insert(1, vec![issue("d1")]);
        let e = |a: &str, b: &str, k: &str| Edge { from: a.into(), to: b.into(), kind: k.into() };
        IssueGraph {
            focus: issue("f"),
            upstream,
            downstream,
            related: vec![crate::dep_graph::RelatedEntry {
                issue: issue("r"),
                kind: "relates".into(),
                direction: "relates".into(),
            }],
            edges: vec![
                e("u2", "u1", "blocks"),
                e("u1", "f", "blocks"),
                e("f", "d1", "blocks"),
                e("f", "r", "relates"),
                e("u1", "d1", "relates"),
            ],
            truncated: false,
        }
    }

    #[test]
    fn columns_run_deepest_upstream_first_then_focus_then_downstream() {
        let ids: Vec<Vec<String>> = column_plan(&graph())
            .into_iter()
            .map(|(_, v)| v.into_iter().map(|i| i.id).collect())
            .collect();
        assert_eq!(ids, vec![vec!["u2"], vec!["u1"], vec!["f", "r"], vec!["d1"]]);
    }

    #[test]
    fn only_depth_one_columns_name_their_side() {
        let heads: Vec<String> = column_plan(&graph()).into_iter().map(|(h, _)| h).collect();
        assert_eq!(heads[0], "Depth 2");
        assert_eq!(heads[1], "Upstream: blocked by");
        assert_eq!(heads[2], "Selected issue");
        assert_eq!(heads[3], "Downstream: blocks");
    }

    #[test]
    fn focus_relates_links_are_not_drawn_but_other_relates_are() {
        let pairs: Vec<(String, String)> =
            drawn_edges(&graph()).into_iter().map(|e| (e.from_id, e.to_id)).collect();
        assert!(!pairs.contains(&("f".into(), "r".into())));
        assert!(pairs.contains(&("u1".into(), "d1".into())));
        assert!(pairs.contains(&("u1".into(), "f".into())));
        assert_eq!(pairs.len(), 4);
    }

    /// a > b > c > d > e (a chain), plus a sibling s under a.
    fn chain_tree(ancestors: &[&str]) -> Tree {
        let kids = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        Tree {
            root: "a".into(),
            ancestors: kids(ancestors),
            children: HashMap::from([
                ("a".to_string(), kids(&["b", "s"])),
                ("b".to_string(), kids(&["c"])),
                ("c".to_string(), kids(&["d"])),
                ("d".to_string(), kids(&["e"])),
            ]),
            truncated: false,
        }
    }

    fn ids(rows: &[OutlineRow]) -> Vec<&str> {
        rows.iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn levels_past_three_start_collapsed() {
        let rows = outline_rows(&chain_tree(&[]), "a", &HashSet::new());
        // depth 0..=2 open (a, b, c); d (depth 3) is shown but collapsed.
        assert_eq!(ids(&rows), vec!["a", "b", "c", "d", "s"]);
        let d = rows.iter().find(|r| r.id == "d").unwrap();
        assert!(d.has_children && !d.expanded);
    }

    #[test]
    fn the_path_to_the_focus_stays_open_past_depth_three() {
        let rows = outline_rows(&chain_tree(&["a", "b", "c", "d"]), "e", &HashSet::new());
        assert_eq!(ids(&rows), vec!["a", "b", "c", "d", "e", "s"]);
    }

    #[test]
    fn a_toggle_flips_the_default_both_ways() {
        let tree = chain_tree(&[]);
        let collapsed = outline_rows(&tree, "a", &HashSet::from(["b".to_string()]));
        assert_eq!(ids(&collapsed), vec!["a", "b", "s"]);
        let opened = outline_rows(&tree, "a", &HashSet::from(["d".to_string()]));
        assert_eq!(ids(&opened), vec!["a", "b", "c", "d", "e", "s"]);
    }

    #[test]
    fn only_open_blockers_inside_the_tree_count() {
        let tree = chain_tree(&[]);
        let mut open = issue("s");
        open.lane_id = "l".into();
        let mut closed = issue("d");
        closed.lane_id = "done".into();
        let outside = issue("zz");
        let by_id: HashMap<&str, &Issue> =
            [("s", &open), ("d", &closed), ("zz", &outside)].into_iter().collect();
        let lane = |id: &str, cat: &str| Lane {
            id: id.into(),
            name: id.into(),
            color: None,
            position: 0,
            team_id: "t".into(),
            remote_id: None,
            category: Some(cat.into()),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let lanes = [lane("l", "started"), lane("done", "completed")];
        let dep = |from: &str, to: &str, kind: &str| IssueDependency {
            id: format!("{from}{to}"),
            remote_id: None,
            blocking_issue_id: from.into(),
            blocked_issue_id: to.into(),
            kind: kind.into(),
            direction: "blocking".into(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let deps = [
            dep("s", "b", "blocks"),
            dep("d", "c", "blocks"),
            dep("zz", "b", "blocks"),
            dep("s", "c", "relates"),
        ];
        let got = tree_blockers(&tree, &deps, &by_id, &lanes);
        assert_eq!(got.len(), 1);
        assert_eq!(got["b"].iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), vec!["s"]);
    }

    #[test]
    fn related_tags_only_for_duplicates() {
        assert_eq!(related_tag("duplicates"), Some("Duplicates"));
        assert_eq!(related_tag("duplicated_by"), Some("Duplicated by"));
        assert_eq!(related_tag("relates"), None);
    }

    #[test]
    fn an_issue_with_no_links_is_empty() {
        let mut g = graph();
        g.upstream.clear();
        g.downstream.clear();
        g.related.clear();
        assert!(is_empty(&g));
    }
}
