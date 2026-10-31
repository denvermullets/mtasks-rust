use std::collections::HashSet;

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Border, Element, Length, Theme};

use crate::app::{DepDirection, Message};
use crate::db::models::{Issue, Team};
use crate::theme as t;

/// State for the searchable issue-dependency picker modal. `direction` is the
/// chosen link kind (defaulting to the sidebar section that opened it); the
/// create handler uses it to orient the new edge.
pub struct IssuePickerState {
    pub query: String,
    pub direction: DepDirection,
}

impl IssuePickerState {
    pub fn new(direction: DepDirection) -> Self {
        Self {
            query: String::new(),
            direction,
        }
    }
}

/// Render the picker modal: a search box over a scrollable list of the team's
/// issues, excluding `excluded` (the open issue + any already-linked target).
/// Selecting a row creates the dependency; Cancel dismisses.
pub fn view<'a>(
    state: &'a IssuePickerState,
    issues: &'a [Issue],
    team: Option<&'a Team>,
    excluded: &HashSet<&str>,
) -> Element<'a, Message> {
    // Kind chips: the active one is highlighted, disabled kinds are inert.
    let mut kinds = row![].spacing(6);
    for kind in DepDirection::ALL {
        let active = kind == state.direction;
        let mut chip = button(text(kind.label()).size(12))
            .padding([4, 10])
            .style(if active { button::primary } else { button::secondary });
        if kind.enabled() {
            chip = chip.on_press(Message::SetDependencyKind(kind));
        }
        kinds = kinds.push(chip);
    }
    let identifier = |issue: &Issue| match team {
        Some(tm) => format!("{}-{}", tm.identifier, issue.team_number.unwrap_or(0)),
        None => format!("#{}", issue.team_number.unwrap_or(0)),
    };

    let needle = state.query.trim().to_lowercase();
    let mut list = column![].spacing(2);
    let mut shown = 0;
    for issue in issues {
        if excluded.contains(issue.id.as_str()) {
            continue;
        }
        let ident = identifier(issue);
        if !needle.is_empty()
            && !ident.to_lowercase().contains(&needle)
            && !issue.title.to_lowercase().contains(&needle)
        {
            continue;
        }
        shown += 1;
        list = list.push(
            button(
                row![]
                    .spacing(10)
                    .push(
                        text(ident)
                            .size(12)
                            .color(t::text_muted())
                            .width(Length::Fixed(90.0)),
                    )
                    .push(text(&issue.title).size(13).color(t::text_primary())),
            )
            .on_press(Message::DependencyPickerSelected(issue.id.clone()))
            .padding([6, 8])
            .width(Length::Fill)
            .style(button::text),
        );
    }

    let results: Element<'a, Message> = if shown == 0 {
        text("No matching issues")
            .size(13)
            .color(t::text_muted())
            .into()
    } else {
        scrollable(list).height(Length::Fixed(320.0)).into()
    };

    let content = column![]
        .spacing(14)
        .width(Length::Fixed(440.0))
        .push(text("Add link").size(18).color(t::text_primary()))
        .push(kinds)
        .push(text("Tab / Shift+Tab to change kind").size(11).color(t::text_muted()))
        .push(
            text_input("Search issues…", &state.query)
                .on_input(Message::DependencyPickerSearchChanged)
                .padding(8)
                .size(13)
                .width(Length::Fill),
        )
        .push(results)
        .push(
            button(text("Cancel").size(14))
                .on_press(Message::CloseModal)
                .padding([8, 24])
                .style(button::secondary),
        );

    let modal = container(content)
        .padding(28)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_modal())),
            border: Border {
                color: t::border(),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        });

    container(modal)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                0.0, 0.0, 0.0, 0.7,
            ))),
            ..Default::default()
        })
        .into()
}
