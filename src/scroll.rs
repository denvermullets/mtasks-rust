//! Pure scroll math for keeping the keyboard-focused row/card in view.
//!
//! List rows, section headers and board cards render at known fixed heights
//! (see the constants below and `views::board::card_height`), so the focused
//! item's position can be computed without querying the widget tree. The app
//! tracks each scrollable's offset/viewport via `on_scroll` and issues
//! `scrollable::scroll_to` with the result of `offset_to_reveal`.

/// Fixed heights of list-mode elements; the view renders with these exact
/// heights so the offsets computed here match what is drawn.
pub const LIST_ROW_H: f32 = 34.0;
pub const LIST_HEADER_H: f32 = 40.0;
pub const LIST_SUBHEAD_H: f32 = 26.0;
pub const LIST_SPACING: f32 = 2.0;
pub const LIST_PAD_TOP: f32 = 4.0;

/// Fixed height of a board sub-group heading and the board lane spacing/padding.
pub const BOARD_SUBHEAD_H: f32 = 24.0;
pub const BOARD_SPACING: f32 = crate::theme::space::S1_5;
pub const BOARD_PAD_TOP: f32 = crate::theme::space::S1;

/// Breathing room kept between the focused item and the viewport edge.
pub const MARGIN: f32 = 8.0;

/// Viewport height assumed until the scrollable reports its real one.
pub const FALLBACK_VIEWPORT_H: f32 = 500.0;

/// Top edge of item `idx` in a vertical stack with uniform `spacing`.
pub fn item_top(heights: &[f32], idx: usize, spacing: f32, pad_top: f32) -> f32 {
    pad_top + heights.iter().take(idx).map(|h| h + spacing).sum::<f32>()
}

/// Smallest scroll change that brings `[top, top + height]` fully into the
/// viewport (with `MARGIN`), or `offset` unchanged when it is already visible.
/// An item taller than the viewport is aligned to its top edge.
pub fn offset_to_reveal(top: f32, height: f32, offset: f32, viewport: f32) -> f32 {
    let bottom = top + height;
    let new = if top - MARGIN < offset || height + 2.0 * MARGIN > viewport {
        top - MARGIN
    } else if bottom + MARGIN > offset + viewport {
        bottom + MARGIN - viewport
    } else {
        offset
    };
    new.max(0.0)
}

/// One list-mode section as the layout sees it.
pub struct ListSection {
    pub len: usize,
    /// Flat issue indices where a sub-group heading precedes the row.
    pub subhead_starts: Vec<usize>,
    pub collapsed: bool,
}

/// `(top, height)` of the row for `(section, issue)` in list mode.
pub fn list_row_span(sections: &[ListSection], section: usize, issue: usize) -> (f32, f32) {
    let mut heights: Vec<f32> = Vec::new();
    let mut target = 0;
    for (i, s) in sections.iter().enumerate() {
        heights.push(LIST_HEADER_H);
        if s.collapsed {
            continue;
        }
        for r in 0..s.len {
            if s.subhead_starts.contains(&r) {
                heights.push(LIST_SUBHEAD_H);
            }
            if i == section && r == issue {
                target = heights.len();
            }
            heights.push(LIST_ROW_H);
        }
    }
    let top = item_top(&heights, target, LIST_SPACING, LIST_PAD_TOP);
    (top, LIST_ROW_H)
}

/// Height of a board card from which optional lines it renders. Line heights
/// come from the type scale (the card texts set the same `LH_*` multiples), and
/// the gaps are the web card's: id to title `S2`, title to chips `S2`, and a
/// `S3` gap before the meta line.
pub fn board_card_height(show_id: bool, has_chips: bool, has_meta: bool) -> f32 {
    use crate::theme::{space, type_scale};
    const TITLE: f32 = type_scale::SM * type_scale::LH_SM;
    const SMALL: f32 = type_scale::XS * type_scale::LH_XS;
    let mut h = 2.0 * space::S2_5 + TITLE;
    if show_id {
        h += SMALL + space::S2;
    }
    if has_chips {
        h += space::S2 + CARD_CHIP_ROW_H;
    }
    if has_meta {
        h += space::S3 + SMALL;
    }
    h
}

/// Fixed height of the chip row on a board card.
pub const CARD_CHIP_ROW_H: f32 = 20.0;

/// `(top, height)` of card `issue` in a board lane. `card_heights` is the
/// card height per issue; `subhead_starts` are flat indices preceded by a
/// sub-group heading.
pub fn board_card_span(card_heights: &[f32], subhead_starts: &[usize], issue: usize) -> (f32, f32) {
    let mut heights: Vec<f32> = Vec::new();
    let mut target = 0;
    for (i, h) in card_heights.iter().enumerate() {
        if subhead_starts.contains(&i) {
            heights.push(BOARD_SUBHEAD_H);
        }
        if i == issue {
            target = heights.len();
        }
        heights.push(*h);
    }
    let top = item_top(&heights, target, BOARD_SPACING, BOARD_PAD_TOP);
    (top, card_heights.get(issue).copied().unwrap_or(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_noop_when_visible() {
        assert_eq!(offset_to_reveal(100.0, 34.0, 50.0, 400.0), 50.0);
    }

    #[test]
    fn reveal_scrolls_down_and_up() {
        // Row at 500..534, viewport 0..400 -> bottom aligned with margin.
        assert_eq!(offset_to_reveal(500.0, 34.0, 0.0, 400.0), 142.0);
        // Row above the viewport -> top aligned with margin.
        assert_eq!(offset_to_reveal(100.0, 34.0, 300.0, 400.0), 92.0);
        // Never negative.
        assert_eq!(offset_to_reveal(2.0, 34.0, 50.0, 400.0), 0.0);
    }

    #[test]
    fn list_span_accounts_for_headers_subheads_and_collapse() {
        let secs = vec![
            ListSection {
                len: 2,
                subhead_starts: vec![],
                collapsed: false,
            },
            ListSection {
                len: 3,
                subhead_starts: vec![0],
                collapsed: false,
            },
        ];
        // header, row, row, header, subhead, row0 ...
        let (top, h) = list_row_span(&secs, 1, 0);
        let expect = LIST_PAD_TOP
            + (LIST_HEADER_H + 2.0 * LIST_ROW_H + LIST_HEADER_H + LIST_SUBHEAD_H)
            + 4.0 * LIST_SPACING
            + LIST_SPACING;
        // 5 preceding items -> 5 spacings
        assert_eq!(top, expect - 0.0);
        assert_eq!(h, LIST_ROW_H);

        let mut collapsed = secs;
        collapsed[0].collapsed = true;
        let (top2, _) = list_row_span(&collapsed, 1, 0);
        // header0, header1, subhead -> 3 preceding items
        assert_eq!(
            top2,
            LIST_PAD_TOP + 2.0 * LIST_HEADER_H + LIST_SUBHEAD_H + 3.0 * LIST_SPACING
        );
    }

    #[test]
    fn board_span_and_card_height() {
        let hs = [50.0, 60.0, 70.0];
        let (top, h) = board_card_span(&hs, &[1], 2);
        assert_eq!(
            top,
            BOARD_PAD_TOP + 50.0 + 60.0 + BOARD_SUBHEAD_H + 3.0 * BOARD_SPACING
        );
        assert_eq!(h, 70.0);
        assert!(board_card_height(true, true, true) > board_card_height(false, false, false));
    }

    #[test]
    fn board_card_height_follows_the_web_card_spacing() {
        // (show_id, has_chips, has_meta) -> padding 2 * 10, title line 20, id line 16
        // + 8 gap, chip row 20 + 8 gap, meta line 16 + 12 gap.
        let cases = [
            ((false, false, false), 40.0),
            ((true, false, false), 64.0),
            ((false, true, false), 68.0),
            ((false, false, true), 68.0),
            ((true, true, true), 120.0),
        ];
        for ((id, chips, meta), want) in cases {
            assert_eq!(board_card_height(id, chips, meta), want, "{id} {chips} {meta}");
        }
    }
}
