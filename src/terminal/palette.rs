//! Terminal colours derived from the active app theme (spec "Colours").
//! Pure: takes the theme's tokens and returns plain data.
use super::{Rgb, TermTheme};
use crate::theme::Tokens;
use iced::Color;

/// Red, green, yellow, blue, magenta, cyan.
const TW_400: [Rgb; 6] = [
    [0xf8, 0x71, 0x71],
    [0x4a, 0xde, 0x80],
    [0xfa, 0xcc, 0x15],
    [0x60, 0xa5, 0xfa],
    [0xc0, 0x84, 0xfc],
    [0x22, 0xd3, 0xee],
];
const TW_300: [Rgb; 6] = [
    [0xfc, 0xa5, 0xa5],
    [0x86, 0xef, 0xac],
    [0xfd, 0xe0, 0x47],
    [0x93, 0xc5, 0xfd],
    [0xd8, 0xb4, 0xfe],
    [0x67, 0xe8, 0xf9],
];
const TW_700: [Rgb; 6] = [
    [0xb9, 0x1c, 0x1c],
    [0x15, 0x80, 0x3d],
    [0xa1, 0x62, 0x07],
    [0x1d, 0x4e, 0xd8],
    [0x7e, 0x22, 0xce],
    [0x0e, 0x74, 0x90],
];
const TW_800: [Rgb; 6] = [
    [0x99, 0x1b, 0x1b],
    [0x16, 0x65, 0x34],
    [0x85, 0x4d, 0x0e],
    [0x1e, 0x40, 0xaf],
    [0x6b, 0x21, 0xa8],
    [0x15, 0x5e, 0x75],
];

fn rgb(c: Color) -> Rgb {
    [
        (c.r * 255.0).round() as u8,
        (c.g * 255.0).round() as u8,
        (c.b * 255.0).round() as u8,
    ]
}

pub fn term_theme(t: &Tokens) -> TermTheme {
    let g = |i: usize| rgb(t.gray[i]);
    let (normal, bright) = if t.light {
        (TW_700, TW_800)
    } else {
        (TW_400, TW_300)
    };
    let (black, white, bright_black, bright_white) = if t.light {
        (g(0), g(3), g(4), g(1))
    } else {
        (rgb(t.menu), g(2), g(3), g(0))
    };
    let mut ansi = [[0u8; 3]; 16];
    ansi[0] = black;
    ansi[1..7].copy_from_slice(&normal);
    ansi[7] = white;
    ansi[8] = bright_black;
    ansi[9..15].copy_from_slice(&bright);
    ansi[15] = bright_white;
    TermTheme {
        fg: g(1),
        bg: rgb(t.background),
        cursor: rgb(t.accent),
        ansi,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::THEMES;

    #[test]
    fn dark_themes_use_the_light_tailwind_steps_and_gray_100_to_300() {
        let dark = THEMES.iter().find(|t| !t.light).unwrap();
        let th = term_theme(dark);
        assert_eq!(th.ansi[1], [0xf8, 0x71, 0x71]);
        assert_eq!(th.ansi[9], [0xfc, 0xa5, 0xa5]);
        assert_eq!(th.fg, [0xe5, 0xe7, 0xeb]); // gray[1]
        assert_eq!(th.ansi[7], [0xd1, 0xd5, 0xdc]); // gray[2]
        assert_eq!(th.ansi[15], [0xf3, 0xf4, 0xf6]); // gray[0]
    }

    #[test]
    fn red_stays_red_on_every_theme() {
        for t in THEMES.iter() {
            let th = term_theme(t);
            for i in [1usize, 9] {
                let [r, g, b] = th.ansi[i];
                assert!(r > g && r > b, "{}: ansi {i} is not red: {:?}", t.key, th.ansi[i]);
            }
            assert_eq!(th.bg, rgb(t.background), "{}", t.key);
        }
    }

    #[test]
    fn light_themes_swap_white_to_a_dark_gray() {
        let light = THEMES.iter().find(|t| t.light).expect("a light theme");
        let th = term_theme(light);
        assert_eq!(th.ansi[1], [0xb9, 0x1c, 0x1c]);
        let lum = |c: Rgb| c.iter().map(|&v| v as u32).sum::<u32>();
        assert!(lum(th.ansi[7]) < lum(th.bg), "white (7) must be darker than a light background");
    }
}
