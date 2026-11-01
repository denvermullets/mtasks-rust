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
/// for every family, plus Inter Bold (a static instance of the bundled
/// variable Inter at wght 700, opsz 14: Iced 0.13 cannot drive the weight axis).
pub const FONT_BYTES: [&[u8]; 10] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Bold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
    include_bytes!("../assets/fonts/SpaceGrotesk-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/FiraCode-Regular.ttf"),
    include_bytes!("../assets/fonts/SUSEMono-Regular.ttf"),
    include_bytes!("../assets/fonts/RobotoMono-Regular.ttf"),
    include_bytes!("../assets/fonts/AtkinsonHyperlegibleMono-Regular.ttf"),
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

/// Families that have a bundled Bold face. Add a family here when its Bold
/// file joins `FONT_BYTES`.
const BOLD_FAMILIES: [&str; 1] = ["Inter"];

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

    /// The weight of the bundled face the text stack picks for a family and
    /// weight, through the same font system Iced draws with.
    fn face_weight(family: &'static str, weight: iced::font::Weight) -> (String, u16) {
        use iced::advanced::graphics::text::{cosmic_text as ct, font_system, to_attributes};
        let mut fs = font_system().write().unwrap();
        for bytes in FONT_BYTES {
            fs.load_font(std::borrow::Cow::Borrowed(bytes));
        }
        let font = Font { weight, ..Font::with_name(family) };
        let mut buf = ct::Buffer::new(fs.raw(), ct::Metrics::new(16.0, 20.0));
        buf.set_text(fs.raw(), "Bold", to_attributes(font), ct::Shaping::Advanced);
        buf.shape_until_scroll(fs.raw(), false);
        let id = buf.layout_runs().next().unwrap().glyphs[0].font_id;
        let face = fs.raw().db().face(id).unwrap();
        (face.families[0].0.clone(), face.weight.0)
    }

    #[test]
    fn bold_request_picks_the_bold_face_where_one_is_bundled() {
        use iced::font::Weight;
        assert_eq!(face_weight("Inter", Weight::Bold), ("Inter".into(), 700));
        assert_eq!(face_weight("Inter", Weight::Normal), ("Inter".into(), 400));
        // Why `bold` leaves other families alone: with no bold face of their
        // own, the stack picks another family's bold (here a system font).
        assert_ne!(face_weight("IBM Plex Sans", Weight::Bold).0, "IBM Plex Sans");
    }

    #[test]
    fn bold_applies_only_to_families_with_a_bold_face() {
        for (key, bolded) in [("inter", true), ("system", true), ("ibm-plex-sans", false), ("jetbrains-mono", false)] {
            let f = bold(font_for_key(key));
            assert_eq!(f.family, font_for_key(key).family, "{key} keeps its family");
            assert_eq!(f.weight == iced::font::Weight::Bold, bolded, "{key}");
        }
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
        // One regular face per bundled family, plus Inter Bold.
        assert_eq!(FONT_BYTES.len(), FONTS.len());
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
