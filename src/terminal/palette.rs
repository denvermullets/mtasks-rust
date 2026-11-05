//! Terminal colours derived from an app theme (spec "Colours", JRST-332).
//!
//! Nothing is hand-defined per theme: the screen colours come from theme tokens,
//! the six hues come from one fixed set for dark themes and one for light, and
//! entries 16-255 stay standard.
use super::types::{Rgb, TermTheme};
use crate::theme::{self, Tokens};
use iced::Color;

/// Red, green, yellow, blue, magenta, cyan.
const TW_400: [Rgb; 6] = hues(["f87171", "4ade80", "facc15", "60a5fa", "c084fc", "22d3ee"]);
const TW_300: [Rgb; 6] = hues(["fca5a5", "86efac", "fde047", "93c5fd", "d8b4fe", "67e8f9"]);
// JRST-392: green and yellow are darker than Tailwind 700 (15803d, a16207) to reach 4.5:1.
const TW_700: [Rgb; 6] = hues(["b91c1c", "146c36", "945806", "1d4ed8", "7e22ce", "0e7490"]);
const TW_800: [Rgb; 6] = hues(["991b1b", "166534", "854d0e", "1e40af", "6b21a8", "155e75"]);

const fn hex_byte(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        _ => panic!("not a lowercase hex digit"),
    }
}

const fn hues<const N: usize>(h: [&str; N]) -> [Rgb; N] {
    let mut out = [[0u8; 3]; N];
    let mut i = 0;
    while i < N {
        let s = h[i].as_bytes();
        let mut c = 0;
        while c < 3 {
            out[i][c] = hex_byte(s[c * 2]) * 16 + hex_byte(s[c * 2 + 1]);
            c += 1;
        }
        i += 1;
    }
    out
}

fn rgb(c: Color) -> Rgb {
    let b = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    [b(c.r), b(c.g), b(c.b)]
}

/// `top` over `base` at `alpha` (0.0 to 1.0).
fn mix(top: Rgb, base: Rgb, alpha: f32) -> Rgb {
    let m = |t: u8, b: u8| (t as f32 * alpha + b as f32 * (1.0 - alpha)).round() as u8;
    [m(top[0], base[0]), m(top[1], base[1]), m(top[2], base[2])]
}

/// The Terminal colours for an app theme.
pub fn term_theme(t: &Tokens) -> TermTheme {
    // The spec's gray[n] is `t.gray[n]` here (0-based, gray[0] is gray-100).
    let g = |i: usize| rgb(t.gray[i]);
    let (normal, bright) = if t.light { (TW_700, TW_800) } else { (TW_400, TW_300) };
    let mut ansi = [[0u8; 3]; 16];
    ansi[0] = if t.light { g(0) } else { rgb(t.menu) };
    ansi[7] = if t.light { g(3) } else { g(2) };
    ansi[8] = if t.light { g(4) } else { g(3) };
    ansi[15] = if t.light { g(1) } else { g(0) };
    ansi[1..7].copy_from_slice(&normal);
    ansi[9..15].copy_from_slice(&bright);
    let bg = rgb(t.background);
    TermTheme {
        fg: g(1),
        bg,
        cursor: rgb(t.accent),
        selection: mix(rgb(t.accent), bg, 0.35),
        light: t.light,
        ansi,
    }
}

/// The Terminal colours for the active app theme. Spawn and live theme changes use this.
pub fn active_term_theme() -> TermTheme {
    term_theme(theme::tok())
}

/// The draw-time contrast floor (JRST-393): below this, a cell's text colour moves.
pub const CONTRAST_FLOOR: f32 = 3.0;

fn lum(c: Rgb) -> f32 {
    let l = |x: u8| {
        let x = x as f32 / 255.0;
        if x <= 0.03928 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * l(c[0]) + 0.7152 * l(c[1]) + 0.0722 * l(c[2])
}

fn ratio(a: Rgb, b: Rgb) -> f32 {
    let (a, b) = (lum(a), lum(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `fg` unchanged if it reads against `bg` at `floor`, otherwise moved toward
/// white or black (whichever contrasts more with `bg`) in 10% steps until it does.
pub fn min_contrast(fg: Rgb, bg: Rgb, floor: f32) -> Rgb {
    if ratio(fg, bg) >= floor {
        return fg;
    }
    let to: Rgb = if ratio([255; 3], bg) >= ratio([0; 3], bg) { [255; 3] } else { [0; 3] };
    for i in 1..=10 {
        let m = i as f32 / 10.0;
        let c = [0, 1, 2].map(|n| (fg[n] as f32 + (to[n] as f32 - fg[n] as f32) * m) as u8);
        if ratio(c, bg) >= floor {
            return c;
        }
    }
    to
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_key(k: &str) -> &'static Tokens {
        theme::THEMES.iter().find(|t| t.key == k).unwrap()
    }

    #[test]
    fn dark_theme_mapping() {
        let t = by_key("default");
        let p = term_theme(t);
        assert!(!p.light);
        assert_eq!(p.bg, rgb(t.background));
        assert_eq!(p.fg, rgb(t.gray[1]));
        assert_eq!(p.cursor, rgb(t.accent));
        assert_eq!(p.ansi[0], rgb(t.menu));
        assert_eq!(p.ansi[1], [0xf8, 0x71, 0x71]);
        assert_eq!(p.ansi[3], [0xfa, 0xcc, 0x15]);
        assert_eq!(p.ansi[7], rgb(t.gray[2]));
        assert_eq!(p.ansi[8], rgb(t.gray[3]));
        assert_eq!(p.ansi[9], [0xfc, 0xa5, 0xa5]);
        assert_eq!(p.ansi[14], [0x67, 0xe8, 0xf9]);
        assert_eq!(p.ansi[15], rgb(t.gray[0]));
    }

    #[test]
    fn light_theme_mapping() {
        let t = by_key("warm-paper");
        let p = term_theme(t);
        assert!(p.light);
        assert_eq!(p.bg, rgb(t.background));
        assert_eq!(p.fg, rgb(t.gray[1]));
        assert_eq!(p.ansi[0], rgb(t.gray[0]));
        assert_eq!(p.ansi[1], [0xb9, 0x1c, 0x1c]);
        assert_eq!(p.ansi[6], [0x0e, 0x74, 0x90]);
        assert_eq!(p.ansi[7], rgb(t.gray[3]));
        assert_eq!(p.ansi[8], rgb(t.gray[4]));
        assert_eq!(p.ansi[9], [0x99, 0x1b, 0x1b]);
        assert_eq!(p.ansi[14], [0x15, 0x5e, 0x75]);
        assert_eq!(p.ansi[15], rgb(t.gray[1]));
    }

    #[test]
    fn selection_is_accent_35_percent_over_background() {
        let p = term_theme(by_key("default"));
        assert_eq!(p.selection, mix(p.cursor, p.bg, 0.35));
        assert_ne!(p.selection, p.bg);
    }

    #[test]
    fn non_black_colours_have_4_5_contrast_on_every_theme() {
        let color = |c: Rgb| Color::from_rgb8(c[0], c[1], c[2]);
        let mut low = vec![];
        for t in theme::THEMES.iter() {
            let p = term_theme(t);
            // Entry 0 is black; the others, including bright black, must read.
            for (i, c) in p.ansi.iter().enumerate().skip(1) {
                let r = theme::contrast_ratio(color(*c), color(p.bg));
                if r < 4.5 {
                    low.push(format!("{} ansi[{i}] {c:02x?} {r:.2}", t.key));
                }
            }
        }
        assert!(low.is_empty(), "under 4.5:1:\n{}", low.join("\n"));
    }

    #[test]
    fn every_theme_produces_a_palette_with_red_staying_red() {
        assert_eq!(theme::THEMES.len(), 15);
        for t in theme::THEMES.iter() {
            let p = term_theme(t);
            assert_eq!(p.light, t.light, "{}", t.key);
            for c in [p.ansi[1], p.ansi[9]] {
                assert!(c[0] > c[1] && c[0] > c[2], "{} red is not red", t.key);
            }
        }
    }

    #[test]
    fn floor_leaves_readable_colours_alone() {
        assert_eq!(min_contrast([255; 3], [0; 3], 3.0), [255; 3]);
        assert_eq!(min_contrast([200, 50, 50], [250, 250, 250], 3.0), [200, 50, 50]);
    }

    #[test]
    fn floor_darkens_white_on_a_light_background() {
        // 256-colour 231 is white: about 1.1:1 on a light theme.
        let bg = [250, 247, 240];
        let out = min_contrast([255; 3], bg, CONTRAST_FLOOR);
        assert!(ratio(out, bg) >= CONTRAST_FLOOR);
        assert!(out[0] < 200, "moved toward black: {out:?}");
    }

    #[test]
    fn floor_lightens_black_on_a_dark_background() {
        let bg = [20, 22, 30];
        let out = min_contrast([0; 3], bg, CONTRAST_FLOOR);
        assert!(ratio(out, bg) >= CONTRAST_FLOOR);
        assert!(out[0] > 40, "moved toward white: {out:?}");
    }

    #[test]
    fn floor_ends_at_the_extreme_when_nothing_reaches_it() {
        // Mid-grey background: neither extreme reaches 21:1.
        assert_eq!(min_contrast([128; 3], [128; 3], 21.0), [0; 3]);
    }

    #[test]
    fn floor_keeps_the_hue_direction_by_blending() {
        let bg = [255; 3];
        let out = min_contrast([255, 255, 100], bg, CONTRAST_FLOOR);
        assert!(ratio(out, bg) >= CONTRAST_FLOOR);
        assert!(out[0] == out[1], "blended toward black, so channels stay in proportion");
    }
}
