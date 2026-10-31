//! Inbox list state: the rows currently shown, the selection and the filter.
//! Pure (no Iced, no SQLite); `App` owns one and feeds it from the mirror's
//! `inbox_list`.

use crate::db::notifications::{Filter, Notification};

#[derive(Debug, Default)]
pub struct InboxState {
    pub items: Vec<Notification>,
    pub selected: usize,
    pub filter: Filter,
}

impl InboxState {
    /// Replace the rows (after a poll or a local mark). The selection stays on
    /// the same Notification when it is still listed; otherwise the index is
    /// kept, clamped to the new list.
    pub fn set_items(&mut self, items: Vec<Notification>) {
        let keep = self.items.get(self.selected).map(|n| n.remote_id);
        let found = keep.and_then(|id| items.iter().position(|n| n.remote_id == id));
        self.selected = found.unwrap_or(self.selected);
        self.items = items;
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
    }

    /// Move the selection by `delta`, clamped to the list.
    pub fn step(&mut self, delta: isize) {
        let last = self.items.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    /// Switch the filter and reset the selection to the top. The caller
    /// reloads the rows afterwards.
    pub fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.selected = 0;
    }

    pub fn selected_item(&self) -> Option<&Notification> {
        self.items.get(self.selected)
    }
}

/// The other filter (what `f` switches to).
pub fn other_filter(filter: Filter) -> Filter {
    match filter {
        Filter::Unread => Filter::All,
        Filter::All => Filter::Unread,
    }
}

/// Empty-state copy for the current filter.
pub fn empty_message(filter: Filter) -> &'static str {
    match filter {
        Filter::Unread => "No unread notifications",
        Filter::All => "No notifications",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(remote_id: i64) -> Notification {
        Notification {
            id: format!("u{remote_id}"),
            remote_id,
            action: "mention".into(),
            message: "m".into(),
            actor_name: None,
            issue_remote_id: None,
            issue_identifier: None,
            issue_title: None,
            team_remote_id: None,
            comment_remote_id: None,
            created_at: String::new(),
            updated_at: String::new(),
            read_at: None,
        }
    }

    fn state(ids: &[i64], selected: usize) -> InboxState {
        InboxState {
            items: ids.iter().map(|i| n(*i)).collect(),
            selected,
            filter: Filter::Unread,
        }
    }

    #[test]
    fn step_clamps_at_both_ends() {
        let mut s = state(&[3, 2, 1], 0);
        s.step(-1);
        assert_eq!(s.selected, 0);
        s.step(1);
        s.step(1);
        s.step(1);
        assert_eq!(s.selected, 2);
    }

    #[test]
    fn step_on_empty_list_stays_zero() {
        let mut s = state(&[], 0);
        s.step(1);
        assert_eq!(s.selected, 0);
        assert!(s.selected_item().is_none());
    }

    #[test]
    fn refresh_keeps_selection_on_same_notification() {
        let mut s = state(&[3, 2, 1], 1);
        s.set_items(vec![n(5), n(4), n(3), n(2), n(1)]);
        assert_eq!(s.selected_item().unwrap().remote_id, 2);
    }

    #[test]
    fn refresh_clamps_when_selected_row_is_gone() {
        let mut s = state(&[3, 2, 1], 2);
        s.set_items(vec![n(3), n(2)]);
        assert_eq!(s.selected, 1);
        s.set_items(vec![]);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn set_filter_resets_selection() {
        let mut s = state(&[3, 2, 1], 2);
        s.set_filter(Filter::All);
        assert_eq!(s.filter, Filter::All);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn other_filter_toggles() {
        assert_eq!(other_filter(Filter::Unread), Filter::All);
        assert_eq!(other_filter(Filter::All), Filter::Unread);
    }

    #[test]
    fn empty_messages_follow_filter() {
        assert_eq!(empty_message(Filter::Unread), "No unread notifications");
        assert_eq!(empty_message(Filter::All), "No notifications");
    }
}
