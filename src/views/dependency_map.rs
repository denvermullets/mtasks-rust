//! One issue's dependency map (`View::DependencyMap`, JRST-442): a Dependencies
//! / Hierarchy segmented switch over a tab body. The header's breadcrumb is
//! built in `app::breadcrumb`.
//!
//! The two tab bodies are placeholders. Replace [`dependencies_body`]
//! (JRST-443) and [`hierarchy_body`] (JRST-444) and extend their arguments
//! as needed; nothing else here needs to change.

use iced::widget::{button, column, container, row};
use iced::{Alignment, Border, Element, Length, Theme};

use crate::app::{DependencyMapTab, Message};
use crate::db::models::Issue;
use crate::theme::{self as t, space, type_scale};
use crate::views::text;

pub fn view<'a>(issue: &'a Issue, tab: DependencyMapTab, depth: u8) -> Element<'a, Message> {
    let body = match tab {
        DependencyMapTab::Dependencies => dependencies_body(issue, depth),
        DependencyMapTab::Hierarchy => hierarchy_body(issue),
    };
    column![]
        .spacing(space::S6)
        .padding(space::S8)
        .width(Length::Fill)
        .height(Length::Fill)
        .push(tab_switch(tab))
        .push(body)
        .into()
}

/// The Dependencies / Hierarchy segmented switch.
fn tab_switch<'a>(active: DependencyMapTab) -> Element<'a, Message> {
    let segment = |tab: DependencyMapTab| {
        let on = tab == active;
        button(text(tab.label()).size(type_scale::SM))
            .on_press(Message::SetDependencyMapTab(tab))
            .padding([space::S1, space::S4])
            .style(move |_theme: &Theme, status| button::Style {
                background: (on || matches!(status, button::Status::Hovered))
                    .then(|| iced::Background::Color(t::bg_card_focused())),
                text_color: if on { t::text_primary() } else { t::text_secondary() },
                border: Border { radius: 4.0.into(), ..Default::default() },
                ..Default::default()
            })
    };
    row![segment(DependencyMapTab::Dependencies), segment(DependencyMapTab::Hierarchy)]
        .spacing(space::S1)
        .align_y(Alignment::Center)
        .into()
}

/// REPLACE (JRST-443): the Dependencies tab (graph, depth selector, lists).
fn dependencies_body<'a>(_issue: &'a Issue, depth: u8) -> Element<'a, Message> {
    placeholder(format!("Dependencies (depth {depth}) coming soon"))
}

/// REPLACE (JRST-444): the Hierarchy tab (sub-issue tree outline).
fn hierarchy_body<'a>(_issue: &'a Issue) -> Element<'a, Message> {
    placeholder("Hierarchy coming soon".to_string())
}

fn placeholder<'a>(label: String) -> Element<'a, Message> {
    container(text(label).size(type_scale::SM).color(t::text_muted()))
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}
