//! Keyboard focus inside a form: the Tab order and the menu cursor.
//!
//! A form lists its stops in on-screen order. A [`Stop::Field`] is a text
//! input or editor, which iced focuses itself. A [`Stop::Menu`] is a
//! dropdown chip, which iced can't focus (it is a button), so the app keeps
//! that focus and draws the ring. Tab and Shift+Tab step through the stops,
//! wrapping at the ends, and never leave the open form.

use iced::advanced::widget::operation::{Focusable, Operation, Outcome};
use iced::advanced::widget::Id;
use iced::{Rectangle, Task};

/// A dropdown in a form that the keyboard can focus and open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuId {
    Issue(crate::views::issue_form::FormMenu),
    Project(crate::views::projects::ProjectMenu),
    Recurring(crate::views::recurring_issue_form::RecurringMenu),
}

/// One place Tab can land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// A text input or editor, by its `undo_input` key.
    Field(String),
    Menu(MenuId),
}

impl Stop {
    pub fn field(key: impl Into<String>) -> Self {
        Stop::Field(key.into())
    }
}

/// Where Tab (`forward`) or Shift+Tab goes from `from`. From nothing (or a
/// stop not in `order`), Tab lands on the first stop and Shift+Tab on the
/// last. `None` only when `order` is empty.
pub fn step<'a>(order: &'a [Stop], from: Option<&Stop>, forward: bool) -> Option<&'a Stop> {
    let len = order.len();
    if len == 0 {
        return None;
    }
    let at = from.and_then(|f| order.iter().position(|s| s == f));
    let next = match (at, forward) {
        (Some(i), true) => (i + 1) % len,
        (Some(i), false) => (i + len - 1) % len,
        (None, true) => 0,
        (None, false) => len - 1,
    };
    order.get(next)
}

/// The field keys in `order`, to ask iced which one has focus.
pub fn field_keys(order: &[Stop]) -> Vec<String> {
    order
        .iter()
        .filter_map(|s| match s {
            Stop::Field(k) => Some(k.clone()),
            Stop::Menu(_) => None,
        })
        .collect()
}

/// The keyboard's place in an open menu: the highlighted row, and the
/// first row the menu's scroll shows (kept so the highlight stays in view).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MenuCursor {
    pub row: usize,
    pub top: usize,
}

impl MenuCursor {
    /// On `row` of a menu showing `visible` rows at once, scrolled so `row`
    /// is in view.
    pub fn at(row: usize, visible: usize) -> Self {
        Self { row, top: 0 }.scrolled(visible)
    }

    /// Move by `delta` rows over a menu of `len` rows (wrapping), scrolling
    /// just enough to keep the row among the `visible` ones.
    pub fn moved(self, delta: i32, len: usize, visible: usize) -> Self {
        if len == 0 {
            return self;
        }
        let n = len as i64;
        let row = (self.row as i64 + delta as i64).rem_euclid(n) as usize;
        Self { row, ..self }.scrolled(visible)
    }

    fn scrolled(self, visible: usize) -> Self {
        let visible = visible.max(1);
        let top = if self.row < self.top {
            self.row
        } else if self.row >= self.top + visible {
            self.row + 1 - visible
        } else {
            self.top
        };
        Self { top, ..self }
    }
}

/// What a form's view needs to draw the keyboard's place: the chip it is
/// on, and the cursor row of the open menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Keys {
    pub focused: Option<MenuId>,
    pub cursor: MenuCursor,
}

/// Take focus away from every text input and editor, so a menu stop can
/// hold the keyboard.
pub fn unfocus_all<T: Send + 'static>() -> Task<T> {
    struct UnfocusAll;

    impl<T> Operation<T> for UnfocusAll {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<T>)) {
            operate(self);
        }

        fn focusable(&mut self, _id: Option<&Id>, _bounds: Rectangle, state: &mut dyn Focusable) {
            state.unfocus();
        }

        fn finish(&self) -> Outcome<T> {
            Outcome::None
        }
    }

    iced::advanced::widget::operate(UnfocusAll)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::issue_form::FormMenu;

    fn order() -> Vec<Stop> {
        vec![
            Stop::field("title"),
            Stop::field("body"),
            Stop::Menu(MenuId::Issue(FormMenu::Lane)),
        ]
    }

    #[test]
    fn tab_steps_forward_and_wraps() {
        let o = order();
        assert_eq!(step(&o, Some(&o[0]), true), Some(&o[1]));
        assert_eq!(step(&o, Some(&o[1]), true), Some(&o[2]));
        assert_eq!(step(&o, Some(&o[2]), true), Some(&o[0]));
    }

    #[test]
    fn shift_tab_steps_back_and_wraps() {
        let o = order();
        assert_eq!(step(&o, Some(&o[2]), false), Some(&o[1]));
        assert_eq!(step(&o, Some(&o[0]), false), Some(&o[2]));
    }

    #[test]
    fn from_nothing_tab_goes_first_and_shift_tab_last() {
        let o = order();
        assert_eq!(step(&o, None, true), Some(&o[0]));
        assert_eq!(step(&o, None, false), Some(&o[2]));
        let gone = Stop::field("not-in-the-form");
        assert_eq!(step(&o, Some(&gone), true), Some(&o[0]));
    }

    #[test]
    fn an_empty_order_has_nowhere_to_go() {
        assert_eq!(step(&[], None, true), None);
    }

    #[test]
    fn field_keys_skip_menus() {
        assert_eq!(field_keys(&order()), vec!["title".to_string(), "body".to_string()]);
    }

    #[test]
    fn menu_cursor_wraps_both_ways() {
        let c = MenuCursor::at(0, 10);
        assert_eq!(c.moved(-1, 3, 10).row, 2);
        assert_eq!(c.moved(1, 3, 10).row, 1);
        assert_eq!(MenuCursor::at(2, 10).moved(1, 3, 10).row, 0);
        assert_eq!(c.moved(1, 0, 10).row, 0);
    }

    #[test]
    fn menu_cursor_scrolls_only_to_keep_the_row_in_view() {
        // 3 rows visible of 10.
        let c = MenuCursor::at(0, 3);
        let c = c.moved(1, 10, 3).moved(1, 10, 3);
        assert_eq!((c.row, c.top), (2, 0));
        let c = c.moved(1, 10, 3);
        assert_eq!((c.row, c.top), (3, 1));
        // Back up inside the view: no scroll until the top edge.
        let c = c.moved(-1, 10, 3).moved(-1, 10, 3);
        assert_eq!((c.row, c.top), (1, 1));
        let c = c.moved(-1, 10, 3);
        assert_eq!((c.row, c.top), (0, 0));
        // Wrapping up from the first row shows the last rows.
        let c = c.moved(-1, 10, 3);
        assert_eq!((c.row, c.top), (9, 7));
        // Opening on a far row scrolls it into view.
        assert_eq!(MenuCursor::at(8, 3).top, 6);
    }
}
