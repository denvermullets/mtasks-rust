//! PROTOTYPE (JRST-4, branch prototype/theme-tokens) — throwaway.
//!
//! Question: how do the Rails design tokens map onto Iced? This swaps the old
//! hard-coded dark palette for a runtime-selected `Tokens` table holding all 15
//! Rails themes (app/assets/tailwind/themes.css + application.css @theme). The
//! old constant names survive as zero-arg fns (`t::ACCENT()`) so every view
//! re-themes without touching its layout code. `[` / `]` cycle themes live.

use std::sync::atomic::{AtomicUsize, Ordering};

use iced::theme::Palette;
use iced::{Color, Theme};

use crate::db::models::{Priority, ProjectStatus};

/// Convert u8 hex to f32 for Color::from_rgb (which is const)
const fn c(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

const fn hx(v: u32) -> Color {
    c((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// One Rails theme: the 9 semantic tokens, the gray-100..900 text scale (Tailwind
/// defaults on dark themes, inverted on light ones) and the dependency colors.
pub struct Tokens {
    pub key: &'static str,
    pub name: &'static str,
    pub light: bool,
    pub menu: Color,
    pub background: Color,
    pub foreground: Color,
    pub context_foreground: Color,
    pub highlight: Color,
    pub stroke: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub hover_highlight: Color,
    /// gray-100 ..= gray-900
    pub gray: [Color; 9],
    /// blocks, blocked-by, relates, duplicates
    pub dep: [Color; 4],
}

// Tailwind v4 default gray scale (what text-gray-* resolves to on dark themes).
const TW_GRAY: [Color; 9] = [
    hx(0xf3f4f6),
    hx(0xe5e7eb),
    hx(0xd1d5dc),
    hx(0x99a1af),
    hx(0x6a7282),
    hx(0x4a5565),
    hx(0x364153),
    hx(0x1e2939),
    hx(0x101828),
];
const DEP_DARK: [Color; 4] = [hx(0xa855f7), hx(0x3b82f6), hx(0x22c55e), hx(0xeab308)];
const DEP_LIGHT: [Color; 4] = [hx(0x9333ea), hx(0x2563eb), hx(0x16a34a), hx(0xb45309)];

macro_rules! dark {
    ($key:literal, $name:literal, $menu:literal, $bg:literal, $fg:literal, $ctx:literal,
     $hl:literal, $stroke:literal, $acc:literal, $acch:literal, $hov:literal) => {
        Tokens {
            key: $key,
            name: $name,
            light: false,
            menu: hx($menu),
            background: hx($bg),
            foreground: hx($fg),
            context_foreground: hx($ctx),
            highlight: hx($hl),
            stroke: hx($stroke),
            accent: hx($acc),
            accent_hover: hx($acch),
            hover_highlight: hx($hov),
            gray: TW_GRAY,
            dep: DEP_DARK,
        }
    };
}

macro_rules! light {
    ($key:literal, $name:literal, $menu:literal, $bg:literal, $fg:literal, $ctx:literal,
     $hl:literal, $stroke:literal, $acc:literal, $acch:literal, $hov:literal,
     [$($g:literal),*]) => {
        Tokens {
            key: $key,
            name: $name,
            light: true,
            menu: hx($menu),
            background: hx($bg),
            foreground: hx($fg),
            context_foreground: hx($ctx),
            highlight: hx($hl),
            stroke: hx($stroke),
            accent: hx($acc),
            accent_hover: hx($acch),
            hover_highlight: hx($hov),
            gray: [$(hx($g)),*],
            dep: DEP_LIGHT,
        }
    };
}

pub const THEMES: [Tokens; 15] = [
    dark!("default", "Default", 0x000000, 0x0B0A0A, 0x121111, 0x191818, 0xBAD4F7, 0x22242B, 0x92E8D4, 0x84d5c2, 0x1E1D1D),
    light!("warm-paper", "Warm Paper", 0xece8e0, 0xf4f0e8, 0xece8e0, 0xded7ca, 0xc9bca8, 0xc9bca8, 0xda5f1c, 0xb84a1a, 0xded7ca,
        [0x1a1008, 0x2a2018, 0x3a2a20, 0x54433a, 0x6a5a4a, 0x8a7a6a, 0xa89988, 0xc9bca8, 0xded7ca]),
    light!("cool-linen", "Cool Linen", 0xe8eaee, 0xf0f2f5, 0xe8eaee, 0xd0d4dc, 0x98a6b4, 0xb8bfc9, 0x1a8aaa, 0x1a7a9a, 0xd0d4dc,
        [0x040915, 0x121a27, 0x212c38, 0x364452, 0x4a5866, 0x6a7a8a, 0x8a9aaa, 0xb8bfc9, 0xd0d4dc]),
    dark!("phosphor-amber", "Phosphor Amber", 0x080604, 0x0e0c08, 0x131008, 0x1a1408, 0xf5dd93, 0x3a2a10, 0xe8a820, 0xd99319, 0x221808),
    dark!("phosphor-green", "Phosphor Green", 0x040804, 0x080e08, 0x0a100a, 0x0e160e, 0xa0f0b0, 0x1e4a22, 0x7ae890, 0x60c870, 0x103018),
    dark!("dusk", "Dusk", 0x100b18, 0x16121e, 0x1a1628, 0x211d30, 0xbcbbe6, 0x4a4060, 0xf07a70, 0xe6594d, 0x2e2840),
    dark!("brutalist-newsprint", "Brutalist Newsprint", 0x0e0c08, 0x181610, 0x1e1c18, 0x242018, 0xdad3cd, 0x3a3830, 0xe8d820, 0xd6c218, 0x2a2820),
    dark!("muted-sage", "Muted Sage", 0x1e2824, 0x283430, 0x2e3c34, 0x344038, 0xa0c8ac, 0x4a6054, 0x60d890, 0x2cbb66, 0x3a4a40),
    dark!("deep-navy", "Deep Navy", 0x0a1020, 0x101828, 0x141e30, 0x1a2438, 0x98a8c0, 0x2a3a52, 0xf08030, 0xda5f1c, 0x1e2840),
    dark!("warm-dusk", "Warm Dusk", 0x180a14, 0x22141c, 0x281820, 0x301c28, 0xb8909a, 0x3a2430, 0x50e8c0, 0x3ab088, 0x382030),
    dark!("ink", "Ink", 0x0a0a0a, 0x141414, 0x181818, 0x1e1e1e, 0xaaaaaa, 0x444444, 0xaaee44, 0x8ab830, 0x242424),
    dark!("ocean-floor", "Ocean Floor", 0x080f16, 0x0e1820, 0x121e28, 0x162030, 0x80a8c0, 0x2a4258, 0xd4a84a, 0xb89038, 0x1a2c3c),
    dark!("ash", "Ash", 0x1e1e1e, 0x282828, 0x2c2c2c, 0x303030, 0xb0b0b0, 0x484848, 0xff5a3c, 0xe04830, 0x333333),
    light!("chalk", "Chalk", 0xefeee8, 0xf5f4f0, 0xefeee8, 0xeae8e4, 0x7a7870, 0xd0cec8, 0x0a6870, 0x208088, 0xeae8e4,
        [0x0a0808, 0x141210, 0x2a2820, 0x3a3830, 0x5c5850, 0x7a7870, 0x9a9890, 0xc0beb8, 0xd0cec8]),
    dark!("dusk-redux", "Dusk Redux", 0x120f1a, 0x181424, 0x1c1830, 0x221c38, 0xa098c8, 0x3a3060, 0xf0a070, 0xc88050, 0x2a2248),
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

pub fn tok() -> &'static Tokens {
    &THEMES[CURRENT.load(Ordering::Relaxed)]
}

pub fn current_index() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

pub fn set_index(i: usize) {
    CURRENT.store(i % THEMES.len(), Ordering::Relaxed);
}

pub fn set_by_key(key: &str) {
    if let Some(i) = THEMES.iter().position(|t| t.key == key) {
        set_index(i);
    }
}

pub fn cycle(delta: isize) {
    let n = THEMES.len() as isize;
    set_index((current_index() as isize + delta).rem_euclid(n) as usize);
}

/// The built-in Iced theme (text inputs, scrollbars, pick lists) derived from tokens.
pub fn iced_theme() -> Theme {
    let tk = tok();
    Theme::custom(
        tk.name.to_string(),
        Palette {
            background: tk.background,
            text: TEXT_PRIMARY(),
            primary: tk.accent,
            success: PRIORITY_LOW(),
            danger: DANGER(),
        },
    )
}

// ── Old palette names, now token lookups ─────────────────────────────────────
#[allow(non_snake_case)]
pub fn BG_MENU() -> Color {
    tok().menu
}
#[allow(non_snake_case)]
pub fn BG_PRIMARY() -> Color {
    tok().background
}
#[allow(non_snake_case)]
pub fn BG_SECONDARY() -> Color {
    tok().foreground
}
#[allow(non_snake_case)]
pub fn BG_CARD() -> Color {
    tok().context_foreground
}
#[allow(non_snake_case)]
pub fn BG_CARD_FOCUSED() -> Color {
    tok().hover_highlight
}
/// No Rails token; modals use `foreground`.
#[allow(non_snake_case)]
pub fn BG_MODAL() -> Color {
    tok().foreground
}
#[allow(non_snake_case)]
pub fn HIGHLIGHT() -> Color {
    tok().highlight
}
#[allow(non_snake_case)]
pub fn TEXT_PRIMARY() -> Color {
    tok().gray[1] // gray-200
}
#[allow(non_snake_case)]
pub fn TEXT_SECONDARY() -> Color {
    tok().gray[3] // gray-400
}
#[allow(non_snake_case)]
pub fn TEXT_MUTED() -> Color {
    tok().gray[4] // gray-500
}
#[allow(non_snake_case)]
pub fn ACCENT() -> Color {
    tok().accent
}
#[allow(non_snake_case)]
pub fn ACCENT_HOVER() -> Color {
    tok().accent_hover
}
#[allow(non_snake_case)]
pub fn BORDER() -> Color {
    tok().stroke
}

// ── Semantic colors: fixed hues, with a darker set on light themes (same move
// Rails makes for the dependency colors) ────────────────────────────────────
macro_rules! semantic {
    ($name:ident, $dark:expr, $light:expr) => {
        #[allow(non_snake_case)]
        pub fn $name() -> Color {
            if tok().light {
                $light
            } else {
                $dark
            }
        }
    };
}

semantic!(PRIORITY_URGENT, Color::from_rgb(0.94, 0.27, 0.27), hx(0xdc2626));
semantic!(PRIORITY_HIGH, Color::from_rgb(0.96, 0.62, 0.23), hx(0xc2410c));
semantic!(PRIORITY_MEDIUM, Color::from_rgb(0.96, 0.84, 0.23), hx(0xa16207));
semantic!(PRIORITY_LOW, Color::from_rgb(0.23, 0.73, 0.53), hx(0x15803d));
semantic!(DANGER, Color::from_rgb(0.94, 0.27, 0.27), hx(0xdc2626));
semantic!(WARNING, Color::from_rgb(0.90, 0.62, 0.22), hx(0xb45309));
// Progress chart series (mirrors Rails progress_chart_controller.js).
semantic!(CHART_SCOPE, Color::from_rgb(0.545, 0.576, 0.655), hx(0x6a5a4a));
semantic!(CHART_STARTED, Color::from_rgb(0.961, 0.753, 0.290), hx(0xb45309));
semantic!(CHART_COMPLETED, Color::from_rgb(0.369, 0.918, 0.831), hx(0x0f766e));

#[allow(non_snake_case)]
pub fn PRIORITY_NONE() -> Color {
    TEXT_MUTED()
}

pub fn project_status_color(status: ProjectStatus) -> Color {
    match status {
        ProjectStatus::Backlog => TEXT_MUTED(),
        ProjectStatus::Started => ACCENT(),
        ProjectStatus::Paused => PRIORITY_HIGH(),
        ProjectStatus::Completed => PRIORITY_LOW(),
        ProjectStatus::Cancelled => DANGER(),
    }
}

pub fn priority_color(priority: Priority) -> Color {
    match priority {
        Priority::Urgent => PRIORITY_URGENT(),
        Priority::High => PRIORITY_HIGH(),
        Priority::Medium => PRIORITY_MEDIUM(),
        Priority::Low => PRIORITY_LOW(),
        Priority::None => PRIORITY_NONE(),
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

// ── Fonts: Iced's default font is fixed at launch, so the font is picked by
// JAIT_FONT at startup (restart to change), not live like the theme ─────────
pub const FONTS: [(&str, &str); 5] = [
    ("inter", "Inter"),
    ("jetbrains-mono", "JetBrains Mono"),
    ("ibm-plex-mono", "IBM Plex Mono"),
    ("space-grotesk", "Space Grotesk"),
    ("system", "System"),
];

pub fn startup_font() -> (&'static str, iced::Font) {
    let key = std::env::var("JAIT_FONT").unwrap_or_else(|_| "inter".into());
    match FONTS.iter().find(|(k, _)| *k == key) {
        Some((_, "System")) | None => ("System", iced::Font::DEFAULT),
        Some((_, family)) => (family, iced::Font::with_name(family)),
    }
}
