//! Sub-issue hierarchy over a slice of issues (pure; never touches the DB).
//! Ports Rails' `IssueHierarchyQuery` and `IssueHierarchy#parent_issue_error`.

// Consumers (pickers, detail view, maps) land in later tickets of project 294.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::db::models::{Issue, Lane};

/// Cap on both walks (and the default tree depth), like Rails' `MAX_DEPTH`.
pub const MAX_DEPTH: usize = 10;

/// Why a parent was refused. `Display` gives the full Rails message, e.g.
/// "Parent issue would create a circular sub-issue chain".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentError {
    DoesNotExist,
    DifferentTeam,
    SelfParent,
    Circular,
}

impl ParentError {
    /// Rails' bare message (without the "Parent issue" prefix).
    pub fn detail(&self) -> &'static str {
        match self {
            Self::DoesNotExist => "does not exist",
            Self::DifferentTeam => "must be in the same team",
            Self::SelfParent => "cannot be the issue itself",
            Self::Circular => "would create a circular sub-issue chain",
        }
    }
}

impl fmt::Display for ParentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Parent issue {}", self.detail())
    }
}

impl std::error::Error for ParentError {}

/// The sub-issue tree an issue belongs to. Ids only; look issues up in the
/// slice you built the [`Hierarchy`] from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub root: String,
    /// Root first, direct parent last; empty when the focus is top-level.
    pub ancestors: Vec<String>,
    /// Parent id -> children in display order. A leaf has no key.
    pub children: HashMap<String, Vec<String>>,
    /// The downward walk stopped with levels left to show.
    pub truncated: bool,
}

impl Tree {
    pub fn children_of(&self, id: &str) -> &[String] {
        self.children.get(id).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Index over issues (any teams; walks stay inside the starting issue's team)
/// plus lane positions for sibling ordering.
pub struct Hierarchy<'a> {
    issues: &'a [Issue],
    by_id: HashMap<&'a str, &'a Issue>,
    lane_pos: HashMap<&'a str, i32>,
}

impl<'a> Hierarchy<'a> {
    pub fn new(issues: &'a [Issue], lanes: &'a [Lane]) -> Self {
        Self {
            issues,
            by_id: issues.iter().map(|i| (i.id.as_str(), i)).collect(),
            lane_pos: lanes.iter().map(|l| (l.id.as_str(), l.position)).collect(),
        }
    }

    /// Ancestor ids, root first, direct parent last. Cycle-safe, capped at
    /// [`MAX_DEPTH`]. Empty for an unknown or top-level issue.
    pub fn ancestors(&self, issue_id: &str) -> Vec<String> {
        self.walk_up(issue_id, MAX_DEPTH)
    }

    /// Every descendant id, breadth-first. Cycle-safe; excludes the issue.
    pub fn descendants(&self, issue_id: &str) -> Vec<String> {
        let Some(start) = self.by_id.get(issue_id) else {
            return Vec::new();
        };
        let mut seen: HashSet<&str> = HashSet::from([issue_id]);
        let mut out = Vec::new();
        let mut frontier = vec![issue_id];
        while !frontier.is_empty() {
            let mut next = Vec::new();
            for parent in &frontier {
                for i in self.issues {
                    if i.team_id == start.team_id
                        && i.parent_issue_id.as_deref() == Some(*parent)
                        && seen.insert(i.id.as_str())
                    {
                        out.push(i.id.clone());
                        next.push(i.id.as_str());
                    }
                }
            }
            frontier = next;
        }
        out
    }

    /// The tree around `focus_id`, `None` for an unknown issue. `max_depth`
    /// is clamped to 1..=[`MAX_DEPTH`] and counts from the root.
    pub fn tree(&self, focus_id: &str, max_depth: usize) -> Option<Tree> {
        let focus = self.by_id.get(focus_id)?;
        let max_depth = max_depth.clamp(1, MAX_DEPTH);
        let ancestors = self.walk_up(focus_id, max_depth);
        let root = ancestors
            .first()
            .cloned()
            .unwrap_or_else(|| focus.id.clone());

        let team = focus.team_id.as_str();
        let mut seen: HashSet<&str> = HashSet::from([root.as_str()]);
        let mut by_parent: HashMap<String, Vec<&Issue>> = HashMap::new();
        let mut frontier: Vec<&str> = vec![root.as_str()];
        let mut truncated = false;
        for level in 0..max_depth {
            let rows: Vec<&Issue> = self
                .issues
                .iter()
                .filter(|i| {
                    i.team_id == team
                        && !seen.contains(i.id.as_str())
                        && i.parent_issue_id
                            .as_deref()
                            .is_some_and(|p| frontier.contains(&p))
                })
                .collect();
            if rows.is_empty() {
                break;
            }
            for r in &rows {
                if let Some(p) = &r.parent_issue_id {
                    by_parent.entry(p.clone()).or_default().push(r);
                }
            }
            frontier = rows.iter().map(|r| r.id.as_str()).collect();
            seen.extend(frontier.iter().copied());
            if level + 1 == max_depth {
                truncated = self.issues.iter().any(|i| {
                    i.team_id == team
                        && i.parent_issue_id
                            .as_deref()
                            .is_some_and(|p| frontier.contains(&p))
                });
            }
        }

        let children = by_parent
            .into_iter()
            .map(|(p, mut kids)| {
                kids.sort_by_key(|i| {
                    (
                        self.lane_pos.get(i.lane_id.as_str()).copied(),
                        i.team_number,
                    )
                });
                (p, kids.into_iter().map(|i| i.id.clone()).collect())
            })
            .collect();
        Some(Tree {
            root,
            ancestors,
            children,
            truncated,
        })
    }

    /// Rails' `parent_issue_error`. `None` (clearing) is always fine.
    pub fn validate_parent(
        &self,
        issue: &Issue,
        parent_id: Option<&str>,
    ) -> Result<(), ParentError> {
        let Some(parent_id) = parent_id else {
            return Ok(());
        };
        let parent = self
            .by_id
            .get(parent_id)
            .ok_or(ParentError::DoesNotExist)?;
        if parent.team_id != issue.team_id {
            return Err(ParentError::DifferentTeam);
        }
        if parent_id == issue.id {
            return Err(ParentError::SelfParent);
        }
        // Uncapped: a deep loop must still be caught.
        let mut chain = self.walk_up(parent_id, usize::MAX);
        chain.push(parent_id.to_string());
        if chain.iter().any(|a| *a == issue.id) {
            return Err(ParentError::Circular);
        }
        Ok(())
    }

    /// Issues the parent picker may offer: same team, not the issue itself,
    /// its descendants, its current parent or canceled.
    pub fn parent_candidates(&self, issue: &Issue) -> Vec<&'a Issue> {
        let descendants: HashSet<String> = self.descendants(&issue.id).into_iter().collect();
        self.issues
            .iter()
            .filter(|i| {
                i.team_id == issue.team_id
                    && i.id != issue.id
                    && i.canceled_at.is_none()
                    && !descendants.contains(&i.id)
                    && issue.parent_issue_id.as_deref() != Some(i.id.as_str())
            })
            .collect()
    }

    /// Issues the add-existing picker may offer: same team, not the issue
    /// itself, its ancestors, its current sub-issues or canceled.
    pub fn child_candidates(&self, issue: &Issue) -> Vec<&'a Issue> {
        let ancestors: HashSet<String> = self.walk_up(&issue.id, usize::MAX).into_iter().collect();
        self.issues
            .iter()
            .filter(|i| {
                i.team_id == issue.team_id
                    && i.id != issue.id
                    && i.canceled_at.is_none()
                    && !ancestors.contains(&i.id)
                    && i.parent_issue_id.as_deref() != Some(issue.id.as_str())
            })
            .collect()
    }

    /// Ancestors of `issue_id` (root first), stopping at a missing or
    /// other-team parent, a repeat, or `limit` entries.
    fn walk_up(&self, issue_id: &str, limit: usize) -> Vec<String> {
        let Some(start) = self.by_id.get(issue_id) else {
            return Vec::new();
        };
        let mut visited: HashSet<&str> = HashSet::from([issue_id]);
        let mut ids: Vec<String> = Vec::new();
        let mut current = start.parent_issue_id.as_deref();
        while let Some(id) = current {
            if visited.contains(id) || ids.len() >= limit {
                break;
            }
            let Some(row) = self.by_id.get(id).filter(|r| r.team_id == start.team_id) else {
                break;
            };
            visited.insert(id);
            ids.push(id.to_string());
            current = row.parent_issue_id.as_deref();
        }
        ids.reverse();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn iss(id: &str, n: i32, parent: Option<&str>) -> Issue {
        Issue {
            id: id.into(),
            title: id.into(),
            description: None,
            priority: 0,
            team_number: Some(n),
            due_date: None,
            estimate: None,
            started_at: None,
            completed_at: None,
            canceled_at: None,
            lane_id: "l1".into(),
            team_id: "t1".into(),
            project_id: None,
            parent_issue_id: parent.map(String::from),
            assignee_id: None,
            creator_id: None,
            remote_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn lane(id: &str, pos: i32) -> Lane {
        Lane {
            id: id.into(),
            name: id.into(),
            color: None,
            position: pos,
            team_id: "t1".into(),
            remote_id: None,
            category: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    pub(super) fn lanes() -> Vec<Lane> {
        vec![lane("l1", 0), lane("l2", 1)]
    }

    /// a -> b -> c -> d, plus a -> e
    pub(super) fn chain() -> Vec<Issue> {
        vec![
            iss("a", 1, None),
            iss("b", 2, Some("a")),
            iss("c", 3, Some("b")),
            iss("d", 4, Some("c")),
            iss("e", 5, Some("a")),
        ]
    }

    #[test]
    fn top_level_issue() {
        let issues = vec![iss("a", 1, None)];
        let ls = lanes();
        let h = Hierarchy::new(&issues, &ls);
        assert!(h.ancestors("a").is_empty());
        assert!(h.descendants("a").is_empty());
        let t = h.tree("a", MAX_DEPTH).unwrap();
        assert_eq!(t.root, "a");
        assert!(t.ancestors.is_empty() && t.children.is_empty() && !t.truncated);
        assert!(h.tree("zzz", 3).is_none());
    }

    #[test]
    fn deep_tree_from_leaf_and_middle() {
        let issues = chain();
        let ls = lanes();
        let h = Hierarchy::new(&issues, &ls);
        assert_eq!(h.ancestors("d"), vec!["a", "b", "c"]);
        let mut d = h.descendants("a");
        d.sort();
        assert_eq!(d, vec!["b", "c", "d", "e"]);

        let t = h.tree("c", MAX_DEPTH).unwrap();
        assert_eq!(t.root, "a");
        assert_eq!(t.ancestors, vec!["a", "b"]);
        assert_eq!(t.children_of("a"), ["b", "e"]);
        assert_eq!(t.children_of("b"), ["c"]);
        assert_eq!(t.children_of("c"), ["d"]);
        assert!(t.children_of("d").is_empty());
        assert!(!t.truncated);
    }

    #[test]
    fn legacy_cycle_terminates() {
        // x -> y -> z -> x (parent pointers)
        let issues = vec![
            iss("x", 1, Some("z")),
            iss("y", 2, Some("x")),
            iss("z", 3, Some("y")),
        ];
        let ls = lanes();
        let h = Hierarchy::new(&issues, &ls);
        assert_eq!(h.ancestors("x"), vec!["y", "z"]);
        let mut d = h.descendants("x");
        d.sort();
        assert_eq!(d, vec!["y", "z"]);
        let t = h.tree("x", MAX_DEPTH).unwrap();
        assert_eq!(t.ancestors.len(), 2);
        assert!(t.children.values().map(Vec::len).sum::<usize>() <= 2);
    }

    fn line(n: usize) -> Vec<Issue> {
        (0..n)
            .map(|k| {
                let p = (k > 0).then(|| format!("i{}", k - 1));
                iss(&format!("i{k}"), k as i32, p.as_deref())
            })
            .collect()
    }

    #[test]
    fn truncation_at_cap_vs_one_past() {
        let ls = lanes();
        // root + 3 levels fits max_depth 3 exactly.
        let issues = line(4);
        let h = Hierarchy::new(&issues, &ls);
        let t = h.tree("i0", 3).unwrap();
        assert!(!t.truncated);
        assert_eq!(t.children_of("i2"), ["i3"]);
        // one more level is cut and flagged.
        let issues = line(5);
        let h = Hierarchy::new(&issues, &ls);
        let t = h.tree("i0", 3).unwrap();
        assert!(t.truncated);
        assert!(t.children_of("i3").is_empty());
        // max_depth is clamped to MAX_DEPTH.
        let issues = line(13);
        let h = Hierarchy::new(&issues, &ls);
        let t = h.tree("i0", 99).unwrap();
        assert!(t.truncated);
        assert_eq!(t.children_of("i9"), ["i10"]);
        assert!(t.children_of("i10").is_empty());
    }

    #[test]
    fn ancestors_capped() {
        let issues = line(15);
        let ls = lanes();
        let h = Hierarchy::new(&issues, &ls);
        assert_eq!(h.ancestors("i14").len(), MAX_DEPTH);
    }

    #[test]
    fn siblings_sorted_by_lane_then_number() {
        let mut issues = vec![iss("p", 1, None)];
        let k1 = iss("k1", 9, Some("p"));
        let mut k2 = iss("k2", 2, Some("p"));
        k2.lane_id = "l2".into();
        let k3 = iss("k3", 3, Some("p"));
        issues.extend([k1, k2, k3]);
        let ls = lanes();
        let h = Hierarchy::new(&issues, &ls);
        let t = h.tree("p", 5).unwrap();
        assert_eq!(t.children_of("p"), ["k3", "k1", "k2"]);
    }

    #[test]
    fn validate_parent_rules() {
        let mut issues = chain();
        let mut o = iss("o", 1, None);
        o.team_id = "t2".into();
        issues.push(o);
        let ls = lanes();
        let h = Hierarchy::new(&issues, &ls);
        let a = &issues[0];
        let b = &issues[1];
        assert_eq!(h.validate_parent(a, None), Ok(()));
        assert_eq!(
            h.validate_parent(a, Some("nope")),
            Err(ParentError::DoesNotExist)
        );
        assert_eq!(
            h.validate_parent(a, Some("o")),
            Err(ParentError::DifferentTeam)
        );
        assert_eq!(
            h.validate_parent(a, Some("a")),
            Err(ParentError::SelfParent)
        );
        // direct child and grandchild as parent
        assert_eq!(h.validate_parent(a, Some("b")), Err(ParentError::Circular));
        assert_eq!(h.validate_parent(a, Some("d")), Err(ParentError::Circular));
        // sibling re-parent is fine, so is the unchanged parent
        assert_eq!(h.validate_parent(b, Some("e")), Ok(()));
        assert_eq!(h.validate_parent(b, Some("a")), Ok(()));
        assert_eq!(
            ParentError::Circular.to_string(),
            "Parent issue would create a circular sub-issue chain"
        );
        assert_eq!(
            ParentError::DifferentTeam.detail(),
            "must be in the same team"
        );
    }

    #[test]
    fn validate_parent_catches_loop_deeper_than_cap() {
        let issues = line(15);
        let ls = lanes();
        let h = Hierarchy::new(&issues, &ls);
        assert_eq!(
            h.validate_parent(&issues[0], Some("i14")),
            Err(ParentError::Circular)
        );
    }

    fn ids(v: Vec<&Issue>) -> Vec<&str> {
        v.into_iter().map(|i| i.id.as_str()).collect()
    }

    #[test]
    fn candidate_sets() {
        let mut issues = chain();
        let mut x = iss("x", 6, None);
        x.canceled_at = Some("2026-01-01".into());
        let mut o = iss("o", 7, None);
        o.team_id = "t2".into();
        issues.extend([x, o]);
        let ls = lanes();
        let h = Hierarchy::new(&issues, &ls);
        // b: not itself, not descendants c/d, not parent a, not canceled/other team.
        assert_eq!(ids(h.parent_candidates(&issues[1])), vec!["e"]);
        // b: not itself, not ancestor a, not child c; d and e are fair game.
        assert_eq!(ids(h.child_candidates(&issues[1])), vec!["d", "e"]);
    }

    #[test]
    fn other_team_issues_are_ignored() {
        let mut issues = chain();
        let mut o = iss("o", 1, Some("a"));
        o.team_id = "t2".into();
        issues.push(o);
        let ls = lanes();
        let h = Hierarchy::new(&issues, &ls);
        assert!(!h.descendants("a").contains(&"o".to_string()));
        assert!(!h
            .tree("a", 5)
            .unwrap()
            .children_of("a")
            .contains(&"o".to_string()));
    }
}
