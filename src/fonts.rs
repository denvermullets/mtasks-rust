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

/// Regular-weight font files embedded in the binary, registered at launch.
pub const FONT_BYTES: [&[u8]; 9] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
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

/// Display name for a font key (unknown -> Inter).
pub fn font_name(key: &str) -> &'static str {
    lookup(key).name
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(FONT_BYTES.len(), FONTS.len() - 1);
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
        assert_eq!(font_name("nope"), "Inter");
    }
}
