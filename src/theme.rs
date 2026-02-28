use iced::Color;

pub const BG_PRIMARY: Color = Color::from_rgb(0.11, 0.11, 0.14);
pub const BG_SECONDARY: Color = Color::from_rgb(0.15, 0.15, 0.19);
pub const BG_CARD: Color = Color::from_rgb(0.18, 0.18, 0.23);
pub const BG_CARD_FOCUSED: Color = Color::from_rgb(0.24, 0.24, 0.30);
pub const BG_MODAL: Color = Color::from_rgb(0.13, 0.13, 0.17);

pub const TEXT_PRIMARY: Color = Color::from_rgb(0.93, 0.93, 0.95);
pub const TEXT_SECONDARY: Color = Color::from_rgb(0.60, 0.60, 0.66);
pub const TEXT_MUTED: Color = Color::from_rgb(0.42, 0.42, 0.48);

pub const ACCENT: Color = Color::from_rgb(0.35, 0.56, 0.98);
pub const ACCENT_HOVER: Color = Color::from_rgb(0.45, 0.64, 1.0);

pub const PRIORITY_URGENT: Color = Color::from_rgb(0.94, 0.27, 0.27);
pub const PRIORITY_HIGH: Color = Color::from_rgb(0.96, 0.62, 0.23);
pub const PRIORITY_MEDIUM: Color = Color::from_rgb(0.96, 0.84, 0.23);
pub const PRIORITY_LOW: Color = Color::from_rgb(0.23, 0.73, 0.53);
pub const PRIORITY_NONE: Color = Color::from_rgb(0.42, 0.42, 0.48);

pub const BORDER: Color = Color::from_rgb(0.25, 0.25, 0.30);
pub const DANGER: Color = Color::from_rgb(0.94, 0.27, 0.27);

pub fn priority_color(priority: i32) -> Color {
    match priority {
        0 => PRIORITY_URGENT,
        1 => PRIORITY_HIGH,
        2 => PRIORITY_MEDIUM,
        3 => PRIORITY_LOW,
        _ => PRIORITY_NONE,
    }
}

pub fn hex_to_color(hex: &str) -> Option<Color> {
    let hex = hex.trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::from_rgb8(r, g, b))
}
