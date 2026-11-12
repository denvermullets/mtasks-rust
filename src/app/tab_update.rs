//! Tab messages and the one place per-Tab state is stashed and restored
//! (ADR 0001). Later Tab tickets extend `stash_active_tab` and `restore_tab`
//! (plus `ParkedTab` in `src/tab_state.rs`) and add `TabMessage` variants.

use iced::Task;

use super::{project_detail, App, Message, View};
use crate::db::queries;
use crate::tab_state::{self, CommentDraft, DetailFrom, ParkedTab, Switch};
use crate::tabs::{Page, TabKey, TabSet};

#[derive(Debug, Clone)]
pub enum TabMessage {
    /// Click a Tab in the bar, by position.
    Focus(usize),
    /// Close a Tab from the bar, by position.
    Close(usize),
    /// Cmd+W: close the active Tab.
    CloseActive,
    /// The ask was answered: close this Tab, busy Terminals and all.
    CloseConfirmed(TabKey),
    /// Back from an issue opened in a project Tab to the project, its root.
    BackToRoot,
    /// Back from a team Tab's issue to the page it was opened from.
    BackToPage(View),
    /// The tab bar scrolled sideways to this offset.
    BarScrolled(f32),
    /// The pointer went down on the Tab at this position: a click or a drag.
    DragPress(usize),
    /// The pointer moved (window x) while a Tab is pressed.
    DragMove(f32),
    /// The pointer was released: a click focuses, a drag drops.
    DragEnd,
}

/// A Tab pressed in the bar. `start_x` is the pointer's first position;
/// `index` follows the Tab as it moves; `moved` is set once the pointer has
/// travelled past the threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct TabDrag {
    pub index: usize,
    pub start_x: Option<f32>,
    pub moved: bool,
}

/// Whether a Tab's team, project or Dashboard is still in the local mirror.
/// An archived team still exists.
fn tab_subject_exists(
    teams: &[crate::db::models::Team],
    dashboards: &[crate::db::dashboards::Dashboard],
    conn: &rusqlite::Connection,
    key: &TabKey,
) -> bool {
    match key {
        TabKey::Team(id) => teams.iter().any(|t| t.id == *id),
        TabKey::Project(id) => matches!(queries::get_project(conn, id), Ok(Some(_))),
        TabKey::Dashboard(id) => dashboards.iter().any(|d| d.id == *id),
        TabKey::MyIssues | TabKey::Inbox => true,
    }
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
            TabMessage::Close(index) => return self.close_tab(index, false),
            TabMessage::CloseActive => {
                self.ensure_tab_set();
                if let Some(index) = self.tab_set.as_ref().map(|s| s.active_index()) {
                    return self.close_tab(index, false);
                }
            }
            TabMessage::CloseConfirmed(key) => {
                if self.modal == super::Modal::TabCloseConfirm(key.clone()) {
                    self.modal = super::Modal::None;
                }
                let index = self.tab_set.as_ref().and_then(|s| s.tabs().iter().position(|k| *k == key));
                if let Some(index) = index {
                    return self.close_tab(index, true);
                }
            }
            TabMessage::BackToRoot => return self.back_to_tab_root(),
            TabMessage::BackToPage(view) => return self.back_to_page(view),
            TabMessage::BarScrolled(x) => self.tab_bar_offset = x,
            TabMessage::DragPress(index) => {
                self.tab_drag = Some(TabDrag { index, start_x: None, moved: false });
            }
            TabMessage::DragMove(x) => self.drag_tab_to(x),
            TabMessage::DragEnd => {
                if let Some(drag) = self.tab_drag.take() {
                    if !drag.moved {
                        return self.update_tab(TabMessage::Focus(drag.index));
                    }
                }
            }
        }
        Task::none()
    }

    /// Move the pressed Tab under the pointer once it has passed the drag
    /// threshold. Parked state is keyed by Tab, so only the order changes.
    fn drag_tab_to(&mut self, x: f32) {
        use crate::views::tab_bar;
        let sidebar_w = self.sidebar_width();
        let Some(drag) = self.tab_drag.as_mut() else {
            return;
        };
        let start = *drag.start_x.get_or_insert(x);
        if !drag.moved && (x - start).abs() < tab_bar::DRAG_THRESHOLD {
            return;
        }
        drag.moved = true;
        let Some(set) = self.tab_set.as_mut() else {
            return;
        };
        let count = set.tabs().len();
        let viewport = tab_bar::tab_viewport(self.window_width, sidebar_w);
        let w = tab_bar::tab_width(count, viewport);
        let to = tab_bar::drop_index(x, sidebar_w, self.tab_bar_offset, count, w);
        if set.move_tab(drag.index, to) {
            drag.index = to;
        }
    }

    /// Close the Tab at `index`. Closing the active Tab (or the last Tab,
    /// which becomes the Home page) restores the Tab that takes over;
    /// closing a background Tab only forgets its parked state, comment draft
    /// included (ADR 0001). The Tab's own Terminal strip (a team Tab's team
    /// Terminals, a project Tab's project Terminals) is killed with it; while
    /// one of them is busy, the Tab asks first unless `confirmed`.
    fn close_tab(&mut self, index: usize, confirmed: bool) -> Task<Message> {
        self.ensure_tab_set();
        let home = self.home_tab_key();
        let Some(key) = self.tab_set.as_ref().and_then(|s| s.tabs().get(index)).cloned() else {
            return Task::none();
        };
        let scope = self.tab_terminal_scope(&key);
        if !confirmed && scope.as_ref().is_some_and(|s| !self.terminals.busy_names(s).is_empty()) {
            self.modal = super::Modal::TabCloseConfirm(key);
            return Task::none();
        }
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
        if let Some(scope) = scope {
            self.terminals.close_scope(&scope);
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

    /// The Terminal strip that belongs to the Tab `key` itself, the one
    /// closing it kills: a team Tab's team strip, a project Tab's project
    /// strip. Cross-team Tabs have none.
    pub(super) fn tab_terminal_scope(&self, key: &TabKey) -> Option<crate::terminal::Scope> {
        let project_team = match key {
            TabKey::Project(id) => self.tab_projects.get(id).map(|(_, team)| team.as_str()),
            _ => None,
        };
        tab_state::panel_scope(key, project_team, None)
    }

    /// Leave the open issue for the active Tab's root page (the project, for a
    /// project Tab). One level: opening issue B from issue A comes back here,
    /// not to A.
    fn back_to_tab_root(&mut self) -> Task<Message> {
        let Some(key) = self.active_tab_key().cloned() else {
            return Task::none();
        };
        self.back_to_page(tab_state::root_view(&key))
    }

    /// Leave the open issue for `view`, the page it was opened from.
    fn back_to_page(&mut self, view: View) -> Task<Message> {
        self.commit_issue_detail();
        self.issue_detail = None;
        self.switch_view(view);
        self.restore_project_detail();
        self.restore_scroll()
    }

    /// Close the Tabs whose team, project or Dashboard a pull removed. An
    /// archived team still exists, so it keeps its Tab. Run after the mirror
    /// is reloaded. When the active Tab goes, a neighbour (or the Home page)
    /// takes over and its parked state is restored.
    pub(super) fn close_removed_tabs(&mut self) -> Task<Message> {
        let home = self.home_tab_key();
        let conn = self.db();
        let Some(set) = self.tab_set.as_mut() else {
            return Task::none();
        };
        let (teams, dashboards, terminals) = (&self.teams, &self.dashboards, &self.terminals);
        let pruned = set.prune(
            |key| tab_subject_exists(teams, dashboards, &conn, key),
            // A removed team with Terminals keeps its Tab (JRST-421).
            |key| tab_state::keeps_removed_team(key, |team| terminals.has_terminals(team)),
            &home,
        );
        let active = set.active().clone();
        for key in &pruned.closed {
            if let Some(draft) = self.parked_tabs.forget(key).and_then(|p| p.comment_draft) {
                self.drop_drafts(&draft.owner_ids());
            }
        }
        let mut task = Task::none();
        if pruned.active_changed {
            // The removed Tab's live state goes with it; edits can't commit to a
            // row that no longer exists.
            self.discard_live_composers();
            task = self.restore_tab(&active);
        }
        self.hold_removed_team_tab();
        task
    }

    /// Whether the active Tab is a team's that is no longer in the local
    /// mirror, kept for its Terminals. It shows a notice and the panel only.
    pub(super) fn removed_team_tab(&self) -> bool {
        matches!(self.active_tab_key(), Some(TabKey::Team(id)) if !self.teams.iter().any(|t| t.id == *id))
    }

    /// A kept removed team's Tab stays on that team: `leave_deleted_teams`
    /// and `reload_data` pick another team when the selected one goes, which
    /// would put that team's data under this Tab.
    fn hold_removed_team_tab(&mut self) {
        let Some(TabKey::Team(id)) = self.active_tab_key().cloned() else {
            return;
        };
        if self.removed_team_tab() {
            self.selected_team_id = Some(id);
            self.view = View::Board;
            self.open_view = None;
            self.issue_detail = None;
            self.reload_data();
        }
    }

    /// The Tab the Home page opens, or `None` when it doesn't resolve.
    fn resolved_home_key(&self) -> Option<TabKey> {
        use crate::db::home_page::{launch_target, LaunchTarget};
        let launch = launch_target(
            self.home_page.as_ref(),
            &self.teams,
            &self.saved_views,
            &self.dashboards,
        );
        match launch {
            LaunchTarget::Team(id) => Some(TabKey::Team(id)),
            LaunchTarget::View { team_id, .. } => Some(TabKey::Team(team_id)),
            LaunchTarget::Dashboard(id) => Some(TabKey::Dashboard(id)),
            LaunchTarget::Default => None,
        }
    }

    /// The Tab shown when nothing else decides: the first team's, else My Issues.
    fn fallback_tab_key(&self) -> TabKey {
        self.teams
            .iter()
            .find(|t| t.archived_at.is_none())
            .or_else(|| self.teams.first())
            .map(|t| t.id.clone())
            .map_or(TabKey::MyIssues, TabKey::Team)
    }

    /// The Tab the Home page opens, falling back to the first team's.
    fn home_tab_key(&self) -> TabKey {
        self.resolved_home_key().unwrap_or_else(|| self.fallback_tab_key())
    }

    /// Write the open Tabs to the local database when they changed since the
    /// last write, so a relaunch can bring them back. Local only: nothing is
    /// queued for Data sync.
    pub(super) fn save_tabs(&mut self) {
        let Some(set) = self.tab_set.clone() else {
            return;
        };
        let pages = self.tab_pages(&set);
        if self.saved_tabs.as_ref().is_some_and(|(s, p)| *s == set && *p == pages) {
            return;
        }
        if crate::db::open_tabs::save(&self.db(), &set, &pages).is_ok() {
            self.saved_tabs = Some((set, pages));
        }
    }

    /// The page each Tab would return to on relaunch, in bar order. The
    /// active Tab reads the live view; a parked one its parked view; a Tab
    /// restored but not focused yet keeps the page it was stored with.
    fn tab_pages(&self, set: &TabSet) -> Vec<Page> {
        set.tabs()
            .iter()
            .enumerate()
            .map(|(i, key)| {
                if i == set.active_index() {
                    tab_state::stored_page(key, &self.view, self.open_view.as_ref())
                } else if let Some(p) = self.parked_tabs.get(key) {
                    tab_state::stored_page(key, &p.view, p.open_view.as_ref())
                } else {
                    self.restored_pages.get(key).cloned().unwrap_or(Page::Root)
                }
            })
            .collect()
    }

    /// Launch: restore the stored Tabs (those whose subject still exists),
    /// focus the Home page's Tab (opening it if absent) and make it live. With
    /// no teams yet there's nothing to show and no set is made. Returns the
    /// task that scrolls the view into place.
    pub(super) fn launch_tabs(&mut self) -> Task<Message> {
        if self.teams.is_empty() {
            return Task::none();
        }
        let conn = self.db();
        let fallback = self.fallback_tab_key();
        let stored = crate::db::open_tabs::load(&conn, &fallback).ok().flatten();
        let (stored_set, pages) = match stored {
            Some(s) => (Some(s.tabs), s.pages),
            None => (None, Default::default()),
        };
        let (teams, dashboards) = (&self.teams, &self.dashboards);
        let set = TabSet::for_launch(stored_set, self.resolved_home_key(), &fallback, |key| {
            tab_subject_exists(teams, dashboards, &conn, key)
        });
        let active = set.active().clone();
        let panels = crate::db::panel_states::load(&conn).unwrap_or_default();
        self.restored_panels = tab_state::launch_panels(panels, set.tabs(), &active);
        self.tab_set = Some(set);
        self.restored_pages = pages;
        self.restore_tab(&active)
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

    /// The Terminal strip the active Tab shows (ADR 0003): a project Tab's
    /// project's; a team Tab's team's own, or a project's while its page is
    /// open there. A cross-team Tab shows none, whatever page it is on (an
    /// issue opened there from another team included). Before the Tab set
    /// exists, the page decides.
    pub(super) fn panel_scope(&self) -> Option<crate::terminal::Scope> {
        match self.active_tab_key() {
            Some(key) => {
                let project_team = match key {
                    TabKey::Project(id) => self.tab_projects.get(id).map(|(_, team)| team.as_str()),
                    _ => None,
                };
                let viewed_project = match &self.view {
                    View::ProjectDetail(id) => Some(id.as_str()),
                    _ => None,
                };
                tab_state::panel_scope(
                    key,
                    project_team.or(self.selected_team_id.as_deref()),
                    viewed_project,
                )
            }
            None => crate::terminal::panel_scope(&self.view, self.selected_team_id.as_deref()),
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
        let key_is_cross_team = key.is_cross_team();
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
                    page: self.detail_from_page.clone(),
                },
                // A cross-team Tab has no panel to park.
                panel: (!key_is_cross_team).then(|| self.terminals.stored_panel()),
                map: tab_state::MapState {
                    map_return: self.map_return.take(),
                    flipped: std::mem::take(&mut self.map_flipped),
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
            View::IssueDetail(id) => if let Ok(Some(issue)) = queries::get_issue(&conn, &id) {
                let mut d = crate::views::issue_detail::IssueDetailState::from_issue(&issue);
                d.comment_composer = Content::with_text(&draft.comment);
                d.reply_to = draft.reply_to;
                d.reply_composer = Content::with_text(&draft.reply);
                d.composer_files = files;
                self.issue_detail = Some(d);
                self.load_detail_dependencies(&conn);
                self.reload_detail_origin(&conn);
                return;
            },
            View::ProjectDetail(id) => if let Ok(Some(project)) = queries::get_project(&conn, &id) {
                let mut d = crate::views::project_detail::ProjectDetailState::from_project(&project);
                d.comment_composer = Content::with_text(&draft.comment);
                d.reply_to = draft.reply_to;
                d.reply_composer = Content::with_text(&draft.reply);
                d.composer_files = files;
                self.project_detail = Some(d);
                self.view = View::ProjectDetail(id);
                self.load_detail_dependencies(&conn);
                return;
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
        // A Tab restored at launch returns to its stored page on first focus.
        let restored = self.restored_pages.remove(key).filter(|_| parked.is_none());
        // A Tab restored at launch returns to its stored panel on first focus.
        let restored_panel = self.restored_panels.remove(key);
        let (view, team, scroll_state, search, focus, collapsed, open_view, draft, detail_from, panel, map) = match parked {
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
                p.panel,
                p.map,
            ),
            None => (
                restored.as_ref().map_or_else(
                    || tab_state::root_view(key),
                    |page| tab_state::page_view(key, page),
                ),
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
                restored_panel,
                Default::default(),
            ),
        };
        // An issue deleted while the Tab was parked: show the Tab's root page,
        // and drop what belonged to the issue (its Back origin, a draft).
        let (view, gone) = {
            let conn = self.db();
            tab_state::resolve_parked(key, view, |id| {
                matches!(queries::get_issue(&conn, id), Ok(Some(_)))
                    || matches!(queries::get_project(&conn, id), Ok(Some(_)))
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
        self.detail_from_page = detail_from.page;
        self.issue_detail = None;
        self.project_detail = None;
        self.dashboard_menu = None;
        self.composer_focused = false;
        (self.focused_lane, self.focused_issue) = focus;
        self.collapsed_sections = collapsed;
        self.selected_team_id = team;
        let map = map.for_view(&view);
        self.map_return = map.map_return;
        self.map_flipped = map.flipped;
        // The project map's cards are rebuilt by the reload below.
        self.project_map = None;
        self.view = view;
        // A Tab never shown, or a cross-team one, has the default hidden panel.
        self.terminals.restore_panel(&panel.unwrap_or_default());
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
            if self.issue_detail.as_ref().is_none_or(|d| d.issue_id != id) {
                self.restore_issue_detail(&id, key);
            }
        }
        // Parked focus may point past rows that went away.
        if matches!(self.view, View::Board | View::MyIssues) {
            self.clamp_focus();
        }
        self.restore_dashboard_session();
        self.restore_project_detail();
        // A Saved view reopens fresh: Modified changes aren't restored. A
        // view deleted meanwhile leaves the Tab on its board.
        let opened = match restored {
            Some(Page::SavedView(id)) => self.update_inner(Message::OpenSavedView(id)),
            _ => Task::none(),
        };
        Task::batch([self.restore_scroll(), opened])
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
        let (list_mode, lanes) = match self.view {
            View::Board | View::MyIssues => (
                self.board_display().view_mode == crate::display::ViewMode::List,
                self.board_columns().len(),
            ),
            _ => (false, 0),
        };
        let mut keys = tab_state::scroll_keys(&self.view, list_mode, lanes);
        // Sub-group rows scroll as one, in place of the per-lane scrolls.
        if self.swimlanes_active() {
            keys = vec![crate::scroll::ScrollKey::Board, crate::scroll::ScrollKey::Swimlanes];
        }
        let tasks = tab_state::restore_offsets(&keys, &self.scroll_state)
            .into_iter()
            .map(|(k, offset)| k.scroll_to(offset));
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
        let viewport = tab_bar::tab_viewport(self.window_width, self.sidebar_width());
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

    /// Per Tab in bar order, what the Terminals of its own strip wait on
    /// the person for. The active Tab shows none for the Terminals on screen
    /// (they lose theirs at `sync`).
    pub(super) fn tab_needs(&self) -> Vec<Option<crate::terminal::Need>> {
        let Some(set) = &self.tab_set else {
            return Vec::new();
        };
        set.tabs()
            .iter()
            .map(|key| self.tab_terminal_scope(key).and_then(|s| self.terminals.scope_need(&s)))
            .collect()
    }

    /// Per Tab, whether it has unread Notifications: only the Inbox Tab, while
    /// the unread count is above zero.
    pub(super) fn tab_unread(&self) -> Vec<bool> {
        let Some(set) = &self.tab_set else {
            return Vec::new();
        };
        set.tabs()
            .iter()
            .map(|key| *key == TabKey::Inbox && self.notif_unread_count > 0)
            .collect()
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

/// Whether a removed team's Tab refuses this message. That Tab keeps only the
/// team's Terminals; its board is gone, so anything that creates, edits or
/// moves an issue or project, or steps through the board, would act on a team
/// that no longer exists (and could enqueue a write for it). Panel keys,
/// Terminal messages, Tab switching and a plain sync are untouched.
pub(super) fn blocked_on_removed_team(message: &Message) -> bool {
    matches!(
        message,
        Message::CreateIssue
            | Message::CreateSubIssue(_)
            | Message::EditFocusedIssue
            | Message::ConfirmDeleteFocusedIssue
            | Message::MoveIssueNextLane
            | Message::MoveIssuePrevLane
            | Message::MoveFocusedIssueToTeam
            | Message::SetFocusedPriority(_)
            | Message::FocusNextIssue
            | Message::FocusPrevIssue
            | Message::FocusNextLane
            | Message::FocusPrevLane
            | Message::ToggleViewMode
            | Message::ToggleFocusedSection
            | Message::CreateProject
            | Message::NewRecurringIssue
            | Message::OpenLabels
            | Message::OpenPropertyPicker(_)
            | Message::PropertyPickerChoose(_)
            | Message::PropertyPickerSubmit
    )
}

#[cfg(test)]
mod removed_team_guard {
    use super::*;
    use crate::terminal::TerminalMessage;

    #[test]
    fn board_and_issue_actions_are_blocked_on_a_removed_teams_tab() {
        for m in [
            Message::CreateIssue,
            Message::EditFocusedIssue,
            Message::ConfirmDeleteFocusedIssue,
            Message::MoveIssueNextLane,
            Message::MoveIssuePrevLane,
            Message::MoveFocusedIssueToTeam,
            Message::SetFocusedPriority(2),
            Message::FocusNextIssue,
            Message::FocusPrevLane,
            Message::CreateProject,
        ] {
            assert!(blocked_on_removed_team(&m), "{m:?}");
        }
    }

    #[test]
    fn terminal_actions_tab_switching_and_sync_still_work() {
        for m in [
            Message::Terminal(TerminalMessage::New),
            Message::Terminal(TerminalMessage::ToggleFocus),
            Message::Terminal(TerminalMessage::Close(crate::terminal::TerminalId(1))),
            Message::Tab(TabMessage::Focus(0)),
            Message::Tab(TabMessage::Close(0)),
            Message::SyncNow,
            Message::OpenCommandPalette,
            Message::CloseModal,
        ] {
            assert!(!blocked_on_removed_team(&m), "{m:?}");
        }
    }
}
