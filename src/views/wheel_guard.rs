//! Keeps vertical wheel scrolling from moving a horizontal scrollable (JRST-529).
//!
//! iced 0.13's horizontal-only `Scrollable` turns a mouse wheel's vertical
//! `Lines` delta into horizontal movement. On the board, a lane's own vertical
//! scrollable stops capturing the wheel once it hits its top or bottom, so the
//! overscroll bubbled up to the board and slid it left or right.
//!
//! Wrap the horizontal scrollable's *content* in [`wheel_guard`]: the content
//! sees every event first, as before. A mostly-vertical wheel event it leaves
//! uncaptured is marked captured here, so the scrollable around it ignores it.
//! Horizontal input (trackpad swipes, tilt wheels, and Shift+wheel, which
//! macOS sends with the axes already swapped) still scrolls the board.
//!
//! "Mostly" matters: smooth-scrolling mouse drivers (e.g. Logi Options+) send
//! pixel deltas with a little sideways jitter, like `Pixels { x: 0.4, y: 18 }`.
//! Requiring `x == 0` let those through and nudged the board right.

use iced::advanced::layout;
use iced::advanced::mouse;
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::tree::Tree;
use iced::advanced::widget::Operation;
use iced::advanced::{Clipboard, Layout, Shell, Widget};
use iced::{event, Element, Event, Length, Rectangle, Size, Theme, Vector};

/// Whether a wheel delta is mostly vertical, so a horizontal scrollable must
/// not act on it. Pure.
pub fn is_mostly_vertical(delta: mouse::ScrollDelta) -> bool {
    match delta {
        mouse::ScrollDelta::Lines { x, y } | mouse::ScrollDelta::Pixels { x, y } => {
            y.abs() > x.abs()
        }
    }
}

pub struct WheelGuard<'a, Message> {
    content: Element<'a, Message>,
}

/// Wrap the content of a horizontal scrollable so vertical wheel scrolling
/// never moves it sideways.
pub fn wheel_guard<'a, Message>(content: impl Into<Element<'a, Message>>) -> WheelGuard<'a, Message> {
    WheelGuard { content: content.into() }
}

impl<'a, Message: 'a> Widget<Message, Theme, iced::Renderer> for WheelGuard<'a, Message> {
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
        let vertical_wheel = matches!(
            event,
            Event::Mouse(mouse::Event::WheelScrolled { delta }) if is_mostly_vertical(delta)
        );
        let status = self.content.as_widget_mut().on_event(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if vertical_wheel && cursor.is_over(layout.bounds()) {
            event::Status::Captured
        } else {
            status
        }
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

impl<'a, Message: 'a> From<WheelGuard<'a, Message>> for Element<'a, Message> {
    fn from(guard: WheelGuard<'a, Message>) -> Self {
        Element::new(guard)
    }
}

#[cfg(test)]
mod tests {
    use super::is_mostly_vertical;
    use iced::mouse::ScrollDelta::{Lines, Pixels};

    #[test]
    fn mostly_vertical_deltas_are_swallowed() {
        let cases = [
            (Lines { x: 0.0, y: -1.0 }, true),
            (Lines { x: 0.0, y: 3.0 }, true),
            (Pixels { x: 0.0, y: -12.5 }, true),
            // Shift+wheel on macOS arrives with the axes already swapped.
            (Lines { x: -1.0, y: 0.0 }, false),
            (Pixels { x: 8.0, y: 0.0 }, false),
            // Smooth-scroll mouse drivers add sideways jitter (JRST-529).
            (Pixels { x: 2.0, y: 9.0 }, true),
            (Pixels { x: -0.4, y: -18.0 }, true),
            (Lines { x: 0.1, y: 1.0 }, true),
            (Pixels { x: 9.0, y: 2.0 }, false),
            (Pixels { x: 5.0, y: 5.0 }, false),
            (Lines { x: 0.0, y: 0.0 }, false),
        ];
        for (delta, want) in cases {
            assert_eq!(is_mostly_vertical(delta), want, "{delta:?}");
        }
    }
}
