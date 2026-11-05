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
use crate::views::{board, inbox, issue_list, projects, recurring_issues, roadmap};

/// Everything remembered about one parked Tab.
#[derive(Debug, Clone, PartialEq)]
pub struct ParkedTab {
    pub view: View,
    pub selected_team_id: Option<String>,
    /// Last reported (offset, viewport) per scrollable, see `scroll_keys`.
    pub scroll_state: HashMap<usize, (f32, f32)>,
    /// View search text and the view it was typed in.
    pub search_query: String,
    pub search_view: View,
}

impl ParkedTab {
    /// Park `view` and the selected team. An open issue or project detail
    /// parks as the team's Board: its buffers aren't parked yet (a later
    /// ticket adds `issue_detail`/`project_detail`), so the detail couldn't
    /// render on return.
    pub fn new(view: View, selected_team_id: Option<String>) -> Self {
        let view = match view {
            View::IssueDetail(_) | View::ProjectDetail(_) => View::Board,
            other => other,
        };
        Self {
            view,
            selected_team_id,
            scroll_state: HashMap::new(),
            search_query: String::new(),
            search_view: View::Board,
        }
    }
}

/// The `scroll_state` keys of the scrollables `view` shows. Widgets keep
/// their scroll offset across Tabs when the tree shape matches, so a switch
/// must scroll every one of them explicitly, to the parked offset or to 0.
/// `list_mode` is the board's List/Board display; `lanes` the column count.
pub fn scroll_keys(view: &View, list_mode: bool, lanes: usize) -> Vec<usize> {
    match view {
        View::Board | View::MyIssues if list_mode => vec![issue_list::SCROLL_KEY],
        View::Board | View::MyIssues => std::iter::once(board::BOARD_SCROLL_KEY)
            .chain(0..lanes)
            .collect(),
        View::Dashboard(_) => vec![issue_list::SCROLL_KEY],
        View::Inbox => vec![inbox::SCROLL_KEY],
        View::ProjectList => vec![projects::SCROLL_KEY],
        View::Roadmap => vec![roadmap::SCROLL_KEY],
        View::RecurringIssues => vec![recurring_issues::SCROLL_KEY],
        // Detail views don't report their offset; they park as the Board.
        View::IssueDetail(_) | View::ProjectDetail(_) => Vec::new(),
    }
}

/// The offset to scroll each key to: the parked one, or the top.
pub fn restore_offsets(
    keys: &[usize],
    state: &HashMap<usize, (f32, f32)>,
) -> Vec<(usize, f32)> {
    keys.iter()
        .map(|k| (*k, state.get(k).map_or(0.0, |(offset, _)| *offset)))
        .collect()
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

/// Focus the team's Tab, opening it if needed. A team never gets a second Tab.
pub fn focus_team(set: &mut TabSet, team_id: &str) -> Switch {
    let from = set.active().clone();
    let to = TabKey::Team(team_id.to_string());
    if from == to {
        return Switch::Stay;
    }
    set.open(to.clone());
    Switch::Moved { from, to }
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
    fn details_park_as_the_board() {
        let tab = ParkedTab::new(View::IssueDetail("i".into()), Some("a".into()));
        assert_eq!(tab.view, View::Board);
        let tab = ParkedTab::new(View::ProjectDetail("p".into()), None);
        assert_eq!(tab.view, View::Board);
        assert_eq!(ParkedTab::new(View::RecurringIssues, None).view, View::RecurringIssues);
    }

    #[test]
    fn scroll_keys_cover_every_view_a_tab_can_show() {
        let board = scroll_keys(&View::Board, false, 3);
        assert_eq!(board, vec![board::BOARD_SCROLL_KEY, 0, 1, 2]);
        assert_eq!(scroll_keys(&View::Board, true, 3), vec![issue_list::SCROLL_KEY]);
        assert_eq!(scroll_keys(&View::MyIssues, true, 3), vec![issue_list::SCROLL_KEY]);
        assert_eq!(scroll_keys(&View::ProjectList, false, 3), vec![projects::SCROLL_KEY]);
        assert_eq!(scroll_keys(&View::Roadmap, false, 3), vec![roadmap::SCROLL_KEY]);
        assert_eq!(
            scroll_keys(&View::RecurringIssues, false, 3),
            vec![recurring_issues::SCROLL_KEY]
        );
        assert_eq!(scroll_keys(&View::Inbox, false, 3), vec![inbox::SCROLL_KEY]);
        assert_eq!(
            scroll_keys(&View::Dashboard("d".into()), false, 3),
            vec![issue_list::SCROLL_KEY]
        );
        let mut all: Vec<usize> = [
            board::BOARD_SCROLL_KEY,
            issue_list::SCROLL_KEY,
            inbox::SCROLL_KEY,
            projects::SCROLL_KEY,
            roadmap::SCROLL_KEY,
            recurring_issues::SCROLL_KEY,
        ]
        .to_vec();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 6, "keys are distinct");
    }

    #[test]
    fn restore_offsets_use_the_parked_offset_or_the_top() {
        let mut state = HashMap::new();
        state.insert(1, (120.0, 500.0));
        assert_eq!(restore_offsets(&[0, 1], &state), vec![(0, 0.0), (1, 120.0)]);
    }

    #[test]
    fn scroll_state_stays_separate_between_tabs() {
        let mut parked = Parked::default();
        let mut a = ParkedTab::new(View::Board, Some("a".into()));
        a.scroll_state.insert(0, (300.0, 500.0));
        parked.save(team("a"), a);
        parked.save(team("b"), ParkedTab::new(View::Board, Some("b".into())));
        assert!(parked.take(&team("b")).unwrap().scroll_state.is_empty());
        assert_eq!(parked.take(&team("a")).unwrap().scroll_state[&0], (300.0, 500.0));
    }

    #[test]
    fn view_search_stays_with_its_tab() {
        let mut parked = Parked::default();
        let mut a = ParkedTab::new(View::Board, Some("a".into()));
        a.search_query = "login".into();
        parked.save(team("a"), a);
        parked.save(team("b"), ParkedTab::new(View::Board, Some("b".into())));
        assert_eq!(parked.take(&team("b")).unwrap().search_query, "");
        let a = parked.take(&team("a")).unwrap();
        assert_eq!((a.search_query.as_str(), a.search_view), ("login", View::Board));
    }
}
