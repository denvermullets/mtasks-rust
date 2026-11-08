//! Pure scroll math for keeping the keyboard-focused row/card in view.
//!
//! List rows, section headers and board cards render at known fixed heights
//! (see the constants below and `views::board::card_height`), so the focused
//! item's position can be computed without querying the widget tree. The app
//! tracks each scrollable's offset/viewport via `on_scroll` and issues
//! `scrollable::scroll_to` with the result of `offset_to_reveal`.

/// Fixed heights of list-mode elements; the view renders with these exact
/// heights so the offsets computed here match what is drawn.
/// A row is one `SM` line plus the web row's `py-2.5`.
pub const LIST_ROW_H: f32 =
    crate::theme::type_scale::SM * crate::theme::type_scale::LH_SM + 2.0 * crate::theme::space::S2_5;
pub const LIST_HEADER_H: f32 = 40.0;
pub const LIST_SUBHEAD_H: f32 = 26.0;
pub const LIST_SPACING: f32 = 2.0;
pub const LIST_PAD_TOP: f32 = 4.0;

/// Fixed height of a board sub-group heading and the board lane spacing/padding.
pub const BOARD_SUBHEAD_H: f32 = 24.0;
pub const BOARD_SPACING: f32 = crate::theme::space::S1_5;
pub const BOARD_PAD_TOP: f32 = crate::theme::space::S1;

/// Board sub-group rows (Rails' swimlanes): the row header bar, the gap
/// from it to the row's cards (`gap-2`), the gap between rows (`gap-6`) and
/// a cell's minimum height (`min-h-[60px]`).
pub const SWIMLANE_HEADER_H: f32 = 28.0;
pub const SWIMLANE_HEADER_GAP: f32 = crate::theme::space::S2;
pub const SWIMLANE_ROW_GAP: f32 = crate::theme::space::S6;
pub const SWIMLANE_CELL_MIN_H: f32 = 60.0;

/// The web board's column width (`w-80`), gap (`gap-3`) and side padding
/// (`px-4`). Columns never shrink: a board wider than the window scrolls.
pub const BOARD_COLUMN_W: f32 = 320.0;
pub const BOARD_COLUMN_GAP: f32 = crate::theme::space::S3;
pub const BOARD_PAD_X: f32 = crate::theme::space::S4;

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

#[cfg(test)]
mod swimlane_tests {
    use super::*;

    #[test]
    fn swimlane_span_stacks_rows_and_cards() {
        let rows = [
            SwimlaneRow { collapsed: false, cells_h: 100.0 },
            SwimlaneRow { collapsed: true, cells_h: 60.0 },
            SwimlaneRow { collapsed: false, cells_h: 60.0 },
        ];
        let first = BOARD_PAD_TOP + SWIMLANE_HEADER_H + SWIMLANE_HEADER_GAP;
        assert_eq!(swimlane_card_span(&rows, 0, &[], 50.0), (first, 50.0));
        assert_eq!(
            swimlane_card_span(&rows, 0, &[40.0], 50.0).0,
            first + 40.0 + BOARD_SPACING
        );
        // Row 2 skips row 1's cells (collapsed) but not its header.
        let want = BOARD_PAD_TOP
            + (SWIMLANE_HEADER_H + SWIMLANE_ROW_GAP + SWIMLANE_HEADER_GAP + 100.0)
            + (SWIMLANE_HEADER_H + SWIMLANE_ROW_GAP)
            + SWIMLANE_HEADER_H
            + SWIMLANE_HEADER_GAP;
        assert_eq!(swimlane_card_span(&rows, 2, &[], 50.0).0, want);
    }

    #[test]
    fn swimlane_cells_have_a_minimum() {
        assert_eq!(swimlane_cells_height(&[]), SWIMLANE_CELL_MIN_H);
        assert_eq!(swimlane_cells_height(&[10.0, 90.0]), 90.0);
        assert_eq!(swimlane_stack_height(&[10.0, 20.0]), 30.0 + BOARD_SPACING);
        assert_eq!(swimlane_stack_height(&[]), 0.0);
    }
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

/// Left edge of board column `idx` within the board's horizontal scroll content.
pub fn board_column_left(idx: usize) -> f32 {
    BOARD_PAD_X + idx as f32 * (BOARD_COLUMN_W + BOARD_COLUMN_GAP)
}

/// Total width of a board with `columns` columns, side padding included.
pub fn board_content_width(columns: usize) -> f32 {
    2.0 * BOARD_PAD_X + columns as f32 * (BOARD_COLUMN_W + BOARD_COLUMN_GAP) - BOARD_COLUMN_GAP
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

/// Fixed height of the chip row on a board card: one label pill, which is an XS
/// line plus `S0_5` padding above and below (see `views::labels::pill`).
pub const CARD_CHIP_ROW_H: f32 =
    crate::theme::type_scale::XS * crate::theme::type_scale::LH_XS + 2.0 * crate::theme::space::S0_5;

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
    fn list_row_matches_the_drawn_row() {
        // One SM line (20) plus the row's vertical padding of 2 * S2_5.
        assert_eq!(LIST_ROW_H, 40.0);
        let secs = vec![ListSection { len: 2, subhead_starts: vec![], collapsed: false }];
        assert_eq!(list_row_span(&secs, 0, 1).0, 4.0 + 40.0 + 2.0 + 40.0 + 2.0);
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
    fn board_columns_are_fixed_width_and_overflow_the_viewport() {
        assert_eq!(board_column_left(0), 16.0);
        assert_eq!(board_column_left(3), 16.0 + 3.0 * 332.0);
        assert_eq!(board_content_width(4), 16.0 + 4.0 * 320.0 + 3.0 * 12.0 + 16.0);
        // Moving focus to a column past the right edge scrolls it just into view.
        let left = board_column_left(3);
        assert_eq!(
            offset_to_reveal(left, BOARD_COLUMN_W, 0.0, 1000.0),
            left + BOARD_COLUMN_W + MARGIN - 1000.0
        );
    }

    #[test]
    fn chip_row_is_one_label_pill_tall() {
        assert_eq!(CARD_CHIP_ROW_H, 20.0);
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

/// Height of one swimlane cell's card stack.
pub fn swimlane_stack_height(card_heights: &[f32]) -> f32 {
    let n = card_heights.len();
    card_heights.iter().sum::<f32>() + BOARD_SPACING * n.saturating_sub(1) as f32
}

/// Height of a swimlane row's cells: its tallest cell, at least the minimum.
/// Every cell in the row renders at this height so rows line up.
pub fn swimlane_cells_height(cell_stacks: &[f32]) -> f32 {
    cell_stacks.iter().copied().fold(SWIMLANE_CELL_MIN_H, f32::max)
}

/// One swimlane row as the layout sees it.
pub struct SwimlaneRow {
    pub collapsed: bool,
    /// From `swimlane_cells_height`.
    pub cells_h: f32,
}

/// `(top, height)` of a card in the swimlane board's vertical scroll content:
/// row `row`, `above` the heights of the cards above it in its cell.
pub fn swimlane_card_span(rows: &[SwimlaneRow], row: usize, above: &[f32], height: f32) -> (f32, f32) {
    let mut top = BOARD_PAD_TOP;
    for r in rows.iter().take(row) {
        top += SWIMLANE_HEADER_H + SWIMLANE_ROW_GAP;
        if !r.collapsed {
            top += SWIMLANE_HEADER_GAP + r.cells_h;
        }
    }
    top += SWIMLANE_HEADER_H + SWIMLANE_HEADER_GAP;
    top += above.iter().map(|h| h + BOARD_SPACING).sum::<f32>();
    (top, height)
}

/// Names one tracked scrollable. It keys `App::scroll_state` and, through
/// `id`, the widget, so the two cannot drift apart. Board lanes are a variant
/// of their own rather than bare indices sharing a number space with the views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScrollKey {
    /// One board lane's vertical scroll.
    Lane(usize),
    /// The board's sideways scroll across its lanes.
    Board,
    /// The board's vertical scroll when sub-grouped into rows (swimlanes),
    /// which replaces the per-lane scrolls.
    Swimlanes,
    IssueList,
    Dashboard,
    Inbox,
    Projects,
    Roadmap,
    RecurringIssues,
    IssueDetail,
    ProjectDetail,
}

impl ScrollKey {
    /// The scrollable widget this key names.
    pub fn id(self) -> iced::widget::scrollable::Id {
        use iced::widget::scrollable::Id;
        match self {
            ScrollKey::Lane(i) => Id::new(format!("board-lane-{i}")),
            ScrollKey::Board => Id::new("board-columns"),
            ScrollKey::Swimlanes => Id::new("board-swimlanes"),
            ScrollKey::IssueList => Id::new("issue-list"),
            ScrollKey::Dashboard => Id::new("dashboard-list"),
            ScrollKey::Inbox => Id::new("inbox-list"),
            ScrollKey::Projects => Id::new("project-list"),
            ScrollKey::Roadmap => Id::new("roadmap"),
            ScrollKey::RecurringIssues => Id::new("recurring-issues"),
            ScrollKey::IssueDetail => Id::new("issue-detail"),
            ScrollKey::ProjectDetail => Id::new("project-detail"),
        }
    }

    /// Whether the scrollable moves sideways (`x`, width) rather than down.
    pub fn horizontal(self) -> bool {
        matches!(self, ScrollKey::Board)
    }

    /// The offset to hand `scrollable::scroll_to` for a stored `offset`.
    pub fn absolute(self, offset: f32) -> iced::widget::scrollable::AbsoluteOffset {
        if self.horizontal() {
            iced::widget::scrollable::AbsoluteOffset { x: offset, y: 0.0 }
        } else {
            iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: offset }
        }
    }

    /// `scrollable::scroll_to` for a stored `offset`.
    pub fn scroll_to<T: 'static>(self, offset: f32) -> iced::Task<T> {
        iced::widget::scrollable::scroll_to(self.id(), self.absolute(offset))
    }

    /// The `(offset, viewport)` pair the scrollable reports for this key.
    fn reading(self, vp: iced::widget::scrollable::Viewport) -> (f32, f32) {
        if self.horizontal() {
            (vp.absolute_offset().x, vp.bounds().width)
        } else {
            (vp.absolute_offset().y, vp.bounds().height)
        }
    }
}

/// Give `scrollable` its id and make it report its offset as
/// `Message::Scrolled(key, ..)`: the one place a view wires scroll tracking.
pub fn track<'a>(
    scrollable: iced::widget::Scrollable<'a, crate::app::Message>,
    key: ScrollKey,
) -> iced::widget::Scrollable<'a, crate::app::Message> {
    scrollable.id(key.id()).on_scroll(move |vp| {
        let (offset, viewport) = key.reading(vp);
        crate::app::Message::Scrolled(key, offset, viewport)
    })
}

#[cfg(test)]
mod scroll_key_tests {
    use super::ScrollKey;

    #[test]
    fn lanes_and_views_never_share_a_key_or_an_id() {
        let keys = [
            ScrollKey::Lane(0),
            ScrollKey::Lane(1),
            ScrollKey::Board,
            ScrollKey::IssueList,
            ScrollKey::Dashboard,
            ScrollKey::Inbox,
            ScrollKey::Projects,
            ScrollKey::Roadmap,
            ScrollKey::RecurringIssues,
            ScrollKey::IssueDetail,
            ScrollKey::ProjectDetail,
        ];
        for (i, a) in keys.iter().enumerate() {
            for b in &keys[i + 1..] {
                assert_ne!(a, b);
                assert_ne!(a.id(), b.id());
            }
        }
    }

    #[test]
    fn only_the_board_scrolls_sideways() {
        assert!(ScrollKey::Board.horizontal());
        assert!(!ScrollKey::Lane(3).horizontal());
        assert_eq!(ScrollKey::Board.absolute(7.0).x, 7.0);
        assert_eq!(ScrollKey::Lane(3).absolute(7.0).y, 7.0);
    }
}
