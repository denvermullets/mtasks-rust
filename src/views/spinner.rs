//! An SVG icon that spins in place. The widget keeps its own start time and
//! asks for the next frame on every redraw, so the animation costs a repaint,
//! not an app message and a full view rebuild (as in `context_menu`).

use std::time::Instant;

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::svg::{self, Renderer as _};
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::{mouse, Clipboard, Shell, Widget};
use iced::{event, window, Color, Element, Event, Length, Radians, Rectangle, Size, Theme};

/// One full turn takes this long.
const PERIOD_SECS: f32 = 1.0;

pub struct Spinner {
    handle: svg::Handle,
    size: f32,
    color: Color,
}

/// `handle` drawn `size` square, tinted `color`, turning clockwise.
pub fn spinner(handle: svg::Handle, size: f32, color: Color) -> Spinner {
    Spinner { handle, size, color }
}

/// The angle `elapsed` seconds into the spin.
fn angle(elapsed: f32) -> Radians {
    Radians(std::f32::consts::TAU * (elapsed / PERIOD_SECS).fract())
}

struct State {
    started: Instant,
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Spinner {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State { started: Instant::now() })
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.size), Length::Fixed(self.size))
    }

    fn layout(&self, _tree: &mut Tree, _renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.resolve(self.size, self.size, Size::new(self.size, self.size)))
    }

    fn on_event(
        &mut self,
        _tree: &mut Tree,
        event: Event,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &iced::Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) -> event::Status {
        if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
            shell.request_redraw(window::RedrawRequest::NextFrame);
        }
        event::Status::Ignored
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let started = tree.state.downcast_ref::<State>().started;
        let svg = svg::Svg::new(self.handle.clone())
            .color(self.color)
            .rotation(angle(started.elapsed().as_secs_f32()));
        renderer.draw_svg(svg, layout.bounds());
    }
}

impl<'a, Message: 'a> From<Spinner> for Element<'a, Message> {
    fn from(s: Spinner) -> Self {
        Element::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_period_comes_back_to_the_start() {
        assert!(angle(0.0).0.abs() < 1e-6);
        assert!(angle(PERIOD_SECS).0.abs() < 1e-6);
        assert!((angle(PERIOD_SECS / 2.0).0 - std::f32::consts::PI).abs() < 1e-4);
    }
}
