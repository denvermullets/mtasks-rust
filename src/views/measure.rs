//! A wrapper that reports its own laid-out size.
//!
//! Some sizes depend on everything stacked around a widget (the tab bar, the
//! header, a status bar) and aren't worth re-deriving by hand. `measure` lays
//! its child out as usual and, on the next event it sees (every frame gets a
//! `RedrawRequested`), publishes `on_size(size)` when the size changed since it
//! last said. The first report comes after the first frame.

use iced::advanced::layout;
use iced::advanced::mouse;
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::widget::Operation;
use iced::advanced::{Clipboard, Layout, Shell, Widget};
use iced::{event, Element, Event, Length, Rectangle, Size, Theme, Vector};

pub struct Measure<'a, Message> {
    content: Element<'a, Message>,
    on_size: Box<dyn Fn(Size) -> Message + 'a>,
}

/// `content`, reporting each new size through `on_size`.
pub fn measure<'a, Message>(
    content: impl Into<Element<'a, Message>>,
    on_size: impl Fn(Size) -> Message + 'a,
) -> Measure<'a, Message> {
    Measure { content: content.into(), on_size: Box::new(on_size) }
}

/// The size last reported.
#[derive(Default)]
struct Reported(Option<Size>);

/// Whether a size `now` is news after `last`: half a pixel of jitter isn't.
fn changed(last: Option<Size>, now: Size) -> bool {
    last.is_none_or(|l| (l.width - now.width).abs() >= 0.5 || (l.height - now.height).abs() >= 0.5)
}

impl<'a, Message: 'a> Widget<Message, Theme, iced::Renderer> for Measure<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Reported>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Reported::default())
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
        let child = self.content.as_widget().layout(&mut tree.children[0], renderer, limits);
        layout::Node::with_children(child.size(), vec![child])
    }

    fn operate(&self, tree: &mut Tree, layout: Layout<'_>, renderer: &iced::Renderer, operation: &mut dyn Operation) {
        let child = layout.children().next().expect("measure has one child");
        self.content.as_widget().operate(&mut tree.children[0], child, renderer, operation);
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
        let size = layout.bounds().size();
        let reported = tree.state.downcast_mut::<Reported>();
        if changed(reported.0, size) {
            reported.0 = Some(size);
            shell.publish((self.on_size)(size));
        }
        let child = layout.children().next().expect("measure has one child");
        self.content
            .as_widget_mut()
            .on_event(&mut tree.children[0], event, child, cursor, renderer, clipboard, shell, viewport)
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let child = layout.children().next().expect("measure has one child");
        self.content.as_widget().mouse_interaction(&tree.children[0], child, cursor, viewport, renderer)
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
        let child = layout.children().next().expect("measure has one child");
        self.content.as_widget().draw(&tree.children[0], renderer, theme, style, child, cursor, viewport);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        let child = layout.children().next().expect("measure has one child");
        self.content.as_widget_mut().overlay(&mut tree.children[0], child, renderer, translation)
    }
}

impl<'a, Message: 'a> From<Measure<'a, Message>> for Element<'a, Message> {
    fn from(m: Measure<'a, Message>) -> Self {
        Element::new(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_real_change_is_reported() {
        let a = Size::new(500.0, 800.0);
        assert!(changed(None, a));
        assert!(!changed(Some(a), Size::new(500.2, 800.3)));
        assert!(changed(Some(a), Size::new(500.0, 760.0)));
    }
}
