//! Parked per-Tab state and the pure switching rules (ADR 0001). `App` keeps
//! the active Tab's state in its own fields; every other Tab's state sits here
//! until that Tab is focused again. No Iced types and no database.
//!
//! To add a per-Tab field: add it to [`ParkedTab`], then fill it in
//! `App::stash_active_tab` and apply it in `App::restore_tab`
//! (`src/app/tab_update.rs`). Those two functions are the only places.

use std::collections::HashMap;

use crate::app::View;
use crate::db::attachments::Attachment;
use crate::tabs::{TabKey, TabSet};

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
        Self { view, selected_team_id, comment_draft: None }
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
    fn details_park_as_the_board() {
        let tab = ParkedTab::new(View::IssueDetail("i".into()), Some("a".into()));
        assert_eq!(tab.view, View::Board);
        let tab = ParkedTab::new(View::ProjectDetail("p".into()), None);
        assert_eq!(tab.view, View::Board);
        assert_eq!(ParkedTab::new(View::RecurringIssues, None).view, View::RecurringIssues);
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
    fn an_empty_draft_is_not_parked() {
        let view = View::IssueDetail("i".into());
        let tab = ParkedTab::new(view.clone(), None)
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
}
