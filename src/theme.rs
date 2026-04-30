use iced::Color;

use crate::db::models::ProjectStatus;

/// Convert u8 hex to f32 for Color::from_rgb (which is const)
const fn c(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

// Mapped from Rails app CSS variables
// --color-menu: #000000
pub const BG_MENU: Color = c(0x00, 0x00, 0x00);
// --color-background: #0B0A0A
pub const BG_PRIMARY: Color = c(0x0B, 0x0A, 0x0A);
// --color-foreground: #121111
pub const BG_SECONDARY: Color = c(0x12, 0x11, 0x11);
// --color-context-foreground: #191818
pub const BG_CARD: Color = c(0x19, 0x18, 0x18);
// --color-hover-highlight: #1E1D1D
pub const BG_CARD_FOCUSED: Color = c(0x1E, 0x1D, 0x1D);
// Modal slightly lighter than background
pub const BG_MODAL: Color = c(0x14, 0x13, 0x13);

// --color-highlight: #BAD4F7
pub const HIGHLIGHT: Color = c(0xBA, 0xD4, 0xF7);

pub const TEXT_PRIMARY: Color = Color::from_rgb(0.90, 0.90, 0.92);
pub const TEXT_SECONDARY: Color = Color::from_rgb(0.55, 0.55, 0.58);
pub const TEXT_MUTED: Color = Color::from_rgb(0.38, 0.38, 0.42);

// --color-accent: #92E8D4
pub const ACCENT: Color = c(0x92, 0xE8, 0xD4);
// --color-accent-hover: #84d5c2
pub const ACCENT_HOVER: Color = c(0x84, 0xD5, 0xC2);

// --color-stroke: #22242B
pub const BORDER: Color = c(0x22, 0x24, 0x2B);

pub const PRIORITY_URGENT: Color = Color::from_rgb(0.94, 0.27, 0.27);
pub const PRIORITY_HIGH: Color = Color::from_rgb(0.96, 0.62, 0.23);
pub const PRIORITY_MEDIUM: Color = Color::from_rgb(0.96, 0.84, 0.23);
pub const PRIORITY_LOW: Color = Color::from_rgb(0.23, 0.73, 0.53);
pub const PRIORITY_NONE: Color = Color::from_rgb(0.42, 0.42, 0.48);

pub const DANGER: Color = Color::from_rgb(0.94, 0.27, 0.27);

pub fn project_status_color(status: ProjectStatus) -> Color {
    match status {
        ProjectStatus::Backlog => TEXT_MUTED,
        ProjectStatus::Started => ACCENT,
        ProjectStatus::Paused => PRIORITY_HIGH,
        ProjectStatus::Completed => PRIORITY_LOW,
        ProjectStatus::Cancelled => DANGER,
    }
}

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
    Some(Color::from_rgb(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
    ))
}
