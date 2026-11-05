//! Pointer rules with no Iced and no engine.

/// The cell under a pixel position inside a pane, as signed numbers so a drag
/// that leaves the pane reports how far outside it went.
pub fn cell_at(x: f32, y: f32, cell_w: f32, cell_h: f32) -> (i32, i32) {
    ((x / cell_w).floor() as i32, (y / cell_h).floor() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

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
