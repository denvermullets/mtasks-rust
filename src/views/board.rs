use std::collections::HashMap;

use iced::widget::{column, container, mouse_area, row, scrollable, tooltip, Column, Row, Space};
use crate::views::text;
use iced::widget::text::LineHeight;
use iced::{Border, Element, Length, Theme};

use crate::app::Message;
use crate::scroll::{track, ScrollKey};
use crate::db::models::{Issue, Label, Lane, Member, Project, Team};
use crate::db::queries::BlockCounts;
use crate::display::{self, Column as BoardColumn, DisplayOptions};
use crate::scroll;
use crate::fonts;
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::card_layout::{self as cl, CardLayout, Pill, Placed};
use crate::views::form_page;
use crate::views::labels as labels_view;
use crate::views::lane_icon;
use crate::views::util;

/// Width of the "Hidden columns" list (Rails' `w-64`).
const HIDDEN_COLUMNS_W: f32 = 256.0;

/// Rails' "Hidden columns" strip: a left-ruled list of the empty columns the
/// board left off (`_board_view.html.erb`).
fn hidden_columns<'a>(hidden: &[display::HiddenColumn]) -> Element<'a, Message> {
    let mut list: Column<'a, Message> = column![].spacing(space::S2);
    for h in hidden {
        list = list.push(
            row![]
                .spacing(space::S2)
                .align_y(iced::Alignment::Center)
                .push(h.lane_icon.map(lane_icon::view))
                .push(text(h.title.clone()).size(type_scale::SM).color(t::text_muted())),
        );
    }
    let body = column![]
        .spacing(space::S3)
        .push(
            text("Hidden columns")
                .size(type_scale::SM)
                .color(t::text_muted()),
        )
        .push(scrollable(list).height(Length::Fill));
    row![
        iced::widget::rule::vertical(1).style(|_theme: &Theme| iced::widget::rule::Style {
            color: t::border(),
            radius: 0.0.into(),
            fill_mode: iced::widget::rule::FillMode::Full,
            snap: true,
        }),
        container(body)
            .padding(iced::Padding::ZERO.top(space::S2_5).left(space::S3).right(space::S3))
            .width(Length::Fixed(HIDDEN_COLUMNS_W))
            .height(Length::Fill),
    ]
    .height(Length::Fill)
    .into()
}

/// Lookup data cards need for their optional properties.
pub struct CardData<'a> {
    /// Each card's identifier uses its own team (cross-team views mix teams).
    pub teams: &'a [Team],
    pub lanes: &'a [Lane],
    pub members: &'a [Member],
    pub projects: &'a [Project],
    pub issue_labels: &'a HashMap<String, Vec<Label>>,
    /// Comments per local issue id (replies included, as the web counts them).
    pub comment_counts: &'a HashMap<String, usize>,
    /// Blocking / blocked-by counts per local issue id (`blocks` edges only).
    pub block_counts: &'a HashMap<String, crate::db::queries::BlockCounts>,
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
    hidden: &[display::HiddenColumn],
    focused_lane: usize,
    focused_issue: usize,
) -> Element<'a, Message> {
    if columns.is_empty() && hidden.is_empty() {
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
            .push(col.lane_icon.map(lane_icon::view))
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
    let board: Element<'a, Message> = if rows.is_empty() {
        lanes_board(columns, header_row, hidden, &data, slot_w, focused_lane, focused_issue)
    } else {
        let body =
            swimlane_body(columns, &rows, row_collapsed, &data, slot_w, focused_lane, focused_issue);
        swimlane_board(columns.len(), header_row, body, hidden)
    };

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
        None => board,
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

/// Lanes mode: headers and lanes in one horizontal scrollable; each lane
/// scrolls vertically on its own, so its scrollbar belongs to the lane.
fn lanes_board<'a>(
    columns: &[BoardColumn<'a>],
    header_row: Row<'a, Message>,
    hidden: &[display::HiddenColumn],
    data: &CardData<'a>,
    slot_w: f32,
    focused_lane: usize,
    focused_issue: usize,
) -> Element<'a, Message> {
    let body = lanes_body(columns, data, slot_w, focused_lane, focused_issue);
    let board = column![]
        .push(header_row)
        .push(body)
        .padding(iced::Padding::ZERO.left(scroll::BOARD_PAD_X))
        .width(Length::Fixed(scroll::board_content_width(columns.len())))
        .height(Length::Fill);
    // Empty columns hidden by Display > "Show empty columns" off, listed after
    // the board as Rails does, so empty lanes don't just vanish.
    let board: Element<'a, Message> = if hidden.is_empty() {
        board.into()
    } else {
        row![board, hidden_columns(hidden)].height(Length::Fill).into()
    };

    // Built horizontal from the start: `scrollable()` validates as vertical, which
    // rejects this `Fill`-height content before `.direction()` could apply.
    track(
        scrollable::Scrollable::with_direction(
            // JRST-529: a lane's vertical overscroll must not slide the board.
            crate::views::wheel_guard::wheel_guard(board),
            scrollable::Direction::Horizontal(scrollable::Scrollbar::default()),
        ),
        ScrollKey::Board,
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Swimlanes mode (JRST-530): the rows scroll both ways in one scrollable that
/// fills the visible board, so its vertical scrollbar sits at the board's
/// visible right edge rather than past the last column. The column headers
/// ride in a bar-less strip above it that `Message::SwimlanesScrolled` keeps
/// at the body's sideways offset. Hidden columns stay pinned to the right.
fn swimlane_board<'a>(
    column_count: usize,
    header_row: Row<'a, Message>,
    body: Column<'a, Message>,
    hidden: &[display::HiddenColumn],
) -> Element<'a, Message> {
    let content_w = Length::Fixed(scroll::board_content_width(column_count));
    let pad = iced::Padding::ZERO.left(scroll::BOARD_PAD_X);
    let hidden_bar = scrollable::Scrollbar::new().width(0).scroller_width(0);
    // The strip has no bar of its own; wheeling over it moves the body, which
    // moves the strip, so the two never drift.
    let headers = mouse_area(container(header_row).padding(pad).width(content_w))
        .on_scroll(Message::SwimlaneHeaderWheel);
    let headers = scrollable::Scrollable::with_direction(
        headers,
        scrollable::Direction::Horizontal(hidden_bar),
    )
    .id(scroll::swimlane_header_id())
    .width(Length::Fill);

    let body = scrollable::Scrollable::with_direction(
        container(body).padding(pad).width(content_w),
        scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::default(),
            horizontal: scrollable::Scrollbar::default(),
        },
    );
    let body = scroll::track_swimlanes(body).width(Length::Fill).height(Length::Fill);

    let board = column![headers, body].width(Length::Fill).height(Length::Fill);
    if hidden.is_empty() {
        board.into()
    } else {
        row![board, hidden_columns(hidden)].height(Length::Fill).into()
    }
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
) -> Column<'a, Message> {
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
        .on_press(Message::ToggleSwimlane(brow.title.clone()))
        .interaction(iced::mouse::Interaction::Pointer);

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

    body
}

/// Rendered height of `issue`'s card. Cards are sized by `CardLayout`, which
/// measures the title and packs the footer pills, so scroll-to-focus can place
/// them without querying the widget tree.
pub fn card_height(issue: &Issue, data: &CardData<'_>) -> f32 {
    CardLayout::new(issue, data).height()
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
    let layout = CardLayout::new(issue, data);
    let (footer_h, card_h) = (layout.footer_h(), layout.height());
    let mut card_content = column![].spacing(0);

    // Id row: identifier, comment count pushed right (web `ml-auto`).
    if layout.show_id {
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
        let comments = (layout.comments > 0).then(|| {
            row![]
                .spacing(space::S0_5)
                .align_y(iced::Alignment::Center)
                .push(form_page::icon(ICON_CHAT, cl::PILL_ICON, t::text_muted()))
                .push(xs_text(layout.comments.to_string()))
        });
        card_content = card_content.push(
            container(
                row![]
                    .align_y(iced::Alignment::Center)
                    .push(
                        text(tag)
                            .size(type_scale::XS)
                            .line_height(LineHeight::Relative(type_scale::LH_XS))
                            .font(fonts::code())
                            .color(t::text_muted()),
                    )
                    .push(block_icon(layout.blocks))
                    .push(iced::widget::space::horizontal())
                    .push(comments),
            )
            .height(Length::Fixed(cl::XS_LINE))
            .padding(iced::Padding::ZERO)
            .clip(true),
        );
        card_content = card_content.push(Space::new().height(cl::ID_GAP));
    }

    // Title: one or two lines as measured; past two it is clipped (web
    // `line-clamp-2`).
    card_content = card_content.push(
        container(
            text(&issue.title)
                .size(type_scale::SM)
                .line_height(LineHeight::Relative(type_scale::LH_SM))
                .color(t::text_primary())
                .wrapping(text::Wrapping::WordOrGlyph),
        )
        .width(Length::Fill)
        .height(Length::Fixed(layout.title_lines as f32 * cl::TITLE_LINE))
        .clip(true),
    );
    card_content = card_content.push(Space::new().height(cl::TITLE_GAP));

    // Footer: the packed pill rows, avatar on the right (web `justify-between
    // items-start`). No pills shows the web's "···".
    let pills: Element<'a, Message> = if layout.rows.is_empty() {
        if layout.assignee.is_some() {
            Space::new().width(Length::Fill).into()
        } else {
            container(form_page::icon(ICON_DOTS, space::S4, t::text_muted()))
                .center_y(Length::Fixed(cl::PILL_H))
                .into()
        }
    } else {
        let mut rows: Column<'a, Message> = column![].spacing(cl::PILL_GAP);
        for placed_row in layout.rows {
            let mut r: Row<'a, Message> = row![].spacing(cl::PILL_GAP);
            for placed in placed_row {
                r = r.push(card_pill(placed));
            }
            rows = rows.push(r);
        }
        rows.into()
    };
    let footer = row![]
        .spacing(cl::AVATAR_GAP)
        .push(container(pills).width(Length::Fill))
        .push(layout.assignee.map(|m| util::avatar(&m.name)))
        .height(Length::Fixed(footer_h));
    card_content = card_content.push(footer);

    let hovered = data.hovered == Some(issue.id.as_str());
    let bg = if focused || hovered {
        t::bg_card_focused()
    } else {
        t::bg_secondary()
    };

    let border_color = if focused { t::accent() } else { t::border() };

    let card = container(card_content)
        .padding([cl::PAD_Y, cl::PAD_X])
        .width(Length::Fill)
        .height(Length::Fixed(card_h))
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
        mouse_area(card)
            .on_press(Message::CardPressed(lane_idx, issue_idx))
            .interaction(iced::mouse::Interaction::Pointer),
        issue.id.clone(),
        Message::CardHover,
    );
    if data.deps.is_some() {
        crate::views::dep_overlay::dep_anchor(issue.id.clone(), card).into()
    } else {
        card.into()
    }
}

/// The id row's dependency icon (web `issues/_issue_card`): red when the issue
/// is blocked, yellow when it only blocks others; hover lists both counts.
fn block_icon<'a>(blocks: BlockCounts) -> Option<Element<'a, Message>> {
    if blocks.blocking == 0 && blocks.blocked_by == 0 {
        return None;
    }
    let color = if blocks.blocked_by > 0 { t::RED_400 } else { t::YELLOW_400 };
    let tip = [
        (blocks.blocking > 0).then(|| format!("Blocking {}", blocks.blocking)),
        (blocks.blocked_by > 0).then(|| format!("Blocked by {}", blocks.blocked_by)),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let icon = container(form_page::icon(ICON_BLOCK, cl::PILL_ICON, color))
        .padding(iced::Padding::ZERO.left(cl::BLOCK_ICON_GAP));
    Some(
        tooltip(
            icon,
            container(text(tip).size(type_scale::XS).color(t::text_strong()))
                .padding([space::S1, space::S2])
                .style(|_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(t::bg_menu())),
                    border: Border { color: t::border(), width: 1.0, radius: radius::SM.into() },
                    ..Default::default()
                }),
            tooltip::Position::Top,
        )
        .gap(space::S1)
        .into(),
    )
}

/// Muted `XS` text on the `XS` line (pill and comment-count text).
fn xs_text<'a>(content: String) -> iced::widget::Text<'a> {
    text(content)
        .size(type_scale::XS)
        .line_height(LineHeight::Relative(type_scale::LH_XS))
        .color(t::text_muted())
        .wrapping(text::Wrapping::None)
}

/// One footer pill at its measured width (web `px-1.5 py-0.5 rounded border`).
fn card_pill<'a>(placed: Placed<'a>) -> Element<'a, Message> {
    let label = xs_text(placed.pill.text());
    let face: Element<'a, Message> = match placed.pill {
        Pill::More(_) => {
            return container(label)
                .center_y(Length::Fixed(cl::PILL_H))
                .width(Length::Fixed(placed.width))
                .into();
        }
        Pill::Priority(_) => row![]
            .spacing(cl::PRIORITY_ICON_GAP)
            .align_y(iced::Alignment::Center)
            .push(form_page::icon(ICON_WARNING, cl::PILL_ICON, t::text_muted()))
            .push(label)
            .into(),
        Pill::Label(l) => {
            let dot = labels_view::chip_color(l.color.as_deref());
            row![]
                .spacing(cl::PILL_ICON_GAP)
                .align_y(iced::Alignment::Center)
                .push(
                    container(Space::new().width(cl::LABEL_DOT).height(cl::LABEL_DOT)).style(move |_: &Theme| {
                        container::Style {
                            background: Some(iced::Background::Color(dot)),
                            border: Border { radius: radius::FULL.into(), ..Default::default() },
                            ..Default::default()
                        }
                    }),
                )
                .push(label)
                .into()
        }
        Pill::Fact { icon, .. } => row![]
            .spacing(cl::PILL_ICON_GAP)
            .align_y(iced::Alignment::Center)
            .push(icon.map(|src| form_page::icon(src, cl::PILL_ICON, t::text_muted())))
            .push(label)
            .into(),
    };
    container(face)
        .padding([cl::PILL_PAD_Y, cl::PILL_PAD_X])
        .width(Length::Fixed(placed.width))
        .height(Length::Fixed(cl::PILL_H))
        .clip(true)
        .style(|_: &Theme| container::Style {
            border: Border { color: t::border(), width: 1.0, radius: radius::SM.into() },
            ..Default::default()
        })
        .into()
}

// Heroicons from the web card.
const ICON_CHAT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M8 12h.01M12 12h.01M16 12h.01M21 12c0 4.418-4.03 8-9 8a9.863 9.863 0 01-4.255-.949L3 20l1.395-3.72C3.512 15.042 3 13.574 3 12c0-4.418 4.03-8 9-8s9 3.582 9 8z"/></svg>"##;
const ICON_WARNING: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z"/></svg>"##;
const ICON_BLOCK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" stroke-width="2" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" d="M18.364 18.364A9 9 0 005.636 5.636m12.728 12.728A9 9 0 015.636 5.636m12.728 12.728L5.636 5.636"/></svg>"##;
const ICON_DOTS: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M5 12h.01M12 12h.01M19 12h.01M6 12a1 1 0 11-2 0 1 1 0 012 0zm7 0a1 1 0 11-2 0 1 1 0 012 0zm7 0a1 1 0 11-2 0 1 1 0 012 0z"/></svg>"##;
