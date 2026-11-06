//! A row of equal-width cells that are all as tall as the tallest one.
//!
//! iced 0.13's `row` can't do this: `Row::push` widens the row's height to
//! `Fill` when a child is `Fill`, so `Fill`-height cells make the row (and its
//! parent column) fill. Inside a scrollable that stretches each table row to
//! the viewport. Here each cell is measured at its natural height first, then
//! laid out again at exactly the tallest height. Children must be
//! `Shrink`-height: pass 2's exact limits stretch a `Shrink` container's
//! background to the row. A `Fill` child would measure unbounded in pass 1, so
//! non-finite measurements are ignored rather than poisoning the row.

use iced::advanced::layout;
use iced::advanced::mouse;
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::tree::Tree;
use iced::advanced::widget::Operation;
use iced::advanced::{Clipboard, Layout, Shell, Widget};
use iced::{event, Element, Event, Length, Point, Rectangle, Size, Theme, Vector};

pub struct EqualRow<'a, Message> {
    children: Vec<Element<'a, Message>>,
    spacing: f32,
}

/// Equal-width, equal-height cells separated by `spacing`.
pub fn equal_row<'a, Message>(children: Vec<Element<'a, Message>>, spacing: f32) -> EqualRow<'a, Message> {
    EqualRow { children, spacing }
}

/// Width of each of `n` cells sharing `total` with `spacing` between them.
fn cell_width(total: f32, n: usize, spacing: f32) -> f32 {
    if n == 0 {
        return 0.0;
    }
    ((total - spacing * (n - 1) as f32) / n as f32).max(0.0)
}

impl<'a, Message: 'a> Widget<Message, Theme, iced::Renderer> for EqualRow<'a, Message> {
    fn children(&self) -> Vec<Tree> {
        self.children.iter().map(Tree::new).collect()
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&self.children);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn layout(&self, tree: &mut Tree, renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        let total = limits.max().width;
        let w = cell_width(total, self.children.len(), self.spacing);
        // Pass 1: natural height of each cell at its width.
        let measure = layout::Limits::new(Size::new(w, 0.0), Size::new(w, f32::INFINITY));
        let h = self
            .children
            .iter()
            .zip(tree.children.iter_mut())
            .map(|(c, t)| c.as_widget().layout(t, renderer, &measure).size().height)
            .filter(|h| h.is_finite())
            .fold(0.0_f32, f32::max);
        // Pass 2: every cell at exactly the tallest height.
        let exact = layout::Limits::new(Size::new(w, h), Size::new(w, h));
        let nodes = self
            .children
            .iter()
            .zip(tree.children.iter_mut())
            .enumerate()
            .map(|(i, (c, t))| {
                c.as_widget()
                    .layout(t, renderer, &exact)
                    .move_to(Point::new(i as f32 * (w + self.spacing), 0.0))
            })
            .collect();
        layout::Node::with_children(Size::new(total, h), nodes)
    }

    fn operate(&self, tree: &mut Tree, layout: Layout<'_>, renderer: &iced::Renderer, operation: &mut dyn Operation) {
        operation.container(None, layout.bounds(), &mut |operation| {
            for ((c, t), l) in self.children.iter().zip(tree.children.iter_mut()).zip(layout.children()) {
                c.as_widget().operate(t, l, renderer, operation);
            }
        });
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
        self.children
            .iter_mut()
            .zip(tree.children.iter_mut())
            .zip(layout.children())
            .map(|((c, t), l)| {
                c.as_widget_mut().on_event(t, event.clone(), l, cursor, renderer, clipboard, shell, viewport)
            })
            .fold(event::Status::Ignored, event::Status::merge)
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.children
            .iter()
            .zip(tree.children.iter())
            .zip(layout.children())
            .map(|((c, t), l)| c.as_widget().mouse_interaction(t, l, cursor, viewport, renderer))
            .max()
            .unwrap_or_default()
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
        for ((c, t), l) in self.children.iter().zip(tree.children.iter()).zip(layout.children()) {
            if l.bounds().intersects(viewport) {
                c.as_widget().draw(t, renderer, theme, style, l, cursor, viewport);
            }
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        overlay::from_children(&mut self.children, tree, layout, renderer, translation)
    }
}

impl<'a, Message: 'a> From<EqualRow<'a, Message>> for Element<'a, Message> {
    fn from(row: EqualRow<'a, Message>) -> Self {
        Element::new(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_split_the_width_after_spacing() {
        assert_eq!(cell_width(301.0, 3, 1.0), 99.666_664);
        assert_eq!(cell_width(100.0, 1, 1.0), 100.0);
        assert_eq!(cell_width(100.0, 0, 1.0), 0.0);
        assert_eq!(cell_width(1.0, 3, 1.0), 0.0);
    }
}
