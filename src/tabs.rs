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

/// The open Tabs in display order, and which one is active. There is always
/// at least one Tab and the active index always points at one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabSet {
    tabs: Vec<TabKey>,
    active: usize,
}

impl TabSet {
    /// A set with one Tab, which is active.
    pub fn new(first: TabKey) -> Self {
        Self { tabs: vec![first], active: 0 }
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
        Self { tabs: kept, active }
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
            return true;
        }
        self.tabs.remove(index);
        if index == self.active {
            self.active = index.min(self.tabs.len() - 1);
            true
        } else {
            if index < self.active {
                self.active -= 1;
            }
            false
        }
    }
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
}
