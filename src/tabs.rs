//! The rules for the set of open Tabs: identity and uniqueness, open or
//! focus, close and neighbour choice, and the Home page fallback when the
//! last Tab closes. Pure: no Iced types, no database. Callers pass in the
//! facts a rule needs (the Home page's key). See ADR 0001 for how `App`
//! parks the state of the Tabs that aren't active.

/// What a Tab is open on. Two Tabs never share a key, so a team, a project or
/// a cross-team page has at most one Tab. Team and project ids are local ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TabKey {
    Team(String),
    Project(String),
    MyIssues,
    Inbox,
    /// A Dashboard, by local id.
    Dashboard(String),
}

impl TabKey {
    /// A cross-team page (My Issues, Inbox, a Dashboard). Issues opened from
    /// one stay in that Tab; a team or project Tab only shows its own team.
    pub fn is_cross_team(&self) -> bool {
        matches!(self, TabKey::MyIssues | TabKey::Inbox | TabKey::Dashboard(_))
    }
}

/// Where an issue opens from the active Tab. `tab_team` is the team a team or
/// project Tab shows. `None` means the issue opens in the active Tab: it is
/// from the Tab's own team, or the Tab is cross-team (My Issues, Inbox, a
/// Dashboard) and loads the issue's team for the detail. `Some` is the team
/// Tab to focus or open, so team and project Tabs only ever show their own team.
pub fn issue_open_target(
    active: &TabKey,
    tab_team: Option<&str>,
    issue_team: &str,
) -> Option<TabKey> {
    match active {
        TabKey::Team(_) | TabKey::Project(_) => match tab_team {
            Some(team) if team != issue_team => Some(TabKey::Team(issue_team.to_string())),
            _ => None,
        },
        TabKey::MyIssues | TabKey::Inbox | TabKey::Dashboard(_) => None,
    }
}

/// The page a relaunched Tab returns to. Only a team-level page or the Tab's
/// open Saved view comes back; anything else is the Tab's root. Scroll
/// position, View search, Modified view changes, comment drafts, focus and
/// selection are never stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Page {
    /// The Tab's root: a team's Board, a project, or a cross-team page.
    Root,
    Board,
    Projects,
    Roadmap,
    Recurring,
    /// A Saved view, by local id.
    SavedView(String),
}

impl Page {
    /// The stored text; `None` for the root.
    pub fn encode(&self) -> Option<String> {
        match self {
            Page::Root => None,
            Page::Board => Some("board".into()),
            Page::Projects => Some("projects".into()),
            Page::Roadmap => Some("roadmap".into()),
            Page::Recurring => Some("recurring".into()),
            Page::SavedView(id) => Some(format!("view:{id}")),
        }
    }

    /// Read stored text back; anything unknown is the root.
    pub fn decode(text: Option<&str>) -> Page {
        match text {
            Some("board") => Page::Board,
            Some("projects") => Page::Projects,
            Some("roadmap") => Page::Roadmap,
            Some("recurring") => Page::Recurring,
            Some(t) => t
                .strip_prefix("view:")
                .filter(|id| !id.is_empty())
                .map_or(Page::Root, |id| Page::SavedView(id.to_string())),
            None => Page::Root,
        }
    }
}

/// The open Tabs in display order, and which one is active. There is always
/// at least one Tab and the active index always points at one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabSet {
    tabs: Vec<TabKey>,
    active: usize,
    /// Team ids in the order their Tabs were last active, most recent last.
    /// May name teams whose Tab has since closed; readers skip those.
    recent_teams: Vec<String>,
}

impl TabSet {
    /// A set with one Tab, which is active.
    pub fn new(first: TabKey) -> Self {
        let mut set = Self { tabs: vec![first], active: 0, recent_teams: Vec::new() };
        set.note_active();
        set
    }

    /// Record the active Tab as the most recently active team Tab, if it is one.
    fn note_active(&mut self) {
        if let TabKey::Team(id) = &self.tabs[self.active] {
            let id = id.clone();
            self.recent_teams.retain(|t| *t != id);
            self.recent_teams.push(id);
        }
    }

    /// The team whose Tab was most recently active among the open Tabs.
    pub fn recent_team(&self) -> Option<&str> {
        self.recent_teams
            .iter()
            .rev()
            .find(|id| self.tabs.contains(&TabKey::Team((*id).clone())))
            .map(String::as_str)
    }

    /// The team a new issue defaults to from a cross-team Tab: the most
    /// recently active team Tab's team, else `first_team` (the first team).
    pub fn new_issue_team<'a>(&'a self, first_team: Option<&'a str>) -> Option<&'a str> {
        self.recent_team().or(first_team)
    }

    /// Rebuild a set from stored parts: duplicates are dropped (first wins),
    /// an empty list becomes the Home page's Tab, and an out-of-range or
    /// dropped active index is clamped to a valid Tab. `active` is an index
    /// into `tabs` as given.
    pub fn from_parts(tabs: Vec<TabKey>, active: usize, home: &TabKey) -> Self {
        let active_key = tabs.get(active).cloned();
        let mut kept: Vec<TabKey> = Vec::with_capacity(tabs.len());
        for key in tabs {
            if !kept.contains(&key) {
                kept.push(key);
            }
        }
        if kept.is_empty() {
            return Self::new(home.clone());
        }
        let active = active_key
            .and_then(|key| kept.iter().position(|k| *k == key))
            .unwrap_or(0);
        // Without history, the first team Tab stands in for the last active one.
        let first_team = kept.iter().find_map(|k| match k {
            TabKey::Team(id) => Some(id.clone()),
            _ => None,
        });
        let mut set = Self { tabs: kept, active, recent_teams: first_team.into_iter().collect() };
        set.note_active();
        set
    }

    pub fn tabs(&self) -> &[TabKey] {
        &self.tabs
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> &TabKey {
        &self.tabs[self.active]
    }

    /// Focus the Tab at `index`. Out of range does nothing; returns whether
    /// the active Tab changed.
    pub fn focus(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.active {
            return false;
        }
        self.active = index;
        self.note_active();
        true
    }

    /// The position `delta` Tabs from the active one, wrapping at either end
    /// (Cmd+Option+Left/Right).
    pub fn offset_index(&self, delta: isize) -> usize {
        let len = self.tabs.len() as isize;
        (self.active as isize + delta).rem_euclid(len) as usize
    }

    /// Move the Tab at `from` to position `to`, shifting the Tabs between.
    /// The active Tab stays the same Tab, wherever it ends up. Out of range
    /// or `from == to` does nothing; returns whether the order changed.
    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return false;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = if self.active == from {
            to
        } else if from < self.active && self.active <= to {
            self.active - 1
        } else if to <= self.active && self.active < from {
            self.active + 1
        } else {
            self.active
        };
        true
    }

    /// Open `key`, or focus its Tab if it's already open. A new Tab goes at
    /// the end and becomes active. Returns the index of the focused Tab.
    pub fn open(&mut self, key: TabKey) -> usize {
        match self.tabs.iter().position(|k| *k == key) {
            Some(index) => self.active = index,
            None => {
                self.tabs.push(key);
                self.active = self.tabs.len() - 1;
            }
        }
        self.note_active();
        self.active
    }

    /// Bring back a closed Tab at `index` (clamped to the end) and focus it.
    /// Already open, it is only focused. Returns the index of the focused Tab.
    pub fn reopen(&mut self, key: TabKey, index: usize) -> usize {
        match self.tabs.iter().position(|k| *k == key) {
            Some(i) => self.active = i,
            None => {
                let at = index.min(self.tabs.len());
                self.tabs.insert(at, key);
                self.active = at;
            }
        }
        self.note_active();
        self.active
    }

    /// Close the Tab at `index`. Out of range does nothing. Closing the active
    /// Tab focuses its right neighbour, or the left one if it was last;
    /// closing another Tab keeps the active Tab focused. Closing the only Tab
    /// leaves a single Tab on `home`. Returns whether the active Tab is now a
    /// different one from before (so the caller must stash and restore).
    pub fn close(&mut self, index: usize, home: &TabKey) -> bool {
        if index >= self.tabs.len() {
            return false;
        }
        if self.tabs.len() == 1 {
            self.tabs[0] = home.clone();
            self.note_active();
            return true;
        }
        self.tabs.remove(index);
        if index == self.active {
            self.active = index.min(self.tabs.len() - 1);
            self.note_active();
            true
        } else {
            if index < self.active {
                self.active -= 1;
            }
            false
        }
    }

    /// The set a launch starts with. `stored` is what the last run left
    /// (`None` on a first run); Tabs whose subject is gone (`exists` false)
    /// don't come back. `home` is the Home page's Tab when it resolves: it is
    /// focused, opened if absent. With no resolvable Home page the stored
    /// set's last active Tab stays focused. `fallback` stands in when nothing
    /// is left to show, and for `home` on a first run.
    pub fn for_launch(
        stored: Option<TabSet>,
        home: Option<TabKey>,
        fallback: &TabKey,
        exists: impl Fn(&TabKey) -> bool,
    ) -> Self {
        let landing = home.clone().unwrap_or_else(|| fallback.clone());
        let mut set = stored.unwrap_or_else(|| Self::new(landing.clone()));
        set.prune(exists, |_| false, &landing);
        if let Some(home) = home {
            set.open(home);
        }
        set
    }

    /// Close every Tab whose subject a pull removed. `exists` says whether a
    /// Tab's team, project or Dashboard is still in the local mirror: an
    /// archived team still exists, so it keeps its Tab. `keep` is the escape
    /// hatch for a removed team that still has running Terminals (JRST-421):
    /// a Tab it returns true for stays open although `exists` is false.
    /// Closing follows `close`: a neighbour takes over from the active Tab,
    /// and the Home page's Tab replaces the last one.
    pub fn prune(
        &mut self,
        exists: impl Fn(&TabKey) -> bool,
        keep: impl Fn(&TabKey) -> bool,
        home: &TabKey,
    ) -> Pruned {
        let before = self.active().clone();
        let gone: Vec<TabKey> = self
            .tabs
            .iter()
            .filter(|k| !exists(k) && !keep(k))
            .cloned()
            .collect();
        for key in &gone {
            if let Some(index) = self.tabs.iter().position(|k| k == key) {
                self.close(index, home);
            }
        }
        Pruned { active_changed: !gone.is_empty() && *self.active() != before, closed: gone }
    }
}

/// What `TabSet::prune` closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pruned {
    /// The Tabs that closed, in bar order.
    pub closed: Vec<TabKey>,
    /// The active Tab is now a different one: the caller restores it.
    pub active_changed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(id: &str) -> TabKey {
        TabKey::Team(id.into())
    }

    fn home() -> TabKey {
        team("home")
    }

    #[test]
    fn offset_index_wraps_around_the_bar() {
        let mut set = TabSet::new(team("a"));
        set.open(team("b"));
        set.open(team("c"));
        set.focus(0);
        assert_eq!(set.offset_index(1), 1);
        assert_eq!(set.offset_index(-1), 2);
        set.focus(2);
        assert_eq!(set.offset_index(1), 0);
        let single = TabSet::new(team("a"));
        assert_eq!(single.offset_index(1), 0);
        assert_eq!(single.offset_index(-1), 0);
    }

    #[test]
    fn reopen_puts_a_closed_tab_back_where_it_was_and_focuses_it() {
        let mut set = TabSet::new(team("a"));
        set.open(team("b"));
        set.open(team("c"));
        set.close(1, &home());
        assert_eq!(set.reopen(team("b"), 1), 1);
        assert_eq!(set.tabs(), &[team("a"), team("b"), team("c")]);
        assert_eq!(set.active(), &team("b"));
        // Past the end goes last; an open Tab is only focused.
        assert_eq!(set.reopen(team("d"), 9), 3);
        assert_eq!(set.reopen(team("a"), 2), 0);
        assert_eq!(set.tabs().len(), 4);
    }

    #[test]
    fn an_issue_from_the_tabs_own_team_opens_in_the_tab() {
        let project = TabKey::Project("p".into());
        assert_eq!(issue_open_target(&team("a"), Some("a"), "a"), None);
        assert_eq!(issue_open_target(&project, Some("a"), "a"), None);
    }

    #[test]
    fn another_teams_issue_goes_to_that_teams_tab() {
        let project = TabKey::Project("p".into());
        assert_eq!(issue_open_target(&team("a"), Some("a"), "b"), Some(team("b")));
        assert_eq!(issue_open_target(&project, Some("a"), "b"), Some(team("b")));
    }

    #[test]
    fn an_issue_from_a_cross_team_tab_stays_in_it() {
        for key in [TabKey::MyIssues, TabKey::Inbox, TabKey::Dashboard("d".into())] {
            assert_eq!(issue_open_target(&key, None, "b"), None);
            assert_eq!(issue_open_target(&key, Some("a"), "b"), None);
        }
    }

    fn set(keys: &[TabKey], active: usize) -> TabSet {
        TabSet::from_parts(keys.to_vec(), active, &home())
    }

    #[test]
    fn moving_a_tab_keeps_the_active_tab_the_same_tab() {
        let keys = [team("a"), team("b"), team("c"), team("d")];
        for active in 0..4 {
            for from in 0..4 {
                for to in 0..4 {
                    let mut tabs = set(&keys, active);
                    let before = tabs.active().clone();
                    let changed = tabs.move_tab(from, to);
                    assert_eq!(changed, from != to);
                    assert_eq!(tabs.active(), &before, "{active} {from}->{to}");
                    assert_eq!(tabs.tabs().len(), 4);
                }
            }
        }
    }

    #[test]
    fn moving_a_tab_reorders_and_out_of_range_does_nothing() {
        let mut tabs = set(&[team("a"), team("b"), team("c")], 0);
        assert!(tabs.move_tab(0, 2));
        assert_eq!(tabs.tabs(), [team("b"), team("c"), team("a")]);
        assert_eq!(tabs.active_index(), 2);
        assert!(!tabs.move_tab(5, 0));
        assert!(!tabs.move_tab(0, 3));
        assert_eq!(tabs.tabs(), [team("b"), team("c"), team("a")]);
    }

    #[test]
    fn opening_a_new_tab_appends_and_focuses_it() {
        let mut tabs = TabSet::new(team("a"));
        assert_eq!(tabs.open(TabKey::Project("p".into())), 1);
        assert_eq!(tabs.tabs(), [team("a"), TabKey::Project("p".into())]);
        assert_eq!(tabs.active(), &TabKey::Project("p".into()));
    }

    #[test]
    fn opening_an_open_team_project_or_page_focuses_it_instead_of_adding() {
        for key in [team("a"), TabKey::Project("p".into()), TabKey::MyIssues, TabKey::Inbox, TabKey::Dashboard("d".into())] {
            let mut tabs = set(&[key.clone(), team("z")], 1);
            assert_eq!(tabs.open(key.clone()), 0);
            assert_eq!(tabs.tabs().len(), 2);
            assert_eq!(tabs.active(), &key);
        }
    }

    #[test]
    fn same_id_of_a_different_kind_is_a_different_tab() {
        let mut tabs = TabSet::new(team("x"));
        tabs.open(TabKey::Project("x".into()));
        tabs.open(TabKey::Dashboard("x".into()));
        assert_eq!(tabs.tabs().len(), 3);
    }

    #[test]
    fn closing_the_active_tab_focuses_the_right_neighbour() {
        let mut tabs = set(&[team("a"), team("b"), team("c")], 1);
        assert!(tabs.close(1, &home()));
        assert_eq!(tabs.tabs(), [team("a"), team("c")]);
        assert_eq!(tabs.active(), &team("c"));
    }

    #[test]
    fn closing_the_active_last_tab_focuses_the_left_neighbour() {
        let mut tabs = set(&[team("a"), team("b"), team("c")], 2);
        assert!(tabs.close(2, &home()));
        assert_eq!(tabs.active(), &team("b"));
    }

    #[test]
    fn closing_a_tab_before_the_active_one_keeps_the_active_tab() {
        let mut tabs = set(&[team("a"), team("b"), team("c")], 2);
        assert!(!tabs.close(0, &home()));
        assert_eq!(tabs.tabs(), [team("b"), team("c")]);
        assert_eq!(tabs.active(), &team("c"));
    }

    #[test]
    fn closing_a_tab_after_the_active_one_keeps_the_active_tab() {
        let mut tabs = set(&[team("a"), team("b"), team("c")], 0);
        assert!(!tabs.close(2, &home()));
        assert_eq!(tabs.active(), &team("a"));
    }

    #[test]
    fn closing_the_last_tab_leaves_a_home_page_tab() {
        let mut tabs = TabSet::new(TabKey::MyIssues);
        assert!(tabs.close(0, &home()));
        assert_eq!(tabs.tabs(), [home()]);
        assert_eq!(tabs.active(), &home());
    }

    #[test]
    fn closing_out_of_range_changes_nothing() {
        let mut tabs = set(&[team("a"), team("b")], 1);
        assert!(!tabs.close(5, &home()));
        assert_eq!(tabs.tabs().len(), 2);
        assert_eq!(tabs.active_index(), 1);
    }

    #[test]
    fn focus_reports_whether_the_active_tab_changed() {
        let mut tabs = set(&[team("a"), team("b")], 0);
        assert!(tabs.focus(1));
        assert!(!tabs.focus(1));
        assert!(!tabs.focus(9));
        assert_eq!(tabs.active(), &team("b"));
    }

    #[test]
    fn from_parts_drops_duplicates_and_follows_the_active_tab() {
        let tabs = set(&[team("a"), team("b"), team("a"), team("c")], 3);
        assert_eq!(tabs.tabs(), [team("a"), team("b"), team("c")]);
        assert_eq!(tabs.active(), &team("c"));
    }

    #[test]
    fn from_parts_falls_back_to_the_first_tab_or_home() {
        let tabs = set(&[team("a"), team("b")], 7);
        assert_eq!(tabs.active_index(), 0);
        let tabs = set(&[], 0);
        assert_eq!(tabs.tabs(), [home()]);
    }

    fn project(id: &str) -> TabKey {
        TabKey::Project(id.into())
    }

    #[test]
    fn a_project_never_gets_two_tabs_and_two_projects_can_be_open() {
        let mut tabs = TabSet::new(team("a"));
        tabs.open(project("p"));
        tabs.open(project("q"));
        tabs.open(project("p"));
        assert_eq!(tabs.tabs(), [team("a"), project("p"), project("q")]);
        assert_eq!(tabs.active(), &project("p"));
    }

    #[test]
    fn a_project_tab_and_its_teams_tab_coexist_and_focus_independently() {
        // The project's id never equals its team's key, whatever the ids are.
        let mut tabs = TabSet::new(team("same"));
        tabs.open(project("same"));
        assert_eq!(tabs.tabs().len(), 2);
        assert!(tabs.focus(0));
        assert_eq!(tabs.active(), &team("same"));
        assert!(tabs.focus(1));
        assert_eq!(tabs.active(), &project("same"));
    }

    #[test]
    fn only_my_issues_inbox_and_dashboards_are_cross_team() {
        assert!(TabKey::MyIssues.is_cross_team());
        assert!(TabKey::Inbox.is_cross_team());
        assert!(TabKey::Dashboard("d".into()).is_cross_team());
        assert!(!team("a").is_cross_team());
        assert!(!project("p").is_cross_team());
    }

    #[test]
    fn two_dashboards_can_be_open_but_each_only_once() {
        let mut tabs = TabSet::new(TabKey::Dashboard("d1".into()));
        tabs.open(TabKey::Dashboard("d2".into()));
        tabs.open(TabKey::Dashboard("d1".into()));
        assert_eq!(tabs.tabs().len(), 2);
    }

    #[test]
    fn new_issues_from_a_cross_team_tab_use_the_most_recent_team_tab() {
        let mut tabs = TabSet::new(team("a"));
        tabs.open(team("b"));
        tabs.open(TabKey::MyIssues);
        assert_eq!(tabs.new_issue_team(Some("first")), Some("b"));
        tabs.focus(0);
        tabs.open(TabKey::MyIssues);
        assert_eq!(tabs.new_issue_team(Some("first")), Some("a"));
    }

    #[test]
    fn project_tabs_do_not_count_as_the_recent_team_tab() {
        let mut tabs = TabSet::new(team("a"));
        tabs.open(project("p"));
        tabs.open(TabKey::Inbox);
        assert_eq!(tabs.new_issue_team(Some("first")), Some("a"));
    }

    #[test]
    fn with_no_team_tab_open_new_issues_use_the_first_team() {
        let mut tabs = TabSet::new(TabKey::MyIssues);
        assert_eq!(tabs.new_issue_team(Some("first")), Some("first"));
        assert_eq!(tabs.new_issue_team(None), None);
        tabs.open(team("a"));
        let a = tabs.tabs().iter().position(|k| *k == team("a")).unwrap();
        tabs.close(a, &TabKey::MyIssues);
        assert_eq!(tabs.new_issue_team(Some("first")), Some("first"));
    }

    fn exists_except(gone: &[TabKey]) -> impl Fn(&TabKey) -> bool + '_ {
        move |k| !gone.contains(k)
    }

    fn never(_: &TabKey) -> bool {
        false
    }

    #[test]
    fn a_removed_team_project_or_dashboard_closes_its_tab() {
        for gone in [team("b"), project("p"), TabKey::Dashboard("d".into())] {
            let mut tabs = set(&[team("a"), gone.clone(), team("c")], 0);
            let pruned = tabs.prune(exists_except(std::slice::from_ref(&gone)), never, &home());
            assert_eq!(tabs.tabs(), [team("a"), team("c")]);
            assert_eq!(pruned.closed, [gone]);
            assert!(!pruned.active_changed);
            assert_eq!(tabs.active(), &team("a"));
        }
    }

    #[test]
    fn a_removed_parked_tab_closes_while_the_active_one_stays() {
        let mut tabs = set(&[team("a"), project("p"), team("c")], 2);
        let pruned = tabs.prune(exists_except(&[project("p")]), never, &home());
        assert!(!pruned.active_changed);
        assert_eq!(tabs.active(), &team("c"));
    }

    #[test]
    fn removing_the_active_tab_focuses_a_neighbour_and_reports_the_change() {
        let mut tabs = set(&[team("a"), team("b"), team("c")], 1);
        let pruned = tabs.prune(exists_except(&[team("b")]), never, &home());
        assert!(pruned.active_changed);
        assert_eq!(tabs.active(), &team("c"));
        let mut tabs = set(&[team("a"), team("b")], 1);
        tabs.prune(exists_except(&[team("b")]), never, &home());
        assert_eq!(tabs.active(), &team("a"));
    }

    #[test]
    fn removing_a_tab_before_the_active_one_keeps_the_active_tab() {
        let mut tabs = set(&[team("a"), team("b"), team("c")], 2);
        let pruned = tabs.prune(exists_except(&[team("a")]), never, &home());
        assert!(!pruned.active_changed);
        assert_eq!(tabs.active(), &team("c"));
    }

    #[test]
    fn removing_every_tab_leaves_the_home_page() {
        let mut tabs = set(&[team("a"), project("p")], 0);
        let pruned = tabs.prune(exists_except(&[team("a"), project("p")]), never, &home());
        assert!(pruned.active_changed);
        assert_eq!(tabs.tabs(), [home()]);
        assert_eq!(tabs.active(), &home());
    }

    #[test]
    fn an_archived_team_still_exists_so_it_keeps_its_tab() {
        // Archiving doesn't remove the row, so `exists` stays true.
        let mut tabs = set(&[team("a"), team("b")], 1);
        let pruned = tabs.prune(|_| true, never, &home());
        assert!(pruned.closed.is_empty());
        assert!(!pruned.active_changed);
        assert_eq!(tabs.tabs().len(), 2);
    }

    #[test]
    fn a_tab_the_keep_rule_names_stays_although_its_team_is_gone() {
        let mut tabs = set(&[team("a"), team("b")], 1);
        let pruned = tabs.prune(exists_except(&[team("b")]), |k| *k == team("b"), &home());
        assert!(pruned.closed.is_empty());
        assert_eq!(tabs.tabs().len(), 2);
        assert_eq!(tabs.active(), &team("b"));
    }

    #[test]
    fn a_removed_teams_last_tab_stays_while_the_exception_holds_then_closes() {
        let mut tabs = TabSet::new(team("a"));
        let held = tabs.prune(|_| false, |k| *k == team("a"), &team("a"));
        assert!(held.closed.is_empty() && !held.active_changed);
        assert_eq!(tabs.tabs(), &[team("a")]);
        // The last Terminal closed: the Tab goes, and the Home page replaces it.
        let home = team("home");
        let gone = tabs.prune(exists_except(&[team("a")]), never, &home);
        assert_eq!(gone.closed, [team("a")]);
        assert_eq!(tabs.tabs(), &[home]);
    }

    #[test]
    fn a_stored_tab_of_a_removed_team_does_not_come_back_at_launch() {
        let stored = set(&[team("a"), team("gone")], 1);
        let launched = TabSet::for_launch(Some(stored), None, &home(), exists_except(&[team("gone")]));
        assert_eq!(launched.tabs(), &[team("a")]);
    }

    #[test]
    fn my_issues_and_inbox_are_untouched_by_a_prune() {
        let mut tabs = set(&[TabKey::MyIssues, TabKey::Inbox, team("a")], 0);
        tabs.prune(exists_except(&[team("a")]), never, &home());
        assert_eq!(tabs.tabs(), [TabKey::MyIssues, TabKey::Inbox]);
    }

    fn all(_: &TabKey) -> bool {
        true
    }

    #[test]
    fn launch_brings_back_the_stored_tabs_in_order() {
        let stored = set(&[team("a"), TabKey::MyIssues, project("p")], 1);
        let tabs = TabSet::for_launch(Some(stored.clone()), None, &home(), all);
        assert_eq!(tabs.tabs(), stored.tabs());
    }

    #[test]
    fn launch_focuses_the_home_page_tab() {
        let stored = set(&[team("a"), team("b")], 0);
        let tabs = TabSet::for_launch(Some(stored), Some(team("b")), &home(), all);
        assert_eq!(tabs.tabs(), [team("a"), team("b")]);
        assert_eq!(tabs.active(), &team("b"));
    }

    #[test]
    fn launch_opens_the_home_page_tab_when_absent() {
        let stored = set(&[team("a")], 0);
        let dash = TabKey::Dashboard("d".into());
        let tabs = TabSet::for_launch(Some(stored), Some(dash.clone()), &home(), all);
        assert_eq!(tabs.tabs(), [team("a"), dash.clone()]);
        assert_eq!(tabs.active(), &dash);
    }

    #[test]
    fn launch_with_an_unresolvable_home_page_keeps_the_last_active_tab() {
        let stored = set(&[team("a"), TabKey::Inbox, team("b")], 1);
        let tabs = TabSet::for_launch(Some(stored), None, &home(), all);
        assert_eq!(tabs.active(), &TabKey::Inbox);
    }

    #[test]
    fn launch_drops_tabs_removed_while_the_app_was_closed() {
        let stored = set(&[team("a"), project("p"), team("b")], 1);
        let tabs = TabSet::for_launch(Some(stored), None, &home(), exists_except(&[project("p")]));
        assert_eq!(tabs.tabs(), [team("a"), team("b")]);
        assert_eq!(tabs.active(), &team("b"));
    }

    #[test]
    fn launch_with_everything_removed_shows_the_home_page_or_fallback() {
        let stored = set(&[team("a")], 0);
        let gone = [team("a")];
        let tabs = TabSet::for_launch(Some(stored.clone()), Some(team("h")), &home(), exists_except(&gone));
        assert_eq!(tabs.tabs(), [team("h")]);
        let tabs = TabSet::for_launch(Some(stored), None, &team("fb"), exists_except(&gone));
        assert_eq!(tabs.tabs(), [team("fb")]);
    }

    #[test]
    fn a_first_launch_opens_the_home_page_or_the_fallback() {
        let tabs = TabSet::for_launch(None, Some(TabKey::Dashboard("d".into())), &home(), all);
        assert_eq!(tabs.tabs(), [TabKey::Dashboard("d".into())]);
        let tabs = TabSet::for_launch(None, None, &team("fb"), all);
        assert_eq!(tabs.tabs(), [team("fb")]);
    }

    #[test]
    fn a_relaunched_set_falls_back_to_its_first_team_tab() {
        let tabs = set(&[TabKey::MyIssues, team("a"), team("b")], 0);
        assert_eq!(tabs.new_issue_team(Some("first")), Some("a"));
    }
}
