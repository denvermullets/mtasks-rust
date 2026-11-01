use std::collections::HashMap;

use iced::widget::{
    column, container, horizontal_rule, mouse_area, row, scrollable, text, Column, Row,
};
use iced::widget::text::LineHeight;
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::db::models::{Issue, Label, Lane, Member, Project, Team};
use crate::display::{CardProperty, Column as BoardColumn, DisplayOptions};
use crate::scroll;
use crate::fonts;
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::labels as labels_view;
use crate::views::lane_icon;
use crate::views::util;

/// Lookup data cards need for their optional properties.
pub struct CardData<'a> {
    /// Each card's identifier uses its own team (cross-team views mix teams).
    pub teams: &'a [Team],
    pub lanes: &'a [Lane],
    pub members: &'a [Member],
    pub projects: &'a [Project],
    pub issue_labels: &'a HashMap<String, Vec<Label>>,
    pub display: &'a DisplayOptions,
}

/// Render the board. Columns come from `display::build_columns`, so they may be
/// lanes, priorities, assignees, etc.; `focused_lane` indexes `columns` and
/// `focused_issue` is the flat index within the column (across sub-groups).
pub fn view<'a>(
    columns: &[BoardColumn<'a>],
    data: CardData<'a>,
    focused_lane: usize,
    focused_issue: usize,
) -> Element<'a, Message> {
    if columns.is_empty() {
        let msg = if data.lanes.is_empty() {
            "No lanes — select a team"
        } else {
            "No columns to show"
        };
        return container(text(msg).size(type_scale::SM).color(t::text_muted()))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into();
    }

    // Column headers row
    let mut header_row = row![].spacing(0).width(Length::Fill);
    for (col_idx, col) in columns.iter().enumerate() {
        let name_color = if col_idx == focused_lane {
            t::text_primary()
        } else {
            t::text_secondary()
        };

        let header = row![]
            .spacing(space::S2)
            .align_y(iced::Alignment::Center)
            .push_maybe(col.lane_icon.map(lane_icon::view))
            .push(
                text(col.title.clone())
                    .size(type_scale::SM)
                    .color(name_color),
            )
            .push(
                text(format!("{}", col.issues.len()))
                    .size(type_scale::XS)
                    .color(t::text_muted()),
            );

        header_row = header_row.push(container(header).padding([space::S2_5, space::S3]).width(Length::Fill));
    }

    // Columns with cards
    let mut lane_columns = row![].spacing(1).width(Length::Fill).height(Length::Fill);

    for (col_idx, col) in columns.iter().enumerate() {
        let is_focused_lane = col_idx == focused_lane;

        let mut issue_cards: Column<'_, Message> =
            column![]
                .spacing(space::S1_5)
                .width(Length::Fill)
                .padding([space::S1, space::S2]);

        for (issue_idx, issue) in col.issues.iter().copied().enumerate() {
            // Sub-group heading before the first card of each section.
            if let Some(section) = col.sections.iter().find(|s| s.start == issue_idx) {
                issue_cards = issue_cards.push(
                    container(
                        row![]
                            .spacing(space::S2)
                            .push(
                                text(section.title.clone())
                                    .size(type_scale::XS)
                                    .color(t::text_secondary()),
                            )
                            .push(
                                text(format!("{}", section.end - section.start))
                                    .size(type_scale::XS)
                                    .color(t::text_muted()),
                            ),
                    )
                    .padding([space::S1, space::S0_5])
                    .height(Length::Fixed(scroll::BOARD_SUBHEAD_H)),
                );
            }
            let is_focused = is_focused_lane && issue_idx == focused_issue;
            issue_cards =
                issue_cards.push(issue_card(issue, &data, is_focused, col_idx, issue_idx));
        }

        let lane_content = scrollable(issue_cards)
            .id(lane_scroll_id(col_idx))
            .on_scroll(move |vp| {
                Message::Scrolled(col_idx, vp.absolute_offset().y, vp.bounds().height)
            })
            .height(Length::Fill);

        let lane_container = container(lane_content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::bg_primary())),
                border: Border {
                    color: t::border(),
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            });

        lane_columns = lane_columns.push(lane_container);
    }

    let board = column![]
        .push(header_row)
        .push(
            container(horizontal_rule(1)).style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::border())),
                ..Default::default()
            }),
        )
        .push(lane_columns)
        .width(Length::Fill)
        .height(Length::Fill);

    container(board)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_primary())),
            ..Default::default()
        })
        .into()
}

/// The muted "fact" strings on a card's meta line, in display order.
fn card_facts(issue: &Issue, data: &CardData<'_>) -> Vec<String> {
    let d = data.display;
    let mut facts: Vec<String> = Vec::new();
    if d.shows(CardProperty::Assignee) {
        let name = issue
            .assignee_id
            .as_ref()
            .and_then(|id| data.members.iter().find(|m| m.id == *id))
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "Unassigned".to_string());
        facts.push(name);
    }
    if d.shows(CardProperty::Project) {
        if let Some(p) = issue
            .project_id
            .as_ref()
            .and_then(|id| data.projects.iter().find(|p| p.id == *id))
        {
            facts.push(p.name.clone());
        }
    }
    if d.shows(CardProperty::DueDate) {
        if let Some(due) = issue.due_date.as_deref() {
            facts.push(format!("Due {}", due.get(..10).unwrap_or(due)));
        }
    }
    if d.shows(CardProperty::Status) {
        if let Some(l) = data.lanes.iter().find(|l| l.id == issue.lane_id) {
            facts.push(l.name.clone());
        }
    }
    if d.shows(CardProperty::Estimate) {
        if let Some(e) = issue.estimate {
            facts.push(util::format_estimate(e));
        }
    }
    if d.shows(CardProperty::Created) {
        facts.push(format!("Created {}", util::short_date(&issue.created_at)));
    }
    if d.shows(CardProperty::Updated) {
        facts.push(format!("Updated {}", util::short_date(&issue.updated_at)));
    }
    facts
}

/// Rendered height of `issue`'s card (cards are fixed-height so scroll-to-focus
/// can be computed; see `scroll::board_card_height`).
pub fn card_height(issue: &Issue, data: &CardData<'_>) -> f32 {
    let d = data.display;
    let has_chips = d.shows(CardProperty::Labels)
        && data
            .issue_labels
            .get(&issue.id)
            .is_some_and(|l| !l.is_empty());
    let has_meta = d.shows(CardProperty::Priority) || !card_facts(issue, data).is_empty();
    scroll::board_card_height(d.shows(CardProperty::Id), has_chips, has_meta)
}

/// Scrollable id of board lane `idx`.
pub fn lane_scroll_id(idx: usize) -> scrollable::Id {
    scrollable::Id::new(format!("board-lane-{idx}"))
}

/// `(top, height)` of the focused card within its lane's scroll content.
pub fn focused_span(col: &BoardColumn<'_>, data: &CardData<'_>, issue: usize) -> (f32, f32) {
    let heights: Vec<f32> = col.issues.iter().map(|i| card_height(i, data)).collect();
    let starts: Vec<usize> = col.sections.iter().map(|s| s.start).collect();
    scroll::board_card_span(&heights, &starts, issue)
}

fn issue_card<'a>(
    issue: &'a Issue,
    data: &CardData<'a>,
    focused: bool,
    lane_idx: usize,
    issue_idx: usize,
) -> Element<'a, Message> {
    let d = data.display;
    let priority_color = t::priority_color(issue.priority_enum());

    let mut card_content = column![].spacing(0);

    if d.shows(CardProperty::Id) {
        let ident = data
            .teams
            .iter()
            .find(|t| t.id == issue.team_id)
            .map(|t| t.identifier.as_str())
            .unwrap_or("---");
        let tag = issue
            .team_number
            .map(|n| format!("{ident}-{n}"))
            .unwrap_or_default();
        card_content = card_content.push(
            container(
                text(tag)
                    .size(type_scale::XS)
                    .line_height(LineHeight::Relative(type_scale::LH_XS))
                    .font(fonts::code())
                    .color(t::text_muted()),
            )
            .padding(iced::Padding::ZERO.bottom(space::S2)),
        );
    }
    card_content = card_content.push(
        text(&issue.title)
            .size(type_scale::SM)
            .line_height(LineHeight::Relative(type_scale::LH_SM))
            .color(t::text_primary())
            .wrapping(text::Wrapping::None),
    );

    let labels = data
        .issue_labels
        .get(&issue.id)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    if d.shows(CardProperty::Labels) && !labels.is_empty() {
        let mut chips: Row<'a, Message> = row![].spacing(space::S1);
        for label in labels {
            chips = chips.push(labels_view::static_chip(label));
        }
        card_content = card_content.push(
            container(
                container(chips)
                    .height(Length::Fixed(scroll::CARD_CHIP_ROW_H))
                    .align_y(iced::alignment::Vertical::Center),
            )
            .padding(iced::Padding::ZERO.top(space::S2)),
        );
    }

    // Meta line: small muted facts, only the enabled ones.
    let facts = card_facts(issue, data);

    let mut meta: Row<'a, Message> = row![].spacing(space::S1_5).align_y(iced::Alignment::Center);
    if d.shows(CardProperty::Priority) {
        // Priority indicator: triangle + number like the Rails app
        meta = meta.push(
            row![]
                .spacing(space::S1)
                .align_y(iced::Alignment::Center)
                .push(
                    text("△")
                        .size(type_scale::XS)
                        .line_height(LineHeight::Relative(type_scale::LH_XS))
                        .color(priority_color),
                )
                .push(
                    text(format!("{}", issue.priority))
                        .size(type_scale::XS)
                        .line_height(LineHeight::Relative(type_scale::LH_XS))
                        .color(t::text_muted()),
                ),
        );
    }
    let has_meta = d.shows(CardProperty::Priority) || !facts.is_empty();
    for f in facts {
        meta = meta.push(
            text(f)
                .size(type_scale::XS)
                .line_height(LineHeight::Relative(type_scale::LH_XS))
                .color(t::text_muted())
                .wrapping(text::Wrapping::None),
        );
    }
    if has_meta {
        card_content = card_content
            .push(container(meta).padding(iced::Padding::ZERO.top(space::S3)));
    }

    let bg = if focused {
        t::bg_card_focused()
    } else {
        t::bg_secondary()
    };

    let border_color = if focused { t::accent() } else { t::border() };

    let card = container(card_content)
        .padding([space::S2_5, space::S3])
        .width(Length::Fill)
        .height(Length::Fixed(card_height(issue, data)))
        .clip(true)
        .style(move |_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(bg)),
            border: Border {
                color: border_color,
                width: 1.0,
                radius: radius::SM.into(),
            },
            ..Default::default()
        });

    mouse_area(card)
        .on_press(Message::CardPressed(lane_idx, issue_idx))
        .into()
}
