//! Board overlay plumbing: card anchors and on-screen bounds collection.
//!
//! Board cards vary in height, the board scrolls horizontally and every column
//! scrolls vertically, so card rects can't be computed; they come from layout.
//!
//! * [`dep_anchor`] wraps a card and reports `(issue id, layout bounds)` to any
//!   `Operation` that walks past it (via `Operation::custom`).
//! * [`dep_overlay`] wraps the board. On every `RedrawRequested` it runs a
//!   collecting `Operation` over its content and keeps the result in its tree
//!   state; `draw` paints the board, then whatever the overlay is asked to draw
//!   (debug outlines for now; connectors in JRST-450) on top.
//!
//! Findings (iced 0.13.2): `Widget::operate` receives *untranslated* layout.
//! `Scrollable::operate` calls `operation.scrollable(.., bounds, content_bounds,
//! translation)` and then `operation.container(.., bounds, ..)` for its content,
//! so an operation sees every enclosing scrollable's viewport and offset. A
//! child's on-screen position is `layout position - sum(translations)`.
//! `operate` takes `&mut Tree` but `draw` only gets `&Tree`, so collection runs
//! in `on_event(RedrawRequested)`, which is dispatched right before `draw` in
//! the same frame (so scrolling never lags). Every event is forwarded to the
//! content untouched, so clicks, drags and scrolls are unaffected.

use std::any::Any;

use iced::advanced::layout;
use iced::advanced::mouse;
use iced::advanced::overlay;
use iced::advanced::renderer::{self, Quad};
use iced::advanced::widget::operation::Outcome;
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::widget::{Id, Operation};
use iced::advanced::{Clipboard, Layout, Renderer as _, Shell, Widget};
use iced::window;
use iced::{event, Border, Color, Element, Event, Length, Rectangle, Size, Theme, Vector};

// ---------------------------------------------------------------------------
// Pure translation / clipping math

/// One enclosing scrollable, as an `Operation` sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollFrame {
    /// The scrollable's viewport in its parent's (untranslated) layout space.
    pub viewport: Rectangle,
    /// Its scroll offset (positive = scrolled right/down).
    pub translation: Vector,
}

/// Where an anchor ended up.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub id: String,
    /// On-screen rect, ignoring clipping (may lie outside its viewport).
    pub rect: Rectangle,
    /// `rect` clipped to every enclosing viewport; `None` when fully hidden.
    pub visible: Option<Rectangle>,
}

/// Resolve a `raw` layout rect inside `frames` (outermost first) to its
/// on-screen rect and visible part.
pub fn resolve(raw: Rectangle, frames: &[ScrollFrame]) -> (Rectangle, Option<Rectangle>) {
    let mut offset = Vector::new(0.0, 0.0);
    let mut clip: Option<Rectangle> = None;
    let mut first = true;
    let mut hidden = false;
    for f in frames {
        let vp = Rectangle { x: f.viewport.x - offset.x, y: f.viewport.y - offset.y, ..f.viewport };
        if first {
            clip = Some(vp);
            first = false;
        } else {
            match clip.and_then(|c| c.intersection(&vp)) {
                Some(c) => clip = Some(c),
                None => hidden = true,
            }
        }
        offset = offset + f.translation;
    }
    let rect = Rectangle { x: raw.x - offset.x, y: raw.y - offset.y, ..raw };
    let visible = if hidden {
        None
    } else {
        match clip {
            Some(c) => c.intersection(&rect).filter(|r| r.width > 0.0 && r.height > 0.0),
            None => Some(rect),
        }
    };
    (rect, visible)
}

// ---------------------------------------------------------------------------
// Anchor widget

/// What an anchor hands to an `Operation::custom` call.
pub struct AnchorReport {
    pub id: String,
    pub bounds: Rectangle,
}

pub struct DepAnchor<'a, Message> {
    id: String,
    content: Element<'a, Message>,
}

/// Tag `card`'s bounds with `issue_id`. Layout, drawing and events pass through.
pub fn dep_anchor<'a, Message>(
    issue_id: impl Into<String>,
    card: impl Into<Element<'a, Message>>,
) -> DepAnchor<'a, Message> {
    DepAnchor { id: issue_id.into(), content: card.into() }
}

impl<'a, Message: 'a> Widget<Message, Theme, iced::Renderer> for DepAnchor<'a, Message> {
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
        let mut report = AnchorReport { id: self.id.clone(), bounds: layout.bounds() };
        operation.custom(&mut report, None);
        let child = layout.children().next().expect("anchor has one child");
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
        let child = layout.children().next().expect("anchor has one child");
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
        let child = layout.children().next().expect("anchor has one child");
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
        let child = layout.children().next().expect("anchor has one child");
        self.content.as_widget().draw(&tree.children[0], renderer, theme, style, child, cursor, viewport);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        let child = layout.children().next().expect("anchor has one child");
        self.content.as_widget_mut().overlay(&mut tree.children[0], child, renderer, translation)
    }
}

impl<'a, Message: 'a> From<DepAnchor<'a, Message>> for Element<'a, Message> {
    fn from(a: DepAnchor<'a, Message>) -> Self {
        Element::new(a)
    }
}

// ---------------------------------------------------------------------------
// Collecting operation

#[derive(Default)]
struct Collector {
    stack: Vec<ScrollFrame>,
    /// Set by `scrollable`, consumed by the `container` call that follows it.
    pending: Option<ScrollFrame>,
    found: Vec<Placed>,
}

impl Operation for Collector {
    fn container(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        operate_on_children: &mut dyn FnMut(&mut dyn Operation),
    ) {
        match self.pending.take() {
            Some(frame) => {
                self.stack.push(frame);
                operate_on_children(self);
                self.stack.pop();
            }
            None => operate_on_children(self),
        }
    }

    fn scrollable(
        &mut self,
        _state: &mut dyn iced::advanced::widget::operation::Scrollable,
        _id: Option<&Id>,
        bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
    ) {
        self.pending = Some(ScrollFrame { viewport: bounds, translation });
    }

    fn custom(&mut self, state: &mut dyn Any, _id: Option<&Id>) {
        if let Some(a) = state.downcast_mut::<AnchorReport>() {
            let (rect, visible) = resolve(a.bounds, &self.stack);
            self.found.push(Placed { id: a.id.clone(), rect, visible });
        }
    }

    fn finish(&self) -> Outcome<()> {
        Outcome::None
    }
}

// ---------------------------------------------------------------------------
// Overlay widget

/// Anchors collected on the last redraw. Rects are relative to the overlay's
/// top-left corner.
#[derive(Debug, Default, Clone)]
pub struct Anchors(pub Vec<Placed>);

impl Anchors {
    pub fn get(&self, id: &str) -> Option<&Placed> {
        self.0.iter().find(|p| p.id == id)
    }

    /// The card whose visible part contains `point` (overlay-relative).
    pub fn hit(&self, point: iced::Point) -> Option<&Placed> {
        self.0.iter().find(|p| p.visible.is_some_and(|v| v.contains(point)))
    }
}

/// Paints on top of the board. Gets the renderer, the overlay's absolute
/// bounds (add its top-left to anchor rects) and the collected anchors.
pub type DrawFn<'a> = Box<dyn Fn(&mut iced::Renderer, Rectangle, &Anchors) + 'a>;

pub struct DepOverlay<'a, Message> {
    content: Element<'a, Message>,
    draw_over: Option<DrawFn<'a>>,
}

/// Wrap the board.
pub fn dep_overlay<'a, Message>(board: impl Into<Element<'a, Message>>) -> DepOverlay<'a, Message> {
    DepOverlay { content: board.into(), draw_over: None }
}

impl<'a, Message> DepOverlay<'a, Message> {
    /// Draw over the board each frame (connectors, JRST-450).
    pub fn draw_over(mut self, f: impl Fn(&mut iced::Renderer, Rectangle, &Anchors) + 'a) -> Self {
        self.draw_over = Some(Box::new(f));
        self
    }
}

impl<'a, Message: 'a> Widget<Message, Theme, iced::Renderer> for DepOverlay<'a, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Anchors>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Anchors::default())
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
        let child = layout.children().next().expect("overlay has one child");
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
        let child = layout.children().next().expect("overlay has one child");
        if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
            let origin = layout.bounds();
            let mut collector = Collector::default();
            self.content.as_widget().operate(&mut tree.children[0], child, renderer, &mut collector);
            let to_local = |r: Rectangle| Rectangle { x: r.x - origin.x, y: r.y - origin.y, ..r };
            let anchors = collector
                .found
                .into_iter()
                .map(|p| Placed {
                    id: p.id,
                    rect: to_local(p.rect),
                    // Also clip to the overlay's own bounds.
                    visible: p.visible.and_then(|v| v.intersection(&origin)).map(to_local),
                })
                .collect();
            *tree.state.downcast_mut::<Anchors>() = Anchors(anchors);
        }
        // JRST-452 hook: a card click (open the Dependency map) belongs here.
        // Match `Event::Mouse(ButtonReleased(Left))` on `cursor.position()`,
        // find the card with `Anchors::hit(..)` (rects are overlay-relative,
        // `layout.bounds()` is the origin) and `shell.publish(..)` a message
        // when the card has lines. Forward the event afterwards as below, so
        // the card's own press handling keeps working.
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
        let child = layout.children().next().expect("overlay has one child");
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
        let child = layout.children().next().expect("overlay has one child");
        self.content.as_widget().draw(&tree.children[0], renderer, theme, style, child, cursor, viewport);

        let bounds = layout.bounds();
        let anchors = tree.state.downcast_ref::<Anchors>();
        renderer.with_layer(bounds, |renderer| {
            if let Some(f) = &self.draw_over {
                f(renderer, bounds, anchors);
            }
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        let child = layout.children().next().expect("overlay has one child");
        self.content.as_widget_mut().overlay(&mut tree.children[0], child, renderer, translation)
    }
}

impl<'a, Message: 'a> From<DepOverlay<'a, Message>> for Element<'a, Message> {
    fn from(o: DepOverlay<'a, Message>) -> Self {
        Element::new(o)
    }
}

// ---------------------------------------------------------------------------
// Board connectors (JRST-450)

/// What the board overlay draws: the edges between visible cards.
#[derive(Debug, Clone, Default)]
pub struct DepLines {
    pub edges: Vec<crate::dep_connectors::EdgeSpec>,
    /// Issue id -> linked ids (from `dep_graph::board_edges`).
    pub neighbors: std::collections::HashMap<String, std::collections::HashSet<String>>,
}

impl DepLines {
    /// True when the card has at least one line on the board.
    pub fn is_linked(&self, id: &str) -> bool {
        self.neighbors.get(id).is_some_and(|n| !n.is_empty())
    }

    /// Build from the stored dependencies and the ids of the cards on screen.
    pub fn new<I, S>(visible_ids: I, deps: &[crate::db::models::IssueDependency]) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        use crate::dep_connectors::{ConnectorKind, EdgeSpec};
        let be = crate::dep_graph::board_edges(visible_ids, deps);
        let edges = be
            .edges
            .iter()
            .filter_map(|e| {
                let kind = match e.kind.as_str() {
                    "blocks" => ConnectorKind::Blocks,
                    "relates" => ConnectorKind::Relates,
                    "duplicates" => ConnectorKind::Duplicates,
                    _ => return None,
                };
                Some(EdgeSpec { from_id: e.from.clone(), to_id: e.to.clone(), kind })
            })
            .collect();
        Self { edges, neighbors: be.neighbors }
    }
}

/// Drawing options for the board while `hovered` is under the pointer
/// (JRST-451): lines not touching it dim and its incoming `blocks` lines read
/// as "blocked by". Cards without lines change nothing.
pub fn hover_options(hovered: Option<&str>, lines: &DepLines) -> crate::dep_connectors::DrawOptions {
    let mut opts = crate::dep_connectors::DrawOptions {
        bow: Some(crate::dep_connectors::BOARD_BOW),
        ..Default::default()
    };
    if let Some(id) = hovered.filter(|id| lines.neighbors.contains_key(*id)) {
        opts.focus_id = Some(id.to_string());
        opts.highlight_id = Some(id.to_string());
    }
    opts
}

/// Ring the hovered card (accent, 2px) and its neighbors (accent at 60%, 1px).
pub fn draw_hover_rings(
    renderer: &mut iced::Renderer,
    bounds: Rectangle,
    anchors: &Anchors,
    hovered: &str,
    lines: &DepLines,
) {
    let Some(neighbors) = lines.neighbors.get(hovered) else { return };
    let ring = |renderer: &mut iced::Renderer, id: &str, color: Color, width: f32| {
        let Some(v) = anchors.get(id).and_then(|p| p.visible) else { return };
        renderer.fill_quad(
            Quad {
                bounds: Rectangle { x: v.x + bounds.x, y: v.y + bounds.y, ..v },
                border: Border { color, width, radius: crate::theme::radius::SM.into() },
                ..Default::default()
            },
            Color::TRANSPARENT,
        );
    };
    let accent = crate::theme::accent();
    for id in neighbors {
        ring(renderer, id, Color { a: 0.6, ..accent }, 1.0);
    }
    ring(renderer, hovered, accent, 2.0);
}

/// Stroke `edges` between the anchors found on the last redraw. Anchors that
/// are fully scrolled out of view drop their lines. Called from `draw_over`.
pub fn draw_connectors(
    renderer: &mut iced::Renderer,
    bounds: Rectangle,
    anchors: &Anchors,
    edges: &[crate::dep_connectors::EdgeSpec],
    opts: &crate::dep_connectors::DrawOptions,
) {
    use iced::advanced::graphics::geometry::Renderer as _;
    let list = crate::dep_connectors::connectors(
        edges,
        |id| anchors.get(id).filter(|p| p.visible.is_some()).map(|p| p.rect),
        opts,
    );
    if list.is_empty() {
        return;
    }
    let mut frame = iced::widget::canvas::Frame::new(renderer, bounds.size());
    crate::dep_connectors::stroke_connectors(&mut frame, &list);
    renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
        renderer.draw_geometry(frame.into_geometry());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rectangle {
        Rectangle { x, y, width: w, height: h }
    }

    fn dep(from: &str, to: &str, kind: &str) -> crate::db::models::IssueDependency {
        crate::db::models::IssueDependency {
            id: format!("{from}-{to}"),
            remote_id: None,
            blocking_issue_id: from.into(),
            blocked_issue_id: to.into(),
            kind: kind.into(),
            direction: "blocking".into(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn dep_lines_map_kinds_and_drop_hidden_ends() {
        use crate::dep_connectors::ConnectorKind::*;
        let deps = vec![dep("a", "b", "blocks"), dep("b", "c", "relates"), dep("c", "z", "duplicates")];
        let l = DepLines::new(["a", "b", "c"], &deps);
        let got: Vec<_> = l.edges.iter().map(|e| (e.from_id.as_str(), e.to_id.as_str(), e.kind)).collect();
        assert_eq!(got, [("a", "b", Blocks), ("b", "c", Relates)]);
        assert!(l.neighbors["b"].contains("a") && l.neighbors["b"].contains("c"));
    }

    #[test]
    fn only_cards_with_a_line_are_linked() {
        let l = DepLines::new(["a", "b", "c"], &[dep("a", "b", "blocks"), dep("c", "z", "blocks")]);
        assert!(l.is_linked("a") && l.is_linked("b"));
        assert!(!l.is_linked("c"), "its only link has a hidden end");
        assert!(!l.is_linked("nope"));
    }

    #[test]
    fn hover_dims_and_focuses_only_linked_cards() {
        let l = DepLines::new(["a", "b", "c"], &[dep("a", "b", "blocks")]);
        let o = hover_options(Some("b"), &l);
        assert_eq!((o.focus_id.as_deref(), o.highlight_id.as_deref()), (Some("b"), Some("b")));
        assert_eq!(o.bow, Some(crate::dep_connectors::BOARD_BOW));
        let none = hover_options(Some("c"), &l);
        assert!(none.focus_id.is_none() && none.highlight_id.is_none());
        assert!(hover_options(None, &l).highlight_id.is_none());
    }

    #[test]
    fn no_scrollables_is_identity() {
        let (rect, vis) = resolve(r(10.0, 20.0, 5.0, 5.0), &[]);
        assert_eq!(rect, r(10.0, 20.0, 5.0, 5.0));
        assert_eq!(vis, Some(rect));
    }

    #[test]
    fn board_scroll_shifts_x() {
        let board = ScrollFrame { viewport: r(0.0, 0.0, 400.0, 600.0), translation: Vector::new(100.0, 0.0) };
        let (rect, vis) = resolve(r(150.0, 50.0, 100.0, 40.0), &[board]);
        assert_eq!(rect, r(50.0, 50.0, 100.0, 40.0));
        assert_eq!(vis, Some(rect));
    }

    #[test]
    fn column_scroll_shifts_y_and_board_scroll_shifts_the_column_viewport() {
        let board = ScrollFrame { viewport: r(0.0, 0.0, 400.0, 600.0), translation: Vector::new(300.0, 0.0) };
        // Column viewport at raw x 320..520 (screen 20..220), y 40..600.
        let col = ScrollFrame { viewport: r(320.0, 40.0, 200.0, 560.0), translation: Vector::new(0.0, 80.0) };
        let (rect, vis) = resolve(r(320.0, 150.0, 200.0, 60.0), &[board, col]);
        assert_eq!(rect, r(20.0, 70.0, 200.0, 60.0));
        assert_eq!(vis, Some(rect));
    }

    #[test]
    fn card_scrolled_above_column_is_hidden_partial_is_clipped() {
        let col = ScrollFrame { viewport: r(0.0, 40.0, 200.0, 500.0), translation: Vector::new(0.0, 300.0) };
        // Fully above the viewport after scroll (screen y -150..-100).
        let (_, vis) = resolve(r(0.0, 150.0, 200.0, 50.0), &[col]);
        assert_eq!(vis, None);
        // Straddles the top edge (screen y 20..70) -> clipped to 40..70.
        let (rect, vis) = resolve(r(0.0, 320.0, 200.0, 50.0), &[col]);
        assert_eq!(rect, r(0.0, 20.0, 200.0, 50.0));
        assert_eq!(vis, Some(r(0.0, 40.0, 200.0, 30.0)));
    }

    #[test]
    fn column_scrolled_off_the_board_viewport_is_hidden() {
        let board = ScrollFrame { viewport: r(0.0, 0.0, 300.0, 600.0), translation: Vector::new(500.0, 0.0) };
        let col = ScrollFrame { viewport: r(0.0, 40.0, 200.0, 560.0), translation: Vector::new(0.0, 0.0) };
        // Column raw x 0..200 -> screen -500..-300: off the board viewport.
        let (_, vis) = resolve(r(0.0, 60.0, 200.0, 50.0), &[board, col]);
        assert_eq!(vis, None);
    }
}
