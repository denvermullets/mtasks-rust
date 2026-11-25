//! A right-click menu over `content`, opened at the pointer. The widget keeps
//! its own open state (like `pick_list`), so opening it is a redraw, not an
//! app message and a full view rebuild.
//!
//! A press outside the menu, or Escape, dismisses it (the press is swallowed,
//! like a native menu). A click in the menu closes it, after an enabled item
//! publishes its message. While open, `content`'s own overlays (tooltips) are hidden.

use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::widget::Operation;
use iced::advanced::{mouse, Clipboard, Shell, Widget};
use iced::{keyboard, Element, Event, Length, Point, Rectangle, Size, Theme, Vector};

pub struct ContextMenu<'a, Message> {
    content: Element<'a, Message>,
    menu: Element<'a, Message>,
    menu_width: f32,
}

/// `content` that opens `menu` (`menu_width` wide) at the pointer on a
/// right-click its children don't handle.
pub fn context_menu<'a, Message>(
    content: impl Into<Element<'a, Message>>,
    menu: impl Into<Element<'a, Message>>,
    menu_width: f32,
) -> ContextMenu<'a, Message> {
    ContextMenu { content: content.into(), menu: menu.into(), menu_width }
}

/// Where the menu opened, in the content's layout space; `None` while closed.
#[derive(Default)]
struct State {
    open_at: Option<Point>,
}

fn reflow<Message>(shell: &mut Shell<'_, Message>) {
    shell.invalidate_layout();
    shell.request_redraw();
}

impl<'a, Message: Clone + 'a> Widget<Message, Theme, iced::Renderer> for ContextMenu<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content), Tree::new(&self.menu)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.content, &self.menu]);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        self.content.as_widget_mut().layout(&mut tree.children[0], renderer, limits)
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
        if shell.is_event_captured() {
            return;
        }
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)) = event {
            if let Some(position) = cursor.position_over(layout.bounds()) {
                tree.state.downcast_mut::<State>().open_at = Some(position);
                reflow(shell);
                shell.capture_event();
            }
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

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        let Some(at) = tree.state.downcast_ref::<State>().open_at else {
            return self.content.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, viewport, translation);
        };
        let (_, rest) = tree.children.split_at_mut(1);
        Some(overlay::Element::new(Box::new(Menu {
            content: &mut self.menu,
            tree: &mut rest[0],
            state: tree.state.downcast_mut::<State>(),
            at: at + translation,
            width: self.menu_width,
        })))
    }
}

impl<'a, Message: Clone + 'a> From<ContextMenu<'a, Message>> for Element<'a, Message> {
    fn from(m: ContextMenu<'a, Message>) -> Self {
        Element::new(m)
    }
}

struct Menu<'a, 'b, Message> {
    content: &'b mut Element<'a, Message>,
    tree: &'b mut Tree,
    state: &'b mut State,
    /// The pointer when the menu opened, in window space.
    at: Point,
    width: f32,
}

/// Where the menu goes: its top-left corner at the pointer, flipped left or up
/// when it would run off the window, always kept inside it. Pure.
fn place(at: Point, menu: Size, window: Size) -> Point {
    let x = if at.x + menu.width > window.width { at.x - menu.width } else { at.x };
    let y = if at.y + menu.height > window.height { at.y - menu.height } else { at.y };
    Point::new(x.min(window.width - menu.width).max(0.0), y.min(window.height - menu.height).max(0.0))
}

impl<'a, 'b, Message: Clone> overlay::Overlay<Message, Theme, iced::Renderer> for Menu<'a, 'b, Message> {
    fn layout(&mut self, renderer: &iced::Renderer, bounds: Size) -> layout::Node {
        let limits = layout::Limits::new(Size::ZERO, bounds).width(Length::Fixed(self.width));
        let node = self.content.as_widget_mut().layout(self.tree, renderer, &limits);
        let position = place(self.at, node.size(), bounds);
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

    fn operate(&mut self, layout: Layout<'_>, renderer: &iced::Renderer, operation: &mut dyn Operation) {
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
        let over = cursor.is_over(layout.bounds());
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }) => {
                self.state.open_at = None;
                reflow(shell);
                shell.capture_event();
                return;
            }
            Event::Mouse(mouse::Event::ButtonPressed(button)) if !over => {
                // A right-click elsewhere in the content moves the menu there.
                self.state.open_at = None;
                reflow(shell);
                if *button != mouse::Button::Right {
                    shell.capture_event();
                }
                return;
            }
            _ => {}
        }
        let viewport = layout.bounds();
        self.content.as_widget_mut().update(self.tree, event, layout, cursor, renderer, clipboard, shell, &viewport);
        // A click in the menu (buttons publish on release) closes it.
        if over && matches!(event, Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))) {
            self.state.open_at = None;
            reflow(shell);
        }
    }

    fn mouse_interaction(&self, layout: Layout<'_>, cursor: mouse::Cursor, renderer: &iced::Renderer) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(self.tree, layout, cursor, &layout.bounds(), renderer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Size = Size::new(800.0, 600.0);
    const MENU: Size = Size::new(200.0, 40.0);

    #[test]
    fn opens_at_the_pointer() {
        assert_eq!(place(Point::new(300.0, 20.0), MENU, WINDOW), Point::new(300.0, 20.0));
    }

    #[test]
    fn flips_left_near_the_right_edge() {
        assert_eq!(place(Point::new(700.0, 20.0), MENU, WINDOW), Point::new(500.0, 20.0));
    }

    #[test]
    fn flips_up_near_the_bottom() {
        assert_eq!(place(Point::new(300.0, 590.0), MENU, WINDOW), Point::new(300.0, 550.0));
    }
}
