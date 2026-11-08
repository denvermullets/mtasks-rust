//! Pure dependency-graph queries over local rows (ports of the Rails
//! `DependencyGraphQuery`, `ProjectDependencyGraphQuery` and
//! `DependencyOverlayHelper#board_dependency_edges`). Never touches the DB.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::db::models::{Issue, IssueDependency, Lane};

/// Default depth of the issue map (Rails `DEFAULT_MAX_DEPTH`).
pub const DEFAULT_MAX_DEPTH: usize = 3;

/// A link between two nodes of a result, oriented blocking -> blocked as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// `blocks` | `relates` | `duplicates`.
    pub kind: String,
}

/// A direct relates/duplicates link of the focus issue.
#[derive(Debug, Clone, PartialEq)]
pub struct RelatedEntry {
    pub issue: Issue,
    pub kind: String,
    /// Relative to the focus: `relates` | `duplicates` | `duplicated_by`.
    pub direction: String,
}

/// Result of [`issue_graph`]. Depth keys start at 1; depths with no nodes are absent.
#[derive(Debug, Clone, PartialEq)]
pub struct IssueGraph {
    pub focus: Issue,
    /// What transitively blocks the focus, by depth.
    pub upstream: BTreeMap<usize, Vec<Issue>>,
    /// What the focus transitively blocks, by depth.
    pub downstream: BTreeMap<usize, Vec<Issue>>,
    pub related: Vec<RelatedEntry>,
    /// Every link between any two nodes in the result (so diamonds draw).
    pub edges: Vec<Edge>,
    /// A node exists past the depth cutoff.
    pub truncated: bool,
}

type SortKey = (i32, Option<i32>);

fn sort_key(issue: &Issue, lanes: &HashMap<&str, i32>) -> SortKey {
    (
        lanes.get(issue.lane_id.as_str()).copied().unwrap_or(i32::MAX),
        issue.team_number,
    )
}

fn lane_positions(lanes: &[Lane]) -> HashMap<&str, i32> {
    lanes.iter().map(|l| (l.id.as_str(), l.position)).collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Direction {
    Upstream,
    Downstream,
}

/// Distinct ids one `blocks` hop from `frontier`, restricted to `scope` issues.
fn neighbor_ids(
    deps: &[IssueDependency],
    scope: &HashMap<&str, &Issue>,
    direction: Direction,
    frontier: &[String],
) -> Vec<String> {
    let frontier: HashSet<&str> = frontier.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for d in deps.iter().filter(|d| d.kind == "blocks") {
        let (matched, neighbor) = match direction {
            Direction::Upstream => (&d.blocked_issue_id, &d.blocking_issue_id),
            Direction::Downstream => (&d.blocking_issue_id, &d.blocked_issue_id),
        };
        if frontier.contains(matched.as_str())
            && scope.contains_key(neighbor.as_str())
            && seen.insert(neighbor.as_str())
        {
            out.push(neighbor.clone());
        }
    }
    out
}

/// Walk the blocks chain around `focus` (an issue id) up to `max_depth` levels.
/// Same team as the focus only. Returns `None` when the focus isn't in `issues`.
pub fn issue_graph(
    focus: &str,
    deps: &[IssueDependency],
    issues: &[Issue],
    lanes: &[Lane],
    max_depth: usize,
) -> Option<IssueGraph> {
    let focus_issue = issues.iter().find(|i| i.id == focus)?;
    let scope: HashMap<&str, &Issue> = issues
        .iter()
        .filter(|i| i.team_id == focus_issue.team_id)
        .map(|i| (i.id.as_str(), i))
        .collect();
    let positions = lane_positions(lanes);

    // Shared across directions and seeded with the focus so cycles terminate.
    let mut visited: HashSet<String> = HashSet::from([focus.to_string()]);
    let mut walk = |direction: Direction| {
        let mut by_depth: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        let mut frontier = vec![focus.to_string()];
        for depth in 1..=max_depth {
            frontier = neighbor_ids(deps, &scope, direction, &frontier)
                .into_iter()
                .filter(|id| !visited.contains(id))
                .collect();
            if frontier.is_empty() {
                break;
            }
            visited.extend(frontier.iter().cloned());
            by_depth.insert(depth, frontier.clone());
        }
        // Only a walk that reached max_depth has a frontier worth probing.
        let probe = if by_depth.len() == max_depth { frontier } else { Vec::new() };
        (by_depth, probe)
    };
    let (up_ids, up_frontier) = walk(Direction::Upstream);
    let (down_ids, down_frontier) = walk(Direction::Downstream);

    // After both walks, so a node the other direction picked up isn't "cut off".
    let truncated = [
        (Direction::Upstream, &up_frontier),
        (Direction::Downstream, &down_frontier),
    ]
    .iter()
    .any(|(dir, frontier)| {
        !frontier.is_empty()
            && neighbor_ids(deps, &scope, *dir, frontier)
                .iter()
                .any(|id| !visited.contains(id))
    });

    let related_links: Vec<&IssueDependency> = deps
        .iter()
        .filter(|d| {
            (d.kind == "relates" || d.kind == "duplicates")
                && (d.blocking_issue_id == focus || d.blocked_issue_id == focus)
        })
        .collect();

    let mut node_ids: HashSet<&str> = visited.iter().map(String::as_str).collect();
    node_ids.extend(related_links.iter().map(|d| other_end(d, focus)));
    node_ids.retain(|id| scope.contains_key(id));

    let bucket = |ids: BTreeMap<usize, Vec<String>>| -> BTreeMap<usize, Vec<Issue>> {
        ids.into_iter()
            .map(|(depth, ids)| {
                let mut v: Vec<Issue> = ids
                    .iter()
                    .filter_map(|id| scope.get(id.as_str()).map(|i| (*i).clone()))
                    .collect();
                v.sort_by_key(|i| sort_key(i, &positions));
                (depth, v)
            })
            .collect()
    };

    let mut related: Vec<RelatedEntry> = related_links
        .iter()
        .filter_map(|d| {
            let other = scope.get(other_end(d, focus))?;
            Some(RelatedEntry {
                issue: (*other).clone(),
                kind: d.kind.clone(),
                direction: direction_for(d, focus),
            })
        })
        .collect();
    related.sort_by_key(|e| sort_key(&e.issue, &positions));

    let edges = deps
        .iter()
        .filter(|d| {
            node_ids.contains(d.blocking_issue_id.as_str())
                && node_ids.contains(d.blocked_issue_id.as_str())
        })
        .map(|d| Edge {
            from: d.blocking_issue_id.clone(),
            to: d.blocked_issue_id.clone(),
            kind: d.kind.clone(),
        })
        .collect();

    Some(IssueGraph {
        focus: focus_issue.clone(),
        upstream: bucket(up_ids),
        downstream: bucket(down_ids),
        related,
        edges,
        truncated,
    })
}

fn other_end<'a>(d: &'a IssueDependency, id: &str) -> &'a str {
    if d.blocking_issue_id == id {
        &d.blocked_issue_id
    } else {
        &d.blocking_issue_id
    }
}

/// Direction of `dep` relative to `issue_id` (Rails `Link.direction_for`).
pub fn direction_for(dep: &IssueDependency, issue_id: &str) -> String {
    let source = dep.blocking_issue_id == issue_id;
    match dep.kind.as_str() {
        "relates" => "relates",
        "duplicates" if source => "duplicates",
        "duplicates" => "duplicated_by",
        _ if source => "blocking",
        _ => "blocked_by",
    }
    .to_string()
}

/// One card on the project map.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectNode {
    pub issue: Issue,
    /// Set only for issues outside the project: their project's name, or "No project".
    pub outside_project: Option<String>,
}

/// Result of [`project_graph`]: columns left to right, in display order.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectGraph {
    pub columns: Vec<Vec<ProjectNode>>,
    pub edges: Vec<Edge>,
    pub active_only: bool,
}

impl ProjectGraph {
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

/// Not completed or canceled, and not in a completed/canceled lane (Rails `Issue.active`).
fn is_active(issue: &Issue, lanes: &[Lane]) -> bool {
    if issue.completed_at.is_some() || issue.canceled_at.is_some() {
        return false;
    }
    !lanes
        .iter()
        .find(|l| l.id == issue.lane_id)
        .and_then(|l| l.category.as_deref())
        .is_some_and(|c| c == "completed" || c == "canceled")
}

type Adjacency<'a> = HashMap<&'a str, Vec<&'a str>>;

/// Every issue of `project` laid out left to right by the blocks chain. Column 0 holds issues
/// nothing blocks (including unlinked ones); outside issues appear only through a surviving
/// direct link. `projects` is used to name outside issues' projects.
pub fn project_graph(
    project: &crate::db::models::Project,
    deps: &[IssueDependency],
    issues: &[Issue],
    lanes: &[Lane],
    projects: &[crate::db::models::Project],
    active_only: bool,
) -> ProjectGraph {
    let positions = lane_positions(lanes);
    let in_scope = |i: &&Issue| {
        i.team_id == project.team_id && (!active_only || is_active(i, lanes))
    };
    let own: Vec<&Issue> = issues
        .iter()
        .filter(|i| i.project_id.as_deref() == Some(project.id.as_str()))
        .filter(in_scope)
        .collect();
    let own_ids: HashSet<&str> = own.iter().map(|i| i.id.as_str()).collect();

    // Links touching a project issue, distinct by (from, to, kind).
    let mut seen = HashSet::new();
    let touching: Vec<&IssueDependency> = deps
        .iter()
        .filter(|d| {
            (own_ids.contains(d.blocking_issue_id.as_str())
                || own_ids.contains(d.blocked_issue_id.as_str()))
                && seen.insert((
                    d.blocking_issue_id.as_str(),
                    d.blocked_issue_id.as_str(),
                    d.kind.as_str(),
                ))
        })
        .collect();

    // Outside issues only come in through a link whose both ends survive the scope.
    let by_id: HashMap<&str, &Issue> = issues
        .iter()
        .filter(in_scope)
        .map(|i| (i.id.as_str(), i))
        .collect();
    let mut loaded: Vec<&Issue> = own.clone();
    let mut loaded_ids = own_ids.clone();
    for d in &touching {
        for id in [&d.blocking_issue_id, &d.blocked_issue_id] {
            if let Some(i) = by_id.get(id.as_str()) {
                if loaded_ids.insert(i.id.as_str()) {
                    loaded.push(i);
                }
            }
        }
    }
    let edges: Vec<Edge> = touching
        .iter()
        .filter(|d| {
            loaded_ids.contains(d.blocking_issue_id.as_str())
                && loaded_ids.contains(d.blocked_issue_id.as_str())
        })
        .map(|d| Edge {
            from: d.blocking_issue_id.clone(),
            to: d.blocked_issue_id.clone(),
            kind: d.kind.clone(),
        })
        .collect();

    let mut blockers: Adjacency = HashMap::new();
    let mut blocked: Adjacency = HashMap::new();
    let mut related: Adjacency = HashMap::new();
    for e in &edges {
        let (from, to) = (e.from.as_str(), e.to.as_str());
        if e.kind == "blocks" {
            blockers.entry(to).or_default().push(from);
            blocked.entry(from).or_default().push(to);
        } else {
            related.entry(from).or_default().push(to);
            related.entry(to).or_default().push(from);
        }
    }
    let in_chain = |id: &str| blockers.contains_key(id) || blocked.contains_key(id);
    let linked = |id: &str| in_chain(id) || related.contains_key(id);

    let ids: Vec<&str> = loaded.iter().map(|i| i.id.as_str()).collect();
    let ranks = ranks_for(&ids, &blockers, &blocked, &related, &in_chain);

    let mut by_rank: BTreeMap<usize, Vec<&Issue>> = BTreeMap::new();
    for i in &loaded {
        by_rank.entry(ranks[i.id.as_str()]).or_default().push(i);
    }

    let mut rows: HashMap<&str, usize> = HashMap::new();
    let mut columns: Vec<Vec<ProjectNode>> = Vec::new();
    for (index, mut column) in by_rank.into_values().enumerate() {
        if index == 0 {
            column.sort_by_key(|i| (u8::from(!linked(&i.id)), sort_key(i, &positions)));
        } else {
            let bary = |id: &str| {
                let placed: Vec<usize> = [&blockers, &blocked, &related]
                    .iter()
                    .flat_map(|adj| adj.get(id).into_iter().flatten())
                    .filter_map(|other| rows.get(other).copied())
                    .collect();
                (!placed.is_empty())
                    .then(|| placed.iter().sum::<usize>() as f64 / placed.len() as f64)
            };
            column.sort_by(|a, b| {
                let ka = bary(&a.id).unwrap_or(f64::INFINITY);
                let kb = bary(&b.id).unwrap_or(f64::INFINITY);
                ka.total_cmp(&kb)
                    .then_with(|| sort_key(a, &positions).cmp(&sort_key(b, &positions)))
            });
        }
        for (row, i) in column.iter().enumerate() {
            rows.insert(i.id.as_str(), row);
        }
        columns.push(
            column
                .into_iter()
                .map(|i| ProjectNode {
                    issue: i.clone(),
                    outside_project: (!own_ids.contains(i.id.as_str()))
                        .then(|| outside_label(i, projects)),
                })
                .collect(),
        );
    }

    ProjectGraph { columns, edges, active_only }
}

fn outside_label(issue: &Issue, projects: &[crate::db::models::Project]) -> String {
    issue
        .project_id
        .as_deref()
        .and_then(|pid| projects.iter().find(|p| p.id == pid))
        .map_or_else(|| "No project".to_string(), |p| p.name.clone())
}

/// Longest path from a source over blocks edges; relates/duplicates-only issues sit beside
/// their chain partners; issues stuck in or behind a cycle land one past their furthest
/// drained blocker.
fn ranks_for<'a>(
    ids: &[&'a str],
    blockers: &Adjacency<'a>,
    blocked: &Adjacency<'a>,
    related: &Adjacency<'a>,
    in_chain: &dyn Fn(&str) -> bool,
) -> HashMap<&'a str, usize> {
    // Kahn's algorithm.
    let mut pending: HashMap<&str, usize> = ids
        .iter()
        .map(|id| (*id, blockers.get(id).map_or(0, Vec::len)))
        .collect();
    let mut queue: std::collections::VecDeque<&str> =
        ids.iter().copied().filter(|id| pending[id] == 0).collect();
    let mut ranks: HashMap<&str, usize> = queue.iter().map(|id| (*id, 0)).collect();
    while let Some(id) = queue.pop_front() {
        let rank = ranks[id];
        for next in blocked.get(id).into_iter().flatten() {
            let r = ranks.entry(next).or_insert(0);
            *r = (*r).max(rank + 1);
            let p = pending.get_mut(next).expect("blocked id is loaded");
            *p -= 1;
            if *p == 0 {
                queue.push_back(next);
            }
        }
    }
    let (stuck, drained): (Vec<&str>, Vec<&str>) = ids.iter().partition(|id| pending[*id] > 0);
    let drained: HashSet<&str> = drained.into_iter().collect();
    for id in stuck {
        let furthest = blockers
            .get(id)
            .into_iter()
            .flatten()
            .filter(|b| drained.contains(*b))
            .filter_map(|b| ranks.get(b))
            .max()
            .copied()
            .unwrap_or(0);
        ranks.insert(id, furthest + 1);
    }
    for id in ids.iter().filter(|id| !in_chain(id)) {
        let partner = related
            .get(id)
            .into_iter()
            .flatten()
            .filter(|p| in_chain(p))
            .filter_map(|p| ranks.get(p))
            .min();
        if let Some(r) = partner {
            ranks.insert(id, *r);
        }
    }
    ranks
}

/// Board dependency lines: [`board_edges`] plus a symmetric neighbors map.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardEdges {
    /// Oriented blocking -> blocked, as stored, in `deps` order.
    pub edges: Vec<Edge>,
    /// Issue id -> ids it is linked to (either direction). Only issues with a line appear.
    pub neighbors: HashMap<String, HashSet<String>>,
}

impl BoardEdges {
    /// Whether the card has at least one line.
    pub fn is_linked(&self, id: &str) -> bool {
        self.neighbors.contains_key(id)
    }
}

/// Every link whose two ends are both on the board. `visible_issue_ids` may repeat an id
/// (an issue under several label groups); each link still appears once.
pub fn board_edges<I, S>(visible_issue_ids: I, deps: &[IssueDependency]) -> BoardEdges
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let visible: HashSet<String> = visible_issue_ids
        .into_iter()
        .map(|s| s.as_ref().to_string())
        .collect();
    let mut seen = HashSet::new();
    let mut out = BoardEdges::default();
    for d in deps {
        if !visible.contains(&d.blocking_issue_id) || !visible.contains(&d.blocked_issue_id) {
            continue;
        }
        if !seen.insert((&d.blocking_issue_id, &d.blocked_issue_id, &d.kind)) {
            continue;
        }
        out.edges.push(Edge {
            from: d.blocking_issue_id.clone(),
            to: d.blocked_issue_id.clone(),
            kind: d.kind.clone(),
        });
        if d.blocking_issue_id != d.blocked_issue_id {
            out.neighbors
                .entry(d.blocking_issue_id.clone())
                .or_default()
                .insert(d.blocked_issue_id.clone());
            out.neighbors
                .entry(d.blocked_issue_id.clone())
                .or_default()
                .insert(d.blocking_issue_id.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn lane(id: &str, position: i32, category: &str) -> Lane {
        Lane {
            id: id.into(),
            name: id.into(),
            color: None,
            position,
            team_id: "t1".into(),
            remote_id: None,
            category: Some(category.into()),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    pub fn issue(id: &str, number: i32) -> Issue {
        Issue {
            id: id.into(),
            title: id.into(),
            description: None,
            priority: 0,
            team_number: Some(number),
            due_date: None,
            estimate: None,
            started_at: None,
            completed_at: None,
            canceled_at: None,
            lane_id: "backlog".into(),
            team_id: "t1".into(),
            project_id: None,
            parent_issue_id: None,
            assignee_id: None,
            creator_id: None,
            remote_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    pub fn dep(from: &str, to: &str, kind: &str) -> IssueDependency {
        IssueDependency {
            id: format!("{from}-{to}-{kind}"),
            remote_id: None,
            blocking_issue_id: from.into(),
            blocked_issue_id: to.into(),
            kind: kind.into(),
            direction: "blocking".into(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    pub fn lanes() -> Vec<Lane> {
        vec![
            lane("backlog", 0, "backlog"),
            lane("started", 1, "started"),
            lane("done", 2, "completed"),
        ]
    }

    /// Issues numbered 1.., chained first -> second -> third ...
    fn chain(ids: &[&str]) -> (Vec<Issue>, Vec<IssueDependency>) {
        let issues = ids
            .iter()
            .enumerate()
            .map(|(n, id)| issue(id, n as i32 + 1))
            .collect();
        let deps = ids.windows(2).map(|w| dep(w[0], w[1], "blocks")).collect();
        (issues, deps)
    }

    fn ids(v: &[Issue]) -> Vec<&str> {
        v.iter().map(|i| i.id.as_str()).collect()
    }

    #[test]
    fn chain_splits_by_depth_around_the_focus() {
        let (issues, deps) = chain(&["a", "b", "c", "d", "e"]);
        let g = issue_graph("c", &deps, &issues, &lanes(), 3).unwrap();
        assert_eq!(ids(&g.upstream[&1]), ["b"]);
        assert_eq!(ids(&g.upstream[&2]), ["a"]);
        assert_eq!(ids(&g.downstream[&1]), ["d"]);
        assert_eq!(ids(&g.downstream[&2]), ["e"]);
        assert!(!g.upstream.contains_key(&3));
        assert!(!g.truncated);
        assert_eq!(g.edges.len(), 4);
    }

    #[test]
    fn diamond_lists_shared_node_once_and_keeps_both_edges() {
        let issues = vec![issue("a", 1), issue("b", 2), issue("c", 3), issue("d", 4)];
        let deps = vec![
            dep("a", "b", "blocks"),
            dep("a", "c", "blocks"),
            dep("b", "d", "blocks"),
            dep("c", "d", "blocks"),
        ];
        let g = issue_graph("a", &deps, &issues, &lanes(), 3).unwrap();
        assert_eq!(ids(&g.downstream[&1]), ["b", "c"]);
        assert_eq!(ids(&g.downstream[&2]), ["d"]);
        assert_eq!(g.edges.len(), 4);
    }

    #[test]
    fn cycle_terminates_and_returns_each_node_once() {
        let issues = vec![issue("a", 1), issue("b", 2), issue("c", 3)];
        let deps = vec![
            dep("a", "b", "blocks"),
            dep("b", "c", "blocks"),
            dep("c", "a", "blocks"),
        ];
        let g = issue_graph("a", &deps, &issues, &lanes(), 5).unwrap();
        let mut all: Vec<&str> = g
            .upstream
            .values()
            .chain(g.downstream.values())
            .flat_map(|v| v.iter().map(|i| i.id.as_str()))
            .collect();
        all.sort();
        assert_eq!(all, ["b", "c"]);
        assert!(!g.truncated);
        assert_eq!(g.edges.len(), 3);
    }

    #[test]
    fn truncation_at_depth_1_3_and_5() {
        let (issues, deps) = chain(&["a", "b", "c", "d", "e", "f", "g"]);
        let g1 = issue_graph("a", &deps, &issues, &lanes(), 1).unwrap();
        assert_eq!(g1.downstream.len(), 1);
        assert!(g1.truncated);
        let g3 = issue_graph("a", &deps, &issues, &lanes(), 3).unwrap();
        assert!(g3.truncated);
        let g5 = issue_graph("a", &deps, &issues, &lanes(), 5).unwrap();
        assert!(g5.truncated);
        assert_eq!(g5.downstream.len(), 5);
        let g6 = issue_graph("a", &deps, &issues, &lanes(), 6).unwrap();
        assert!(!g6.truncated);
        // A chain ending exactly at max_depth is not truncated.
        let (i4, d4) = chain(&["a", "b", "c", "d"]);
        assert!(!issue_graph("a", &d4, &i4, &lanes(), 3).unwrap().truncated);
        assert!(issue_graph("a", &d4, &i4, &lanes(), 2).unwrap().truncated);
    }

    #[test]
    fn related_and_duplicate_directions() {
        let issues: Vec<Issue> = ["focus", "ro", "ri", "canon", "dupe", "far"]
            .iter()
            .enumerate()
            .map(|(n, id)| issue(id, n as i32 + 1))
            .collect();
        let deps = vec![
            dep("focus", "ro", "relates"),
            dep("ri", "focus", "relates"),
            dep("focus", "canon", "duplicates"),
            dep("dupe", "focus", "duplicates"),
            dep("ro", "far", "relates"),
        ];
        let g = issue_graph("focus", &deps, &issues, &lanes(), 3).unwrap();
        let got: Vec<(&str, &str, &str)> = g
            .related
            .iter()
            .map(|e| (e.issue.id.as_str(), e.kind.as_str(), e.direction.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("ro", "relates", "relates"),
                ("ri", "relates", "relates"),
                ("canon", "duplicates", "duplicates"),
                ("dupe", "duplicates", "duplicated_by"),
            ]
        );
        assert!(g
            .edges
            .iter()
            .any(|e| e.from == "focus" && e.to == "ro" && e.kind == "relates"));
        // "far" is two hops away through a relates link: not a node.
        assert!(!g.edges.iter().any(|e| e.to == "far"));
    }

    #[test]
    fn node_reached_in_both_directions_appears_once() {
        // b is both upstream and downstream of f through a loop.
        let issues = vec![issue("f", 1), issue("a", 2), issue("b", 3)];
        let deps = vec![
            dep("a", "f", "blocks"),
            dep("f", "b", "blocks"),
            dep("b", "a", "blocks"),
        ];
        let g = issue_graph("f", &deps, &issues, &lanes(), 3).unwrap();
        let count = g
            .upstream
            .values()
            .chain(g.downstream.values())
            .flat_map(|v| v.iter())
            .filter(|i| i.id == "b")
            .count();
        assert_eq!(count, 1);
        assert!(!g.truncated);
    }

    #[test]
    fn other_teams_never_appear_even_through_a_bad_row() {
        let mut other = issue("x", 9);
        other.team_id = "t2".into();
        let issues = vec![issue("a", 1), issue("b", 2), other];
        let deps = vec![dep("x", "a", "blocks"), dep("a", "b", "blocks")];
        let g = issue_graph("a", &deps, &issues, &lanes(), 3).unwrap();
        assert!(g.upstream.is_empty());
        assert_eq!(g.edges.len(), 1);
    }

    #[test]
    fn depth_buckets_sort_by_lane_position_then_number() {
        let mut a = issue("a", 1);
        a.lane_id = "started".into();
        let mut b = issue("b", 5);
        b.lane_id = "backlog".into();
        let c = issue("c", 3);
        let f = issue("f", 9);
        let deps = vec![
            dep("f", "a", "blocks"),
            dep("f", "b", "blocks"),
            dep("f", "c", "blocks"),
        ];
        let g = issue_graph("f", &deps, &[a, b, c, f], &lanes(), 3).unwrap();
        assert_eq!(ids(&g.downstream[&1]), ["c", "b", "a"]);
    }

    fn project(id: &str, name: &str) -> crate::db::models::Project {
        crate::db::models::Project {
            id: id.into(),
            remote_id: None,
            team_id: "t1".into(),
            name: name.into(),
            description: None,
            status: None,
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

    fn in_project(id: &str, number: i32, project_id: Option<&str>) -> Issue {
        let mut i = issue(id, number);
        i.project_id = project_id.map(String::from);
        i
    }

    fn cols(g: &ProjectGraph) -> Vec<Vec<&str>> {
        g.columns
            .iter()
            .map(|c| c.iter().map(|n| n.issue.id.as_str()).collect())
            .collect()
    }

    fn run(issues: &[Issue], deps: &[IssueDependency], active_only: bool) -> ProjectGraph {
        let ps = [project("p", "Launch"), project("q", "Infra")];
        project_graph(&ps[0], deps, issues, &lanes(), &ps, active_only)
    }

    #[test]
    fn project_empty() {
        let g = run(&[], &[], false);
        assert!(g.is_empty());
        assert!(g.edges.is_empty());
    }

    #[test]
    fn project_unlinked_only_sorted_by_lane_then_number() {
        let a = in_project("a", 3, Some("p"));
        let mut b = in_project("b", 1, Some("p"));
        b.lane_id = "started".into();
        let c = in_project("c", 2, Some("p"));
        let g = run(&[a, b, c], &[], false);
        assert_eq!(cols(&g), [["c", "a", "b"]]);
    }

    #[test]
    fn project_chain_and_longest_path_diamond() {
        let issues: Vec<Issue> = ["a", "b", "c", "d"]
            .iter()
            .enumerate()
            .map(|(n, id)| in_project(id, n as i32 + 1, Some("p")))
            .collect();
        let deps = vec![
            dep("a", "b", "blocks"),
            dep("b", "c", "blocks"),
            dep("a", "c", "blocks"),
            dep("d", "c", "blocks"),
        ];
        let g = run(&issues, &deps, false);
        assert_eq!(cols(&g), [vec!["a", "d"], vec!["b"], vec!["c"]]);
        assert_eq!(g.edges.len(), 4);
    }

    #[test]
    fn project_stuck_cycle_is_placed_after_drained_blockers() {
        let issues: Vec<Issue> = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(n, id)| in_project(id, n as i32 + 1, Some("p")))
            .collect();
        let deps = vec![
            dep("a", "b", "blocks"),
            dep("b", "c", "blocks"),
            dep("c", "b", "blocks"),
        ];
        let g = run(&issues, &deps, false);
        // b and c never drain; both sit one past a (rank 0).
        assert_eq!(cols(&g), [vec!["a"], vec!["b", "c"]]);
    }

    #[test]
    fn project_related_only_sits_beside_partner() {
        let issues: Vec<Issue> = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(n, id)| in_project(id, n as i32 + 1, Some("p")))
            .collect();
        let deps = vec![dep("a", "b", "blocks"), dep("b", "c", "relates")];
        let g = run(&issues, &deps, false);
        assert_eq!(cols(&g), [vec!["a"], vec!["b", "c"]]);
    }

    #[test]
    fn project_first_column_puts_linked_before_unlinked() {
        let issues = vec![
            in_project("loner", 1, Some("p")),
            in_project("a", 2, Some("p")),
            in_project("b", 3, Some("p")),
        ];
        let g = run(&issues, &[dep("a", "b", "blocks")], false);
        assert_eq!(cols(&g), [vec!["a", "loner"], vec!["b"]]);
    }

    #[test]
    fn project_later_columns_order_by_barycenter() {
        // Column 0 order: x1 (#1), x2 (#2). y1 (#3) depends on x2, y2 (#4) on x1,
        // so barycenter flips them against their numbers.
        let issues = vec![
            in_project("x1", 1, Some("p")),
            in_project("x2", 2, Some("p")),
            in_project("y1", 3, Some("p")),
            in_project("y2", 4, Some("p")),
        ];
        let deps = vec![dep("x2", "y1", "blocks"), dep("x1", "y2", "blocks")];
        let g = run(&issues, &deps, false);
        assert_eq!(cols(&g), [vec!["x1", "x2"], vec!["y2", "y1"]]);
    }

    #[test]
    fn project_active_only_drops_closed_and_their_links() {
        let mut shipped = in_project("shipped", 1, Some("p"));
        shipped.completed_at = Some("2024-01-01".into());
        let mut staged = in_project("staged", 2, Some("p"));
        staged.lane_id = "done".into(); // completed lane, no timestamp
        let open = in_project("open", 3, Some("p"));
        let deps = vec![dep("shipped", "open", "blocks"), dep("staged", "open", "blocks")];
        let all = run(&[shipped.clone(), staged.clone(), open.clone()], &deps, false);
        assert_eq!(all.columns.iter().map(Vec::len).sum::<usize>(), 3);
        let active = run(&[shipped, staged, open], &deps, true);
        assert_eq!(cols(&active), [["open"]]);
        assert!(active.edges.is_empty());
        assert!(active.active_only);
    }

    #[test]
    fn project_outside_issues_tagged_and_only_via_links() {
        let outside = in_project("out", 1, Some("q"));
        let nowhere = in_project("nowhere", 2, None);
        let inside = in_project("in", 3, Some("p"));
        let unrelated = in_project("unrelated", 4, Some("q"));
        let deps = vec![dep("out", "in", "blocks"), dep("in", "nowhere", "relates")];
        let g = run(&[outside, nowhere, inside, unrelated], &deps, false);
        assert_eq!(cols(&g), [vec!["out"], vec!["in", "nowhere"]]);
        let tag = |c: usize, r: usize| g.columns[c][r].outside_project.clone();
        assert_eq!(tag(0, 0).as_deref(), Some("Infra"));
        assert_eq!(tag(1, 0), None);
        assert_eq!(tag(1, 1).as_deref(), Some("No project"));
    }

    #[test]
    fn project_other_team_issues_are_left_out() {
        let a = in_project("a", 1, Some("p"));
        let mut x = in_project("x", 2, Some("q"));
        x.team_id = "t2".into();
        let g = run(&[a, x], &[dep("a", "x", "blocks")], false);
        assert_eq!(cols(&g), [["a"]]);
        assert!(g.edges.is_empty());
    }

    #[test]
    fn board_edges_drop_links_with_a_hidden_end() {
        let deps = vec![dep("a", "b", "blocks"), dep("b", "hidden", "blocks")];
        let b = board_edges(["a", "b"], &deps);
        assert_eq!(b.edges.len(), 1);
        assert!(!b.is_linked("hidden"));
        assert!(b.is_linked("a"));
    }

    #[test]
    fn board_edges_keep_kinds_and_orientation() {
        let deps = vec![
            dep("a", "b", "blocks"),
            dep("b", "c", "relates"),
            dep("c", "a", "duplicates"),
        ];
        let b = board_edges(vec!["a".to_string(), "b".into(), "c".into()], &deps);
        let got: Vec<(&str, &str, &str)> = b
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str(), e.kind.as_str()))
            .collect();
        assert_eq!(
            got,
            [("a", "b", "blocks"), ("b", "c", "relates"), ("c", "a", "duplicates")]
        );
    }

    #[test]
    fn board_edges_dedupe_when_an_issue_shows_twice() {
        let deps = vec![dep("a", "b", "blocks")];
        let b = board_edges(["a", "b", "a", "b"], &deps);
        assert_eq!(b.edges.len(), 1);
    }

    #[test]
    fn board_neighbors_are_symmetric() {
        let deps = vec![dep("a", "b", "blocks"), dep("c", "a", "relates")];
        let b = board_edges(["a", "b", "c", "lonely"], &deps);
        assert_eq!(b.neighbors["a"], HashSet::from(["b".to_string(), "c".to_string()]));
        assert_eq!(b.neighbors["b"], HashSet::from(["a".to_string()]));
        assert_eq!(b.neighbors["c"], HashSet::from(["a".to_string()]));
        assert!(!b.is_linked("lonely"));
    }

    #[test]
    fn missing_focus_is_none() {
        assert!(issue_graph("nope", &[], &[], &lanes(), 3).is_none());
    }
}
