//! The "What's new" modal: the bundled CHANGELOG.md, newest release first.

use iced::widget::{button, column, container, row, scrollable};
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::theme as t;
use crate::views::text;

/// The changelog, baked in at build time so it always matches this build.
const CHANGELOG: &str = include_str!("../../CHANGELOG.md");

/// The changelog's release sections: everything from the first `## ` heading
/// on, so the `last-sha` marker and the `# What's new` title (the modal has
/// its own) are left out. Pure.
pub fn releases(changelog: &str) -> &str {
    let start = if changelog.starts_with("## ") {
        Some(0)
    } else {
        changelog.find("\n## ").map(|i| i + 1)
    };
    start.map_or("", |i| changelog[i..].trim_end())
}

pub fn view<'a>(ctx: &crate::views::body::BodyCtx<'a>) -> Element<'a, Message> {
    let notes = crate::views::body::view(releases(CHANGELOG), 13.0, t::text_secondary(), ctx);
    let content = column![]
        .spacing(14)
        .width(Length::Fixed(560.0))
        .push(
            row![]
                .align_y(iced::Alignment::Center)
                .push(text("What's new").size(18).color(t::text_primary()))
                .push(iced::widget::space::horizontal().width(Length::Fill))
                .push(
                    text(format!("You have {}", env!("CARGO_PKG_VERSION")))
                        .size(12)
                        .color(t::text_muted()),
                ),
        )
        .push(
            scrollable(container(notes).padding(iced::Padding { right: 12.0, ..Default::default() }))
                .height(Length::Fixed(460.0)),
        )
        .push(
            row![].push(iced::widget::space::horizontal().width(Length::Fill)).push(
                button(text("Close").size(14))
                    .on_press(Message::CloseModal)
                    .padding([8, 24])
                    .style(button::secondary),
            ),
        );

    let modal = container(content).padding(28).style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::bg_modal())),
        border: Border { color: t::border(), width: 1.0, radius: 8.0.into() },
        ..Default::default()
    });
    crate::views::util::modal_backdrop(modal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_drops_marker_and_title() {
        let log = "<!-- last-sha: abc -->\n# What's new\n\n## 1.0.0 · 2026-01-01\n- **A.** b\n";
        assert_eq!(releases(log), "## 1.0.0 · 2026-01-01\n- **A.** b");
    }

    #[test]
    fn releases_of_bundled_changelog_starts_at_a_release() {
        assert!(releases(CHANGELOG).starts_with("## "));
    }

    #[test]
    fn releases_without_any_release_is_empty() {
        assert_eq!(releases("# What's new\n"), "");
    }
}
