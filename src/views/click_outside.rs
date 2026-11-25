//! Tells the app when a left click lands outside an inline field.
//!
//! iced 0.13's `text_editor` and `text_input` drop their own focus on a click
//! outside them, but they send no message. So the open description editor
//! stayed open and buffered edits (title, name, estimate…) sat uncommitted.
//!
//! Wrap the field in [`click_outside`]: the content sees every event as
//! before, and a left press whose cursor isn't over the wrapper publishes
//! `on_outside` (the event isn't captured, so the click still does whatever
//! it was aimed at). Always wrap, and pass `None` while the field is idle:
//! adding or removing the wrapper would change the widget tree and reset the
//! editor's focus mid-edit.

use iced::advanced::layout;
use iced::advanced::mouse;
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::tree::Tree;
use iced::advanced::widget::Operation;
use iced::advanced::{Clipboard, Layout, Shell, Widget};
use iced::{Element, Event, Length, Rectangle, Size, Theme, Vector};

/// Whether `event` is a left press outside `bounds`. An unavailable cursor
/// (the press hit an overlay, or fell outside an enclosing scrollable's
/// viewport) counts as outside. Pure.
pub fn is_outside_press(event: &Event, cursor: mouse::Cursor, bounds: Rectangle) -> bool {
    matches!(event, Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)))
        && !cursor.is_over(bounds)
}

pub struct ClickOutside<'a, Message> {
    content: Element<'a, Message>,
    on_outside: Option<Message>,
}

/// Wrap `content`; while `on_outside` is `Some`, a left click elsewhere publishes it.
pub fn click_outside<'a, Message>(
    content: impl Into<Element<'a, Message>>,
    on_outside: Option<Message>,
) -> ClickOutside<'a, Message> {
    ClickOutside { content: content.into(), on_outside }
}

impl<'a, Message: Clone + 'a> Widget<Message, Theme, iced::Renderer> for ClickOutside<'a, Message> {
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
        if let Some(msg) = &self.on_outside {
            if is_outside_press(event, cursor, layout.bounds()) {
                shell.publish(msg.clone());
            }
        }
        self.content.as_widget_mut().update(
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
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

impl<'a, Message: Clone + 'a> From<ClickOutside<'a, Message>> for Element<'a, Message> {
    fn from(w: ClickOutside<'a, Message>) -> Self {
        Element::new(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Point;

    fn bounds() -> Rectangle {
        Rectangle { x: 10.0, y: 10.0, width: 100.0, height: 20.0 }
    }

    fn press(button: mouse::Button) -> Event {
        Event::Mouse(mouse::Event::ButtonPressed(button))
    }

    #[test]
    fn left_press_outside_counts() {
        let cursor = mouse::Cursor::Available(Point::new(200.0, 15.0));
        assert!(is_outside_press(&press(mouse::Button::Left), cursor, bounds()));
    }

    #[test]
    fn left_press_inside_does_not_count() {
        let cursor = mouse::Cursor::Available(Point::new(20.0, 15.0));
        assert!(!is_outside_press(&press(mouse::Button::Left), cursor, bounds()));
    }

    #[test]
    fn unavailable_cursor_counts_as_outside() {
        assert!(is_outside_press(&press(mouse::Button::Left), mouse::Cursor::Unavailable, bounds()));
    }

    #[test]
    fn other_buttons_and_events_do_not_count() {
        let cursor = mouse::Cursor::Available(Point::new(200.0, 15.0));
        assert!(!is_outside_press(&press(mouse::Button::Right), cursor, bounds()));
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        assert!(!is_outside_press(&release, cursor, bounds()));
    }
}
