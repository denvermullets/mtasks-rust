//! Where a hover card goes (JRST-309). Pure; mirrors the web's
//! `hover_card_controller.js#position`: below the anchor with an 8px margin,
//! flipped above when the card would pass the window bottom, then clamped
//! inside the window margin. Left edge aligns with the anchor, shifted left
//! to fit the right edge, never past the left margin.
//!
//! The card's height is an estimate (the view can't measure before layout and
//! the app gets no per-move messages), so a flipped card is anchored by its
//! bottom edge: a wrong estimate can't leave a gap above the anchor. It only
//! affects the decision to flip.

use iced::{Rectangle, Size};

/// Gap between anchor and card, and minimum gap to the window edge.
pub const MARGIN: f32 = 8.0;

/// Where the card's vertical edge sits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Vertical {
    /// Card top, from the window top.
    Top(f32),
    /// Card bottom, as a gap from the window bottom (card flipped above).
    Bottom(f32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Card left, from the window left.
    pub left: f32,
    pub vertical: Vertical,
}

/// `anchor` is in window coordinates; `card` is the card's width and
/// estimated height.
pub fn place(anchor: Rectangle, card: Size, window: Size) -> Placement {
    let left = if anchor.x + card.width > window.width - MARGIN {
        window.width - card.width - MARGIN
    } else {
        anchor.x
    }
    .max(MARGIN);

    let below = anchor.y + anchor.height + MARGIN;
    let vertical = if below + card.height <= window.height - MARGIN {
        Vertical::Top(below)
    } else {
        let above = anchor.y - MARGIN - card.height;
        if above >= MARGIN {
            Vertical::Bottom(window.height - (anchor.y - MARGIN))
        } else {
            // No room either way (small window): pin to the top margin.
            Vertical::Top(MARGIN)
        }
    };
    Placement { left, vertical }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Point;

    const CARD: Size = Size { width: 320.0, height: 100.0 };
    const WINDOW: Size = Size { width: 1000.0, height: 800.0 };

    fn anchor(x: f32, y: f32) -> Rectangle {
        Rectangle::new(Point::new(x, y), Size::new(40.0, 14.0))
    }

    #[test]
    fn places_cards_by_the_web_rules() {
        use Vertical::*;
        let tiny = Size::new(200.0, 60.0);
        let tall = Size::new(320.0, 700.0);
        let rows: [(&str, Rectangle, Size, Size, f32, Vertical); 9] = [
            ("default: below, left-aligned", anchor(100.0, 200.0), CARD, WINDOW, 100.0, Top(222.0)),
            ("fits exactly at the bottom", anchor(100.0, 670.0), CARD, WINDOW, 100.0, Top(692.0)),
            ("one pixel short flips above", anchor(100.0, 671.0), CARD, WINDOW, 100.0, Bottom(137.0)),
            ("near bottom flips above", anchor(100.0, 780.0), CARD, WINDOW, 100.0, Bottom(28.0)),
            ("right edge shifts left", anchor(900.0, 200.0), CARD, WINDOW, 672.0, Top(222.0)),
            ("left edge clamps to margin", anchor(-20.0, 200.0), CARD, WINDOW, 8.0, Top(222.0)),
            ("bottom-right corner", anchor(990.0, 790.0), CARD, WINDOW, 672.0, Bottom(18.0)),
            ("no room above or below pins to top", anchor(100.0, 100.0), tall, WINDOW, 100.0, Top(8.0)),
            ("window narrower than card keeps left margin", anchor(50.0, 10.0), CARD, tiny, 8.0, Top(8.0)),
        ];
        for (name, a, card, window, left, vertical) in rows {
            assert_eq!(place(a, card, window), Placement { left, vertical }, "{name}");
        }
    }

    #[test]
    fn a_flipped_card_is_never_clipped_at_the_top() {
        let window = Size::new(1000.0, 150.0);
        // Room above is exactly the card plus both margins.
        let p = place(anchor(100.0, 116.0), CARD, window);
        assert_eq!(p.vertical, Vertical::Bottom(150.0 - 108.0));
        // One pixel less room: pins to the top margin instead.
        let p = place(anchor(100.0, 115.0), CARD, window);
        assert_eq!(p.vertical, Vertical::Top(MARGIN));
    }
}
