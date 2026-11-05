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
    #[allow(dead_code)] // used by the close-Tab ticket
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

/// The Tab a view opens in when it is a cross-team page; `None` for views
/// that stay inside the current team or project Tab.
pub fn cross_team_key(view: &View) -> Option<TabKey> {
    match view {
        View::MyIssues => Some(TabKey::MyIssues),
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
        for view in [View::Board, View::ProjectList, View::Roadmap, View::RecurringIssues] {
            assert_eq!(cross_team_key(&view), None);
        }
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
}
