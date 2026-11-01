//! Where a window file-drop goes. ONE place decides the target from the
//! current view and modal; project (JRST-205) and comment-composer (JRST-208)
//! tickets add their arms here.

use crate::app::{Modal, View};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropTarget {
    /// Attach to the open issue (routes to `Message::FilesPicked`).
    Issue,
    /// Attach to the open project (routes to `Message::FilesPicked`).
    Project,
    /// Nothing to attach to; the drop is ignored.
    Ignore,
}

pub fn drop_target(view: &View, modal: &Modal) -> DropTarget {
    match (view, modal) {
        (View::IssueDetail(_), Modal::None) => DropTarget::Issue,
        (View::ProjectDetail(_), Modal::None) => DropTarget::Project,
        _ => DropTarget::Ignore,
    }
}

/// Collects the per-file `FileDropped` events of one multi-file drop so they
/// reach the target as a single batch (one refusal toast, spec story 46).
#[derive(Debug, Default)]
pub struct DropBuffer {
    paths: Vec<std::path::PathBuf>,
}

impl DropBuffer {
    /// Add a dropped file. True for the first file of a batch: the caller must
    /// then schedule a flush.
    pub fn push(&mut self, path: std::path::PathBuf) -> bool {
        let first = self.paths.is_empty();
        self.paths.push(path);
        first
    }

    /// Take the whole batch, in arrival order.
    pub fn take(&mut self) -> Vec<std::path::PathBuf> {
        std::mem::take(&mut self.paths)
    }
}

/// How long to wait for the rest of a multi-file drop.
pub const BATCH_WINDOW: std::time::Duration = std::time::Duration::from_millis(150);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_on_open_project_attaches_to_it() {
        assert_eq!(drop_target(&View::ProjectDetail("p".into()), &Modal::None), DropTarget::Project);
        assert_eq!(drop_target(&View::ProjectDetail("p".into()), &Modal::Settings), DropTarget::Ignore);
    }

    #[test]
    fn a_multi_file_drop_flushes_as_one_ordered_batch() {
        let mut b = DropBuffer::default();
        assert!(b.push("a".into()));
        assert!(!b.push("b".into()));
        assert_eq!(b.take(), vec![std::path::PathBuf::from("a"), "b".into()]);
        assert!(b.take().is_empty());
        assert!(b.push("c".into())); // a new batch needs a new flush
    }

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
