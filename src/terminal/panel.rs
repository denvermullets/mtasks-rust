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

/// What a pane says over its last screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneNotice {
    /// The shell exited: Restart or Close.
    Exited,
}

/// The notice for the Terminal a pane shows, from its latest frame.
pub fn pane_notice(exited: bool) -> Option<PaneNotice> {
    exited.then_some(PaneNotice::Exited)
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

    #[test]
    fn a_pane_shows_its_pick_only_while_the_team_owns_it() {
        let (a, b, c) = (TerminalId(1), TerminalId(2), TerminalId(3));
        assert_eq!(resolve_shown(Some(b), &[a, b]), Some(b));
        assert_eq!(resolve_shown(Some(c), &[a, b]), Some(a));
        assert_eq!(resolve_shown(None, &[b]), Some(b));
        assert_eq!(resolve_shown(Some(a), &[]), None);
    }

    #[test]
    fn only_an_exited_shell_gets_a_notice() {
        assert_eq!(pane_notice(true), Some(PaneNotice::Exited));
        assert_eq!(pane_notice(false), None);
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
