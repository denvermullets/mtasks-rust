use std::sync::atomic::{AtomicUsize, Ordering};

use iced::theme::Palette;
use iced::widget::button;
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
    dark!(
        "default", "Default", 0x000000, 0x0B0A0A, 0x121111, 0x191818, 0xBAD4F7, 0x22242B, 0x92E8D4,
        0x84d5c2, 0x1E1D1D
    ),
    light!(
        "warm-paper",
        "Warm Paper",
        0xece8e0,
        0xf4f0e8,
        0xece8e0,
        0xded7ca,
        0xc9bca8,
        0xc9bca8,
        0xda5f1c,
        0xb84a1a,
        0xded7ca,
        [
            0x1a1008, 0x2a2018, 0x3a2a20, 0x54433a, 0x6a5a4a, 0x8a7a6a, 0xa89988, 0xc9bca8,
            0xded7ca
        ]
    ),
    light!(
        "cool-linen",
        "Cool Linen",
        0xe8eaee,
        0xf0f2f5,
        0xe8eaee,
        0xd0d4dc,
        0x98a6b4,
        0xb8bfc9,
        0x1a8aaa,
        0x1a7a9a,
        0xd0d4dc,
        [
            0x040915, 0x121a27, 0x212c38, 0x364452, 0x4a5866, 0x6a7a8a, 0x8a9aaa, 0xb8bfc9,
            0xd0d4dc
        ]
    ),
    dark!(
        "phosphor-amber",
        "Phosphor Amber",
        0x080604,
        0x0e0c08,
        0x131008,
        0x1a1408,
        0xf5dd93,
        0x3a2a10,
        0xe8a820,
        0xd99319,
        0x221808
    ),
    dark!(
        "phosphor-green",
        "Phosphor Green",
        0x040804,
        0x080e08,
        0x0a100a,
        0x0e160e,
        0xa0f0b0,
        0x1e4a22,
        0x7ae890,
        0x60c870,
        0x103018
    ),
    dark!(
        "dusk", "Dusk", 0x100b18, 0x16121e, 0x1a1628, 0x211d30, 0xbcbbe6, 0x4a4060, 0xf07a70,
        0xe6594d, 0x2e2840
    ),
    dark!(
        "brutalist-newsprint",
        "Brutalist Newsprint",
        0x0e0c08,
        0x181610,
        0x1e1c18,
        0x242018,
        0xdad3cd,
        0x3a3830,
        0xe8d820,
        0xd6c218,
        0x2a2820
    ),
    dark!(
        "muted-sage",
        "Muted Sage",
        0x1e2824,
        0x283430,
        0x2e3c34,
        0x344038,
        0xa0c8ac,
        0x4a6054,
        0x60d890,
        0x2cbb66,
        0x3a4a40
    ),
    dark!(
        "deep-navy",
        "Deep Navy",
        0x0a1020,
        0x101828,
        0x141e30,
        0x1a2438,
        0x98a8c0,
        0x2a3a52,
        0xf08030,
        0xda5f1c,
        0x1e2840
    ),
    dark!(
        "warm-dusk",
        "Warm Dusk",
        0x180a14,
        0x22141c,
        0x281820,
        0x301c28,
        0xb8909a,
        0x3a2430,
        0x50e8c0,
        0x3ab088,
        0x382030
    ),
    dark!(
        "ink", "Ink", 0x0a0a0a, 0x141414, 0x181818, 0x1e1e1e, 0xaaaaaa, 0x444444, 0xaaee44,
        0x8ab830, 0x242424
    ),
    dark!(
        "ocean-floor",
        "Ocean Floor",
        0x080f16,
        0x0e1820,
        0x121e28,
        0x162030,
        0x80a8c0,
        0x2a4258,
        0xd4a84a,
        0xb89038,
        0x1a2c3c
    ),
    dark!(
        "ash", "Ash", 0x1e1e1e, 0x282828, 0x2c2c2c, 0x303030, 0xb0b0b0, 0x484848, 0xff5a3c,
        0xe04830, 0x333333
    ),
    light!(
        "chalk",
        "Chalk",
        0xefeee8,
        0xf5f4f0,
        0xefeee8,
        0xeae8e4,
        0x7a7870,
        0xd0cec8,
        0x0a6870,
        0x208088,
        0xeae8e4,
        [
            0x0a0808, 0x141210, 0x2a2820, 0x3a3830, 0x5c5850, 0x7a7870, 0x9a9890, 0xc0beb8,
            0xd0cec8
        ]
    ),
    dark!(
        "dusk-redux",
        "Dusk Redux",
        0x120f1a,
        0x181424,
        0x1c1830,
        0x221c38,
        0xa098c8,
        0x3a3060,
        0xf0a070,
        0xc88050,
        0x2a2248
    ),
];

/// Key of the theme used when none (or an unknown one) is configured.
pub const DEFAULT_THEME_KEY: &str = "default";

static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// The active theme's tokens.
pub fn tok() -> &'static Tokens {
    &THEMES[CURRENT.load(Ordering::Relaxed)]
}

/// Catalog index for a key; an unknown key resolves to `DEFAULT_THEME_KEY`.
fn index_for_key(key: &str) -> usize {
    THEMES
        .iter()
        .position(|t| t.key == key)
        .or_else(|| THEMES.iter().position(|t| t.key == DEFAULT_THEME_KEY))
        .unwrap_or(0)
}

/// Select the current theme by key. An unknown key selects `DEFAULT_THEME_KEY`.
pub fn set_by_key(key: &str) {
    CURRENT.store(index_for_key(key), Ordering::Relaxed);
}

/// Background of an accent-filled button: `accent`, or `accent_hover` while hovered.
pub fn accent_fill(status: button::Status) -> Color {
    match status {
        button::Status::Hovered => accent_hover(),
        _ => accent(),
    }
}

/// A catalog entry for pick lists: displays as the theme name, carries the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeChoice {
    pub key: &'static str,
    pub name: &'static str,
}

impl std::fmt::Display for ThemeChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

/// All themes in catalog order, for a picker.
pub fn choices() -> Vec<ThemeChoice> {
    THEMES
        .iter()
        .map(|t| ThemeChoice {
            key: t.key,
            name: t.name,
        })
        .collect()
}

/// The choice for a config key; an unknown key resolves to `default`.
pub fn choice_for_key(key: &str) -> ThemeChoice {
    let t = &THEMES[index_for_key(key)];
    ThemeChoice {
        key: t.key,
        name: t.name,
    }
}

/// The built-in Iced theme (text inputs, scrollbars, pick lists) derived from the current tokens.
pub fn iced_theme() -> Theme {
    let tk = tok();
    Theme::custom(
        tk.name.to_string(),
        Palette {
            background: tk.background,
            text: text_primary(),
            primary: tk.accent,
            success: priority_low(),
            danger: danger(),
        },
    )
}

macro_rules! token_fn {
    ($($(#[$m:meta])* $name:ident => $e:expr;)*) => {
        $($(#[$m])* pub fn $name() -> Color { let t = tok(); $e(t) })*
    };
}

token_fn! {
    bg_menu => |t: &Tokens| t.menu;
    bg_primary => |t: &Tokens| t.background;
    bg_secondary => |t: &Tokens| t.foreground;
    bg_card => |t: &Tokens| t.context_foreground;
    bg_card_focused => |t: &Tokens| t.hover_highlight;
    /// Rails has no modal token; modals use `foreground`.
    bg_modal => |t: &Tokens| t.foreground;
    highlight => |t: &Tokens| t.highlight;
    border => |t: &Tokens| t.stroke;
    accent => |t: &Tokens| t.accent;
    accent_hover => |t: &Tokens| t.accent_hover;
    /// Web `text-gray-100`: titles and emphasised text.
    text_strong => |t: &Tokens| t.gray[0];
    text_primary => |t: &Tokens| t.gray[1];
    /// Web `text-gray-300`: markdown and running body text.
    #[allow(dead_code)]
    text_body => |t: &Tokens| t.gray[2];
    text_secondary => |t: &Tokens| t.gray[3];
    text_muted => |t: &Tokens| t.gray[4];
    dep_blocks => |t: &Tokens| t.dep[0];
    dep_blocked_by => |t: &Tokens| t.dep[1];
    dep_relates => |t: &Tokens| t.dep[2];
    dep_duplicates => |t: &Tokens| t.dep[3];
}

// Semantic colors: fixed hues, with a darker set on light themes.
macro_rules! semantic {
    ($name:ident, $dark:expr, $light:expr) => {
        pub fn $name() -> Color {
            if tok().light {
                $light
            } else {
                $dark
            }
        }
    };
}

semantic!(
    priority_urgent,
    Color::from_rgb(0.94, 0.27, 0.27),
    hx(0xdc2626)
);
semantic!(
    priority_high,
    Color::from_rgb(0.96, 0.62, 0.23),
    hx(0xc2410c)
);
semantic!(
    priority_medium,
    Color::from_rgb(0.96, 0.84, 0.23),
    hx(0xa16207)
);
semantic!(
    priority_low,
    Color::from_rgb(0.23, 0.73, 0.53),
    hx(0x15803d)
);
/// Code block frame: the same dark color on every theme, like the web's
/// fixed dark code blocks (gray-900 background).
pub const CODE_BLOCK_BG: Color = hx(0x111827);
/// Plain text inside a code block frame (gray-200); legible on `CODE_BLOCK_BG`.
pub const CODE_BLOCK_TEXT: Color = hx(0xe5e7eb);

semantic!(danger, Color::from_rgb(0.94, 0.27, 0.27), hx(0xdc2626));
semantic!(warning, Color::from_rgb(0.90, 0.62, 0.22), hx(0xb45309));
// Progress chart series (mirrors Rails progress_chart_controller.js).
semantic!(
    chart_scope,
    Color::from_rgb(0.545, 0.576, 0.655),
    hx(0x6a5a4a)
);
semantic!(
    chart_started,
    Color::from_rgb(0.961, 0.753, 0.290),
    hx(0xb45309)
);
semantic!(
    chart_completed,
    Color::from_rgb(0.369, 0.918, 0.831),
    hx(0x0f766e)
);

/// The None priority is drawn in the theme's muted text color.
pub fn priority_none() -> Color {
    text_muted()
}

pub fn project_status_color(status: ProjectStatus) -> Color {
    match status {
        ProjectStatus::Backlog => text_muted(),
        ProjectStatus::Started => accent(),
        ProjectStatus::Paused => priority_high(),
        ProjectStatus::Completed => priority_low(),
        ProjectStatus::Cancelled => danger(),
    }
}

pub fn priority_color(priority: Priority) -> Color {
    match priority {
        Priority::Urgent => priority_urgent(),
        Priority::High => priority_high(),
        Priority::Medium => priority_medium(),
        Priority::Low => priority_low(),
        Priority::None => priority_none(),
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

/// The web's Tailwind v4 default type scale, in logical pixels (rem * 16).
/// Each size pairs with the web's line height as a multiple of the size.
// Whole Tailwind scale kept; the polish tickets (JRST-315 to 320) will use the rest.
#[allow(dead_code)]
pub mod type_scale {
    pub const XS: f32 = 12.0;
    pub const SM: f32 = 14.0;
    pub const BASE: f32 = 16.0;
    pub const LG: f32 = 18.0;
    pub const XL: f32 = 20.0;
    pub const XXL: f32 = 24.0;
    pub const XXXL: f32 = 30.0;
    pub const XXXXL: f32 = 36.0;

    /// Line height multiples (Tailwind v4: 16/12, 20/14, 24/16, 28/18, 28/20, 32/24, 36/30, 40/36).
    pub const LH_XS: f32 = 16.0 / 12.0;
    pub const LH_SM: f32 = 20.0 / 14.0;
    pub const LH_BASE: f32 = 1.5;
    pub const LH_LG: f32 = 28.0 / 18.0;
    pub const LH_XL: f32 = 28.0 / 20.0;
    pub const LH_XXL: f32 = 32.0 / 24.0;
    pub const LH_XXXL: f32 = 1.2;
    pub const LH_XXXXL: f32 = 40.0 / 36.0;
}

/// The web's Tailwind v4 spacing scale: `N` steps of 0.25rem (4px), named by the Tailwind step.
/// Use for padding, gaps and margins.
// Whole Tailwind scale kept; the polish tickets (JRST-315 to 320) will use the rest.
#[allow(dead_code)]
pub mod space {
    pub const S0_5: f32 = 2.0;
    pub const S1: f32 = 4.0;
    pub const S1_5: f32 = 6.0;
    pub const S2: f32 = 8.0;
    pub const S2_5: f32 = 10.0;
    pub const S3: f32 = 12.0;
    pub const S4: f32 = 16.0;
    pub const S5: f32 = 20.0;
    pub const S6: f32 = 24.0;
    pub const S8: f32 = 32.0;
    pub const S10: f32 = 40.0;
    pub const S12: f32 = 48.0;
    pub const S16: f32 = 64.0;
}

/// The web's Tailwind v4 corner radii in logical pixels (rounded-sm = 0.25rem, md = 0.375rem,
/// lg = 0.5rem, xl = 0.75rem, 2xl = 1rem). `FULL` is `rounded-full`: large enough to make a pill.
// Whole Tailwind scale kept; the polish tickets (JRST-315 to 320) will use the rest.
#[allow(dead_code)]
pub mod radius {
    pub const XS: f32 = 2.0;
    pub const SM: f32 = 4.0;
    pub const MD: f32 = 6.0;
    pub const LG: f32 = 8.0;
    pub const XL: f32 = 12.0;
    pub const XXL: f32 = 16.0;
    pub const FULL: f32 = 9999.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_values_match_the_webs_tailwind_radii() {
        use radius::*;
        assert_eq!([XS, SM, MD, LG, XL, XXL], [2.0, 4.0, 6.0, 8.0, 12.0, 16.0]);
        assert!(FULL >= 9999.0);
        // The web's markdown: inline code 0.25rem, images 0.375rem, code blocks 0.5rem.
        assert_eq!([SM, MD, LG], [0.25 * 16.0, 0.375 * 16.0, 0.5 * 16.0]);
    }

    #[test]
    fn spacing_scale_matches_the_webs_tailwind_steps() {
        use space::*;
        let steps = [
            (0.5, S0_5),
            (1.0, S1),
            (1.5, S1_5),
            (2.0, S2),
            (2.5, S2_5),
            (3.0, S3),
            (4.0, S4),
            (5.0, S5),
            (6.0, S6),
            (8.0, S8),
            (10.0, S10),
            (12.0, S12),
            (16.0, S16),
        ];
        for (step, px) in steps {
            assert_eq!(px, step * 4.0, "spacing step {step}");
        }
    }

    const RAILS_KEYS: [&str; 15] = [
        "default",
        "warm-paper",
        "cool-linen",
        "phosphor-amber",
        "phosphor-green",
        "dusk",
        "brutalist-newsprint",
        "muted-sage",
        "deep-navy",
        "warm-dusk",
        "ink",
        "ocean-floor",
        "ash",
        "chalk",
        "dusk-redux",
    ];

    #[test]
    fn choices_list_all_themes_in_catalog_order_by_name() {
        let c = choices();
        assert_eq!(c.len(), 15);
        for (choice, t) in c.iter().zip(THEMES.iter()) {
            assert_eq!(choice.key, t.key);
            assert_eq!(choice.to_string(), t.name);
        }
        assert_eq!(c[0].key, "default");
    }

    #[test]
    fn choice_for_key_resolves_known_and_falls_back_to_default() {
        assert_eq!(choice_for_key("ink").key, "ink");
        assert_eq!(choice_for_key("nope").key, DEFAULT_THEME_KEY);
    }

    fn lum(c: Color) -> f32 {
        0.299 * c.r + 0.587 * c.g + 0.114 * c.b
    }

    /// Restores the default theme on drop, so a failed assertion cannot leak
    /// a non-default theme into other tests.
    struct RestoreDefault;

    impl Drop for RestoreDefault {
        fn drop(&mut self) {
            set_by_key(DEFAULT_THEME_KEY);
        }
    }

    #[test]
    fn default_theme_key_is_the_first_catalog_entry() {
        assert_eq!(DEFAULT_THEME_KEY, "default");
        assert_eq!(THEMES[0].key, DEFAULT_THEME_KEY);
    }

    // The current theme is process-global, so everything touching it runs in one test.
    #[test]
    fn theme_selection_and_read_functions() {
        let _restore = RestoreDefault;
        let keys: Vec<_> = THEMES.iter().map(|t| t.key).collect();
        assert_eq!(keys, RAILS_KEYS);
        let light: Vec<_> = THEMES.iter().filter(|t| t.light).map(|t| t.name).collect();
        assert_eq!(light, ["Warm Paper", "Cool Linen", "Chalk"]);

        set_by_key(DEFAULT_THEME_KEY);
        let (dark_acc, dark_bg) = (accent(), bg_primary());
        let dark = [
            priority_urgent(),
            priority_high(),
            priority_medium(),
            priority_low(),
            danger(),
            warning(),
            chart_scope(),
            chart_started(),
            chart_completed(),
        ];
        assert_eq!(priority_color(Priority::None), text_muted());
        assert_eq!(project_status_color(ProjectStatus::Started), dark_acc);

        set_by_key("warm-paper");
        assert_ne!(accent(), dark_acc);
        assert_ne!(bg_primary(), dark_bg);
        let light = [
            priority_urgent(),
            priority_high(),
            priority_medium(),
            priority_low(),
            danger(),
            warning(),
            chart_scope(),
            chart_started(),
            chart_completed(),
        ];
        for (d, l) in dark.iter().zip(light.iter()) {
            assert_ne!(d, l);
        }
        assert!(lum(text_primary()) < lum(bg_primary()));
        assert_eq!(priority_color(Priority::None), text_muted());
        assert_eq!(project_status_color(ProjectStatus::Started), accent());
        assert_eq!(project_status_color(ProjectStatus::Cancelled), danger());
        assert_eq!(bg_modal(), bg_secondary());
        assert_eq!(accent_fill(button::Status::Active), accent());
        assert_eq!(accent_fill(button::Status::Hovered), accent_hover());

        let p = iced_theme().palette();
        assert_eq!(p.background, bg_primary());
        assert_eq!(p.text, text_primary());
        assert_eq!(p.primary, accent());
        assert_eq!(p.danger, danger());
        set_by_key(DEFAULT_THEME_KEY);
        assert_ne!(iced_theme().palette().background, p.background);

        // Text strength: strong >= body > secondary in contrast against the
        // background, on every theme.
        for t in THEMES.iter() {
            set_by_key(t.key);
            let contrast = |c: Color| (lum(c) - lum(bg_primary())).abs();
            let (strong, primary, body, secondary) =
                (text_strong(), text_primary(), text_body(), text_secondary());
            assert_eq!(strong, t.gray[0], "{}", t.key);
            assert_eq!(body, t.gray[2], "{}", t.key);
            assert!(contrast(strong) >= contrast(primary), "{} strong", t.key);
            assert!(contrast(primary) >= contrast(body), "{} body", t.key);
            assert!(contrast(body) >= contrast(secondary), "{} secondary", t.key);
            assert!(contrast(body) > 0.4, "{} body readable", t.key);
        }
        set_by_key(DEFAULT_THEME_KEY);

        set_by_key("no-such-theme");
        assert_eq!(tok().key, DEFAULT_THEME_KEY);
        assert_eq!(accent(), dark_acc);
    }

    #[test]
    fn type_scale_matches_the_webs_tailwind_sizes() {
        use type_scale::*;
        let sizes = [XS, SM, BASE, LG, XL, XXL, XXXL, XXXXL];
        assert_eq!(sizes, [12.0, 14.0, 16.0, 18.0, 20.0, 24.0, 30.0, 36.0]);
        let lines = [LH_XS, LH_SM, LH_BASE, LH_LG, LH_XL, LH_XXL, LH_XXXL, LH_XXXXL];
        let px = [16.0, 20.0, 24.0, 28.0, 28.0, 32.0, 36.0, 40.0];
        for ((s, l), want) in sizes.iter().zip(lines).zip(px) {
            assert!((s * l - want).abs() < 0.01, "{s}px line height");
        }
        // The web's markdown headings: h1 1.5rem, h2 1.25rem, h3 1.125rem, h4+ 1rem.
        assert_eq!([XXL, XL, LG, BASE], [24.0, 20.0, 18.0, 16.0]);
    }
}
