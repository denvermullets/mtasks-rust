use iced::widget::{button, container, row, text, text_input};
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Issue, Project, Team};
use crate::theme as t;

/// Id of the header search box, so a shortcut can focus it.
pub fn search_input_id() -> text_input::Id {
    text_input::Id::new("header-search")
}

/// Normalize a raw search query into the lowercase needle the matchers expect.
/// An empty needle matches everything.
pub fn needle(query: &str) -> String {
    query.trim().to_lowercase()
}

/// Whether `issue` matches `needle` by title or identifier (e.g. "JAIT-111").
pub fn issue_matches(issue: &Issue, team: Option<&Team>, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let ident = match team {
        Some(tm) => format!("{}-{}", tm.identifier, issue.team_number.unwrap_or(0)),
        None => format!("#{}", issue.team_number.unwrap_or(0)),
    };
    ident.to_lowercase().contains(needle) || issue.title.to_lowercase().contains(needle)
}

/// Whether `project` matches `needle` by name.
pub fn project_matches(project: &Project, needle: &str) -> bool {
    needle.is_empty() || project.name.to_lowercase().contains(needle)
}

/// Top strip above the main pane: breadcrumb on the left; Search, Display and
/// "+ New" on the right. `search_enabled` is false on views with nothing to
/// filter; `new_action` is `None` where "+ New" has no target. `board_controls`
/// is `Some(active_filter_count)` on the board, where Display and Filter apply,
/// and `None` elsewhere (the buttons are disabled).
pub fn view<'a>(
    crumbs: Vec<String>,
    query: &'a str,
    search_enabled: bool,
    board_controls: Option<usize>,
    new_action: Option<Message>,
    back: Option<Message>,
) -> Element<'a, Message> {
    let mut trail = row![].spacing(8).align_y(Alignment::Center);
    if let Some(msg) = back {
        trail = trail.push(
            button(text("\u{2039} Back").size(13))
                .on_press(msg)
                .padding([3, 9])
                .style(button::secondary),
        );
    }
    let last = crumbs.len().saturating_sub(1);
    for (i, crumb) in crumbs.into_iter().enumerate() {
        if i > 0 {
            trail = trail.push(text("\u{203A}").size(14).color(t::TEXT_MUTED())); // ›
        }
        let color = if i == last {
            t::TEXT_PRIMARY()
        } else {
            t::TEXT_SECONDARY()
        };
        trail = trail.push(text(crumb).size(14).color(color));
    }

    let mut search = text_input(if search_enabled { "Search…" } else { "" }, query)
        .id(search_input_id())
        .padding([5, 10])
        .size(13)
        .width(Length::Fixed(220.0));
    if search_enabled {
        search = search.on_input(Message::HeaderSearchChanged);
    }

    let display = button(text("Display").size(13))
        .on_press_maybe(board_controls.map(|_| Message::OpenDisplayOptions))
        .padding([5, 12])
        .style(button::secondary);

    // Filter button with an active-filter count badge.
    let filter_count = board_controls.unwrap_or(0);
    let filter_label = if filter_count > 0 {
        format!("Filter \u{b7} {}", filter_count)
    } else {
        "Filter".to_string()
    };
    let filter = button(text(filter_label).size(13))
        .on_press_maybe(board_controls.map(|_| Message::OpenFilterPanel))
        .padding([5, 12])
        .style(move |theme: &Theme, status| {
            let mut style = button::secondary(theme, status);
            if filter_count > 0 {
                style.text_color = t::ACCENT();
                style.border = Border {
                    color: t::ACCENT(),
                    width: 1.0,
                    radius: 4.0.into(),
                };
            }
            style
        });

    let enabled = new_action.is_some();
    let mut new_button = button(text("+ New").size(13).color(if enabled {
        t::BG_PRIMARY()
    } else {
        t::TEXT_MUTED()
    }))
    .padding([5, 12])
    .style(move |_theme: &Theme, _status| button::Style {
        background: Some(iced::Background::Color(if enabled {
            t::ACCENT()
        } else {
            t::BG_CARD()
        })),
        border: Border {
            radius: 6.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });
    if let Some(msg) = new_action {
        new_button = new_button.on_press(msg);
    }

    let content = row![]
        .spacing(8)
        .align_y(Alignment::Center)
        .push(trail)
        .push(iced::widget::horizontal_space())
        .push(search)
        .push(filter)
        .push(display)
        .push(new_button);

    container(content)
        .padding([8, 16])
        .width(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::BG_SECONDARY())),
            border: Border {
                color: t::BORDER(),
                width: 0.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
}
