//! The app's `text` widget: Iced's, but with font fallback for non-ASCII.
//!
//! Iced 0.13's `text()` defaults to `Shaping::Basic`, which never falls back
//! to another font, so any glyph the active UI font lacks (●, ▸, ⌨, ✓, CJK,
//! emoji, …) renders as tofu. This wrapper switches to `Shaping::Advanced`
//! (cosmic-text's system-font fallback) whenever the content isn't pure ASCII,
//! and keeps the cheaper basic shaping otherwise.
//!
//! Import it as `use crate::views::text;` instead of `iced::widget::text`;
//! the module re-exports Iced's, so `text::Wrapping` etc. still resolve.

pub use iced::widget::text::*;

/// Iced's `text()` with shaping chosen by [`shaping_for`].
pub fn text<'a, Theme, Renderer>(content: impl IntoFragment<'a>) -> Text<'a, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: iced::advanced::text::Renderer,
{
    let content = content.into_fragment();
    let shaping = shaping_for(&content);
    Text::new(content).shaping(shaping)
}

/// Basic shaping for ASCII, Advanced (with font fallback) for anything else.
pub fn shaping_for(content: &str) -> Shaping {
    if content.is_ascii() {
        Shaping::Basic
    } else {
        Shaping::Advanced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_keeps_basic_shaping() {
        assert_eq!(shaping_for("MTG-2951 Backlog"), Shaping::Basic);
    }

    #[test]
    fn glyphs_get_advanced_shaping_for_font_fallback() {
        assert_eq!(shaping_for("\u{25CF} Scope"), Shaping::Advanced);
        assert_eq!(shaping_for("\u{2328} 3"), Shaping::Advanced);
        assert_eq!(shaping_for("日本語"), Shaping::Advanced);
    }
}
