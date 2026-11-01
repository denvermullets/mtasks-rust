//! Hover-card anchor for a whole row (JRST-312, 313, 314).
//!
//! Wraps any element and reports when the pointer enters or leaves its bounds,
//! together with the row's rectangle in window pixels, which `mouse_area`
//! cannot give (it has enter and exit but no bounds). Chosen over a
//! `mouse_area` plus a cursor subscription because no per-move app message is
//! sent: the widget watches events itself and publishes only when "inside"
//! changes.
//!
//! The rectangle comes the way `hover_rich` does it: inside a scrollable the
//! layout and the cursor are both in content space, so their difference is
//! the row's offset from the pointer in window pixels. Adding the last raw
//! `CursorMoved` position (window space, passed through scrollables
//! untranslated) gives the window rectangle. Enter always follows a pointer
//! move, so that position is known when it matters.
//!
//! The widget never captures an event, so the row's own buttons (open, remove)
//! and focus behave as before. Hand the result to `BodyHover::body_hover` with
//! the widget's owner id, like a body span.

use iced::advanced::layout;
use iced::advanced::mouse;
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::widget::Operation;
use iced::advanced::{Clipboard, Layout, Shell, Widget};
use iced::{event, Element, Event, Length, Point, Rectangle, Size, Theme, Vector};

use crate::body_hover::Anchor;
use crate::views::hover_rich::window_rect;

/// Source of owner ids. Starts high so a row's id can never equal a body
/// widget's (those count up from 1 in `hover_rich`).
static NEXT_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 32);

/// Remembers whether the pointer is inside so only changes are reported. Pure.
#[derive(Debug, Default)]
pub struct InsideTracker {
    inside: bool,
}

impl InsideTracker {
    /// `Some(now_inside)` when the state changed since the last call.
    pub fn observe(&mut self, inside: bool) -> Option<bool> {
        (self.inside != inside).then(|| {
            self.inside = inside;
            inside
        })
    }
}

type OnHover<'a, Message> = Box<dyn Fn(u64, Option<(Anchor, Rectangle)>) -> Message + 'a>;

pub struct RowHover<'a, Message> {
    content: Element<'a, Message>,
    anchor: Anchor,
    on_hover: OnHover<'a, Message>,
}

/// Wrap `content` as the hover anchor for `anchor`. `on_hover` gets this
/// widget's owner id and `Some((anchor, window rectangle))` on enter, `None`
/// on leave.
pub fn row_hover<'a, Message>(
    content: impl Into<Element<'a, Message>>,
    anchor: Anchor,
    on_hover: impl Fn(u64, Option<(Anchor, Rectangle)>) -> Message + 'a,
) -> RowHover<'a, Message> {
    RowHover { content: content.into(), anchor, on_hover: Box::new(on_hover) }
}

struct State {
    inside: InsideTracker,
    owner: u64,
    /// Last pointer position seen in a raw `CursorMoved` (window space).
    window_cursor: Option<Point>,
}

impl<'a, Message: 'a> Widget<Message, Theme, iced::Renderer> for RowHover<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State {
            inside: InsideTracker::default(),
            owner: NEXT_OWNER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            window_cursor: None,
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(&self, tree: &mut Tree, renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        self.content.as_widget().layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget().operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) -> event::Status {
        if let Event::Mouse(mouse_event) = &event {
            let state = tree.state.downcast_mut::<State>();
            if let mouse::Event::CursorMoved { position } = mouse_event {
                state.window_cursor = Some(*position);
            }
            let bounds = layout.bounds();
            if let Some(entered) = state.inside.observe(cursor.is_over(bounds)) {
                let hover = if entered {
                    // Content space inside a scrollable; window space otherwise.
                    // Without a seen move, fall back to the layout bounds.
                    let rect = match (cursor.position(), state.window_cursor) {
                        (Some(at), Some(w)) => window_rect(bounds, at, w),
                        _ => bounds,
                    };
                    Some((self.anchor.clone(), rect))
                } else {
                    None
                };
                shell.publish((self.on_hover)(state.owner, hover));
            }
        }

        // Never captures: the row's own buttons and focus are untouched.
        self.content.as_widget_mut().on_event(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        )
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, translation)
    }
}

impl<'a, Message: 'a> From<RowHover<'a, Message>> for Element<'a, Message> {
    fn from(row: RowHover<'a, Message>) -> Self {
        Element::new(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_only_changes_of_inside() {
        let mut t = InsideTracker::default();
        let rows = [
            (false, None),
            (true, Some(true)),
            (true, None),
            (true, None),
            (false, Some(false)),
            (false, None),
            (true, Some(true)),
        ];
        for (inside, want) in rows {
            assert_eq!(t.observe(inside), want, "observe({inside})");
        }
    }
}
