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
    #[allow(dead_code)] // used by the issue-opening tickets
    pub fn is_cross_team(&self) -> bool {
        matches!(self, TabKey::MyIssues | TabKey::Inbox | TabKey::Dashboard(_))
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

    fn set(keys: &[TabKey], active: usize) -> TabSet {
        TabSet::from_parts(keys.to_vec(), active, &home())
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
            let pruned = tabs.prune(exists_except(&[gone.clone()]), never, &home());
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
    fn my_issues_and_inbox_are_untouched_by_a_prune() {
        let mut tabs = set(&[TabKey::MyIssues, TabKey::Inbox, team("a")], 0);
        tabs.prune(exists_except(&[team("a")]), never, &home());
        assert_eq!(tabs.tabs(), [TabKey::MyIssues, TabKey::Inbox]);
    }

    #[test]
    fn a_relaunched_set_falls_back_to_its_first_team_tab() {
        let tabs = set(&[TabKey::MyIssues, team("a"), team("b")], 0);
        assert_eq!(tabs.new_issue_team(Some("first")), Some("a"));
    }
}
