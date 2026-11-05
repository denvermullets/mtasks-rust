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

/// What one Tab remembers about its panel. Terminals themselves live outside.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PanelState {
    pub visible: bool,
    /// `None` means the default, a third of the window.
    pub height: Option<f32>,
    /// The Terminal in the one pane (layouts come later).
    pub pane: Option<TerminalId>,
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

/// Pixels available to the one pane for this window.
pub fn pane_pixels(window_w: f32, window_h: f32, requested: Option<f32>) -> (f32, f32) {
    let w = (window_w - SIDEBAR_WIDTH).max(0.0);
    let h = (panel_height(window_h, requested) - STRIP_HEIGHT).max(0.0);
    (w, h)
}

/// Whole cells that fit, never below 1x1.
pub fn grid_for(width: f32, height: f32, cell_w: f32, cell_h: f32) -> (u16, u16) {
    let cols = (width / cell_w).floor().clamp(1.0, u16::MAX as f32) as u16;
    let rows = (height / cell_h).floor().clamp(1.0, u16::MAX as f32) as u16;
    (cols, rows)
}

/// The Terminal a pane shows: its own pick if the team still owns it,
/// otherwise the team's first.
pub fn resolve_shown(
    preferred: Option<TerminalId>,
    owned_by_team: &[TerminalId],
) -> Option<TerminalId> {
    preferred
        .filter(|id| owned_by_team.contains(id))
        .or_else(|| owned_by_team.first().copied())
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

    #[test]
    fn a_pane_shows_its_pick_only_while_the_team_owns_it() {
        let (a, b, c) = (TerminalId(1), TerminalId(2), TerminalId(3));
        assert_eq!(resolve_shown(Some(b), &[a, b]), Some(b));
        assert_eq!(resolve_shown(Some(c), &[a, b]), Some(a));
        assert_eq!(resolve_shown(None, &[b]), Some(b));
        assert_eq!(resolve_shown(Some(a), &[]), None);
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
