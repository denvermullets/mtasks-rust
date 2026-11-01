//! Per-dashboard header state and focus, kept in memory for the session.
//! Never persisted or synced. The live copy for the open dashboard stays in
//! `App.dashboard_header`; this map is where it is parked while away.

use std::collections::HashMap;

use crate::dashboard_eval::Header;

/// Everything remembered about one dashboard's header. `Header` is stored
/// whole so fields added to it later are kept without changes here.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    pub header: Header,
    pub focused_lane: usize,
    pub focused_issue: usize,
}

#[derive(Debug, Default)]
pub struct Sessions(HashMap<String, Session>);

impl Sessions {
    pub fn save(&mut self, dashboard_id: &str, session: Session) {
        self.0.insert(dashboard_id.to_string(), session);
    }

    /// The saved session, or the defaults on a dashboard's first open.
    pub fn restore(&self, dashboard_id: &str) -> Session {
        self.0.get(dashboard_id).cloned().unwrap_or_default()
    }

    /// Forget a dashboard's state (reset): the next restore gives defaults.
    pub fn reset(&mut self, dashboard_id: &str) {
        self.0.remove(dashboard_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard_eval::{Filter, Sort};

    fn custom() -> Session {
        Session {
            header: Header {
                filter: Filter::Open, sort: Sort::Priority,
                search: "bug".into(),
                ..Header::default()
            },
            focused_lane: 2,
            focused_issue: 3,
        }
    }

    #[test]
    fn first_open_gives_defaults() {
        let s = Sessions::default();
        let d = s.restore("a");
        assert_eq!(d.header, Header::default());
        assert_eq!(d.header.filter, Filter::Today);
        assert_eq!(d.header.sort, Sort::Due);
        assert!(d.header.search.is_empty());
        assert_eq!((d.focused_lane, d.focused_issue), (0, 0));
    }

    #[test]
    fn state_is_kept_per_dashboard() {
        let mut s = Sessions::default();
        s.save("a", custom());
        assert_eq!(s.restore("a"), custom());
        assert_eq!(s.restore("b"), Session::default());
    }

    #[test]
    fn reset_restores_defaults() {
        let mut s = Sessions::default();
        s.save("a", custom());
        s.reset("a");
        assert_eq!(s.restore("a"), Session::default());
    }
}
