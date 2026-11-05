//! Tab messages and the one place per-Tab state is stashed and restored
//! (ADR 0001). Later Tab tickets extend `stash_active_tab` and `restore_tab`
//! (plus `ParkedTab` in `src/tab_state.rs`) and add `TabMessage` variants.

use iced::Task;

use super::{project_detail, App, Message, View};
use crate::db::queries;
use crate::tab_state::{self, CommentDraft, DetailFrom, ParkedTab, Switch};
use crate::tabs::{TabKey, TabSet};

#[derive(Debug, Clone)]
pub enum TabMessage {
    /// Click a Tab in the bar, by position.
    Focus(usize),
    /// Close a Tab from the bar, by position.
    Close(usize),
    /// Back from an issue opened in a project Tab to the project, its root.
    BackToRoot,
    /// The tab bar scrolled sideways to this offset.
    BarScrolled(f32),
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
            TabMessage::Close(index) => return self.close_tab(index),
            TabMessage::BackToRoot => return self.back_to_tab_root(),
            TabMessage::BarScrolled(x) => self.tab_bar_offset = x,
        }
        Task::none()
    }

    /// Close the Tab at `index`. Closing the active Tab (or the last Tab,
    /// which becomes the Home page) restores the Tab that takes over;
    /// closing a background Tab only forgets its parked state, comment draft
    /// included (ADR 0001).
    fn close_tab(&mut self, index: usize) -> Task<Message> {
        self.ensure_tab_set();
        let home = self.home_tab_key();
        let Some(set) = self.tab_set.as_mut() else {
            return Task::none();
        };
        let from = set.active().clone();
        let Some(closed) = tab_state::close_index(set, index, &home) else {
            return Task::none();
        };
        // The closed Tab's comment draft goes with it, staged files included.
        if let Some(draft) = self.parked_tabs.forget(&closed.closed).and_then(|p| p.comment_draft) {
            self.drop_drafts(&draft.owner_ids());
        }
        if closed.closed == from {
            self.discard_live_composers();
        }
        if let Some(to) = closed.to {
            // The closed Tab is gone, so its live state is dropped, not
            // stashed. Edits still commit, as on any switch.
            self.commit_issue_detail();
            self.commit_project_detail();
            self.stash_dashboard_session();
            if closed.closed != from {
                self.stash_active_tab(from);
            }
            return self.restore_tab(&to);
        }
        Task::none()
    }

    /// Leave the open issue for the active Tab's root page (the project, for a
    /// project Tab). One level: opening issue B from issue A comes back here,
    /// not to A.
    fn back_to_tab_root(&mut self) -> Task<Message> {
        let Some(key) = self.active_tab_key().cloned() else {
            return Task::none();
        };
        self.commit_issue_detail();
        self.issue_detail = None;
        self.switch_view(tab_state::root_view(&key));
        self.restore_project_detail();
        self.restore_scroll()
    }

    /// The Tab the Home page opens, falling back to the first team's.
    fn home_tab_key(&self) -> TabKey {
        use crate::db::home_page::{launch_target, LaunchTarget};
        let launch = launch_target(
            self.home_page.as_ref(),
            &self.teams,
            &self.saved_views,
            &self.dashboards,
        );
        let team = match launch {
            LaunchTarget::Team(id) => Some(id),
            LaunchTarget::View { team_id, .. } => Some(team_id),
            LaunchTarget::Dashboard(id) => return TabKey::Dashboard(id),
            LaunchTarget::Default => None,
        };
        team.or_else(|| {
            self.teams
                .iter()
                .find(|t| t.archived_at.is_none())
                .or_else(|| self.teams.first())
                .map(|t| t.id.clone())
        })
        .map_or(TabKey::MyIssues, TabKey::Team)
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

    /// Focus the Tab for `key`, opening it at the end of the bar if it isn't
    /// open. This is how every Tab kind is opened. On a move, the live state
    /// is stashed and the target restored, with its data reloaded.
    pub(super) fn focus_tab(&mut self, key: TabKey) -> Task<Message> {
        self.ensure_tab_set();
        if let Some(set) = self.tab_set.as_mut() {
            let switch = tab_state::focus_key(set, key);
            return self.apply_switch(switch);
        }
        Task::none()
    }

    /// Navigating to a team-level page (Board, Projects, Roadmap, Recurring
    /// issues) from a project or cross-team Tab belongs to a team's Tab: the
    /// team to go to, or `None` when the page is fine where it is.
    pub(super) fn team_tab_for_navigation(&self, view: &View) -> Option<String> {
        let team_page = matches!(
            view,
            View::Board | View::ProjectList | View::Roadmap | View::RecurringIssues
        );
        match self.active_tab_key()? {
            TabKey::Team(_) => None,
            _ if !team_page => None,
            TabKey::Project(_) => self.selected_team_id.clone(),
            _ => {
                let first = self.teams.first().map(|t| t.id.as_str());
                self.tab_set
                    .as_ref()
                    .and_then(|set| set.new_issue_team(first))
                    .map(str::to_string)
                    .or_else(|| self.selected_team_id.clone())
            }
        }
    }

    /// The team a team or project Tab shows; `None` on a cross-team Tab.
    pub(super) fn active_tab_team(&self) -> Option<String> {
        match self.active_tab_key()? {
            TabKey::Team(id) => Some(id.clone()),
            TabKey::Project(id) => self
                .tab_projects
                .get(id)
                .map(|(_, team_id)| team_id.clone())
                .or_else(|| self.selected_team_id.clone()),
            _ => None,
        }
    }

    /// The active Tab's key, once the set exists.
    pub(super) fn active_tab_key(&self) -> Option<&TabKey> {
        self.tab_set.as_ref().map(|set| set.active())
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
        let draft = self.take_comment_draft();
        let view = tab_state::parked_view(&key, self.view.clone());
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
                detail_from: DetailFrom {
                    my_issues: self.detail_from_my_issues,
                    inbox: self.detail_from_inbox,
                    dashboard: self.detail_from_dashboard.clone(),
                },
                ..ParkedTab::new(view, self.selected_team_id.clone())
                    .with_comment_draft(self.view.clone(), draft)
            },
        );
    }

    /// Lift the open detail's comment draft out of the live state, dropping the
    /// detail so its staged files aren't cancelled with it. `None` (and nothing
    /// touched) when there's no draft worth parking.
    fn take_comment_draft(&mut self) -> Option<CommentDraft> {
        let draft = match &self.view {
            View::IssueDetail(id) => self.issue_detail.as_ref().filter(|d| d.issue_id == *id).map(|d| {
                (&d.comment_composer, d.reply_to.clone(), &d.reply_composer, &d.composer_files)
            }),
            View::ProjectDetail(id) => {
                self.project_detail.as_ref().filter(|d| d.project_id == *id).map(|d| {
                    (&d.comment_composer, d.reply_to.clone(), &d.reply_composer, &d.composer_files)
                })
            }
            _ => None,
        }
        .map(|(comment, reply_to, reply, files)| CommentDraft {
            owner: self.view.clone(),
            comment: comment.text().trim_end_matches('\n').to_string(),
            reply_to,
            reply: reply.text().trim_end_matches('\n').to_string(),
            comment_id: files.comment_id.clone(),
            reply_id: files.reply_id.clone(),
            files: files.files.clone(),
        })
        .filter(CommentDraft::has_content)?;
        self.issue_detail = None;
        self.project_detail = None;
        Some(draft)
    }

    /// Cancel the live detail's unsent staged files and drop it: used when its
    /// Tab closes, so nothing is parked.
    fn discard_live_composers(&mut self) {
        let mut ids = Vec::new();
        if let Some(d) = self.issue_detail.take() {
            ids.extend([d.composer_files.comment_id, d.composer_files.reply_id]);
        }
        if let Some(d) = self.project_detail.take() {
            ids.extend([d.composer_files.comment_id, d.composer_files.reply_id]);
        }
        if !ids.is_empty() {
            self.drop_drafts(&ids);
        }
    }

    /// Rebuild the detail page a parked draft was typed on and put the draft
    /// back. When the issue or project is gone the draft is discarded and the
    /// Tab falls back to its root page.
    fn restore_comment_draft(&mut self, draft: CommentDraft, key: &TabKey) {
        use iced::widget::text_editor::Content;
        let conn = self.db();
        let files = crate::views::issue_detail::ComposerFiles {
            comment_id: draft.comment_id.clone(),
            files: draft.files.clone(),
            reply_id: draft.reply_id.clone(),
        };
        match draft.owner.clone() {
            View::IssueDetail(id) => match queries::get_issue(&conn, &id) {
                Ok(Some(issue)) => {
                    let mut d = crate::views::issue_detail::IssueDetailState::from_issue(&issue);
                    d.comment_composer = Content::with_text(&draft.comment);
                    d.reply_to = draft.reply_to;
                    d.reply_composer = Content::with_text(&draft.reply);
                    d.composer_files = files;
                    self.issue_detail = Some(d);
                    self.load_detail_dependencies(&conn);
                    self.reload_detail_origin(&conn);
                    return;
                }
                _ => {}
            },
            View::ProjectDetail(id) => match queries::get_project(&conn, &id) {
                Ok(Some(project)) => {
                    let mut d = crate::views::project_detail::ProjectDetailState::from_project(&project);
                    d.comment_composer = Content::with_text(&draft.comment);
                    d.reply_to = draft.reply_to;
                    d.reply_composer = Content::with_text(&draft.reply);
                    d.composer_files = files;
                    self.project_detail = Some(d);
                    self.view = View::ProjectDetail(id);
                    self.load_detail_dependencies(&conn);
                    return;
                }
                _ => {}
            },
            _ => {}
        }
        self.view = tab_state::root_view(key);
        self.drop_drafts(&draft.owner_ids());
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
        let (view, team, scroll_state, search, focus, collapsed, open_view, draft, detail_from) = match parked {
            Some(p) => (
                p.view,
                p.selected_team_id,
                p.scroll_state,
                (p.search_query, p.search_view),
                (p.focused_lane, p.focused_issue),
                p.collapsed_sections,
                p.open_view,
                p.comment_draft,
                p.detail_from,
            ),
            None => (
                tab_state::root_view(key),
                match key {
                    TabKey::Team(id) => Some(id.clone()),
                    // A project's Tab shows its own team's data.
                    TabKey::Project(id) => queries::get_project(&self.db(), id)
                        .ok()
                        .flatten()
                        .map(|p| p.team_id)
                        .or_else(|| self.selected_team_id.clone()),
                    _ => self.selected_team_id.clone(),
                },
                Default::default(),
                (String::new(), View::Board),
                (0, 0),
                Default::default(),
                None,
                None,
                DetailFrom::default(),
            ),
        };
        // An issue deleted while the Tab was parked: show the Tab's root page,
        // and drop what belonged to the issue (its Back origin, a draft).
        let (view, gone) = {
            let conn = self.db();
            tab_state::resolve_parked(key, view, |id| {
                matches!(queries::get_issue(&conn, id), Ok(Some(_)))
            })
        };
        let (detail_from, draft) = if gone {
            if let Some(draft) = &draft {
                self.drop_drafts(&draft.owner_ids());
            }
            (DetailFrom::default(), None)
        } else {
            (detail_from, draft)
        };
        self.scroll_state = scroll_state;
        (self.search_query, self.search_view) = search;
        self.open_view = open_view;
        self.detail_from_my_issues = detail_from.my_issues;
        self.detail_from_inbox = detail_from.inbox;
        self.detail_from_dashboard = detail_from.dashboard;
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
        if let Some(draft) = draft {
            self.restore_comment_draft(draft, key);
        }
        if let View::IssueDetail(id) = self.view.clone() {
            if self.issue_detail.as_ref().map_or(true, |d| d.issue_id != id) {
                self.restore_issue_detail(&id, key);
            }
        }
        // Parked focus may point past rows that went away.
        if matches!(self.view, View::Board | View::MyIssues) {
            self.clamp_focus();
        }
        self.restore_dashboard_session();
        self.restore_project_detail();
        self.restore_scroll()
    }

    /// A parked Tab's open issue: rebuild its buffers from the local
    /// database (edits were committed when the Tab was left).
    fn restore_issue_detail(&mut self, id: &str, key: &TabKey) {
        let conn = self.db();
        match queries::get_issue(&conn, id) {
            Ok(Some(issue)) => {
                self.issue_detail =
                    Some(crate::views::issue_detail::IssueDetailState::from_issue(&issue));
                self.load_detail_dependencies(&conn);
                self.reload_detail_origin(&conn);
            }
            _ => self.view = tab_state::root_view(key),
        }
    }

    /// A project Tab's root is the project page: rebuild its buffers from the
    /// reloaded project (they aren't parked; edits committed on leaving).
    pub(super) fn restore_project_detail(&mut self) {
        let View::ProjectDetail(id) = &self.view else {
            return;
        };
        // A restored comment draft already rebuilt the detail.
        if self.project_detail.as_ref().is_some_and(|d| d.project_id == *id) {
            return;
        }
        let Some(project) = self.projects.iter().find(|p| p.id == *id) else {
            return;
        };
        self.project_detail = Some(project_detail::ProjectDetailState::from_project(project));
        let conn = self.db();
        self.load_detail_dependencies(&conn);
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

    /// Scroll the tab bar so the active Tab is in view, when the active Tab or
    /// the number of Tabs changed since `before` (index, count).
    pub(super) fn reveal_active_tab(&mut self, before: (usize, usize)) -> Task<Message> {
        use crate::views::tab_bar;
        let Some(set) = &self.tab_set else {
            return Task::none();
        };
        let now = (set.active_index(), set.tabs().len());
        if now == before {
            return Task::none();
        }
        let viewport = tab_bar::tab_viewport(self.window_width);
        let w = tab_bar::tab_width(now.1, viewport);
        let x = tab_bar::reveal_offset(now.0, now.1, w, viewport, self.tab_bar_offset);
        self.tab_bar_offset = x;
        iced::widget::scrollable::scroll_to(
            tab_bar::scroll_id(),
            iced::widget::scrollable::AbsoluteOffset { x, y: 0.0 },
        )
    }

    /// Active index and Tab count, for `reveal_active_tab`.
    pub(super) fn tab_position(&self) -> (usize, usize) {
        self.tab_set
            .as_ref()
            .map_or((0, 0), |s| (s.active_index(), s.tabs().len()))
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
                let label = tab_state::label(
                    key,
                    |id| {
                        self.teams
                            .iter()
                            .find(|t| t.id == id)
                            .map(|t| (t.identifier.clone(), t.name.clone()))
                    },
                    |id| self.tab_projects.get(id).cloned(),
                    |id| {
                        self.dashboards
                            .iter()
                            .find(|d| d.id == id)
                            .map(|d| d.name.clone())
                    },
                );
                (label, i == set.active_index())
            })
            .collect()
    }

    /// Re-read the name and team of every open project Tab, so a rename that
    /// arrived from Data sync shows from this reload on. Projects of other
    /// teams aren't in `projects`, so this asks the local database.
    pub(super) fn reload_tab_projects(&mut self, conn: &rusqlite::Connection) {
        self.tab_projects.clear();
        let Some(set) = &self.tab_set else {
            return;
        };
        for key in set.tabs() {
            if let TabKey::Project(id) = key {
                if let Ok(Some(p)) = queries::get_project(conn, id) {
                    self.tab_projects.insert(id.clone(), (p.name, p.team_id));
                }
            }
        }
    }
}
