use std::collections::HashSet;

use iced::widget::{button, column, container, row, scrollable};
use crate::views::text;
use iced::{Border, Element, Length, Theme};

use crate::app::{DepDirection, Message};
use crate::db::models::{Issue, Team};
use crate::theme as t;

/// State for the searchable issue-dependency picker modal. `direction` is the
/// chosen link kind (defaulting to the sidebar section that opened it); the
/// create handler uses it to orient the new edge.
pub struct IssuePickerState {
    pub query: String,
    pub direction: DepDirection,
    pub mode: PickerMode,
}

/// What selecting a row does: add a dependency link, or (hierarchy) set the
/// open issue's parent / add the pick as a sub-issue of the open issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerMode {
    Dependency,
    Parent,
    Child,
}

impl IssuePickerState {
    pub fn new(direction: DepDirection) -> Self {
        Self {
            query: String::new(),
            direction,
            mode: PickerMode::Dependency,
        }
    }

    /// A single-select hierarchy picker (no link-kind chips).
    pub fn hierarchy(mode: PickerMode) -> Self {
        Self {
            query: String::new(),
            direction: DepDirection::Blocking,
            mode,
        }
    }
}

/// Render the picker modal: a search box over a scrollable list of the team's
/// issues, excluding `excluded` (the open issue + any already-linked target).
/// Selecting a row creates the dependency; Cancel dismisses.
pub fn view<'a>(
    state: &'a IssuePickerState,
    issues: &'a [Issue],
    team: Option<&'a Team>,
    excluded: &HashSet<&str>,
) -> Element<'a, Message> {
    // Kind chips: the active one is highlighted.
    let mut kinds = row![].spacing(6);
    let is_dep = state.mode == PickerMode::Dependency;
    for kind in DepDirection::ALL.into_iter().filter(|_| is_dep) {
        let active = kind == state.direction;
        let chip = button(text(kind.label()).size(12))
            .padding([4, 10])
            .style(if active { button::primary } else { button::secondary })
            .on_press(Message::SetDependencyKind(kind));
        kinds = kinds.push(chip);
    }
    let identifier = |issue: &Issue| match team {
        Some(tm) => format!("{}-{}", tm.identifier, issue.team_number.unwrap_or(0)),
        None => format!("#{}", issue.team_number.unwrap_or(0)),
    };

    let needle = state.query.trim().to_lowercase();
    let mut list = column![].spacing(2);
    let mut shown = 0;
    for issue in issues {
        if excluded.contains(issue.id.as_str()) {
            continue;
        }
        let ident = identifier(issue);
        if !needle.is_empty()
            && !ident.to_lowercase().contains(&needle)
            && !issue.title.to_lowercase().contains(&needle)
        {
            continue;
        }
        shown += 1;
        list = list.push(
            button(
                row![]
                    .spacing(10)
                    .push(
                        text(ident)
                            .size(12)
                            .color(t::text_muted())
                            .width(Length::Fixed(90.0)),
                    )
                    .push(text(&issue.title).size(13).color(t::text_primary())),
            )
            .on_press(Message::DependencyPickerSelected(issue.id.clone()))
            .padding([6, 8])
            .width(Length::Fill)
            .style(button::text),
        );
    }

    let results: Element<'a, Message> = if shown == 0 {
        text("No matching issues")
            .size(13)
            .color(t::text_muted())
            .into()
    } else {
        scrollable(list).height(Length::Fixed(320.0)).into()
    };

    let content = column![]
        .spacing(14)
        .width(Length::Fixed(440.0))
        .push(
            text(match state.mode {
                PickerMode::Dependency => "Add link",
                PickerMode::Parent => "Set parent issue",
                PickerMode::Child => "Add existing sub-issue",
            })
            .size(18)
            .color(t::text_primary()),
        )
        .push_maybe(is_dep.then_some(kinds))
        .push_maybe(is_dep.then(|| {
            text("Tab / Shift+Tab to change kind").size(11).color(t::text_muted())
        }))
        .push(
            crate::undo_input::text_input("dependency-picker-search", "Search issues…", &state.query, Message::DependencyPickerSearchChanged)
                .padding(8)
                .size(13)
                .width(Length::Fill),
        )
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

    crate::views::util::modal_backdrop(modal)
        .into()
}

/// Ids a hierarchy picker must not offer: everything but the spec's
/// candidates for `open_id` (`Hierarchy::parent_candidates` for Parent,
/// `child_candidates` for Child). Empty for the dependency picker, which
/// has its own exclusions, and an unknown `open_id` excludes everything.
pub fn hierarchy_excluded<'a>(
    mode: PickerMode,
    open_id: &str,
    issues: &'a [Issue],
    lanes: &[crate::db::models::Lane],
) -> HashSet<&'a str> {
    if mode == PickerMode::Dependency {
        return HashSet::new();
    }
    let hierarchy = crate::hierarchy::Hierarchy::new(issues, lanes);
    let ok: HashSet<&str> = match issues.iter().find(|i| i.id == open_id) {
        Some(open) if mode == PickerMode::Parent => hierarchy.parent_candidates(open),
        Some(open) => hierarchy.child_candidates(open),
        None => Vec::new(),
    }
    .into_iter()
    .map(|i| i.id.as_str())
    .collect();
    issues.iter().map(|i| i.id.as_str()).filter(|id| !ok.contains(id)).collect()
}

/// The parent change picking `picked` makes in `mode` while `open_id` is
/// open: `(child, parent)`. Parent mode sets the open issue's parent; Child
/// mode makes the pick a sub-issue of the open issue. `None` for the
/// dependency picker, which adds a link instead.
pub fn picked_parent_change<'a>(
    mode: PickerMode,
    open_id: &'a str,
    picked: &'a str,
) -> Option<(&'a str, &'a str)> {
    match mode {
        PickerMode::Parent => Some((open_id, picked)),
        PickerMode::Child => Some((picked, open_id)),
        PickerMode::Dependency => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iss(id: &str, team: &str, parent: Option<&str>, canceled: bool) -> Issue {
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
            canceled_at: canceled.then(|| "2026-01-01T00:00:00.000Z".to_string()),
            lane_id: "l1".into(),
            team_id: team.into(),
            project_id: None,
            parent_issue_id: parent.map(String::from),
            assignee_id: None,
            creator_id: None,
            remote_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    /// a > b > c (c is b's child, b is a's), x unrelated, y canceled, z in another team.
    fn issues() -> Vec<Issue> {
        vec![
            iss("a", "t", None, false),
            iss("b", "t", Some("a"), false),
            iss("c", "t", Some("b"), false),
            iss("x", "t", None, false),
            iss("y", "t", None, true),
            iss("z", "u", None, false),
        ]
    }

    fn sorted(set: HashSet<&str>) -> Vec<&str> {
        let mut v: Vec<&str> = set.into_iter().collect();
        v.sort();
        v
    }

    #[test]
    fn parent_picker_hides_self_descendants_current_parent_canceled_and_other_teams() {
        let issues = issues();
        let ex = hierarchy_excluded(PickerMode::Parent, "b", &issues, &[]);
        // b itself, its child c, its parent a, canceled y, other-team z.
        assert_eq!(sorted(ex), vec!["a", "b", "c", "y", "z"]);
    }

    #[test]
    fn child_picker_hides_self_ancestors_current_children_canceled_and_other_teams() {
        let issues = issues();
        let ex = hierarchy_excluded(PickerMode::Child, "b", &issues, &[]);
        // b itself, its ancestor a, its child c, canceled y, other-team z.
        assert_eq!(sorted(ex), vec!["a", "b", "c", "y", "z"]);
        let ex = hierarchy_excluded(PickerMode::Child, "x", &issues, &[]);
        assert_eq!(sorted(ex), vec!["x", "y", "z"]);
    }

    #[test]
    fn dependency_mode_adds_no_hierarchy_exclusions_and_unknown_issue_excludes_all() {
        let issues = issues();
        assert!(hierarchy_excluded(PickerMode::Dependency, "b", &issues, &[]).is_empty());
        assert_eq!(hierarchy_excluded(PickerMode::Parent, "nope", &issues, &[]).len(), issues.len());
    }

    #[test]
    fn picking_sets_the_open_issues_parent_or_adopts_the_pick() {
        assert_eq!(picked_parent_change(PickerMode::Parent, "open", "p"), Some(("open", "p")));
        assert_eq!(picked_parent_change(PickerMode::Child, "open", "k"), Some(("k", "open")));
        assert_eq!(picked_parent_change(PickerMode::Dependency, "open", "k"), None);
    }
}
