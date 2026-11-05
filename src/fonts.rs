//! Bundled UI fonts. Keys match the Rails `AVAILABLE_FONTS`.
//!
//! Iced 0.13 fixes the default font at launch, so a font change applies only
//! after a restart.

use iced::Font;

pub const DEFAULT_FONT_KEY: &str = "inter";

pub struct FontDef {
    pub key: &'static str,
    pub name: &'static str,
    /// Family name inside the font file; `None` for the system font.
    family: Option<&'static str>,
}

/// Ordered font catalog (matches Rails `AVAILABLE_FONTS`).
pub const FONTS: [FontDef; 10] = [
    FontDef {
        key: "inter",
        name: "Inter",
        family: Some("Inter"),
    },
    FontDef {
        key: "ibm-plex-sans",
        name: "IBM Plex Sans",
        family: Some("IBM Plex Sans"),
    },
    FontDef {
        key: "space-grotesk",
        name: "Space Grotesk",
        family: Some("Space Grotesk"),
    },
    FontDef {
        key: "ibm-plex-mono",
        name: "IBM Plex Mono",
        family: Some("IBM Plex Mono"),
    },
    FontDef {
        key: "jetbrains-mono",
        name: "JetBrains Mono",
        family: Some("JetBrains Mono"),
    },
    FontDef {
        key: "fira-code",
        name: "Fira Code",
        family: Some("Fira Code"),
    },
    FontDef {
        key: "suse-mono",
        name: "SUSE Mono",
        family: Some("SUSE Mono"),
    },
    FontDef {
        key: "roboto-mono",
        name: "Roboto Mono",
        family: Some("Roboto Mono"),
    },
    FontDef {
        key: "atkinson-hyperlegible-mono",
        name: "Atkinson Hyperlegible Mono",
        family: Some("Atkinson Hyperlegible Mono"),
    },
    FontDef {
        key: "system",
        name: "System",
        family: None,
    },
];

/// Font files embedded in the binary, registered at launch. Regular weight
/// for every family, plus a Bold face for each (and Italic and Bold Italic
/// where upstream has them). Iced 0.13 cannot drive a weight axis, so the
/// faces are static files: official static TTFs where upstream ships them
/// (Inter 4.1 release, IBM Plex Mono), otherwise instances of the Google Fonts
/// variable fonts at wght 700 (and wdth 100, opsz 14 for Inter), keeping the
/// family name. Provenance is recorded on JRST-267, -268, -270 and -271.
pub const FONT_BYTES: [&[u8]; 32] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Bold.ttf"),
    include_bytes!("../assets/fonts/Inter-Italic.ttf"),
    include_bytes!("../assets/fonts/Inter-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Bold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Italic.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/SpaceGrotesk-Regular.ttf"),
    include_bytes!("../assets/fonts/SpaceGrotesk-Bold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-Bold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-Italic.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/FiraCode-Regular.ttf"),
    include_bytes!("../assets/fonts/FiraCode-Bold.ttf"),
    include_bytes!("../assets/fonts/SUSEMono-Regular.ttf"),
    include_bytes!("../assets/fonts/SUSEMono-Bold.ttf"),
    include_bytes!("../assets/fonts/SUSEMono-Italic.ttf"),
    include_bytes!("../assets/fonts/SUSEMono-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/RobotoMono-Regular.ttf"),
    include_bytes!("../assets/fonts/RobotoMono-Bold.ttf"),
    include_bytes!("../assets/fonts/RobotoMono-Italic.ttf"),
    include_bytes!("../assets/fonts/RobotoMono-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/AtkinsonHyperlegibleMono-Regular.ttf"),
    include_bytes!("../assets/fonts/AtkinsonHyperlegibleMono-Bold.ttf"),
    include_bytes!("../assets/fonts/AtkinsonHyperlegibleMono-Italic.ttf"),
    include_bytes!("../assets/fonts/AtkinsonHyperlegibleMono-BoldItalic.ttf"),
];

fn lookup(key: &str) -> &'static FontDef {
    FONTS.iter().find(|f| f.key == key).unwrap_or(&FONTS[0]) // unknown -> Inter
}

/// Resolve a font key to an Iced font. `system` is Iced's default font;
/// unknown keys fall back to Inter.
pub fn font_for_key(key: &str) -> Font {
    match lookup(key).family {
        Some(family) => Font::with_name(family),
        None => Font::DEFAULT,
    }
}

static ACTIVE: std::sync::OnceLock<Font> = std::sync::OnceLock::new();

/// Record the app's UI font (set once at launch, where it is also fixed).
pub fn set_active(font: Font) {
    let _ = ACTIVE.set(font);
}

/// The app's UI font; Iced's default before `set_active` runs.
pub fn active() -> Font {
    ACTIVE.get().copied().unwrap_or(Font::DEFAULT)
}

/// The UI font in bold (see `bold`).
pub fn bold_active() -> Font {
    bold(active())
}

/// Families that have a bundled Bold face (and Bold Italic, where they have an
/// Italic). Add a family here when its Bold file joins `FONT_BYTES`.
const BOLD_FAMILIES: [&str; 9] = [
    "Inter",
    "IBM Plex Sans",
    "Space Grotesk",
    "IBM Plex Mono",
    "JetBrains Mono",
    "Fira Code",
    "SUSE Mono",
    "Roboto Mono",
    "Atkinson Hyperlegible Mono",
];

/// `font` at bold weight, if its family has a bundled Bold face. Without one,
/// Iced's text stack does not fall back to the nearest weight of the same
/// family: it draws a bold face of some other (system) family, so a family
/// without a bold file stays regular.
pub fn bold(font: Font) -> Font {
    match font.family {
        iced::font::Family::Name(n) if !BOLD_FAMILIES.contains(&n) => font,
        _ => Font { weight: iced::font::Weight::Bold, ..font },
    }
}

/// Families that have a bundled Italic face. Add a family here when its Italic
/// file joins `FONT_BYTES`.
const ITALIC_FAMILIES: [&str; 7] = [
    "Inter",
    "IBM Plex Sans",
    "IBM Plex Mono",
    "JetBrains Mono",
    "SUSE Mono",
    "Roboto Mono",
    "Atkinson Hyperlegible Mono",
];

/// Families whose upstream release has no italic, with the bundled family
/// whose italic stands in for it.
const ITALIC_SUBSTITUTES: [(&str, &str); 2] = [("Space Grotesk", "Inter"), ("Fira Code", "JetBrains Mono")];

/// `font` in italic style, if its family has a bundled Italic face, or the
/// substitute family's italic if upstream ships none; other families stay
/// upright for the same reason as in [`bold`]. Composes with [`bold`] in either
/// order: every family that has an italic also has a bold italic.
pub fn italic(font: Font) -> Font {
    let style = iced::font::Style::Italic;
    match font.family {
        iced::font::Family::Name(n) if ITALIC_FAMILIES.contains(&n) => Font { style, ..font },
        iced::font::Family::Name(n) => match ITALIC_SUBSTITUTES.iter().find(|(from, _)| *from == n) {
            Some((_, to)) => Font { family: iced::font::Family::Name(to), style, ..font },
            None => font,
        },
        _ => Font { style, ..font },
    }
}

/// Family names of the monospace entries in the catalog.
const MONO_FAMILIES: [&str; 6] = [
    "IBM Plex Mono",
    "JetBrains Mono",
    "Fira Code",
    "SUSE Mono",
    "Roboto Mono",
    "Atkinson Hyperlegible Mono",
];

/// The bundled monospace face used when the UI font is not monospace.
const FALLBACK_MONO: &str = "JetBrains Mono";

/// Font for code: the UI font when that is monospace, else JetBrains Mono.
pub fn code() -> Font {
    code_for(active())
}

fn code_for(ui: Font) -> Font {
    match ui.family {
        iced::font::Family::Name(n) if MONO_FAMILIES.contains(&n) => ui,
        _ => Font::with_name(FALLBACK_MONO),
    }
}

/// Default Terminal font: a config key from the catalog.
pub const DEFAULT_TERMINAL_FONT_KEY: &str = "jetbrains-mono";

/// Default Terminal font size, in points.
pub const DEFAULT_TERMINAL_SIZE: f32 = 13.0;

/// Sizes the picker offers.
pub const TERMINAL_SIZES: [u16; 12] = [9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24];

/// A Terminal font size from config, kept inside the picker's range so a
/// hand-edited file can't ask for a zero-sized or absurd grid.
pub fn clamp_terminal_size(size: f32) -> f32 {
    if size.is_finite() {
        size.clamp(TERMINAL_SIZES[0] as f32, TERMINAL_SIZES[TERMINAL_SIZES.len() - 1] as f32)
    } else {
        DEFAULT_TERMINAL_SIZE
    }
}

/// A monospace catalog entry, which is all the Terminal font can be.
fn terminal_def(key: &str) -> &'static FontDef {
    FONTS
        .iter()
        .find(|f| f.key == key && f.family.is_some_and(|n| MONO_FAMILIES.contains(&n)))
        .unwrap_or_else(|| lookup(DEFAULT_TERMINAL_FONT_KEY))
}

/// Family name for a Terminal font key. A key that is unknown or not
/// monospace resolves to JetBrains Mono.
pub fn terminal_family(key: &str) -> &'static str {
    terminal_def(key).family.unwrap_or(FALLBACK_MONO)
}

/// The monospace fonts a Terminal can use, in catalog order, for a picker.
pub fn terminal_choices() -> Vec<FontChoice> {
    FONTS
        .iter()
        .filter(|f| f.family.is_some_and(|n| MONO_FAMILIES.contains(&n)))
        .map(|f| FontChoice { key: f.key, name: f.name })
        .collect()
}

/// The picker choice for a Terminal font key.
pub fn terminal_choice_for_key(key: &str) -> FontChoice {
    let f = terminal_def(key);
    FontChoice { key: f.key, name: f.name }
}

/// A catalog entry for pick lists: displays as the font name, carries the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontChoice {
    pub key: &'static str,
    pub name: &'static str,
}

impl std::fmt::Display for FontChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

/// All fonts in catalog order, for a picker.
pub fn choices() -> Vec<FontChoice> {
    FONTS
        .iter()
        .map(|f| FontChoice {
            key: f.key,
            name: f.name,
        })
        .collect()
}

/// The choice for a config key; an unknown key resolves to the default font.
pub fn choice_for_key(key: &str) -> FontChoice {
    let f = lookup(key);
    FontChoice {
        key: f.key,
        name: f.name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Family, weight and whether it is italic, for the face the text stack
    /// picks for `font`, through the same font system Iced draws with. A
    /// missing face does not fail: the stack silently picks another family,
    /// so every bundled face needs an assertion on this.
    fn face(font: Font) -> (String, u16, bool) {
        use iced::advanced::graphics::text::{cosmic_text as ct, font_system, to_attributes};
        let mut fs = font_system().write().unwrap();
        for bytes in FONT_BYTES {
            fs.load_font(std::borrow::Cow::Borrowed(bytes));
        }
        let mut buf = ct::Buffer::new(fs.raw(), ct::Metrics::new(16.0, 20.0));
        buf.set_text(fs.raw(), "Bold", to_attributes(font), ct::Shaping::Advanced);
        buf.shape_until_scroll(fs.raw(), false);
        let id = buf.layout_runs().next().unwrap().glyphs[0].font_id;
        let face = fs.raw().db().face(id).unwrap();
        (face.families[0].0.clone(), face.weight.0, face.style != ct::fontdb::Style::Normal)
    }

    /// What `bold` and `italic` turn a catalog font into, as the face drawn.
    fn styled(key: &str, bold_it: bool, italic_it: bool) -> (String, u16, bool) {
        let mut f = font_for_key(key);
        if bold_it {
            f = bold(f);
        }
        if italic_it {
            f = italic(f);
        }
        face(f)
    }

    #[test]
    fn sans_families_draw_their_own_bold_and_italic_faces() {
        // (key, family, has its own italic). Bold, italic and bold italic all
        // land on a face of the same family with the right weight and slant.
        for (key, family) in [("inter", "Inter"), ("ibm-plex-sans", "IBM Plex Sans")] {
            assert_eq!(styled(key, false, false), (family.into(), 400, false), "{key} regular");
            assert_eq!(styled(key, true, false), (family.into(), 700, false), "{key} bold");
            assert_eq!(styled(key, false, true), (family.into(), 400, true), "{key} italic");
            assert_eq!(styled(key, true, true), (family.into(), 700, true), "{key} bold italic");
        }
        assert_eq!(styled("space-grotesk", false, false), ("Space Grotesk".into(), 400, false));
        assert_eq!(styled("space-grotesk", true, false), ("Space Grotesk".into(), 700, false));
    }

    #[test]
    fn space_grotesk_has_no_italic_so_italic_uses_inter_italic() {
        // Upstream Space Grotesk ships no italic: the substitute is Inter's.
        assert_eq!(styled("space-grotesk", false, true), ("Inter".into(), 400, true));
        assert_eq!(styled("space-grotesk", true, true), ("Inter".into(), 700, true));
    }

    #[test]
    fn mono_families_draw_their_own_bold_and_italic_faces() {
        for (key, family) in [
            ("ibm-plex-mono", "IBM Plex Mono"),
            ("jetbrains-mono", "JetBrains Mono"),
            ("suse-mono", "SUSE Mono"),
            ("roboto-mono", "Roboto Mono"),
            ("atkinson-hyperlegible-mono", "Atkinson Hyperlegible Mono"),
        ] {
            assert_eq!(styled(key, false, false), (family.into(), 400, false), "{key} regular");
            assert_eq!(styled(key, true, false), (family.into(), 700, false), "{key} bold");
            assert_eq!(styled(key, false, true), (family.into(), 400, true), "{key} italic");
            assert_eq!(styled(key, true, true), (family.into(), 700, true), "{key} bold italic");
        }
        assert_eq!(styled("fira-code", false, false), ("Fira Code".into(), 400, false));
        assert_eq!(styled("fira-code", true, false), ("Fira Code".into(), 700, false));
    }

    #[test]
    fn fira_code_has_no_italic_so_italic_uses_jetbrains_mono_italic() {
        // Upstream Fira Code ships no italic: the monospace substitute is
        // JetBrains Mono's, so code stays monospace.
        assert_eq!(styled("fira-code", false, true), ("JetBrains Mono".into(), 400, true));
        assert_eq!(styled("fira-code", true, true), ("JetBrains Mono".into(), 700, true));
    }

    #[test]
    fn bold_and_italic_compose_in_either_order() {
        for key in FONTS.iter().map(|f| f.key) {
            let a = italic(bold(font_for_key(key)));
            let b = bold(italic(font_for_key(key)));
            assert_eq!(face(a), face(b), "{key}");
            assert_eq!((a.weight, a.style), (iced::font::Weight::Bold, iced::font::Style::Italic), "{key}");
        }
    }

    #[test]
    fn system_font_bold_and_italic_draw_distinct_faces() {
        let regular = face(Font::DEFAULT);
        let (_, w, i) = face(bold(Font::DEFAULT));
        assert!(w >= 700 && !i, "system bold");
        let (_, w, i) = face(italic(Font::DEFAULT));
        assert!(w < 700 && i, "system italic");
        assert_eq!(regular.1, 400);
    }

    #[test]
    fn unbundled_named_families_are_left_alone() {
        let f = Font::with_name("Comic Sans");
        assert_eq!(bold(f), f);
        assert_eq!(italic(f), f);
    }

    #[test]
    fn code_font_is_monospace_for_every_font_choice() {
        for f in FONTS.iter() {
            let code = code_for(font_for_key(f.key));
            let iced::font::Family::Name(n) = code.family else { panic!("{} gave a non-named family", f.key) };
            assert!(MONO_FAMILIES.contains(&n), "{}: {n}", f.key);
            // A monospace choice keeps itself; everything else gets JetBrains Mono.
            let mono = MONO_FAMILIES.iter().any(|m| Some(*m) == f.family);
            assert_eq!(n, if mono { f.family.unwrap() } else { FALLBACK_MONO }, "{}", f.key);
        }
    }

    #[test]
    fn terminal_choices_are_the_six_monospace_fonts() {
        let names: Vec<_> = terminal_choices().iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            ["IBM Plex Mono", "JetBrains Mono", "Fira Code", "SUSE Mono", "Roboto Mono", "Atkinson Hyperlegible Mono"]
        );
        assert_eq!(terminal_choice_for_key("fira-code").key, "fira-code");
    }

    #[test]
    fn terminal_size_is_clamped_to_the_pickers_range() {
        assert_eq!(clamp_terminal_size(13.0), 13.0);
        assert_eq!(clamp_terminal_size(0.0), 9.0);
        assert_eq!(clamp_terminal_size(400.0), 24.0);
        assert_eq!(clamp_terminal_size(f32::NAN), DEFAULT_TERMINAL_SIZE);
        assert!(TERMINAL_SIZES.contains(&(DEFAULT_TERMINAL_SIZE as u16)));
    }

    #[test]
    fn terminal_font_falls_back_to_jetbrains_mono() {
        for key in ["inter", "system", "nope", ""] {
            assert_eq!(terminal_family(key), "JetBrains Mono", "{key}");
            assert_eq!(terminal_choice_for_key(key).key, DEFAULT_TERMINAL_FONT_KEY, "{key}");
        }
        assert_eq!(terminal_family("roboto-mono"), "Roboto Mono");
    }

    #[test]
    fn choices_list_all_fonts_in_catalog_order_by_name() {
        let c = choices();
        assert_eq!(c.len(), FONTS.len());
        for (choice, f) in c.iter().zip(FONTS.iter()) {
            assert_eq!(choice.key, f.key);
            assert_eq!(choice.to_string(), f.name);
        }
        assert_eq!(c[0].key, DEFAULT_FONT_KEY);
    }

    #[test]
    fn choice_for_key_resolves_known_and_falls_back_to_default() {
        assert_eq!(choice_for_key("fira-code").key, "fira-code");
        assert_eq!(choice_for_key("nope").key, DEFAULT_FONT_KEY);
    }

    #[test]
    fn catalog_has_all_rails_keys() {
        let keys: Vec<_> = FONTS.iter().map(|f| f.key).collect();
        assert_eq!(
            keys,
            [
                "inter",
                "ibm-plex-sans",
                "space-grotesk",
                "ibm-plex-mono",
                "jetbrains-mono",
                "fira-code",
                "suse-mono",
                "roboto-mono",
                "atkinson-hyperlegible-mono",
                "system"
            ]
        );
    }

    #[test]
    fn known_key_resolves_to_named_font() {
        assert_eq!(
            font_for_key("jetbrains-mono"),
            Font::with_name("JetBrains Mono")
        );
    }

    #[test]
    fn system_resolves_to_default_font() {
        assert_eq!(font_for_key("system"), Font::DEFAULT);
    }

    #[test]
    fn unknown_key_resolves_to_inter() {
        assert_eq!(font_for_key("comic-sans"), Font::with_name("Inter"));
    }
}
