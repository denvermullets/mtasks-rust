//! One line of text cut with "…" to fit its width (web `truncate`). iced 0.13
//! has no equivalent: its `text` ignores `Wrapping::None` and always wraps.
//!
//! As wide as the text, up to the width available. The cut is cached per
//! content and width, so laying out an unchanged row measures nothing.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::text::{self, Paragraph as _, Renderer as _};
use iced::advanced::widget::tree::{self, Tree};
use iced::advanced::{mouse, Widget};
use iced::alignment::{Horizontal, Vertical};
use iced::widget::text::{LineHeight, Shaping, Wrapping};
use iced::{Color, Element, Font, Length, Pixels, Rectangle, Size, Theme};

type Paragraph = <iced::Renderer as text::Renderer>::Paragraph;

const ELLIPSIS: char = '\u{2026}';

pub struct Truncated {
    content: String,
    size: f32,
    font: Font,
    color: Color,
}

/// `content` on one line, at `size` in the active font.
pub fn truncated(content: impl Into<String>, size: f32, color: Color) -> Truncated {
    Truncated { content: content.into(), size, font: crate::fonts::active(), color }
}

/// The most characters, out of `len`, for which `fits` holds, assuming a longer
/// prefix never fits when a shorter one doesn't. Pure.
fn longest_fitting(len: usize, mut fits: impl FnMut(usize) -> bool) -> usize {
    let (mut low, mut high) = (0, len);
    while low < high {
        let mid = (low + high).div_ceil(2);
        if fits(mid) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low
}

/// `content`'s first `count` characters plus the ellipsis, without a space before it.
fn cut(content: &str, count: usize) -> String {
    let prefix: String = content.chars().take(count).collect();
    format!("{}{ELLIPSIS}", prefix.trim_end())
}

#[derive(Default)]
struct State {
    /// What `paragraph` was built for: content, available width and text size.
    key: Option<(String, u32, u32)>,
    paragraph: Paragraph,
}

impl Truncated {
    fn measure(&self, content: &str) -> Paragraph {
        Paragraph::with_text(text::Text {
            content,
            bounds: Size::INFINITE,
            size: Pixels(self.size),
            line_height: LineHeight::default(),
            font: self.font,
            align_x: Horizontal::Left.into(),
            align_y: Vertical::Top,
            // Full shaping, so characters the font lacks fall back to another.
            shaping: Shaping::Advanced,
            wrapping: Wrapping::None,
        })
    }

    fn fit(&self, width: f32) -> Paragraph {
        let full = self.measure(&self.content);
        if full.min_width() <= width {
            return full;
        }
        let chars = self.content.chars().count();
        let count = longest_fitting(chars, |n| self.measure(&cut(&self.content, n)).min_width() <= width);
        self.measure(&cut(&self.content, count))
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Truncated {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(&mut self, tree: &mut Tree, _renderer: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        let state = tree.state.downcast_mut::<State>();
        let width = limits.max().width;
        let key = (self.content.clone(), width.to_bits(), self.size.to_bits());
        if state.key.as_ref() != Some(&key) {
            state.paragraph = self.fit(width);
            state.key = Some(key);
        }
        let bounds = state.paragraph.min_bounds();
        layout::Node::new(limits.resolve(Length::Shrink, Length::Shrink, bounds))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        renderer.fill_paragraph(&state.paragraph, bounds.position(), self.color, *viewport);
    }
}

impl<'a, Message: 'a> From<Truncated> for Element<'a, Message> {
    fn from(t: Truncated) -> Self {
        Element::new(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_longest_prefix_that_fits() {
        assert_eq!(longest_fitting(10, |n| n <= 4), 4);
        assert_eq!(longest_fitting(10, |n| n <= 10), 10);
        assert_eq!(longest_fitting(10, |_| false), 0);
        assert_eq!(longest_fitting(0, |_| true), 0);
    }

    #[test]
    fn cut_drops_the_space_before_the_ellipsis_and_respects_characters() {
        assert_eq!(cut("two words", 4), "two…");
        assert_eq!(cut("🎉🎉🎉", 2), "🎉🎉…");
        assert_eq!(cut("abc", 0), "…");
    }
}
