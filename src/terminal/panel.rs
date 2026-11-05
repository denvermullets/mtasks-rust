//! Panel rules with no Iced and no engine: sizes, which Terminal a pane
//! shows, and what Cmd+J does. `PanelState` is the per-Tab panel state; until
//! Tabs wire it (JRST-417) `TerminalState` holds one as the implicit Tab.
use super::TerminalId;

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

/// The Terminal in each pane, by slot. Slots past the layout are hidden: they
/// keep their Terminal so growing the layout again brings it back.
pub type Panes = [Option<TerminalId>; MAX_PANES];

/// What one Tab remembers about its panel: whether it shows, how tall, the
/// layout, which Terminal fills each pane and which pane has focus. Terminals
/// themselves live outside. JRST-417 parks this per Tab.
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

    /// Move focus to the next (`1`) or previous (`-1`) pane, wrapping.
    /// This is what Cmd+] and Cmd+[ call (JRST-405).
    pub fn cycle_focus(&mut self, delta: i32) {
        self.focused_pane = next_slot(self.focused_pane, self.layout, delta);
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

/// The pane `delta` steps from `current`, wrapping within the layout.
pub fn next_slot(current: usize, layout: Layout, delta: i32) -> usize {
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

    #[test]
    fn panes_cycle_both_ways_and_wrap() {
        assert_eq!(next_slot(0, Layout::Columns, 1), 1);
        assert_eq!(next_slot(1, Layout::Columns, 1), 0);
        assert_eq!(next_slot(0, Layout::Columns, -1), 1);
        assert_eq!(next_slot(0, Layout::Single, 1), 0);
        // A slot past the layout (after shrinking) lands inside it.
        assert_eq!(next_slot(3, Layout::Single, 1), 0);
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
        assert_eq!(next_slot(3, Layout::Grid, 1), 0);
        assert_eq!(next_slot(0, Layout::Grid, -1), 3);
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
