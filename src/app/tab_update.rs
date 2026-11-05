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
                    return self.apply_switch(switch);
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
    /// The task scrolls the restored Tab's views back into place.
    pub(super) fn focus_team_tab(&mut self, team_id: &str) -> Task<Message> {
        self.ensure_tab_set();
        if let Some(set) = self.tab_set.as_mut() {
            let switch = tab_state::focus_team(set, team_id);
            return self.apply_switch(switch);
        }
        Task::none()
    }

    fn apply_switch(&mut self, switch: Switch) -> Task<Message> {
        let Switch::Moved { from, to } = switch else {
            return Task::none();
        };
        // Edits commit when leaving, as they do when navigating today.
        self.commit_issue_detail();
        self.commit_project_detail();
        self.stash_dashboard_session();
        self.stash_active_tab(from);
        self.restore_tab(&to)
    }

    /// Park the live per-Tab fields under `key`. Add new per-Tab fields here.
    pub(super) fn stash_active_tab(&mut self, key: TabKey) {
        self.parked_tabs.save(
            key,
            ParkedTab {
                scroll_state: self.scroll_state.clone(),
                search_query: self.search_query.clone(),
                search_view: self.search_view.clone(),
                focused_lane: self.focused_lane,
                focused_issue: self.focused_issue,
                collapsed_sections: self.collapsed_sections.clone(),
                open_view: self.open_view.clone(),
                ..ParkedTab::new(self.view.clone(), self.selected_team_id.clone())
            },
        );
    }

    /// Make `key`'s Tab live: apply its parked fields (or defaults on first
    /// focus), then reload data from the local database. Per-Tab fields that
    /// aren't parked yet are reset so they can't leak between Tabs. Add new
    /// per-Tab fields here.
    ///
    /// The returned task scrolls every scrollable the restored view shows to
    /// the Tab's offset (see `tab_state::scroll_keys`).
    pub(super) fn restore_tab(&mut self, key: &TabKey) -> Task<Message> {
        let parked = self.parked_tabs.take(key);
        let (view, team, scroll_state, search, focus, collapsed, open_view) = match parked {
            Some(p) => (
                p.view,
                p.selected_team_id,
                p.scroll_state,
                (p.search_query, p.search_view),
                (p.focused_lane, p.focused_issue),
                p.collapsed_sections,
                p.open_view,
            ),
            None => (
                View::Board,
                match key {
                    TabKey::Team(id) => Some(id.clone()),
                    _ => self.selected_team_id.clone(),
                },
                Default::default(),
                (String::new(), View::Board),
                (0, 0),
                Default::default(),
                None,
            ),
        };
        self.scroll_state = scroll_state;
        (self.search_query, self.search_view) = search;
        self.open_view = open_view;
        self.detail_from_my_issues = false;
        self.detail_from_inbox = false;
        self.detail_from_dashboard = None;
        self.issue_detail = None;
        self.project_detail = None;
        self.dashboard_menu = None;
        self.composer_focused = false;
        (self.focused_lane, self.focused_issue) = focus;
        self.collapsed_sections = collapsed;
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
        // Parked focus may point past rows that went away.
        if matches!(self.view, View::Board | View::MyIssues) {
            self.clamp_focus();
        }
        self.restore_dashboard_session();
        self.restore_scroll()
    }

    /// Scroll the live view's scrollables to `scroll_state`. Widgets keep
    /// their offset across Tabs, so this runs on every switch, back to 0 for a
    /// view the Tab never scrolled.
    fn restore_scroll(&self) -> Task<Message> {
        use crate::views::{board, dashboard, inbox, issue_list, projects, recurring_issues, roadmap};
        use iced::widget::scrollable::{scroll_to, AbsoluteOffset};
        let (list_mode, lanes) = match self.view {
            View::Board | View::MyIssues => (
                self.board_display().view_mode == crate::display::ViewMode::List,
                self.board_columns().len(),
            ),
            _ => (false, 0),
        };
        let keys = tab_state::scroll_keys(&self.view, list_mode, lanes);
        let tasks = tab_state::restore_offsets(&keys, &self.scroll_state)
            .into_iter()
            .map(|(k, offset)| {
                let y = AbsoluteOffset { x: 0.0, y: offset };
                match k {
                    board::BOARD_SCROLL_KEY => {
                        scroll_to(board::board_scroll_id(), AbsoluteOffset { x: offset, y: 0.0 })
                    }
                    inbox::SCROLL_KEY => scroll_to(inbox::scroll_id(), y),
                    projects::SCROLL_KEY => scroll_to(projects::scroll_id(), y),
                    roadmap::SCROLL_KEY => scroll_to(roadmap::scroll_id(), y),
                    recurring_issues::SCROLL_KEY => scroll_to(recurring_issues::scroll_id(), y),
                    issue_list::SCROLL_KEY if matches!(self.view, View::Dashboard(_)) => {
                        scroll_to(dashboard::scroll_id(), y)
                    }
                    issue_list::SCROLL_KEY => scroll_to(issue_list::scroll_id(), y),
                    lane => scroll_to(board::lane_scroll_id(lane), y),
                }
            });
        Task::batch(tasks)
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
