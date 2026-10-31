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

    /// Row index of the Notification with this remote id, if listed.
    pub fn index_of(&self, remote_id: i64) -> Option<usize> {
        self.items.iter().position(|n| n.remote_id == remote_id)
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

/// Where Enter on a Notification should go.
#[derive(Debug, PartialEq)]
pub enum OpenTarget {
    /// The issue is already in the local mirror.
    Local(crate::db::models::Issue),
    /// Not mirrored (e.g. outside the synced window): one-shot fetch it.
    Fetch { team_remote_id: i64, local_team_id: String, issue_remote_id: i64 },
    /// No issue on the Notification, or its team isn't known locally.
    Unavailable,
}

/// Resolve a Notification's issue against the mirror.
pub fn resolve_target(conn: &rusqlite::Connection, n: &Notification) -> OpenTarget {
    use crate::db::queries;
    let Some(issue_remote_id) = n.issue_remote_id else {
        return OpenTarget::Unavailable;
    };
    if let Ok(Some(issue)) = queries::find_issue_by_remote_id(conn, issue_remote_id) {
        return OpenTarget::Local(issue);
    }
    let Some(team_remote_id) = n.team_remote_id else {
        return OpenTarget::Unavailable;
    };
    match queries::find_team_by_remote_id(conn, team_remote_id) {
        Ok(Some(team)) => OpenTarget::Fetch {
            team_remote_id,
            local_team_id: team.id,
            issue_remote_id,
        },
        _ => OpenTarget::Unavailable,
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
    fn index_of_finds_listed_remote_id() {
        let s = state(&[7, 8, 9], 0);
        assert_eq!(s.index_of(8), Some(1));
        assert_eq!(s.index_of(42), None);
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

    fn conn_with_issue(issue_remote: Option<i64>, team_remote: Option<i64>) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        let now = "2026-01-01T00:00:00.000Z";
        conn.execute_batch(&format!(
            "INSERT INTO teams (id, name, identifier, remote_id, created_at, updated_at)
               VALUES ('t1', 'T', 'T', {}, '{now}', '{now}');
             INSERT INTO lanes (id, name, position, team_id, created_at, updated_at)
               VALUES ('l1', 'B', 0, 't1', '{now}', '{now}');
             INSERT INTO issues (id, title, priority, lane_id, team_id, remote_id, created_at, updated_at)
               VALUES ('i1', 'Issue', 0, 'l1', 't1', {}, '{now}', '{now}');",
            team_remote.map_or("NULL".to_string(), |t| t.to_string()),
            issue_remote.map_or("NULL".to_string(), |t| t.to_string()),
        ))
        .unwrap();
        conn
    }

    fn for_issue(issue: Option<i64>, team: Option<i64>) -> Notification {
        Notification { issue_remote_id: issue, team_remote_id: team, ..n(1) }
    }

    #[test]
    fn target_is_the_local_issue_when_mirrored() {
        let conn = conn_with_issue(Some(50), Some(47));
        match resolve_target(&conn, &for_issue(Some(50), Some(47))) {
            OpenTarget::Local(i) => assert_eq!(i.id, "i1"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn target_fetches_when_issue_missing_but_team_known() {
        let conn = conn_with_issue(Some(50), Some(47));
        assert_eq!(
            resolve_target(&conn, &for_issue(Some(99), Some(47))),
            OpenTarget::Fetch { team_remote_id: 47, local_team_id: "t1".into(), issue_remote_id: 99 }
        );
    }

    #[test]
    fn target_unavailable_without_issue_or_known_team() {
        let conn = conn_with_issue(Some(50), Some(47));
        assert_eq!(resolve_target(&conn, &for_issue(None, Some(47))), OpenTarget::Unavailable);
        assert_eq!(resolve_target(&conn, &for_issue(Some(99), None)), OpenTarget::Unavailable);
        assert_eq!(resolve_target(&conn, &for_issue(Some(99), Some(1000))), OpenTarget::Unavailable);
    }
}
