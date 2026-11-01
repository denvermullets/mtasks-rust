//! Where a window file-drop goes. ONE place decides the target from the
//! current view and modal; project (JRST-205) and comment-composer (JRST-208)
//! tickets add their arms here.

use crate::app::{Modal, View};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropTarget {
    /// Attach to the open issue (routes to `Message::FilesPicked`).
    Issue,
    /// Stage on the focused comment composer (routes to `Message::CommentFilesPicked`).
    CommentComposer,
    /// Nothing to attach to; the drop is ignored.
    Ignore,
}

/// `composer_focused`: the top-level comment composer was the last editor the
/// user touched (`App.composer_focused`).
pub fn drop_target(view: &View, modal: &Modal, composer_focused: bool) -> DropTarget {
    match (view, modal) {
        (View::IssueDetail(_) | View::ProjectDetail(_), Modal::None) if composer_focused => {
            DropTarget::CommentComposer
        }
        (View::IssueDetail(_), Modal::None) => DropTarget::Issue,
        _ => DropTarget::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_on_open_issue_attaches_to_it() {
        assert_eq!(drop_target(&View::IssueDetail("i".into()), &Modal::None, false), DropTarget::Issue);
    }

    #[test]
    fn drop_with_composer_focused_stages_on_the_composer() {
        for v in [View::IssueDetail("i".into()), View::ProjectDetail("p".into())] {
            assert_eq!(drop_target(&v, &Modal::None, true), DropTarget::CommentComposer);
        }
    }

    #[test]
    fn composer_focus_does_not_beat_a_modal_or_other_views() {
        assert_eq!(drop_target(&View::IssueDetail("i".into()), &Modal::Settings, true), DropTarget::Ignore);
        assert_eq!(drop_target(&View::Board, &Modal::None, true), DropTarget::Ignore);
    }

    #[test]
    fn drop_elsewhere_or_under_a_modal_is_ignored() {
        assert_eq!(drop_target(&View::Board, &Modal::None, false), DropTarget::Ignore);
        assert_eq!(drop_target(&View::IssueDetail("i".into()), &Modal::Settings, false), DropTarget::Ignore);
    }
}
