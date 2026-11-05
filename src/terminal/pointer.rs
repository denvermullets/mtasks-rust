//! Pointer rules with no Iced and no engine.

/// The cell under a pixel position inside a pane, as signed numbers so a drag
/// that leaves the pane reports how far outside it went.
pub fn cell_at(x: f32, y: f32, cell_w: f32, cell_h: f32) -> (i32, i32) {
    ((x / cell_w).floor() as i32, (y / cell_h).floor() as i32)
}

/// Who receives a click or drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerTarget {
    /// The program, as mouse reports.
    Program,
    /// The Terminal's own text selection.
    Selection,
}

/// Where a gesture that starts now goes: to a program that tracks the mouse,
/// unless Shift is held (the usual terminal way to select anyway). The choice
/// holds for the whole gesture, so releasing Shift mid-drag changes nothing.
pub fn route(tracking: bool, shift: bool) -> PointerTarget {
    if tracking && !shift {
        PointerTarget::Program
    } else {
        PointerTarget::Selection
    }
}

/// Rows moved per wheel notch (a mouse wheel; a trackpad reports pixels instead).
pub const LINES_PER_NOTCH: f32 = 3.0;

/// Wheel or trackpad movement as rows, positive toward older output.
pub fn wheel_rows(delta_y: f32, in_pixels: bool, cell_h: f32) -> f32 {
    if in_pixels {
        delta_y / cell_h
    } else {
        delta_y * LINES_PER_NOTCH
    }
}

/// Where a wheel event goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelTarget {
    /// The program asked for mouse events: send it wheel buttons.
    Program,
    /// A full-screen program without mouse tracking (a pager): send arrow keys.
    Arrows,
    /// Plain shell output: move through scrollback.
    Scrollback,
}

pub fn wheel_target(tracking: bool, alt_screen: bool) -> WheelTarget {
    if tracking {
        WheelTarget::Program
    } else if alt_screen {
        WheelTarget::Arrows
    } else {
        WheelTarget::Scrollback
    }
}

/// Collects fractional rows from a trackpad until they add up to whole ones.
#[derive(Debug, Default)]
pub struct WheelAccum(f32);

impl WheelAccum {
    /// Add movement; returns the whole rows now due (positive toward older output).
    pub fn push(&mut self, rows: f32) -> i32 {
        self.0 += rows;
        let whole = self.0.trunc();
        self.0 -= whole;
        whole as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_takes_a_gesture_from_a_tracking_program() {
        assert_eq!(route(true, false), PointerTarget::Program);
        assert_eq!(route(true, true), PointerTarget::Selection);
        assert_eq!(route(false, false), PointerTarget::Selection);
        assert_eq!(route(false, true), PointerTarget::Selection);
    }

    #[test]
    fn a_trackpad_adds_up_small_movements_into_whole_rows() {
        let mut a = WheelAccum::default();
        let got: Vec<i32> = [0.4, 0.4, 0.4, -2.5, 0.0].iter().map(|r| a.push(*r)).collect();
        assert_eq!(got, vec![0, 0, 1, -2, 0]);
        // What's left is 0.2 - 0.5 = -0.3; another -0.75 completes a row.
        assert_eq!(a.push(-0.75), -1);
    }

    #[test]
    fn pixels_become_rows_by_cell_height_and_notches_by_three() {
        assert_eq!(wheel_rows(32.0, true, 16.0), 2.0);
        assert_eq!(wheel_rows(-8.0, true, 16.0), -0.5);
        assert_eq!(wheel_rows(1.0, false, 16.0), 3.0);
    }

    #[test]
    fn the_wheel_goes_to_the_program_before_arrows_before_scrollback() {
        assert_eq!(wheel_target(true, true), WheelTarget::Program);
        assert_eq!(wheel_target(true, false), WheelTarget::Program);
        assert_eq!(wheel_target(false, true), WheelTarget::Arrows);
        assert_eq!(wheel_target(false, false), WheelTarget::Scrollback);
    }

    #[test]
    fn a_pixel_maps_to_its_cell_and_outside_goes_negative() {
        assert_eq!(cell_at(0.0, 0.0, 8.0, 16.0), (0, 0));
        assert_eq!(cell_at(7.9, 15.9, 8.0, 16.0), (0, 0));
        assert_eq!(cell_at(8.0, 32.0, 8.0, 16.0), (1, 2));
        assert_eq!(cell_at(-1.0, -20.0, 8.0, 16.0), (-1, -2));
        // A fractional cell width, as the font measures it.
        assert_eq!(cell_at(100.0, 0.0, 7.8, 16.0).0, 12);
    }
}
