use std::collections::HashMap;

use iced::widget::{column, container, mouse_area, row, scrollable, Column, Row};
use crate::views::text;
use iced::widget::text::LineHeight;
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::scroll::{track, ScrollKey};
use crate::db::models::{Issue, Label, Lane, Member, Project, Team};
use crate::display::{self, CardProperty, Column as BoardColumn, DisplayOptions};
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
    /// Local id of the card or row under the pointer; painted with the hover
    /// highlight (Rails' `hover:bg-hover-highlight`).
    pub hovered: Option<&'a str>,
    /// Dependency lines for the board overlay (JRST-450); `None` when the
    /// "Dependencies" display option is off, which also skips the card anchors.
    pub deps: Option<crate::views::dep_overlay::DepLines>,
}

/// Render the board. Columns come from `display::build_columns`, so they may be
/// lanes, priorities, assignees, etc.; `focused_lane` indexes `columns` and
/// `focused_issue` is the flat index within the column (across sub-groups).
/// With a sub-group the columns' aligned sections are drawn as rows spanning
/// the board (Rails' swimlanes); `row_collapsed` holds one flag per row.
pub fn view<'a>(
    columns: &[BoardColumn<'a>],
    data: CardData<'a>,
    row_collapsed: &[bool],
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
    // Each column's fixed slot is the column width plus a right gutter, which
    // forms the gap to the next column and holds the lane's vertical scrollbar
    // clear of the cards.
    let slot_w = scroll::BOARD_COLUMN_W + scroll::BOARD_COLUMN_GAP;
    let mut header_row = row![];
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

        header_row = header_row.push(
            container(header)
                .padding(
                    iced::Padding::ZERO
                        .top(space::S2_5)
                        .bottom(space::S2_5)
                        .right(scroll::BOARD_COLUMN_GAP),
                )
                .width(Length::Fixed(slot_w))
                .clip(true),
        );
    }

    let rows = display::board_rows(columns);
    let body: Element<'a, Message> = if rows.is_empty() {
        lanes_body(columns, &data, slot_w, focused_lane, focused_issue)
    } else {
        swimlane_body(columns, &rows, row_collapsed, &data, slot_w, focused_lane, focused_issue)
    };

    let board = column![]
        .push(header_row)
        .push(body)
        .padding(iced::Padding::ZERO.left(scroll::BOARD_PAD_X))
        .width(Length::Fixed(scroll::board_content_width(columns.len())))
        .height(Length::Fill);

    // Built horizontal from the start: `scrollable()` validates as vertical, which
    // rejects this `Fill`-height content before `.direction()` could apply.
    let board = track(
        scrollable::Scrollable::with_direction(
            board,
            scrollable::Direction::Horizontal(scrollable::Scrollbar::default()),
        ),
        ScrollKey::Board,
    )
        .width(Length::Fill)
        .height(Length::Fill);

    // Dependency lines (JRST-450): only wrapped when the display option is on.
    let board: Element<'a, Message> = match data.deps.clone() {
        Some(lines) => {
            // Hover state comes from the cards' own `CardHover` tracking.
            let hovered = data.hovered.map(str::to_string);
            let opts = crate::views::dep_overlay::hover_options(hovered.as_deref(), &lines);
            crate::views::dep_overlay::dep_overlay(board)
                .draw_over(move |renderer, bounds, anchors| {
                    crate::views::dep_overlay::draw_connectors(renderer, bounds, anchors, &lines.edges, &opts);
                    if let Some(id) = &hovered {
                        crate::views::dep_overlay::draw_hover_rings(renderer, bounds, anchors, id, &lines);
                    }
                })
                .into()
        }
        None => board.into(),
    };

    container(board)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme: &Theme| container::Style {
            background: t::repaint(t::bg_primary()),
            ..Default::default()
        })
        .into()
}

/// Columns side by side, each scrolling on its own (no sub-group).
fn lanes_body<'a>(
    columns: &[BoardColumn<'a>],
    data: &CardData<'a>,
    slot_w: f32,
    focused_lane: usize,
    focused_issue: usize,
) -> Element<'a, Message> {
    // Columns with cards
    let mut lane_columns = row![].height(Length::Fill);

    for (col_idx, col) in columns.iter().enumerate() {
        let is_focused_lane = col_idx == focused_lane;

        let mut issue_cards: Column<'_, Message> =
            column![]
                .spacing(space::S1_5)
                .width(Length::Fixed(slot_w))
                .padding(
                    // The bottom padding lifts the last card clear of the board's
                    // horizontal scrollbar, which floats over the lanes.
                    iced::Padding::ZERO
                        .top(scroll::BOARD_PAD_TOP)
                        .bottom(space::S3)
                        .right(scroll::BOARD_COLUMN_GAP),
                );

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
                issue_cards.push(issue_card(issue, data, is_focused, col_idx, issue_idx));
        }

        let lane_content =
            track(scrollable(issue_cards), ScrollKey::Lane(col_idx)).height(Length::Fill);

        lane_columns = lane_columns.push(lane_content);
    }

    lane_columns.into()
}

/// Sub-group rows spanning every column, scrolling together (Rails'
/// swimlanes). Each row is a header bar (click to collapse) over one cell per
/// column; cells in a row share the tallest cell's height so rows line up.
/// Heights follow `scroll::swimlane_*` so scroll-to-focus can compute them.
fn swimlane_body<'a>(
    columns: &[BoardColumn<'a>],
    rows: &[display::BoardRow],
    row_collapsed: &[bool],
    data: &CardData<'a>,
    slot_w: f32,
    focused_lane: usize,
    focused_issue: usize,
) -> Element<'a, Message> {
    let row_w = columns.len() as f32 * slot_w - scroll::BOARD_COLUMN_GAP;
    let mut body: Column<'_, Message> = column![]
        .spacing(scroll::SWIMLANE_ROW_GAP)
        .width(Length::Fixed(columns.len() as f32 * slot_w))
        .padding(iced::Padding::ZERO.top(scroll::BOARD_PAD_TOP).bottom(space::S3));

    for (r, brow) in rows.iter().enumerate() {
        let collapsed = row_collapsed.get(r).copied().unwrap_or(false);
        let marker = if collapsed { "▸" } else { "▾" };
        let header = mouse_area(
            container(
                row![]
                    .spacing(space::S2)
                    .align_y(iced::Alignment::Center)
                    .push(text(marker).size(type_scale::XS).font(fonts::SYMBOL).color(t::text_muted()))
                    .push(
                        text(brow.title.clone())
                            .size(type_scale::XS)
                            .color(t::accent()),
                    )
                    .push(
                        text(format!("{}", brow.count))
                            .size(type_scale::XS)
                            .color(t::text_muted()),
                    ),
            )
            .padding([0.0, space::S3])
            .center_y(Length::Fixed(scroll::SWIMLANE_HEADER_H))
            .width(Length::Fixed(row_w))
            .clip(true)
            // Rails' row bar is `bg-hover-highlight rounded-md`.
            .style(|_theme: &Theme| container::Style {
                background: Some(iced::Background::Color(t::bg_card_focused())),
                border: Border {
                    radius: radius::MD.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .on_press(Message::ToggleSwimlane(brow.title.clone()));

        let mut block: Column<'_, Message> =
            column![].spacing(scroll::SWIMLANE_HEADER_GAP).push(header);
        if !collapsed {
            let stacks: Vec<f32> = columns
                .iter()
                .map(|c| {
                    let heights: Vec<f32> = c
                        .sections
                        .get(r)
                        .map(|s| c.issues[s.start..s.end].iter().map(|i| card_height(i, data)).collect())
                        .unwrap_or_default();
                    scroll::swimlane_stack_height(&heights)
                })
                .collect();
            let cells_h = scroll::swimlane_cells_height(&stacks);
            let mut cells = row![];
            for (col_idx, col) in columns.iter().enumerate() {
                let mut cell: Column<'_, Message> = column![]
                    .spacing(scroll::BOARD_SPACING)
                    .width(Length::Fixed(slot_w))
                    .height(Length::Fixed(cells_h))
                    .padding(iced::Padding::ZERO.right(scroll::BOARD_COLUMN_GAP));
                if let Some(sec) = col.sections.get(r) {
                    for issue_idx in sec.start..sec.end {
                        let issue = col.issues[issue_idx];
                        let focused = col_idx == focused_lane && issue_idx == focused_issue;
                        cell = cell.push(issue_card(issue, data, focused, col_idx, issue_idx));
                    }
                }
                cells = cells.push(cell);
            }
            block = block.push(cells);
        }
        body = body.push(block);
    }

    track(scrollable(body), ScrollKey::Swimlanes)
        .height(Length::Fill)
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

/// `(top, height)` of the focused card within its lane's scroll content.
pub fn focused_span(col: &BoardColumn<'_>, data: &CardData<'_>, issue: usize) -> (f32, f32) {
    let heights: Vec<f32> = col.issues.iter().map(|i| card_height(i, data)).collect();
    let starts: Vec<usize> = col.sections.iter().map(|s| s.start).collect();
    scroll::board_card_span(&heights, &starts, issue)
}

/// `(top, height)` of the focused card in the swimlane board's vertical
/// scroll content, or `None` when it isn't on screen (collapsed row).
pub fn swimlane_focused_span(
    columns: &[BoardColumn<'_>],
    row_collapsed: &[bool],
    data: &CardData<'_>,
    lane: usize,
    issue: usize,
) -> Option<(f32, f32)> {
    let col = columns.get(lane)?;
    let row = display::swimlane_row_of(&col.sections, issue)?;
    if row_collapsed.get(row).copied().unwrap_or(false) {
        return None;
    }
    let rows: Vec<scroll::SwimlaneRow> = (0..col.sections.len())
        .map(|r| {
            let stacks: Vec<f32> = columns
                .iter()
                .map(|c| {
                    let heights: Vec<f32> = c
                        .sections
                        .get(r)
                        .map(|s| c.issues[s.start..s.end].iter().map(|i| card_height(i, data)).collect())
                        .unwrap_or_default();
                    scroll::swimlane_stack_height(&heights)
                })
                .collect();
            scroll::SwimlaneRow {
                collapsed: row_collapsed.get(r).copied().unwrap_or(false),
                cells_h: scroll::swimlane_cells_height(&stacks),
            }
        })
        .collect();
    let start = col.sections[row].start;
    let above: Vec<f32> = col.issues[start..issue].iter().map(|i| card_height(i, data)).collect();
    Some(scroll::swimlane_card_span(&rows, row, &above, card_height(col.issues[issue], data)))
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

    let hovered = data.hovered == Some(issue.id.as_str());
    let bg = if focused || hovered {
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

    let card = crate::views::row_hover::row_hover(
        mouse_area(card).on_press(Message::CardPressed(lane_idx, issue_idx)),
        issue.id.clone(),
        Message::CardHover,
    );
    if data.deps.is_some() {
        crate::views::dep_overlay::dep_anchor(issue.id.clone(), card).into()
    } else {
        card.into()
    }
}
