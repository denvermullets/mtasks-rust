//! Parked per-Tab state and the pure switching rules (ADR 0001). `App` keeps
//! the active Tab's state in its own fields; every other Tab's state sits here
//! until that Tab is focused again. No Iced types and no database.
//!
//! To add a per-Tab field: add it to [`ParkedTab`], then fill it in
//! `App::stash_active_tab` and apply it in `App::restore_tab`
//! (`src/app/tab_update.rs`). Those two functions are the only places.

use std::collections::{HashMap, HashSet};

use crate::app::View;
use crate::db::attachments::Attachment;
use crate::saved_view::OpenView;
use crate::tabs::{Page, TabKey, TabSet};
use crate::views::{board, inbox, issue_list, projects, recurring_issues, roadmap};

/// The comment or reply being typed on an issue or project detail, and its
/// staged files. Parked with the Tab; closing the Tab discards it, and it isn't
/// restored on relaunch (the spec's "comment draft" rule).
#[derive(Debug, Clone, PartialEq)]
pub struct CommentDraft {
    /// The detail page the draft belongs to.
    pub owner: View,
    pub comment: String,
    pub reply_to: Option<String>,
    pub reply: String,
    /// Ids the comment and the open reply will be inserted with; they own the
    /// staged files and pasted images.
    pub comment_id: String,
    pub reply_id: String,
    pub files: Vec<Attachment>,
}

impl CommentDraft {
    /// Whether there's anything worth parking.
    pub fn has_content(&self) -> bool {
        !self.comment.is_empty()
            || !self.reply.is_empty()
            || self.reply_to.is_some()
            || !self.files.is_empty()
    }

    /// The ids whose unsent staged files go when the draft is discarded.
    pub fn owner_ids(&self) -> [String; 2] {
        [self.comment_id.clone(), self.reply_id.clone()]
    }
}

/// Everything remembered about one parked Tab.
#[derive(Debug, Clone, PartialEq)]
pub struct ParkedTab {
    pub view: View,
    pub selected_team_id: Option<String>,
    pub comment_draft: Option<CommentDraft>,
    /// Last reported (offset, viewport) per scrollable, see `scroll_keys`.
    pub scroll_state: HashMap<usize, (f32, f32)>,
    /// View search text and the view it was typed in.
    pub search_query: String,
    pub search_view: View,
    pub focused_lane: usize,
    pub focused_issue: usize,
    pub collapsed_sections: HashSet<String>,
    /// The Saved view open in this Tab with any unsaved changes (a Modified
    /// view). Switching Tabs keeps it; leaving the view inside the Tab drops
    /// it as before.
    pub open_view: Option<OpenView>,
}

impl ParkedTab {
    /// Park `view` and the selected team. The caller maps an open detail to
    /// its Tab's root first (see [`parked_view`]): detail buffers aren't
    /// parked yet, so a detail couldn't render on return.
    pub fn new(view: View, selected_team_id: Option<String>) -> Self {
        Self {
            view,
            selected_team_id,
            comment_draft: None,
            scroll_state: HashMap::new(),
            search_query: String::new(),
            search_view: View::Board,
            focused_lane: 0,
            focused_issue: 0,
            collapsed_sections: HashSet::new(),
            open_view: None,
        }
    }

    /// Park a comment draft with the Tab. Its detail page parks too (`live_view`
    /// is the view the draft was typed on), so the draft has somewhere to
    /// return to. An empty draft isn't parked.
    pub fn with_comment_draft(mut self, live_view: View, draft: Option<CommentDraft>) -> Self {
        if let Some(draft) = draft.filter(|d| d.has_content() && d.owner == live_view) {
            self.view = live_view;
            self.comment_draft = Some(draft);
        }
        self
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

    /// A parked Tab's state, without taking it.
    pub fn get(&self, key: &TabKey) -> Option<&ParkedTab> {
        self.0.get(key)
    }

    /// Forget a closed Tab's state. Returns it so the caller can discard what
    /// it owned, such as a comment draft's staged files.
    pub fn forget(&mut self, key: &TabKey) -> Option<ParkedTab> {
        self.0.remove(key)
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

/// The page to store for a Tab so a relaunch can return to it: a team-level
/// page, or the Tab's open Saved view on its board. An issue or project
/// detail, and every page of a project or cross-team Tab, is the root (a
/// team's Board, or the project or page itself).
pub fn stored_page(key: &TabKey, view: &View, open_view: Option<&OpenView>) -> Page {
    if !matches!(key, TabKey::Team(_)) {
        return Page::Root;
    }
    match view {
        View::Board => open_view.map_or(Page::Board, |v| Page::SavedView(v.view_id.clone())),
        View::ProjectList => Page::Projects,
        View::Roadmap => Page::Roadmap,
        View::RecurringIssues => Page::Recurring,
        _ => Page::Board,
    }
}

/// The view a relaunched Tab opens on for its stored `page`. A Saved view
/// opens on the board; the caller opens the view itself.
pub fn page_view(key: &TabKey, page: &Page) -> View {
    if !matches!(key, TabKey::Team(_)) {
        return root_view(key);
    }
    match page {
        Page::Projects => View::ProjectList,
        Page::Roadmap => View::Roadmap,
        Page::Recurring => View::RecurringIssues,
        Page::Root | Page::Board | Page::SavedView(_) => View::Board,
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

    #[test]
    fn focus_and_collapsed_sections_stay_with_their_tab() {
        let mut parked = Parked::default();
        let mut a = ParkedTab::new(View::Board, Some("a".into()));
        a.focused_lane = 2;
        a.focused_issue = 5;
        a.collapsed_sections.insert("Done".into());
        parked.save(team("a"), a);
        parked.save(team("b"), ParkedTab::new(View::Board, Some("b".into())));
        let b = parked.take(&team("b")).unwrap();
        assert_eq!((b.focused_lane, b.focused_issue), (0, 0));
        assert!(b.collapsed_sections.is_empty());
        let a = parked.take(&team("a")).unwrap();
        assert_eq!((a.focused_lane, a.focused_issue), (2, 5));
        assert!(a.collapsed_sections.contains("Done"));
    }

    #[test]
    fn a_modified_view_parks_with_its_tab() {
        use crate::saved_view::{BoardState, OpenView, RawQuery};
        let mut modified = BoardState::default();
        modified.search = "changed".into();
        let open = OpenView {
            view_id: "v".into(),
            team_id: "a".into(),
            name: "Mine".into(),
            state: modified,
            raw: RawQuery::new(),
            baseline: BoardState::default(),
        };
        let mut parked = Parked::default();
        let mut a = ParkedTab::new(View::Board, Some("a".into()));
        a.open_view = Some(open.clone());
        parked.save(team("a"), a);
        parked.save(team("b"), ParkedTab::new(View::Board, Some("b".into())));
        assert!(parked.take(&team("b")).unwrap().open_view.is_none());
        assert_eq!(parked.take(&team("a")).unwrap().open_view, Some(open));
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

    fn draft(owner: View, comment: &str) -> CommentDraft {
        CommentDraft {
            owner,
            comment: comment.into(),
            reply_to: None,
            reply: String::new(),
            comment_id: "c".into(),
            reply_id: "r".into(),
            files: Vec::new(),
        }
    }

    #[test]
    fn a_comment_draft_parks_with_its_detail_page() {
        let view = View::IssueDetail("i".into());
        let tab = ParkedTab::new(view.clone(), Some("a".into()))
            .with_comment_draft(view.clone(), Some(draft(view.clone(), "half a thought")));
        assert_eq!(tab.view, view);
        assert_eq!(tab.comment_draft.unwrap().comment, "half a thought");
    }

    #[test]
    fn a_draft_beats_parked_view_for_every_tab_kind() {
        let detail = View::IssueDetail("i".into());
        for key in [team("a"), TabKey::Project("p".into()), TabKey::MyIssues, TabKey::Inbox, TabKey::Dashboard("d".into())] {
            let tab = ParkedTab::new(parked_view(&key, detail.clone()), None)
                .with_comment_draft(detail.clone(), Some(draft(detail.clone(), "x")));
            assert_eq!(tab.view, detail);
            assert!(tab.comment_draft.is_some());
        }
    }

    #[test]
    fn an_empty_draft_is_not_parked() {
        let view = View::IssueDetail("i".into());
        // The caller maps the detail to its Tab's root first (`parked_view`).
        let tab = ParkedTab::new(parked_view(&team("a"), view.clone()), None)
            .with_comment_draft(view.clone(), Some(draft(view, "")));
        assert_eq!(tab.view, View::Board);
        assert!(tab.comment_draft.is_none());
    }

    #[test]
    fn staged_files_alone_or_an_open_reply_make_a_draft() {
        let view = View::ProjectDetail("p".into());
        let mut d = draft(view.clone(), "");
        d.reply_to = Some("x".into());
        assert!(d.has_content());
        let mut d = draft(view, "");
        d.reply = "re".into();
        assert!(d.has_content());
    }

    #[test]
    fn a_draft_for_another_page_is_not_parked() {
        let tab = ParkedTab::new(View::Board, None).with_comment_draft(
            View::Board,
            Some(draft(View::IssueDetail("i".into()), "x")),
        );
        assert!(tab.comment_draft.is_none());
    }

    #[test]
    fn drafts_stay_in_their_own_tab_and_closing_hands_back_its_ids() {
        let view = View::IssueDetail("i".into());
        let mut parked = Parked::default();
        parked.save(
            team("a"),
            ParkedTab::new(view.clone(), None).with_comment_draft(view.clone(), Some(draft(view, "x"))),
        );
        parked.save(team("b"), ParkedTab::new(View::Board, None));
        assert!(parked.take(&team("b")).unwrap().comment_draft.is_none());
        let gone = parked.forget(&team("a")).unwrap().comment_draft.unwrap();
        assert_eq!(gone.owner_ids(), ["c".to_string(), "r".to_string()]);
        assert!(parked.take(&team("a")).is_none());
    }

    fn saved_view(id: &str) -> crate::saved_view::OpenView {
        use crate::saved_view::{BoardState, OpenView, RawQuery};
        OpenView {
            view_id: id.into(),
            team_id: "a".into(),
            name: "Mine".into(),
            state: BoardState::default(),
            raw: RawQuery::new(),
            baseline: BoardState::default(),
        }
    }

    #[test]
    fn each_team_level_page_is_stored_and_comes_back() {
        for (view, page) in [
            (View::Board, Page::Board),
            (View::ProjectList, Page::Projects),
            (View::Roadmap, Page::Roadmap),
            (View::RecurringIssues, Page::Recurring),
        ] {
            assert_eq!(stored_page(&team("a"), &view, None), page);
            assert_eq!(page_view(&team("a"), &page), view);
        }
    }

    #[test]
    fn an_open_saved_view_is_stored_and_comes_back_on_the_board() {
        let open = saved_view("v1");
        let page = stored_page(&team("a"), &View::Board, Some(&open));
        assert_eq!(page, Page::SavedView("v1".into()));
        assert_eq!(page_view(&team("a"), &page), View::Board);
    }

    #[test]
    fn an_issue_or_project_detail_in_a_team_tab_reopens_as_the_board() {
        for view in [View::IssueDetail("i".into()), View::ProjectDetail("p".into())] {
            assert_eq!(stored_page(&team("a"), &view, None), Page::Board);
        }
        let open = saved_view("v1");
        assert_eq!(
            stored_page(&team("a"), &View::IssueDetail("i".into()), Some(&open)),
            Page::Board
        );
    }

    #[test]
    fn project_and_cross_team_tabs_always_return_to_their_root() {
        let project = TabKey::Project("p".into());
        assert_eq!(stored_page(&project, &View::IssueDetail("i".into()), None), Page::Root);
        assert_eq!(page_view(&project, &Page::Roadmap), View::ProjectDetail("p".into()));
        for key in [TabKey::MyIssues, TabKey::Inbox, TabKey::Dashboard("d".into())] {
            assert_eq!(stored_page(&key, &View::Board, None), Page::Root);
            assert_eq!(page_view(&key, &Page::Board), root_view(&key));
        }
    }

    #[test]
    fn a_team_tab_stored_as_the_root_opens_on_the_board() {
        assert_eq!(page_view(&team("a"), &Page::Root), View::Board);
    }

    #[test]
    fn pages_survive_their_stored_text_and_unknown_text_is_the_root() {
        for page in [
            Page::Root,
            Page::Board,
            Page::Projects,
            Page::Roadmap,
            Page::Recurring,
            Page::SavedView("v:1".into()),
        ] {
            assert_eq!(Page::decode(page.encode().as_deref()), page);
        }
        assert_eq!(Page::decode(Some("hologram")), Page::Root);
        assert_eq!(Page::decode(Some("view:")), Page::Root);
    }
}
