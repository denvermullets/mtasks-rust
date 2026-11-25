//! Rails for wheel scrolling on the board.
//!
//! Trackpads and smooth-scrolling mice send a little of the other axis with
//! almost every stroke, so a vertical swipe drifted the board sideways
//! (swimlanes' two-axis scrollable takes both) and a sideways swipe nudged a
//! lane up or down. Wrap the board in [`axis_lock`]: a wheel delta whose
//! larger component is at least [`RAIL_RATIO`] times the smaller one is cut
//! down to that axis. Real diagonals pass through untouched, and every event
//! is judged on its own, so nothing sticks between strokes.

use iced::advanced::layout;
use iced::advanced::mouse;
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::tree::Tree;
use iced::advanced::widget::Operation;
use iced::advanced::{Clipboard, Layout, Shell, Widget};
use iced::mouse::ScrollDelta;
use iced::{Element, Event, Length, Rectangle, Size, Theme, Vector};

/// How much larger one axis must be than the other before the smaller is
/// dropped. Below this the stroke counts as diagonal and keeps both.
pub const RAIL_RATIO: f32 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// The axis a delta should be snapped to, or `None` to leave it as is (a zero
/// or diagonal delta). Pure.
pub fn rail(delta: ScrollDelta) -> Option<Axis> {
    let (x, y) = components(delta);
    let (ax, ay) = (x.abs(), y.abs());
    if ay > 0.0 && ay >= ax * RAIL_RATIO {
        Some(Axis::Vertical)
    } else if ax > 0.0 && ax >= ay * RAIL_RATIO {
        Some(Axis::Horizontal)
    } else {
        None
    }
}

/// `delta` with the component off `axis` zeroed. Pure.
pub fn lock_to(delta: ScrollDelta, axis: Axis) -> ScrollDelta {
    let keep = |x: f32, y: f32| match axis {
        Axis::Vertical => (0.0, y),
        Axis::Horizontal => (x, 0.0),
    };
    match delta {
        ScrollDelta::Lines { x, y } => {
            let (x, y) = keep(x, y);
            ScrollDelta::Lines { x, y }
        }
        ScrollDelta::Pixels { x, y } => {
            let (x, y) = keep(x, y);
            ScrollDelta::Pixels { x, y }
        }
    }
}

fn components(delta: ScrollDelta) -> (f32, f32) {
    match delta {
        ScrollDelta::Lines { x, y } | ScrollDelta::Pixels { x, y } => (x, y),
    }
}

pub struct AxisLock<'a, Message> {
    content: Element<'a, Message>,
}

/// Wrap a scrolling region so near-straight wheel strokes stay on their axis.
pub fn axis_lock<'a, Message>(content: impl Into<Element<'a, Message>>) -> AxisLock<'a, Message> {
    AxisLock { content: content.into() }
}

impl<'a, Message: 'a> Widget<Message, Theme, iced::Renderer> for AxisLock<'a, Message> {
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        self.content.as_widget_mut().layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let locked;
        let event = match event {
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if cursor.is_over(layout.bounds()) => {
                match rail(*delta) {
                    Some(axis) => {
                        locked = Event::Mouse(mouse::Event::WheelScrolled { delta: lock_to(*delta, axis) });
                        &locked
                    }
                    None => event,
                }
            }
            _ => event,
        };
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
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
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, Message: 'a> From<AxisLock<'a, Message>> for Element<'a, Message> {
    fn from(lock: AxisLock<'a, Message>) -> Self {
        Element::new(lock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::mouse::ScrollDelta::{Lines, Pixels};

    #[test]
    fn near_straight_strokes_snap_and_diagonals_pass() {
        let cases = [
            (Pixels { x: 0.4, y: 18.0 }, Some(Axis::Vertical)),
            (Pixels { x: 4.0, y: -9.0 }, Some(Axis::Vertical)),
            (Pixels { x: -12.0, y: 3.0 }, Some(Axis::Horizontal)),
            (Lines { x: 0.0, y: -1.0 }, Some(Axis::Vertical)),
            (Lines { x: 1.0, y: 0.0 }, Some(Axis::Horizontal)),
            (Pixels { x: 5.0, y: -5.0 }, None),
            (Pixels { x: 6.0, y: 10.0 }, None),
            (Pixels { x: 0.0, y: 0.0 }, None),
        ];
        for (delta, want) in cases {
            assert_eq!(rail(delta), want, "{delta:?}");
        }
    }

    #[test]
    fn lock_to_zeroes_the_other_axis() {
        assert_eq!(lock_to(Pixels { x: 3.0, y: 9.0 }, Axis::Vertical), Pixels { x: 0.0, y: 9.0 });
        assert_eq!(lock_to(Lines { x: -2.0, y: 1.0 }, Axis::Horizontal), Lines { x: -2.0, y: 0.0 });
    }
}
