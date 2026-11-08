//! Shared dependency graph canvas: columns of fixed-size issue cards with
//! curved connectors drawn behind them (JRST-441). Renders no data of its own;
//! the issue and project dependency maps feed it columns and edges.
//!
//! Card positions come from the pure [`layout`] fn, which both the connector
//! layer and the card columns use, so lines always meet the cards.

use std::collections::HashMap;

use iced::widget::canvas::{self, Canvas, Frame, Geometry};
use iced::widget::{button, column, container, row, scrollable, stack, Column, Row};
use iced::{Border, Color, Element, Length, Rectangle, Renderer, Size, Theme};

use crate::app::Message;
use crate::dep_connectors::{self as dc, DrawOptions, EdgeSpec};
use crate::lane_icon::LaneIcon;
use crate::theme::{self as t, radius, space, type_scale};
use crate::views::text;
use crate::views::truncated::truncated;

pub const CARD_W: f32 = 240.0;
pub const CARD_H: f32 = 56.0;
/// Card height when it carries a tag line.
pub const CARD_H_TAGGED: f32 = 72.0;
pub const COLUMN_GAP: f32 = 72.0;
pub const ROW_GAP: f32 = 16.0;
/// Space around the whole graph (leaves room for same-column bows).
pub const PAD: f32 = 40.0;
const DOT: f32 = 6.0;
const CLOSED_ALPHA: f32 = 0.55;

/// What a card shows.
#[derive(Debug, Clone)]
pub struct NodeCard {
    /// Issue id (the key edges refer to, and what the click message gets).
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub lane_icon: LaneIcon,
    pub assignee_initials: Option<String>,
    pub label_colors: Vec<Color>,
    /// Closed (completed / canceled) issues are dimmed.
    pub closed: bool,
    /// Optional extra line (the project map's status line).
    pub tag: Option<String>,
}

/// Where every node sits, and the total canvas size.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphLayout {
    pub rects: HashMap<String, Rectangle>,
    pub size: Size,
}

/// Lay out `columns` of node ids (left to right, top to bottom) with fixed card
/// size and gaps. Pure. Every card is `CARD_W` x `CARD_H`.
pub fn layout(columns: &[Vec<String>]) -> GraphLayout {
    layout_with(columns, |_| CARD_H)
}

/// [`layout`] with a per-id card height (tagged cards are taller).
pub fn layout_with(columns: &[Vec<String>], height_of: impl Fn(&str) -> f32) -> GraphLayout {
    let mut rects = HashMap::new();
    let mut max_bottom: f32 = 0.0;
    for (ci, col) in columns.iter().enumerate() {
        let x = PAD + ci as f32 * (CARD_W + COLUMN_GAP);
        let mut y = PAD;
        for id in col {
            let h = height_of(id);
            rects.insert(id.clone(), Rectangle { x, y, width: CARD_W, height: h });
            y += h + ROW_GAP;
            max_bottom = max_bottom.max(y - ROW_GAP);
        }
    }
    let width = if columns.is_empty() {
        PAD * 2.0
    } else {
        PAD * 2.0 + columns.len() as f32 * CARD_W + (columns.len() - 1) as f32 * COLUMN_GAP
    };
    let height = max_bottom.max(PAD) + PAD;
    GraphLayout { rects, size: Size::new(width, height) }
}

struct Connectors {
    list: Vec<dc::Connector>,
}

impl canvas::Program<Message> for Connectors {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        dc::stroke_connectors(&mut frame, &self.list);
        vec![frame.into_geometry()]
    }
}

fn dimmed(c: Color, closed: bool) -> Color {
    if closed {
        Color { a: c.a * CLOSED_ALPHA, ..c }
    } else {
        c
    }
}

/// One card. `selected` is the focus variant; clicking emits `on_click(id)`.
pub fn card<'a>(node: &'a NodeCard, selected: bool, on_click: Message) -> Element<'a, Message> {
    let closed = node.closed;
    let mut top = Row::new().spacing(space::S2).align_y(iced::Alignment::Center);
    top = top.push(crate::views::lane_icon::view(node.lane_icon));
    top = top.push(
        text(&node.identifier)
            .size(type_scale::XS)
            .color(dimmed(t::text_secondary(), closed)),
    );
    top = top.push(container(text("")).width(Length::Fill));
    for c in &node.label_colors {
        let c = dimmed(*c, closed);
        top = top.push(
            container(text(""))
                .width(Length::Fixed(DOT))
                .height(Length::Fixed(DOT))
                .style(move |_: &Theme| container::Style {
                    background: Some(iced::Background::Color(c)),
                    border: Border { radius: radius::FULL.into(), ..Default::default() },
                    ..Default::default()
                }),
        );
    }
    if let Some(initials) = &node.assignee_initials {
        top = top.push(
            container(text(initials.as_str()).size(9).color(dimmed(t::text_primary(), closed)))
                .center_x(Length::Fixed(16.0))
                .center_y(Length::Fixed(16.0))
                .style(move |_: &Theme| container::Style {
                    background: Some(iced::Background::Color(t::bg_card())),
                    border: Border { color: t::border(), width: 1.0, radius: radius::FULL.into() },
                    ..Default::default()
                }),
        );
    }

    let mut body: Column<'a, Message> = column![
        top,
        truncated(node.title.as_str(), type_scale::SM, dimmed(t::text_primary(), closed)),
    ]
    .spacing(space::S1);
    if let Some(tag) = &node.tag {
        body = body.push(truncated(tag.as_str(), type_scale::XS, dimmed(t::text_muted(), closed)));
    }

    let height = if node.tag.is_some() { CARD_H_TAGGED } else { CARD_H };
    button(body)
        .on_press(on_click)
        .padding(space::S2)
        .width(Length::Fixed(CARD_W))
        .height(Length::Fixed(height))
        .style(move |_: &Theme, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let bg = if selected || hovered { t::bg_card_focused() } else { t::bg_card() };
            button::Style {
                background: Some(iced::Background::Color(bg)),
                text_color: t::text_primary(),
                border: Border {
                    color: if selected { t::accent() } else { t::border() },
                    width: if selected { 2.0 } else { 1.0 },
                    radius: radius::LG.into(),
                },
                ..Default::default()
            }
        })
        .into()
}

/// The graph: connectors under columns of cards, in a two-axis scrollable.
/// `columns` and `edges` refer to cards by `NodeCard::id`; `selected` is the
/// focus card; `opts` carries focus/highlight ids and the bow; `on_click`
/// builds the message for a card click.
pub fn view<'a>(
    columns: &'a [Vec<NodeCard>],
    edges: &[EdgeSpec],
    selected: Option<&str>,
    opts: &DrawOptions,
    on_click: impl Fn(&str) -> Message,
) -> Element<'a, Message> {
    let ids: Vec<Vec<String>> = columns.iter().map(|c| c.iter().map(|n| n.id.clone()).collect()).collect();
    let tagged: HashMap<&str, bool> =
        columns.iter().flatten().map(|n| (n.id.as_str(), n.tag.is_some())).collect();
    let lay = layout_with(&ids, |id| if tagged.get(id).copied().unwrap_or(false) { CARD_H_TAGGED } else { CARD_H });
    let list = dc::connectors(edges, |id| lay.rects.get(id).copied(), opts);

    let lines = Canvas::new(Connectors { list })
        .width(Length::Fixed(lay.size.width))
        .height(Length::Fixed(lay.size.height));

    let mut cols = Row::new().spacing(COLUMN_GAP);
    for col in columns {
        let mut c = Column::new().spacing(ROW_GAP);
        for node in col {
            c = c.push(card(node, selected == Some(node.id.as_str()), on_click(&node.id)));
        }
        cols = cols.push(c);
    }
    let cards = container(cols)
        .padding(PAD)
        .width(Length::Fixed(lay.size.width))
        .height(Length::Fixed(lay.size.height));

    scrollable(stack![lines, cards])
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::default(),
            horizontal: scrollable::Scrollbar::default(),
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Legend labels paired with their color, in display order.
pub fn legend_entries() -> [(&'static str, Color); 4] {
    [
        ("Blocks", t::dep_blocks()),
        ("Blocked by", t::dep_blocked_by()),
        ("Relates to", t::dep_relates()),
        ("Duplicates", t::dep_duplicates()),
    ]
}

/// A row of colored swatches (Blocks / Blocked by / Relates to / Duplicates).
pub fn legend<'a>() -> Element<'a, Message> {
    let mut r = Row::new().spacing(space::S4).align_y(iced::Alignment::Center);
    for (label, color) in legend_entries() {
        let swatch = container(text(""))
            .width(Length::Fixed(16.0))
            .height(Length::Fixed(3.0))
            .style(move |_: &Theme| container::Style {
                background: Some(iced::Background::Color(color)),
                border: Border { radius: radius::FULL.into(), ..Default::default() },
                ..Default::default()
            });
        r = r.push(
            row![swatch, text(label).size(type_scale::XS).color(t::text_secondary())]
                .spacing(space::S1_5)
                .align_y(iced::Alignment::Center),
        );
    }
    r.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(spec: &[&[&str]]) -> Vec<Vec<String>> {
        spec.iter().map(|c| c.iter().map(|s| s.to_string()).collect()).collect()
    }

    #[test]
    fn empty_graph_is_just_padding() {
        let l = layout(&[]);
        assert!(l.rects.is_empty());
        assert_eq!(l.size, Size::new(PAD * 2.0, PAD * 2.0));
    }

    #[test]
    fn columns_step_by_card_width_plus_gap() {
        let l = layout(&cols(&[&["a"], &["b"], &["c"]]));
        assert_eq!(l.rects["a"].x, PAD);
        assert_eq!(l.rects["b"].x, PAD + CARD_W + COLUMN_GAP);
        assert_eq!(l.rects["c"].x, PAD + 2.0 * (CARD_W + COLUMN_GAP));
        assert_eq!(l.size.width, PAD * 2.0 + 3.0 * CARD_W + 2.0 * COLUMN_GAP);
    }

    #[test]
    fn rows_stack_with_the_row_gap() {
        let l = layout(&cols(&[&["a", "b", "c"]]));
        assert_eq!(l.rects["a"].y, PAD);
        assert_eq!(l.rects["b"].y, PAD + CARD_H + ROW_GAP);
        assert_eq!(l.rects["c"].y, PAD + 2.0 * (CARD_H + ROW_GAP));
        assert_eq!(l.rects["c"].width, CARD_W);
        assert_eq!(l.rects["c"].height, CARD_H);
    }

    #[test]
    fn height_follows_the_tallest_column() {
        let l = layout(&cols(&[&["a"], &["b", "c"]]));
        assert_eq!(l.size.height, PAD * 2.0 + 2.0 * CARD_H + ROW_GAP);
    }

    #[test]
    fn tagged_cards_are_taller_and_push_the_next_row() {
        let l = layout_with(&cols(&[&["a", "b"]]), |id| if id == "a" { CARD_H_TAGGED } else { CARD_H });
        assert_eq!(l.rects["a"].height, CARD_H_TAGGED);
        assert_eq!(l.rects["b"].y, PAD + CARD_H_TAGGED + ROW_GAP);
    }

    #[test]
    fn legend_lists_the_four_kinds_in_order() {
        let names: Vec<_> = legend_entries().iter().map(|e| e.0).collect();
        assert_eq!(names, ["Blocks", "Blocked by", "Relates to", "Duplicates"]);
    }
}
