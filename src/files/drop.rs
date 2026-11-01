//! Where a window file-drop goes. ONE place decides the target from the
//! current view and modal; project (JRST-205) and comment-composer (JRST-208)
//! tickets add their arms here.

use crate::app::{Modal, View};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropTarget {
    /// Attach to the open issue (routes to `Message::FilesPicked`).
    Issue,
    /// Nothing to attach to; the drop is ignored.
    Ignore,
}

pub fn drop_target(view: &View, modal: &Modal) -> DropTarget {
    match (view, modal) {
        (View::IssueDetail(_), Modal::None) => DropTarget::Issue,
        _ => DropTarget::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_on_open_issue_attaches_to_it() {
        assert_eq!(drop_target(&View::IssueDetail("i".into()), &Modal::None), DropTarget::Issue);
    }

    #[test]
    fn drop_elsewhere_or_under_a_modal_is_ignored() {
        assert_eq!(drop_target(&View::Board, &Modal::None), DropTarget::Ignore);
        assert_eq!(drop_target(&View::IssueDetail("i".into()), &Modal::Settings), DropTarget::Ignore);
    }
}
