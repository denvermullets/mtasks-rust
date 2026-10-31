//! Lane category icon: a small circle glyph drawn on a canvas in theme colors.

use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::{Element, Length, Point, Rectangle, Renderer, Theme};

use crate::app::Message;
use crate::theme as t;

/// Which icon a lane shows, derived from its Rails workflow category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneIcon {
    Backlog,
    Unstarted,
    Started,
    Completed,
    Canceled,
    /// No category (local seed lanes) or one we don't know.
    Plain,
}

/// Map the optional category string to an icon kind.
pub fn icon_for(category: Option<&str>) -> LaneIcon {
    match category {
        Some("backlog") => LaneIcon::Backlog,
        Some("unstarted") => LaneIcon::Unstarted,
        Some("started") => LaneIcon::Started,
        Some("completed") => LaneIcon::Completed,
        Some("canceled") => LaneIcon::Canceled,
        _ => LaneIcon::Plain,
    }
}

/// The icon's current theme color.
fn icon_color(icon: LaneIcon) -> iced::Color {
    match icon {
        LaneIcon::Started => t::warning(),
        LaneIcon::Completed => t::priority_low(),
        _ => t::text_muted(),
    }
}

struct IconProgram(LaneIcon);

impl canvas::Program<Message> for IconProgram {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let color = icon_color(self.0);
        let c = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let r = bounds.width.min(bounds.height) / 2.0 - 1.0;
        let ring = Path::circle(c, r);
        let mut stroke = Stroke::default().with_color(color).with_width(1.5);
        if self.0 == LaneIcon::Backlog {
            stroke.line_dash = canvas::LineDash {
                segments: &[2.0, 2.0],
                offset: 0,
            };
        }
        frame.stroke(&ring, stroke);
        let inner = r - 2.0;
        match self.0 {
            LaneIcon::Started => {
                // Right half filled.
                let half = Path::new(|b| {
                    b.move_to(Point::new(c.x, c.y - inner));
                    b.arc(canvas::path::Arc {
                        center: c,
                        radius: inner,
                        start_angle: iced::Radians(-std::f32::consts::FRAC_PI_2),
                        end_angle: iced::Radians(std::f32::consts::FRAC_PI_2),
                    });
                    b.close();
                });
                frame.fill(&half, color);
            }
            LaneIcon::Completed => {
                let check = Path::new(|b| {
                    b.move_to(Point::new(c.x - inner * 0.55, c.y));
                    b.line_to(Point::new(c.x - inner * 0.1, c.y + inner * 0.45));
                    b.line_to(Point::new(c.x + inner * 0.6, c.y - inner * 0.4));
                });
                frame.stroke(&check, Stroke::default().with_color(color).with_width(1.5));
            }
            LaneIcon::Canceled => {
                let d = inner * 0.5;
                let cross = Path::new(|b| {
                    b.move_to(Point::new(c.x - d, c.y - d));
                    b.line_to(Point::new(c.x + d, c.y + d));
                    b.move_to(Point::new(c.x + d, c.y - d));
                    b.line_to(Point::new(c.x - d, c.y + d));
                });
                frame.stroke(&cross, Stroke::default().with_color(color).with_width(1.5));
            }
            LaneIcon::Backlog | LaneIcon::Unstarted | LaneIcon::Plain => {}
        }
        vec![frame.into_geometry()]
    }
}

/// A small inline icon (14px), drawn in the current theme's colors.
pub fn view<'a>(icon: LaneIcon) -> Element<'a, Message> {
    Canvas::new(IconProgram(icon))
        .width(Length::Fixed(14.0))
        .height(Length::Fixed(14.0))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_category() {
        assert_eq!(icon_for(Some("backlog")), LaneIcon::Backlog);
        assert_eq!(icon_for(Some("unstarted")), LaneIcon::Unstarted);
        assert_eq!(icon_for(Some("started")), LaneIcon::Started);
        assert_eq!(icon_for(Some("completed")), LaneIcon::Completed);
        assert_eq!(icon_for(Some("canceled")), LaneIcon::Canceled);
    }

    #[test]
    fn unknown_and_none_are_plain() {
        assert_eq!(icon_for(Some("archived")), LaneIcon::Plain);
        assert_eq!(icon_for(Some("")), LaneIcon::Plain);
        assert_eq!(icon_for(None), LaneIcon::Plain);
    }
}
