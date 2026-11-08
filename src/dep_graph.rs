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

    #[test]
    fn missing_focus_is_none() {
        assert!(issue_graph("nope", &[], &[], &lanes(), 3).is_none());
    }
}
