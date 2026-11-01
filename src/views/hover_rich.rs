//! Rich text that reports which span the pointer is over (JRST-297).
//!
//! Iced's stock `rich_text` draws the link hover underline and publishes link
//! clicks but never tells the app about hover. This is a trimmed copy of it
//! (iced_widget 0.13.4 `text/rich.rs`) that keeps layout, drawing and click
//! handling and adds hover reporting. A message goes out only when the hovered
//! span changes, never for movement within a span.
//!
//! Inside a `scrollable` the layout is NOT translated. The scrollable lays its
//! content out unscrolled and hands children a cursor shifted by the scroll
//! offset, so `cursor.position_in(layout.bounds())` and the paragraph's span
//! bounds are both in content space and hit-testing needs no correction. A
//! reported rectangle is therefore content-space too: see [`Hover`].

use iced::advanced::graphics::text::Paragraph;
use iced::advanced::layout;
use iced::advanced::mouse;
use iced::advanced::renderer;
use iced::advanced::text::{Difference, Paragraph as _, Renderer as _, Span};
use iced::advanced::widget::text::{self, Style};
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::{Clipboard, Layout, Renderer as _, Shell, Text, Widget};
use iced::widget::text::{LineHeight, Shaping, Wrapping};
use iced::{alignment, event, Element, Event, Length, Pixels, Point, Rectangle, Size, Theme, Vector};

/// The span the pointer is over.
#[derive(Debug, Clone, PartialEq)]
pub struct Hover<Link> {
    /// Index into the widget's spans.
    pub span: usize,
    /// The span's link, if it has one.
    pub link: Option<Link>,
    /// First line box of the span, in the same coordinate space as `cursor`.
    pub bounds: Rectangle,
    /// Pointer position in that space. Inside a scrollable this is content
    /// space (window position plus scroll offset); `bounds` is in the same
    /// space, so `bounds.position() - cursor` is the span's offset from the
    /// pointer in window pixels, whatever the scroll position.
    pub cursor: Point,
    /// The visible clip rectangle, in that same space.
    pub viewport: Rectangle,
}

/// Whether a left press on a span is captured: only for a span known to be a
/// link, so plain text and gaps pass the press on to enclosing click areas.
fn takes_press(span_is_link: Option<bool>) -> bool {
    span_is_link == Some(true)
}

/// Remembers the hovered span so only changes are reported. Pure.
#[derive(Debug, Default)]
pub struct HoverTracker {
    current: Option<usize>,
}

impl HoverTracker {
    /// Feed the span under the pointer (`None` = none). `Some(change)` only
    /// when it differs from the last observation; `change` is the new state.
    pub fn observe(&mut self, hit: Option<usize>) -> Option<Option<usize>> {
        (self.current != hit).then(|| {
            self.current = hit;
            hit
        })
    }
}

type OnLink<'a, Link, Message> = Box<dyn Fn(Link) -> Message + 'a>;
type OnHover<'a, Link, Message> = Box<dyn Fn(Option<Hover<Link>>) -> Message + 'a>;

pub struct HoverRich<'a, Link, Message>
where
    Link: Clone + 'static,
{
    spans: Vec<Span<'a, Link, iced::Font>>,
    size: Option<Pixels>,
    on_link: OnLink<'a, Link, Message>,
    on_hover: Option<OnHover<'a, Link, Message>>,
}

pub fn hover_rich<'a, Link, Message>(
    spans: Vec<Span<'a, Link, iced::Font>>,
    on_link: impl Fn(Link) -> Message + 'a,
) -> HoverRich<'a, Link, Message>
where
    Link: Clone + 'static,
{
    HoverRich { spans, size: None, on_link: Box::new(on_link), on_hover: None }
}

impl<'a, Link, Message> HoverRich<'a, Link, Message>
where
    Link: Clone + 'static,
{
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// Called when the hovered span changes; `None` when the pointer leaves.
    pub fn on_hover(mut self, f: impl Fn(Option<Hover<Link>>) -> Message + 'a) -> Self {
        self.on_hover = Some(Box::new(f));
        self
    }
}

struct State<Link> {
    spans: Vec<Span<'static, Link, iced::Font>>,
    span_pressed: Option<usize>,
    hover: HoverTracker,
    paragraph: Paragraph,
}

impl<'a, Link, Message> Widget<Message, Theme, iced::Renderer> for HoverRich<'a, Link, Message>
where
    Link: Clone + 'static,
    Message: 'a,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Link>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Link> {
            spans: Vec::new(),
            span_pressed: None,
            hover: HoverTracker::default(),
            paragraph: Paragraph::default(),
        })
    }

    fn size(&self) -> Size<Length> {
        Size { width: Length::Shrink, height: Length::Shrink }
    }

    fn layout(&self, tree: &mut Tree, renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        let state = tree.state.downcast_mut::<State<Link>>();
        layout::sized(limits, Length::Shrink, Length::Shrink, |limits| {
            let bounds = limits.max();
            let size = self.size.unwrap_or_else(|| renderer.default_size());
            let font = renderer.default_font();
            let line_height = LineHeight::default();
            let (hx, vy) = (alignment::Horizontal::Left, alignment::Vertical::Top);
            let with_spans = || Text {
                content: self.spans.as_slice(),
                bounds,
                size,
                line_height,
                font,
                horizontal_alignment: hx,
                vertical_alignment: vy,
                shaping: Shaping::Advanced,
                wrapping: Wrapping::default(),
            };
            if state.spans != self.spans {
                state.paragraph = Paragraph::with_spans(with_spans());
                state.spans = self.spans.iter().cloned().map(Span::to_static).collect();
            } else {
                match state.paragraph.compare(Text {
                    content: (),
                    bounds,
                    size,
                    line_height,
                    font,
                    horizontal_alignment: hx,
                    vertical_alignment: vy,
                    shaping: Shaping::Advanced,
                    wrapping: Wrapping::default(),
                }) {
                    Difference::None => {}
                    Difference::Bounds => state.paragraph.resize(bounds),
                    Difference::Shape => state.paragraph = Paragraph::with_spans(with_spans()),
                }
            }
            state.paragraph.min_bounds()
        })
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State<Link>>();
        let hovered_span = cursor
            .position_in(layout.bounds())
            .and_then(|p| state.paragraph.hit_span(p));

        for (index, span) in self.spans.iter().enumerate() {
            let is_hovered_link = span.link.is_some() && Some(index) == hovered_span;
            if span.highlight.is_some() || span.underline || span.strikethrough || is_hovered_link {
                let translation = layout.position() - Point::ORIGIN;
                let regions = state.paragraph.span_bounds(index);

                if let Some(highlight) = span.highlight {
                    for bounds in &regions {
                        let bounds = Rectangle::new(
                            bounds.position() - Vector::new(span.padding.left, span.padding.top),
                            bounds.size() + Size::new(span.padding.horizontal(), span.padding.vertical()),
                        );
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: bounds + translation,
                                border: highlight.border,
                                ..Default::default()
                            },
                            highlight.background,
                        );
                    }
                }

                if span.underline || span.strikethrough || is_hovered_link {
                    let size = span.size.or(self.size).unwrap_or(renderer.default_size());
                    let line_height = span.line_height.unwrap_or(LineHeight::default()).to_absolute(size);
                    let color = span.color.unwrap_or(defaults.text_color);
                    let baseline = translation + Vector::new(0.0, size.0 + (line_height.0 - size.0) / 2.0);

                    if span.underline || is_hovered_link {
                        for bounds in &regions {
                            renderer.fill_quad(
                                renderer::Quad {
                                    bounds: Rectangle::new(
                                        bounds.position() + baseline - Vector::new(0.0, size.0 * 0.08),
                                        Size::new(bounds.width, 1.0),
                                    ),
                                    ..Default::default()
                                },
                                color,
                            );
                        }
                    }
                    if span.strikethrough {
                        for bounds in &regions {
                            renderer.fill_quad(
                                renderer::Quad {
                                    bounds: Rectangle::new(
                                        bounds.position() + baseline - Vector::new(0.0, size.0 / 2.0),
                                        Size::new(bounds.width, 1.0),
                                    ),
                                    ..Default::default()
                                },
                                color,
                            );
                        }
                    }
                }
            }
        }

        text::draw(renderer, defaults, layout, &state.paragraph, Style::default(), viewport);
    }

    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) -> event::Status {
        let state = tree.state.downcast_mut::<State<Link>>();
        let Event::Mouse(mouse_event) = &event else { return event::Status::Ignored };

        // Hover: checked on every mouse event, but only a changed span is published.
        if let Some(on_hover) = &self.on_hover {
            let hit = cursor
                .position_in(layout.bounds())
                .and_then(|p| state.paragraph.hit_span(p));
            if let Some(change) = state.hover.observe(hit) {
                let hover = change.zip(cursor.position()).map(|(span, at)| {
                    let origin = layout.position() - Point::ORIGIN;
                    let line = state
                        .paragraph
                        .span_bounds(span)
                        .first()
                        .copied()
                        .unwrap_or(Rectangle::new(Point::ORIGIN, Size::ZERO));
                    Hover {
                        span,
                        link: self.spans.get(span).and_then(|s| s.link.clone()),
                        bounds: line + origin,
                        cursor: at,
                        viewport: *viewport,
                    }
                });
                shell.publish(on_hover(hover));
            }
        }

        match mouse_event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                if let Some(p) = cursor.position_in(layout.bounds()) {
                    // Only a link takes the press; plain text lets it reach
                    // an enclosing click area (the description's click-to-edit).
                    let hit = state.paragraph.hit_span(p);
                    if let Some(span) = hit.filter(|&s| takes_press(self.spans.get(s).map(|s| s.link.is_some()))) {
                        state.span_pressed = Some(span);
                        return event::Status::Captured;
                    }
                }
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                if let Some(pressed) = state.span_pressed.take() {
                    if let Some(p) = cursor.position_in(layout.bounds()) {
                        if state.paragraph.hit_span(p) == Some(pressed) {
                            if let Some(link) = self.spans.get(pressed).and_then(|s| s.link.clone()) {
                                shell.publish((self.on_link)(link));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        event::Status::Ignored
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State<Link>>();
        let linked = cursor
            .position_in(layout.bounds())
            .and_then(|p| state.paragraph.hit_span(p))
            .and_then(|i| self.spans.get(i))
            .is_some_and(|s| s.link.is_some());
        if linked { mouse::Interaction::Pointer } else { mouse::Interaction::None }
    }
}

impl<'a, Link, Message> From<HoverRich<'a, Link, Message>> for Element<'a, Message>
where
    Link: Clone + 'static,
    Message: 'a,
{
    fn from(w: HoverRich<'a, Link, Message>) -> Self {
        Element::new(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_link_spans_take_a_press() {
        let cases = [(Some(true), true), (Some(false), false), (None, false)];
        for (span_is_link, expected) in cases {
            assert_eq!(takes_press(span_is_link), expected, "{span_is_link:?}");
        }
    }

    #[test]
    fn tracker_reports_only_changes_of_the_hovered_span() {
        let mut t = HoverTracker::default();
        let cases = [
            (None, None),
            (Some(2), Some(Some(2))),
            (Some(2), None),
            (Some(2), None),
            (Some(0), Some(Some(0))),
            (None, Some(None)),
            (None, None),
            (Some(0), Some(Some(0))),
        ];
        for (hit, want) in cases {
            assert_eq!(t.observe(hit), want, "hit {hit:?}");
        }
    }
}
