//! Parked per-Tab state and the pure switching rules (ADR 0001). `App` keeps
//! the active Tab's state in its own fields; every other Tab's state sits here
//! until that Tab is focused again. No Iced types and no database.
//!
//! To add a per-Tab field: add it to [`ParkedTab`], then fill it in
//! `App::stash_active_tab` and apply it in `App::restore_tab`
//! (`src/app/tab_update.rs`). Those two functions are the only places.

use std::collections::HashMap;

use crate::app::View;
use crate::tabs::{TabKey, TabSet};

/// Everything remembered about one parked Tab.
#[derive(Debug, Clone, PartialEq)]
pub struct ParkedTab {
    pub view: View,
    pub selected_team_id: Option<String>,
}

impl ParkedTab {
    /// Park `view` and the selected team. The caller maps an open detail to
    /// its Tab's root first (see [`parked_view`]): detail buffers aren't
    /// parked yet, so a detail couldn't render on return.
    pub fn new(view: View, selected_team_id: Option<String>) -> Self {
        Self { view, selected_team_id }
    }
}

#[derive(Debug, Default)]
pub struct Parked(HashMap<TabKey, ParkedTab>);

impl Parked {
    pub fn save(&mut self, key: TabKey, tab: ParkedTab) {
        self.0.insert(key, tab);
    }

    /// Take a Tab's parked state out: it becomes the live state. `None` on a
    /// Tab's first focus.
    pub fn take(&mut self, key: &TabKey) -> Option<ParkedTab> {
        self.0.remove(key)
    }

    /// Forget a closed Tab's state.
    pub fn forget(&mut self, key: &TabKey) {
        self.0.remove(key);
    }
}

/// The outcome of asking the set to change focus. On `Moved` the caller
/// stashes the live state under `from` and restores `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Switch {
    Stay,
    Moved { from: TabKey, to: TabKey },
}

/// Focus the Tab for `to`, opening it if needed. No key ever gets a second Tab.
/// This is how every Tab kind is opened.
pub fn focus_key(set: &mut TabSet, to: TabKey) -> Switch {
    let from = set.active().clone();
    if from == to {
        return Switch::Stay;
    }
    set.open(to.clone());
    Switch::Moved { from, to }
}

/// Focus the team's Tab, opening it if needed. A team never gets a second Tab.
pub fn focus_team(set: &mut TabSet, team_id: &str) -> Switch {
    focus_key(set, TabKey::Team(team_id.to_string()))
}

/// The view a Tab parks under `key`. An open issue or project detail has no
/// buffers parked yet, so it parks as the Tab's root: the team's Board, the
/// project, or the cross-team page. A team Tab's other views park as they are.
pub fn parked_view(key: &TabKey, view: View) -> View {
    match (key, view) {
        (TabKey::Team(_), View::IssueDetail(_) | View::ProjectDetail(_)) => View::Board,
        (TabKey::Team(_), other) => other,
        // Project and cross-team Tabs have one page of their own. Whatever
        // was opened from it (an issue, which may have switched the live
        // view to the issue's team) parks as that page.
        (key, _) => root_view(key),
    }
}

/// The text a Tab shows in the bar. A team reads "IDENT Name"; a project
/// reads "Name · IDENT" with its team's identifier last, so a team and a
/// project with similar names differ at a glance; a page is its name.
/// Lookups return `(identifier, name)` for a team, `(name, team id)` for a
/// project and the name for a Dashboard; a missing row gives a plain kind
/// word, never an empty label.
pub fn label(
    key: &TabKey,
    team: impl Fn(&str) -> Option<(String, String)>,
    project: impl Fn(&str) -> Option<(String, String)>,
    dashboard: impl Fn(&str) -> Option<String>,
) -> String {
    match key {
        TabKey::Team(id) => team(id)
            .map(|(identifier, name)| format!("{identifier} {name}"))
            .unwrap_or_else(|| "Team".to_string()),
        TabKey::Project(id) => match project(id) {
            Some((name, team_id)) => match team(&team_id) {
                Some((identifier, _)) => format!("{name} \u{b7} {identifier}"),
                None => name,
            },
            None => "Project".to_string(),
        },
        TabKey::MyIssues => "My Issues".to_string(),
        TabKey::Inbox => "Inbox".to_string(),
        TabKey::Dashboard(id) => dashboard(id).unwrap_or_else(|| "Dashboard".to_string()),
    }
}

/// The Tab a view opens in when it is a cross-team page; `None` for views
/// that stay inside the current team or project Tab.
pub fn cross_team_key(view: &View) -> Option<TabKey> {
    match view {
        View::MyIssues => Some(TabKey::MyIssues),
        View::Inbox => Some(TabKey::Inbox),
        View::Dashboard(id) => Some(TabKey::Dashboard(id.clone())),
        _ => None,
    }
}

/// The view a Tab starts on, and returns to when it has nothing better.
pub fn root_view(key: &TabKey) -> View {
    match key {
        TabKey::Team(_) => View::Board,
        TabKey::Project(id) => View::ProjectDetail(id.clone()),
        TabKey::MyIssues => View::MyIssues,
        TabKey::Inbox => View::Inbox,
        TabKey::Dashboard(id) => View::Dashboard(id.clone()),
    }
}

/// Focus the Tab at `index`. Out of range, or already active, stays.
pub fn focus_index(set: &mut TabSet, index: usize) -> Switch {
    let from = set.active().clone();
    if !set.focus(index) {
        return Switch::Stay;
    }
    Switch::Moved { from, to: set.active().clone() }
}

/// The outcome of closing a Tab. `closed` is the key that went away (its
/// parked state is to be forgotten, and a comment draft with it); `to` is the
/// Tab to make live when the active Tab was the one closed, or when closing
/// the last Tab opened the Home page. `None` when the active Tab is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closed {
    pub closed: TabKey,
    pub to: Option<TabKey>,
}

/// Close the Tab at `index`. Out of range does nothing (`None`). The last Tab
/// is replaced by the Home page's Tab, so there's always one open.
pub fn close_index(set: &mut TabSet, index: usize, home: &TabKey) -> Option<Closed> {
    let closed = set.tabs().get(index)?.clone();
    let changed = set.close(index, home);
    let to = changed.then(|| set.active().clone());
    Some(Closed { closed, to })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(id: &str) -> TabKey {
        TabKey::Team(id.into())
    }

    #[test]
    fn focusing_the_active_team_stays() {
        let mut set = TabSet::new(team("a"));
        assert_eq!(focus_team(&mut set, "a"), Switch::Stay);
        assert_eq!(set.tabs().len(), 1);
    }

    #[test]
    fn a_team_never_gets_two_tabs() {
        let mut set = TabSet::new(team("a"));
        assert_eq!(
            focus_team(&mut set, "b"),
            Switch::Moved { from: team("a"), to: team("b") }
        );
        focus_team(&mut set, "a");
        focus_team(&mut set, "b");
        assert_eq!(set.tabs(), &[team("a"), team("b")]);
        assert_eq!(set.active(), &team("b"));
    }

    #[test]
    fn focus_index_moves_or_stays() {
        let mut set = TabSet::new(team("a"));
        focus_team(&mut set, "b");
        assert_eq!(
            focus_index(&mut set, 0),
            Switch::Moved { from: team("b"), to: team("a") }
        );
        assert_eq!(focus_index(&mut set, 0), Switch::Stay);
        assert_eq!(focus_index(&mut set, 9), Switch::Stay);
    }

    #[test]
    fn parked_tabs_stay_separate_and_come_back_once() {
        let mut parked = Parked::default();
        parked.save(team("a"), ParkedTab::new(View::Roadmap, Some("a".into())));
        parked.save(team("b"), ParkedTab::new(View::ProjectList, Some("b".into())));
        let a = parked.take(&team("a")).unwrap();
        assert_eq!(a.view, View::Roadmap);
        assert_eq!(a.selected_team_id.as_deref(), Some("a"));
        assert!(parked.take(&team("a")).is_none());
        assert_eq!(parked.take(&team("b")).unwrap().view, View::ProjectList);
    }

    #[test]
    fn a_detail_parks_as_its_tabs_root() {
        let detail = View::IssueDetail("i".into());
        let project = TabKey::Project("p".into());
        assert_eq!(parked_view(&team("a"), detail.clone()), View::Board);
        assert_eq!(parked_view(&team("a"), View::ProjectDetail("p".into())), View::Board);
        assert_eq!(parked_view(&project, detail.clone()), View::ProjectDetail("p".into()));
        assert_eq!(parked_view(&TabKey::MyIssues, detail.clone()), View::MyIssues);
        assert_eq!(parked_view(&TabKey::Inbox, detail.clone()), View::Inbox);
        assert_eq!(
            parked_view(&TabKey::Dashboard("d".into()), detail),
            View::Dashboard("d".into())
        );
        // Team-level pages and a project Tab's own page park as they are.
        assert_eq!(parked_view(&team("a"), View::RecurringIssues), View::RecurringIssues);
        assert_eq!(
            parked_view(&project, View::ProjectDetail("p".into())),
            View::ProjectDetail("p".into())
        );
    }

    #[test]
    fn a_cross_team_tab_parks_as_its_page_even_when_the_live_view_moved_to_a_team() {
        // Opening an issue from My Issues switches the live view to that
        // issue's team; the Tab still parks as My Issues.
        assert_eq!(parked_view(&TabKey::MyIssues, View::Board), View::MyIssues);
        assert_eq!(
            parked_view(&TabKey::Dashboard("d".into()), View::Board),
            View::Dashboard("d".into())
        );
    }

    #[test]
    fn cross_team_views_open_their_own_tab_and_team_views_do_not() {
        assert_eq!(cross_team_key(&View::MyIssues), Some(TabKey::MyIssues));
        assert_eq!(cross_team_key(&View::Inbox), Some(TabKey::Inbox));
        assert_eq!(
            cross_team_key(&View::Dashboard("d".into())),
            Some(TabKey::Dashboard("d".into()))
        );
        for view in [View::Board, View::ProjectList, View::Roadmap, View::RecurringIssues] {
            assert_eq!(cross_team_key(&view), None);
        }
    }

    fn label_of(key: &TabKey) -> String {
        label(
            key,
            |id| (id == "t").then(|| ("JAIT".to_string(), "Desktop".to_string())),
            |id| (id == "p").then(|| ("Desktop".to_string(), "t".to_string())),
            |id| (id == "d").then(|| "Weekly".to_string()),
        )
    }

    #[test]
    fn labels_say_what_each_tab_is() {
        assert_eq!(label_of(&team("t")), "JAIT Desktop");
        assert_eq!(label_of(&TabKey::Project("p".into())), "Desktop \u{b7} JAIT");
        assert_eq!(label_of(&TabKey::MyIssues), "My Issues");
        assert_eq!(label_of(&TabKey::Inbox), "Inbox");
        assert_eq!(label_of(&TabKey::Dashboard("d".into())), "Weekly");
    }

    #[test]
    fn a_team_and_a_project_with_similar_names_get_different_labels() {
        assert_ne!(label_of(&team("t")), label_of(&TabKey::Project("p".into())));
    }

    #[test]
    fn a_missing_row_still_gets_a_label() {
        assert_eq!(label_of(&team("gone")), "Team");
        assert_eq!(label_of(&TabKey::Project("gone".into())), "Project");
        assert_eq!(label_of(&TabKey::Dashboard("gone".into())), "Dashboard");
    }

    #[test]
    fn focus_key_opens_each_kind_once_and_reports_the_move() {
        let mut set = TabSet::new(team("a"));
        for key in [TabKey::Project("p".into()), TabKey::MyIssues, TabKey::Inbox, TabKey::Dashboard("d".into())] {
            let from = set.active().clone();
            assert_eq!(focus_key(&mut set, key.clone()), Switch::Moved { from, to: key.clone() });
            assert_eq!(focus_key(&mut set, key), Switch::Stay);
        }
        assert_eq!(set.tabs().len(), 5);
        assert_eq!(
            focus_key(&mut set, TabKey::MyIssues),
            Switch::Moved { from: TabKey::Dashboard("d".into()), to: TabKey::MyIssues }
        );
        assert_eq!(set.tabs().len(), 5);
    }

    #[test]
    fn closing_a_background_tab_keeps_the_active_one() {
        let mut set = TabSet::new(team("a"));
        focus_team(&mut set, "b");
        focus_team(&mut set, "c");
        let out = close_index(&mut set, 0, &team("a")).unwrap();
        assert_eq!(out, Closed { closed: team("a"), to: None });
        assert_eq!(set.tabs(), &[team("b"), team("c")]);
        assert_eq!(set.active(), &team("c"));
    }

    #[test]
    fn closing_the_active_tab_focuses_a_neighbour() {
        let mut set = TabSet::new(team("a"));
        focus_team(&mut set, "b");
        focus_team(&mut set, "c");
        focus_index(&mut set, 1);
        let out = close_index(&mut set, 1, &team("a")).unwrap();
        assert_eq!(out, Closed { closed: team("b"), to: Some(team("c")) });
        let out = close_index(&mut set, 1, &team("a")).unwrap();
        assert_eq!(out.to, Some(team("a")));
    }

    #[test]
    fn closing_the_last_tab_opens_home() {
        let mut set = TabSet::new(team("a"));
        let out = close_index(&mut set, 0, &team("h")).unwrap();
        assert_eq!(out, Closed { closed: team("a"), to: Some(team("h")) });
        assert_eq!(set.tabs(), &[team("h")]);
    }

    #[test]
    fn closing_out_of_range_does_nothing() {
        let mut set = TabSet::new(team("a"));
        assert_eq!(close_index(&mut set, 3, &team("h")), None);
        assert_eq!(set.tabs(), &[team("a")]);
    }

    #[test]
    fn a_team_row_click_returns_to_the_page_the_tab_was_left_on() {
        let mut set = TabSet::new(team("a"));
        let mut parked = Parked::default();
        // Leave team a on Roadmap for team b, as a sidebar row click does.
        let Switch::Moved { from, to } = focus_team(&mut set, "b") else { panic!() };
        parked.save(from, ParkedTab::new(View::Roadmap, Some("a".into())));
        assert!(parked.take(&to).is_none()); // b opens fresh, on its Board
        // Click a's row again: a's Tab is focused, not duplicated.
        let Switch::Moved { from, to } = focus_team(&mut set, "a") else { panic!() };
        parked.save(from, ParkedTab::new(View::Board, Some("b".into())));
        assert_eq!(parked.take(&to).unwrap().view, View::Roadmap);
        assert_eq!(set.tabs().len(), 2);
    }

    #[test]
    fn a_sub_row_of_the_current_team_adds_no_tab() {
        let mut set = TabSet::new(team("a"));
        assert_eq!(focus_team(&mut set, "a"), Switch::Stay);
        assert_eq!(set.tabs().len(), 1);
        // A sub-row of another team switches to (or opens) that team's Tab.
        assert_eq!(
            focus_team(&mut set, "b"),
            Switch::Moved { from: team("a"), to: team("b") }
        );
    }
}
