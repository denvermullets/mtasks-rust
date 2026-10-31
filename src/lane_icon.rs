//! Which lane-category icon a lane shows (pure; the canvas widget is in
//! `views::lane_icon`).

/// Which icon a lane shows, derived from its Rails workflow category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneIcon {
    Backlog,
    Unstarted,
    Started,
    Completed,
    Canceled,
    /// No category (local seed lanes) or one we don't know.
    Plain,
}

/// Map the optional category string to an icon kind.
pub fn icon_for(category: Option<&str>) -> LaneIcon {
    match category {
        Some("backlog") => LaneIcon::Backlog,
        Some("unstarted") => LaneIcon::Unstarted,
        Some("started") => LaneIcon::Started,
        Some("completed") => LaneIcon::Completed,
        Some("canceled") => LaneIcon::Canceled,
        _ => LaneIcon::Plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_category() {
        assert_eq!(icon_for(Some("backlog")), LaneIcon::Backlog);
        assert_eq!(icon_for(Some("unstarted")), LaneIcon::Unstarted);
        assert_eq!(icon_for(Some("started")), LaneIcon::Started);
        assert_eq!(icon_for(Some("completed")), LaneIcon::Completed);
        assert_eq!(icon_for(Some("canceled")), LaneIcon::Canceled);
    }

    #[test]
    fn unknown_and_none_are_plain() {
        assert_eq!(icon_for(Some("archived")), LaneIcon::Plain);
        assert_eq!(icon_for(Some("")), LaneIcon::Plain);
        assert_eq!(icon_for(None), LaneIcon::Plain);
    }
}
