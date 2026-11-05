//! Panel rules with no Iced and no engine: sizes, which Terminal a pane
//! shows, and what Cmd+J does. `PanelState` is the active Tab's panel state;
//! other Tabs park theirs as a `StoredPanel` (keys, not ids).
use super::TerminalId;
use crate::db::panel_states::StoredPanel;

pub const MIN_HEIGHT: f32 = 120.0;
/// The main area keeps at least this much.
pub const MAIN_MIN: f32 = 200.0;
/// Height of the strip above the panes.
pub const STRIP_HEIGHT: f32 = 28.0;
/// Width of the left sidebar, which the panel doesn't cover.
pub const SIDEBAR_WIDTH: f32 = 220.0;

/// Gap between panes, in pixels.
pub const PANE_GAP: f32 = 1.0;
/// Most panes any layout has.
pub const MAX_PANES: usize = 4;

/// How the panel divides into panes. Fixed layouts only, no custom splits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Single,
    /// Two panes side by side.
    Columns,
    /// Two by two.
    Grid,
}

impl Layout {
    /// Panes the layout shows.
    pub fn slots(self) -> usize {
        match self {
            Layout::Single => 1,
            Layout::Columns => 2,
            Layout::Grid => 4,
        }
    }

    /// Columns and rows of panes.
    pub fn grid(self) -> (usize, usize) {
        match self {
            Layout::Single => (1, 1),
            Layout::Columns => (2, 1),
            Layout::Grid => (2, 2),
        }
    }
}

impl Layout {
    /// The name stored for this layout.
    pub fn name(self) -> &'static str {
        match self {
            Layout::Single => "single",
            Layout::Columns => "columns",
            Layout::Grid => "grid",
        }
    }

    /// The layout stored under `name`; an unknown name is `Single`.
    pub fn from_name(name: &str) -> Layout {
        match name {
            "columns" => Layout::Columns,
            "grid" => Layout::Grid,
            _ => Layout::Single,
        }
    }
}

/// The Terminal in each pane, by slot. Slots past the layout are hidden: they
/// keep their Terminal so growing the layout again brings it back.
pub type Panes = [Option<TerminalId>; MAX_PANES];

/// What one Tab remembers about its panel: whether it shows, how tall, the
/// layout, which Terminal fills each pane and which pane has focus. Terminals
/// themselves live outside. Each Tab parks its own.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PanelState {
    pub visible: bool,
    /// `None` means the default, a third of the window.
    pub height: Option<f32>,
    pub layout: Layout,
    pub panes: Panes,
    /// Slot of the pane that has focus. Always inside the layout.
    pub focused_pane: usize,
}

impl PanelState {
    /// The Terminal in the focused pane.
    pub fn focused_terminal(&self) -> Option<TerminalId> {
        self.panes[self.focused_pane.min(self.layout.slots() - 1)]
    }

    /// The state with each pane's Terminal as its record `key`, which outlives
    /// a restart or a relaunch (a `TerminalId` doesn't). `key_of` is `None` for
    /// a Terminal that no longer exists.
    pub fn to_stored(&self, key_of: impl Fn(TerminalId) -> Option<String>) -> StoredPanel {
        StoredPanel {
            visible: self.visible,
            height: self.height,
            layout: self.layout.name().to_string(),
            panes: std::array::from_fn(|i| self.panes[i].and_then(&key_of)),
            focused_pane: self.focused_pane,
        }
    }

    /// The state stored panel `p` describes. A pane whose key `id_of` can't
    /// find (the Terminal was closed) comes back empty, and a key in two
    /// panes keeps only the first.
    pub fn from_stored(p: &StoredPanel, id_of: impl Fn(&str) -> Option<TerminalId>) -> PanelState {
        let layout = Layout::from_name(&p.layout);
        let mut panes: Panes = [None; MAX_PANES];
        for (slot, key) in p.panes.iter().enumerate() {
            let id = key.as_deref().and_then(&id_of);
            if id.is_some() && !panes.contains(&id) {
                panes[slot] = id;
            }
        }
        PanelState {
            visible: p.visible,
            height: p.height,
            layout,
            panes,
            focused_pane: p.focused_pane.min(layout.slots() - 1),
        }
    }

    /// Move focus to the next (`1`) or previous (`-1`) pane, wrapping.
    /// This is what Cmd+] and Cmd+[ call. Empty panes are skipped.
    pub fn cycle_focus(&mut self, delta: i32) {
        self.focused_pane = next_pane(&self.panes, self.layout, self.focused_pane, delta);
    }
}

/// Put `id` in `slot`. A Terminal fills one pane per Tab, so it leaves any
/// pane it was in, which is left empty.
pub fn assign(panes: &mut Panes, slot: usize, id: TerminalId) {
    for p in panes.iter_mut() {
        if *p == Some(id) {
            *p = None;
        }
    }
    panes[slot] = Some(id);
}

/// The panes after the team's Terminals changed: Terminals it no longer owns
/// or that appear twice are dropped, and a panel with every visible pane
/// empty shows the team's first Terminal in the first pane.
pub fn reconcile(panes: &Panes, owned_by_team: &[TerminalId], layout: Layout) -> Panes {
    let mut out: Panes = [None; MAX_PANES];
    for (i, p) in panes.iter().enumerate() {
        let Some(id) = p.filter(|id| owned_by_team.contains(id)) else { continue };
        if !out.contains(&Some(id)) {
            out[i] = Some(id);
        }
    }
    if out[..layout.slots()].iter().all(Option::is_none) {
        if let Some(first) = owned_by_team.first() {
            assign(&mut out, 0, *first);
        }
    }
    out
}

/// The team's Terminals no visible pane shows: what an empty pane offers.
pub fn unshown(owned_by_team: &[TerminalId], panes: &Panes, layout: Layout) -> Vec<TerminalId> {
    let shown = &panes[..layout.slots()];
    owned_by_team.iter().copied().filter(|id| !shown.contains(&Some(*id))).collect()
}

/// The nearest pane holding a Terminal, `delta` steps (1 or -1) from
/// `current` and wrapping within the layout. With no other pane to go to,
/// focus stays where it is.
pub fn next_pane(panes: &Panes, layout: Layout, current: usize, delta: i32) -> usize {
    let mut slot = current.min(layout.slots() - 1);
    for _ in 0..layout.slots() {
        slot = next_slot(slot, layout, delta);
        if panes[slot].is_some() {
            return slot;
        }
    }
    current.min(layout.slots() - 1)
}

/// The pane `delta` steps from `current`, wrapping within the layout.
fn next_slot(current: usize, layout: Layout, delta: i32) -> usize {
    let n = layout.slots() as i32;
    ((current.min(layout.slots() - 1) as i32 + delta).rem_euclid(n)) as usize
}

/// Where "New Terminal" puts its shell: the first empty visible pane, else
/// the focused one.
pub fn slot_for_new(panes: &Panes, layout: Layout, focused: usize) -> usize {
    panes[..layout.slots()]
        .iter()
        .position(Option::is_none)
        .unwrap_or_else(|| focused.min(layout.slots() - 1))
}

/// Panel height for a window: the requested height, else a third, kept between
/// the minimum and what leaves the main area its minimum.
pub fn panel_height(window_h: f32, requested: Option<f32>) -> f32 {
    let wanted = requested.unwrap_or(window_h / 3.0);
    let max = (window_h - MAIN_MIN).max(MIN_HEIGHT);
    wanted.clamp(MIN_HEIGHT, max)
}

/// Panel height while dragging the strip: it grows as the pointer moves up
/// (`y` shrinks), kept within both limits.
pub fn drag_height(window_h: f32, start_height: f32, start_y: f32, y: f32) -> f32 {
    panel_height(window_h, Some(start_height + (start_y - y)))
}

/// Approximate height of the status bar under the panel. Only clicks within a
/// few pixels of the panel's bottom edge depend on it being exact.
pub const STATUS_HEIGHT: f32 = 28.0;

/// Whether a left press at window point (`x`, `y`) lands outside the panel,
/// so keyboard focus belongs to the app. The panel spans the window right of
/// the sidebar, from its top edge down to the status bar. A press inside it
/// is the pane's or strip's own business.
pub fn press_leaves_panel(window_h: f32, requested: Option<f32>, x: f32, y: f32) -> bool {
    let bottom = window_h - STATUS_HEIGHT;
    let top = bottom - panel_height(window_h, requested);
    x < SIDEBAR_WIDTH || y < top || y > bottom
}

/// Pixels available to the whole panel body (every pane) for this window.
pub fn pane_pixels(window_w: f32, window_h: f32, requested: Option<f32>) -> (f32, f32) {
    let w = (window_w - SIDEBAR_WIDTH).max(0.0);
    let h = (panel_height(window_h, requested) - STRIP_HEIGHT).max(0.0);
    (w, h)
}

/// Pixels available to each pane of `layout`, which divides the panel evenly.
pub fn pane_size(window_w: f32, window_h: f32, requested: Option<f32>, layout: Layout) -> (f32, f32) {
    let (w, h) = pane_pixels(window_w, window_h, requested);
    let (cols, rows) = layout.grid();
    (
        (w - PANE_GAP * (cols - 1) as f32) / cols as f32,
        (h - PANE_GAP * (rows - 1) as f32) / rows as f32,
    )
}

/// A pane smaller than this many columns by rows says "too small".
pub const MIN_COLS: u16 = 20;
pub const MIN_ROWS: u16 = 4;

/// Whether a pane of `cols` x `rows` cells is too small to use. Its PTY then
/// keeps its last size, so the shell doesn't reflow into something unusable.
pub fn too_small(cols: u16, rows: u16) -> bool {
    cols < MIN_COLS || rows < MIN_ROWS
}

/// The size a new shell starts at: the pane's, or the smallest usable one.
pub fn spawn_grid((cols, rows): (u16, u16)) -> (u16, u16) {
    (cols.max(MIN_COLS), rows.max(MIN_ROWS))
}

/// Whole cells that fit, never below 1x1.
pub fn grid_for(width: f32, height: f32, cell_w: f32, cell_h: f32) -> (u16, u16) {
    let cols = (width / cell_w).floor().clamp(1.0, u16::MAX as f32) as u16;
    let rows = (height / cell_h).floor().clamp(1.0, u16::MAX as f32) as u16;
    (cols, rows)
}

/// Whether the keyboard belongs to the Terminal. A modal takes it while open;
/// focus itself is kept, so closing the modal hands the keyboard back.
pub fn keys_go_to_terminal(focused: bool, panel_visible: bool, modal_open: bool) -> bool {
    focused && panel_visible && !modal_open
}

/// What a pane says over its last screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneNotice {
    /// The shell exited: Restart or Close.
    Exited,
    /// The Terminal's owner thread panicked: Close only.
    Dead,
}

/// The notice for the Terminal a pane shows, from its latest frame.
pub fn pane_notice(exited: bool, dead: bool) -> Option<PaneNotice> {
    if dead {
        Some(PaneNotice::Dead)
    } else {
        exited.then_some(PaneNotice::Exited)
    }
}

/// What a quit request does about Terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitPlan {
    /// A Terminal is busy: ask first.
    Ask,
    /// Close every Terminal, then exit.
    Shutdown,
    /// A shutdown is already under way.
    Ignore,
}

pub fn quit_plan(any_busy: bool, confirmed: bool, shutting_down: bool) -> QuitPlan {
    if shutting_down {
        QuitPlan::Ignore
    } else if any_busy && !confirmed {
        QuitPlan::Ask
    } else {
        QuitPlan::Shutdown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    /// Nothing to do (a cross-team page).
    Ignore,
    /// Show the panel and focus it, starting a first Terminal if the team has none.
    Show { create: bool },
    SetFocus(bool),
}

/// Cmd+J.
pub fn toggle(on_team: bool, panel_visible: bool, has_terminal: bool, focused: bool) -> Toggle {
    if !on_team {
        Toggle::Ignore
    } else if !panel_visible || !has_terminal {
        Toggle::Show { create: !has_terminal }
    } else {
        Toggle::SetFocus(!focused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_defaults_to_a_third_and_respects_both_limits() {
        assert_eq!(panel_height(900.0, None), 300.0);
        assert_eq!(panel_height(900.0, Some(50.0)), MIN_HEIGHT);
        assert_eq!(panel_height(900.0, Some(850.0)), 700.0);
        // 800x500 minimum window: the main area keeps its 200.
        assert_eq!(panel_height(500.0, Some(450.0)), 300.0);
        assert_eq!(panel_height(300.0, None), MIN_HEIGHT);
    }

    #[test]
    fn dragging_up_grows_the_panel_within_both_limits() {
        assert_eq!(drag_height(900.0, 300.0, 600.0, 500.0), 400.0);
        assert_eq!(drag_height(900.0, 300.0, 600.0, 650.0), 250.0);
        assert_eq!(drag_height(900.0, 300.0, 600.0, 900.0), MIN_HEIGHT);
        assert_eq!(drag_height(900.0, 300.0, 600.0, 0.0), 700.0);
        // 800x500 window.
        assert_eq!(drag_height(500.0, 166.0, 300.0, 0.0), 300.0);
        assert_eq!(drag_height(500.0, 166.0, 300.0, 500.0), MIN_HEIGHT);
    }

    #[test]
    fn a_press_outside_the_panel_moves_focus_to_the_app() {
        // 1100x900 window: panel is 300 high, above a 28 high status bar.
        let outside = |x, y| press_leaves_panel(900.0, None, x, y);
        assert!(!outside(600.0, 700.0), "on the pane");
        assert!(!outside(600.0, 575.0), "on the strip");
        assert!(outside(600.0, 300.0), "on the board");
        assert!(outside(100.0, 700.0), "on the sidebar");
        assert!(outside(600.0, 895.0), "on the status bar");
        // A dragged height moves the top edge.
        assert!(!press_leaves_panel(900.0, Some(500.0), 600.0, 400.0));
    }

    #[test]
    fn the_grid_counts_whole_cells_and_never_hits_zero() {
        assert_eq!(grid_for(800.0, 240.0, 8.0, 17.0), (100, 14));
        assert_eq!(grid_for(7.0, 3.0, 8.0, 17.0), (1, 1));
    }

    #[test]
    fn the_pane_leaves_the_sidebar_and_strip_out() {
        assert_eq!(pane_pixels(1100.0, 900.0, None), (880.0, 272.0));
    }

    fn ids() -> (TerminalId, TerminalId, TerminalId) {
        (TerminalId(1), TerminalId(2), TerminalId(3))
    }

    #[test]
    fn picking_a_terminal_shown_in_another_pane_moves_it() {
        let (a, b, _) = ids();
        let mut panes: Panes = [Some(a), Some(b), None, None];
        assign(&mut panes, 1, a);
        assert_eq!(panes, [None, Some(a), None, None]);
    }

    #[test]
    fn picking_into_a_filled_pane_replaces_it_without_losing_the_terminal() {
        let (a, b, c) = ids();
        let mut panes: Panes = [Some(a), Some(b), None, None];
        assign(&mut panes, 0, c);
        assert_eq!(panes, [Some(c), Some(b), None, None]);
    }

    #[test]
    fn a_pane_keeps_only_terminals_the_team_owns_and_none_twice() {
        let (a, b, c) = ids();
        let panes: Panes = [Some(c), Some(b), None, None];
        assert_eq!(reconcile(&panes, &[a, b], Layout::Columns), [None, Some(b), None, None]);
        let twice: Panes = [Some(a), Some(a), None, None];
        assert_eq!(reconcile(&twice, &[a], Layout::Columns), [Some(a), None, None, None]);
    }

    #[test]
    fn an_empty_panel_shows_the_teams_first_terminal_in_the_first_pane() {
        let (a, b, _) = ids();
        assert_eq!(reconcile(&[None; MAX_PANES], &[a, b], Layout::Columns), [Some(a), None, None, None]);
        // A move that empties pane 0 leaves it empty: pane 1 is still filled.
        assert_eq!(reconcile(&[None, Some(a), None, None], &[a, b], Layout::Columns), [None, Some(a), None, None]);
    }

    #[test]
    fn an_empty_pane_offers_the_terminals_no_pane_shows() {
        let (a, b, c) = ids();
        let panes: Panes = [Some(a), None, None, None];
        assert_eq!(unshown(&[a, b, c], &panes, Layout::Columns), vec![b, c]);
    }

    fn full() -> Panes {
        [Some(TerminalId(1)), Some(TerminalId(2)), Some(TerminalId(3)), Some(TerminalId(4))]
    }

    fn key_of(id: TerminalId) -> Option<String> {
        Some(format!("key{}", id.0))
    }

    fn id_of(key: &str) -> Option<TerminalId> {
        key.strip_prefix("key")?.parse().ok().map(TerminalId)
    }

    #[test]
    fn panel_state_round_trips_through_keys() {
        let state = PanelState {
            visible: true,
            height: Some(333.0),
            layout: Layout::Grid,
            panes: [Some(TerminalId(7)), None, Some(TerminalId(3)), None],
            focused_pane: 2,
        };
        let stored = state.to_stored(key_of);
        assert_eq!(stored.layout, "grid");
        assert_eq!(stored.panes[0].as_deref(), Some("key7"));
        assert_eq!(PanelState::from_stored(&stored, id_of), state);
    }

    #[test]
    fn a_pane_whose_terminal_is_gone_comes_back_empty() {
        let mut stored = PanelState {
            visible: true,
            layout: Layout::Columns,
            panes: [Some(TerminalId(1)), Some(TerminalId(2)), None, None],
            ..PanelState::default()
        }
        .to_stored(key_of);
        stored.panes[1] = Some("closed-long-ago".into());
        let back = PanelState::from_stored(&stored, id_of);
        assert_eq!(back.panes, [Some(TerminalId(1)), None, None, None]);
    }

    #[test]
    fn an_unknown_layout_is_single_and_focus_stays_inside_it() {
        let stored = StoredPanel { layout: "tiles".into(), focused_pane: 3, ..StoredPanel::default() };
        let back = PanelState::from_stored(&stored, id_of);
        assert_eq!((back.layout, back.focused_pane), (Layout::Single, 0));
    }

    #[test]
    fn a_terminal_stored_in_two_panes_fills_only_the_first() {
        let mut stored = StoredPanel { layout: "columns".into(), ..StoredPanel::default() };
        stored.panes[0] = Some("key5".into());
        stored.panes[1] = Some("key5".into());
        let back = PanelState::from_stored(&stored, id_of);
        assert_eq!(back.panes, [Some(TerminalId(5)), None, None, None]);
    }

    #[test]
    fn panes_cycle_both_ways_and_wrap() {
        let p = full();
        assert_eq!(next_pane(&p, Layout::Columns, 0, 1), 1);
        assert_eq!(next_pane(&p, Layout::Columns, 1, 1), 0);
        assert_eq!(next_pane(&p, Layout::Columns, 0, -1), 1);
        // A slot past the layout (after shrinking) lands inside it.
        assert_eq!(next_pane(&p, Layout::Columns, 3, 1), 0);
    }

    #[test]
    fn cycling_with_one_pane_stays_put() {
        let p = full();
        assert_eq!(next_pane(&p, Layout::Single, 0, 1), 0);
        assert_eq!(next_pane(&p, Layout::Single, 0, -1), 0);
        assert_eq!(next_pane(&p, Layout::Single, 3, 1), 0);
    }

    #[test]
    fn cycling_skips_empty_panes_and_stays_when_none_other_has_a_terminal() {
        let a = Some(TerminalId(1));
        let b = Some(TerminalId(2));
        let p: Panes = [a, None, None, b];
        assert_eq!(next_pane(&p, Layout::Grid, 0, 1), 3);
        assert_eq!(next_pane(&p, Layout::Grid, 3, 1), 0);
        assert_eq!(next_pane(&p, Layout::Grid, 0, -1), 3);
        let lone: Panes = [None, a, None, None];
        assert_eq!(next_pane(&lone, Layout::Grid, 1, 1), 1);
        assert_eq!(next_pane(&[None; MAX_PANES], Layout::Columns, 0, 1), 0);
    }

    #[test]
    fn two_columns_split_the_width_and_leave_a_gap() {
        assert_eq!(pane_size(1100.0, 900.0, None, Layout::Single), (880.0, 272.0));
        assert_eq!(pane_size(1100.0, 900.0, None, Layout::Columns), (439.5, 272.0));
    }

    #[test]
    fn a_new_terminal_takes_an_empty_pane_before_replacing_the_focused_one() {
        let (a, b, _) = ids();
        assert_eq!(slot_for_new(&[Some(a), None, None, None], Layout::Columns, 0), 1);
        assert_eq!(slot_for_new(&[Some(a), Some(b), None, None], Layout::Columns, 1), 1);
        assert_eq!(slot_for_new(&[None, Some(b), None, None], Layout::Columns, 1), 0);
    }

    #[test]
    fn shrinking_hides_the_extra_terminals_without_dropping_them() {
        let (a, b, c) = ids();
        let d = TerminalId(4);
        let grid: Panes = [Some(a), Some(b), Some(c), Some(d)];
        let owned = [a, b, c, d];
        // Down to one pane: the rest stay assigned, and are the ones offered.
        assert_eq!(reconcile(&grid, &owned, Layout::Single), grid);
        assert_eq!(unshown(&owned, &grid, Layout::Single), vec![b, c, d]);
        assert_eq!(unshown(&owned, &grid, Layout::Columns), vec![c, d]);
        assert_eq!(unshown(&owned, &grid, Layout::Grid), Vec::<TerminalId>::new());
        // Growing again brings them back to the same panes.
        assert_eq!(reconcile(&reconcile(&grid, &owned, Layout::Single), &owned, Layout::Grid), grid);
    }

    #[test]
    fn shrinking_past_an_empty_first_pane_fills_it_without_duplicating() {
        let (a, b, c) = ids();
        let panes: Panes = [None, Some(b), Some(c), None];
        let out = reconcile(&panes, &[a, b, c], Layout::Single);
        assert_eq!(out, [Some(a), Some(b), Some(c), None]);
        // The first Terminal came out of a hidden pane: it moves, never twice.
        let hidden: Panes = [None, None, Some(a), None];
        assert_eq!(reconcile(&hidden, &[a, b], Layout::Single), [Some(a), None, None, None]);
    }

    #[test]
    fn a_pick_in_a_four_pane_layout_moves_between_any_two_panes() {
        let (a, b, _) = ids();
        let mut panes: Panes = [Some(a), None, None, Some(b)];
        assign(&mut panes, 2, b);
        assert_eq!(panes, [Some(a), None, Some(b), None]);
    }

    #[test]
    fn a_grid_has_four_panes_that_cycle_and_a_quarter_of_the_panel_each() {
        assert_eq!(Layout::Grid.slots(), 4);
        assert_eq!(next_pane(&full(), Layout::Grid, 3, 1), 0);
        assert_eq!(next_pane(&full(), Layout::Grid, 0, -1), 3);
        assert_eq!(pane_size(1100.0, 900.0, None, Layout::Grid), (439.5, 135.5));
        assert_eq!(slot_for_new(&[Some(TerminalId(1)), Some(TerminalId(2)), None, None], Layout::Grid, 0), 2);
    }

    #[test]
    fn a_pane_under_20_by_4_cells_is_too_small() {
        assert!(!too_small(20, 4));
        assert!(too_small(19, 4));
        assert!(too_small(20, 3));
        assert!(too_small(1, 1));
        assert!(!too_small(120, 30));
    }

    #[test]
    fn a_shell_started_in_a_small_pane_gets_the_smallest_usable_size() {
        assert_eq!(spawn_grid((12, 2)), (20, 4));
        assert_eq!(spawn_grid((80, 24)), (80, 24));
        assert_eq!(spawn_grid((19, 30)), (20, 30));
    }

    #[test]
    fn a_2x2_grid_in_the_minimum_window_is_too_small_until_the_panel_grows() {
        // The default panel there is 166 px; a pane in 2x2 is 68 px tall.
        let (w, h) = pane_size(800.0, 500.0, None, Layout::Grid);
        let (cols, rows) = grid_for(w, h, 8.0, 18.0);
        assert!(too_small(cols, rows));
        // A taller panel fits it again.
        let (w, h) = pane_size(800.0, 500.0, Some(300.0), Layout::Grid);
        let (cols, rows) = grid_for(w, h, 8.0, 18.0);
        assert!(!too_small(cols, rows));
    }

    #[test]
    fn a_modal_takes_the_keyboard_and_gives_it_back() {
        assert!(keys_go_to_terminal(true, true, false));
        assert!(!keys_go_to_terminal(true, true, true), "modal open: nothing to the shell");
        assert!(!keys_go_to_terminal(false, true, false));
        assert!(!keys_go_to_terminal(true, false, false));
    }

    #[test]
    fn an_exited_shell_gets_a_notice_and_a_live_one_none() {
        assert_eq!(pane_notice(true, false), Some(PaneNotice::Exited));
        assert_eq!(pane_notice(false, false), None);
    }

    #[test]
    fn a_dead_terminal_says_so_even_though_it_also_counts_as_exited() {
        assert_eq!(pane_notice(true, true), Some(PaneNotice::Dead));
    }

    #[test]
    fn quitting_asks_only_when_something_is_busy_and_never_twice() {
        assert_eq!(quit_plan(false, false, false), QuitPlan::Shutdown);
        assert_eq!(quit_plan(true, false, false), QuitPlan::Ask);
        assert_eq!(quit_plan(true, true, false), QuitPlan::Shutdown);
        assert_eq!(quit_plan(true, true, true), QuitPlan::Ignore);
        assert_eq!(quit_plan(false, false, true), QuitPlan::Ignore);
    }

    #[test]
    fn cmd_j_shows_creates_or_toggles_focus() {
        assert_eq!(toggle(false, true, true, true), Toggle::Ignore);
        assert_eq!(toggle(true, false, true, false), Toggle::Show { create: false });
        assert_eq!(toggle(true, true, false, false), Toggle::Show { create: true });
        assert_eq!(toggle(true, true, true, true), Toggle::SetFocus(false));
        assert_eq!(toggle(true, true, true, false), Toggle::SetFocus(true));
    }
}
