//! Tab messages and the one place per-Tab state is stashed and restored
//! (ADR 0001). Later Tab tickets extend `stash_active_tab` and `restore_tab`
//! (plus `ParkedTab` in `src/tab_state.rs`) and add `TabMessage` variants.

use iced::Task;

use super::{App, Message, View};
use crate::tab_state::{self, ParkedTab, Switch};
use crate::tabs::{TabKey, TabSet};

#[derive(Debug, Clone)]
pub enum TabMessage {
    /// Click a Tab in the bar, by position.
    Focus(usize),
}

impl App {
    pub(super) fn update_tab(&mut self, message: TabMessage) -> Task<Message> {
        match message {
            TabMessage::Focus(index) => {
                self.ensure_tab_set();
                if let Some(set) = self.tab_set.as_mut() {
                    let switch = tab_state::focus_index(set, index);
                    self.apply_switch(switch);
                }
            }
        }
        Task::none()
    }

    /// The set starts with the selected team's Tab once a team exists.
    pub(super) fn ensure_tab_set(&mut self) {
        if self.tab_set.is_none() {
            if let Some(id) = self.selected_team_id.clone() {
                self.tab_set = Some(TabSet::new(TabKey::Team(id)));
            }
        }
    }

    /// Focus (or open) a team's Tab, for navigation into that team.
    pub(super) fn focus_team_tab(&mut self, team_id: &str) {
        self.ensure_tab_set();
        if let Some(set) = self.tab_set.as_mut() {
            let switch = tab_state::focus_team(set, team_id);
            self.apply_switch(switch);
        }
    }

    fn apply_switch(&mut self, switch: Switch) {
        let Switch::Moved { from, to } = switch else {
            return;
        };
        // Edits commit when leaving, as they do when navigating today.
        self.commit_issue_detail();
        self.commit_project_detail();
        self.stash_dashboard_session();
        self.stash_active_tab(from);
        self.restore_tab(&to);
    }

    /// Park the live per-Tab fields under `key`. Add new per-Tab fields here.
    pub(super) fn stash_active_tab(&mut self, key: TabKey) {
        self.parked_tabs.save(
            key,
            ParkedTab::new(self.view.clone(), self.selected_team_id.clone()),
        );
    }

    /// Make `key`'s Tab live: apply its parked fields (or defaults on first
    /// focus), then reload data from the local database. Per-Tab fields that
    /// aren't parked yet are reset so they can't leak between Tabs. Add new
    /// per-Tab fields here.
    pub(super) fn restore_tab(&mut self, key: &TabKey) {
        let parked = self.parked_tabs.take(key);
        let (view, team) = match parked {
            Some(p) => (p.view, p.selected_team_id),
            None => (
                View::Board,
                match key {
                    TabKey::Team(id) => Some(id.clone()),
                    _ => self.selected_team_id.clone(),
                },
            ),
        };
        self.open_view = None;
        self.detail_from_my_issues = false;
        self.detail_from_inbox = false;
        self.detail_from_dashboard = None;
        self.issue_detail = None;
        self.project_detail = None;
        self.dashboard_menu = None;
        self.composer_focused = false;
        self.focused_lane = 0;
        self.focused_issue = 0;
        self.selected_team_id = team;
        self.view = view;
        self.reload_data();
        if self.view == View::Inbox {
            self.reload_inbox();
        }
        if self.view == View::RecurringIssues {
            let conn = self.db();
            self.reload_recurring(&conn);
        }
        self.restore_dashboard_session();
    }

    /// Bar labels in order, with whether each is the active Tab.
    pub(super) fn tab_labels(&self) -> Vec<(String, bool)> {
        let Some(set) = &self.tab_set else {
            return Vec::new();
        };
        set.tabs()
            .iter()
            .enumerate()
            .map(|(i, key)| {
                let label = match key {
                    TabKey::Team(id) => self
                        .teams
                        .iter()
                        .find(|t| t.id == *id)
                        .map(|t| format!("{} {}", t.identifier, t.name))
                        .unwrap_or_else(|| "Team".to_string()),
                    _ => String::new(), // other kinds arrive in later tickets
                };
                (label, i == set.active_index())
            })
            .collect()
    }
}
