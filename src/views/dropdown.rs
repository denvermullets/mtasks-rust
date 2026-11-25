//! A click-to-open menu anchored under a button, drawn above everything else
//! (iced has no dropdown widget, and `pick_list` menus are only as wide as the
//! control). The owner keeps the open state: pass `menu: Some(..)` while open.
//!
//! A press outside the menu dismisses it and is swallowed, like a native menu,
//! except on the anchor, which is left to toggle the state itself. Escape also
//! dismisses.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::tree::Tree;
use iced::advanced::widget::Operation;
use iced::advanced::{mouse, Clipboard, Shell, Widget};
use iced::{keyboard, Element, Event, Length, Point, Rectangle, Size, Theme, Vector};

use crate::theme::space;

/// Gap between the anchor and the menu.
const GAP: f32 = space::S1;

pub struct Dropdown<'a, Message> {
    anchor: Element<'a, Message>,
    menu: Option<Element<'a, Message>>,
    menu_width: f32,
    on_dismiss: Message,
    align_left: bool,
}

/// `anchor` with `menu` (right-aligned to it, `menu_width` wide) open below it,
/// or above it when there is no room.
pub fn dropdown<'a, Message>(
    anchor: impl Into<Element<'a, Message>>,
    menu: Option<Element<'a, Message>>,
    menu_width: f32,
    on_dismiss: Message,
) -> Dropdown<'a, Message> {
    Dropdown { anchor: anchor.into(), menu, menu_width, on_dismiss, align_left: false }
}

impl<Message> Dropdown<'_, Message> {
    /// Open the menu flush with the anchor's left edge (web `left-0`).
    pub fn align_left(mut self) -> Self {
        self.align_left = true;
        self
    }
}

impl<'a, Message: Clone + 'a> Widget<Message, Theme, iced::Renderer> for Dropdown<'a, Message> {
    fn children(&self) -> Vec<Tree> {
        std::iter::once(&self.anchor).chain(self.menu.as_ref()).map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        let children: Vec<&Element<'a, Message>> =
            std::iter::once(&self.anchor).chain(self.menu.as_ref()).collect();
        tree.diff_children(&children);
    }

    fn size(&self) -> Size<Length> {
        self.anchor.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        self.anchor.as_widget_mut().layout(&mut tree.children[0], renderer, limits)
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
        self.anchor.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.anchor.as_widget_mut().operate(&mut tree.children[0], layout, renderer, operation);
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
        self.anchor.as_widget_mut().update(
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
        self.anchor.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        _renderer: &iced::Renderer,
        _viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        let content = self.menu.as_mut()?;
        Some(overlay::Element::new(Box::new(Menu {
            content,
            tree: &mut tree.children[1],
            anchor: layout.bounds() + translation,
            width: self.menu_width,
            on_dismiss: self.on_dismiss.clone(),
            align_left: self.align_left,
        })))
    }
}

impl<'a, Message: Clone + 'a> From<Dropdown<'a, Message>> for Element<'a, Message> {
    fn from(d: Dropdown<'a, Message>) -> Self {
        Element::new(d)
    }
}

struct Menu<'a, 'b, Message> {
    content: &'b mut Element<'a, Message>,
    tree: &'b mut Tree,
    /// The anchor in window space.
    anchor: Rectangle,
    width: f32,
    on_dismiss: Message,
    align_left: bool,
}

/// Where the menu goes: right-aligned (or left-aligned) under the anchor,
/// flipped above it when it would run off the window bottom, always kept
/// inside the window. Pure.
fn place(anchor: Rectangle, menu: Size, window: Size, align_left: bool) -> Point {
    let below = anchor.y + anchor.height + GAP;
    let y = if below + menu.height > window.height { anchor.y - menu.height - GAP } else { below };
    let x = if align_left { anchor.x } else { anchor.x + anchor.width - menu.width };
    Point::new(x.min(window.width - menu.width).max(0.0), y.max(0.0))
}

impl<'a, 'b, Message: Clone> overlay::Overlay<Message, Theme, iced::Renderer> for Menu<'a, 'b, Message> {
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        let limits = layout::Limits::new(Size::ZERO, bounds).width(Length::Fixed(self.width));
        let node = self.content.as_widget_mut().layout(self.tree, renderer, &limits);
        let position = place(self.anchor, node.size(), bounds, self.align_left);
        node.move_to(position)
    }

    fn draw(
        &self,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        self.content.as_widget().draw(self.tree, renderer, theme, style, layout, cursor, &layout.bounds());
    }

    fn operate(
        &mut self,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content.as_widget_mut().operate(self.tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
    ) {
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }) => {
                shell.publish(self.on_dismiss.clone());
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::ButtonPressed(_))
                if !cursor.is_over(layout.bounds()) =>
            {
                if cursor.is_over(self.anchor) {
                    return;
                }
                shell.publish(self.on_dismiss.clone());
                shell.capture_event();
                return;
            }
            _ => {}
        }
        let viewport = layout.bounds();
        self.content.as_widget_mut().update(
            self.tree, event, layout, cursor, renderer, clipboard, shell, &viewport,
        )
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(self.tree, layout, cursor, &layout.bounds(), renderer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Size = Size::new(800.0, 600.0);
    const MENU: Size = Size::new(160.0, 100.0);

    fn anchor(x: f32, y: f32) -> Rectangle {
        Rectangle { x, y, width: 24.0, height: 24.0 }
    }

    #[test]
    fn opens_below_right_aligned_to_the_anchor() {
        let p = place(anchor(400.0, 100.0), MENU, WINDOW, false);
        assert_eq!(p, Point::new(424.0 - 160.0, 124.0 + GAP));
    }

    #[test]
    fn flips_above_when_there_is_no_room_below() {
        let p = place(anchor(400.0, 560.0), MENU, WINDOW, false);
        assert_eq!(p.y, 560.0 - 100.0 - GAP);
    }

    #[test]
    fn left_aligned_opens_flush_with_the_anchor() {
        let p = place(anchor(400.0, 100.0), MENU, WINDOW, true);
        assert_eq!(p, Point::new(400.0, 124.0 + GAP));
        let p = place(anchor(700.0, 100.0), MENU, WINDOW, true);
        assert_eq!(p.x, 800.0 - 160.0);
    }

    #[test]
    fn stays_inside_the_window() {
        let p = place(anchor(10.0, 20.0), Size::new(160.0, 700.0), WINDOW, false);
        assert_eq!(p, Point::new(0.0, 0.0));
    }
}
