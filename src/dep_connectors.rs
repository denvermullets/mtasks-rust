//! Curved dependency connectors between two card rects (pure geometry plus a
//! canvas stroking helper). Port of the web's `lib/dependency_connectors.js`
//! `ConnectorLayer`; shared by the dependency map canvas and the board overlay.

use iced::widget::canvas::{self, Frame, Path, Stroke};
use iced::{Color, Point, Rectangle};

use crate::theme as t;

const MIN_CONTROL_OFFSET: f32 = 40.0;
/// Default horizontal control offset when both anchors sit on the left edge.
pub const SAME_COLUMN_BOW: f32 = 28.0;
/// The board overlay's tighter bow.
pub const BOARD_BOW: f32 = 12.0;
/// Horizontal overlap still treated as "beside", not "same column".
const COLUMN_TOLERANCE: f32 = 12.0;
pub const SOURCE_DOT_RADIUS: f32 = 4.0;
pub const TARGET_RING_RADIUS: f32 = 6.0;
/// Opacity of a line that doesn't touch the highlighted id.
pub const DIMMED_ALPHA: f32 = 0.15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorKind {
    Blocks,
    Relates,
    Duplicates,
}

/// One edge to draw, in terms of issue ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeSpec {
    pub from_id: String,
    pub to_id: String,
    pub kind: ConnectorKind,
}

/// Anchors and bezier control x's of a connector (control y's equal the anchors').
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Route {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub cx1: f32,
    pub cx2: f32,
    /// Which way the line travels as it arrives at the target (for the chevron).
    pub arrives_rightward: bool,
}

/// Route from `source` to `target`; `bow` is the same-column control offset.
pub fn route(source: Rectangle, target: Rectangle, bow: f32) -> Route {
    let y1 = source.y + source.height / 2.0;
    let y2 = target.y + target.height / 2.0;
    let (s_left, s_right) = (source.x, source.x + source.width);
    let (t_left, t_right) = (target.x, target.x + target.width);

    if t_left >= s_right - COLUMN_TOLERANCE {
        let (x1, x2) = (s_right, t_left);
        let offset = MIN_CONTROL_OFFSET.max((x2 - x1).abs() / 2.0);
        return Route { x1, y1, x2, y2, cx1: x1 + offset, cx2: x2 - offset, arrives_rightward: true };
    }
    if t_right <= s_left + COLUMN_TOLERANCE {
        let (x1, x2) = (s_left, t_right);
        let offset = MIN_CONTROL_OFFSET.max((x2 - x1).abs() / 2.0);
        return Route { x1, y1, x2, y2, cx1: x1 - offset, cx2: x2 + offset, arrives_rightward: false };
    }
    let (x1, x2) = (s_left, t_left);
    let b = x1.min(x2) - bow;
    Route { x1, y1, x2, y2, cx1: b, cx2: b, arrives_rightward: true }
}

/// Options shared by every connector of one drawing.
#[derive(Debug, Clone, Default)]
pub struct DrawOptions {
    /// Same-column bow; `None` is the default 28.
    pub bow: Option<f32>,
    /// The issue the view is focused on: `blocks` edges into it read as "blocked by".
    pub focus_id: Option<String>,
    /// Lines not touching this id are dimmed.
    pub highlight_id: Option<String>,
}

/// A fully resolved connector, ready to stroke.
#[derive(Debug, Clone, PartialEq)]
pub struct Connector {
    pub route: Route,
    pub kind: ConnectorKind,
    pub color: Color,
    pub alpha: f32,
    pub dashed: bool,
}

/// Line color: by kind, with a `blocks` edge into the focus issue shown as blocked-by.
pub fn color_for(kind: ConnectorKind, to_is_focus: bool) -> Color {
    match kind {
        ConnectorKind::Blocks if to_is_focus => t::dep_blocked_by(),
        ConnectorKind::Blocks => t::dep_blocks(),
        ConnectorKind::Relates => t::dep_relates(),
        ConnectorKind::Duplicates => t::dep_duplicates(),
    }
}

/// Alpha for an edge given the highlighted id (full when nothing is highlighted).
pub fn alpha_for(edge: &EdgeSpec, highlight_id: Option<&str>) -> f32 {
    match highlight_id {
        Some(id) if edge.from_id != id && edge.to_id != id => DIMMED_ALPHA,
        _ => 1.0,
    }
}

/// Resolve one edge between the two measured rects.
pub fn connector(edge: &EdgeSpec, source: Rectangle, target: Rectangle, opts: &DrawOptions) -> Connector {
    let to_is_focus = opts.focus_id.as_deref() == Some(edge.to_id.as_str());
    Connector {
        route: route(source, target, opts.bow.unwrap_or(SAME_COLUMN_BOW)),
        kind: edge.kind,
        color: color_for(edge.kind, to_is_focus),
        alpha: alpha_for(edge, opts.highlight_id.as_deref()),
        dashed: edge.kind == ConnectorKind::Duplicates,
    }
}

/// Resolve every edge whose two ends both have a rect (`rect_for` returns None otherwise).
pub fn connectors(
    edges: &[EdgeSpec],
    rect_for: impl Fn(&str) -> Option<Rectangle>,
    opts: &DrawOptions,
) -> Vec<Connector> {
    edges
        .iter()
        .filter_map(|e| {
            let (s, t) = (rect_for(&e.from_id)?, rect_for(&e.to_id)?);
            Some(connector(e, s, t, opts))
        })
        .collect()
}

fn with_alpha(c: Color, alpha: f32) -> Color {
    Color { a: c.a * alpha, ..c }
}

/// Stroke the connectors onto `frame`: curve, source dot, target ring and chevron.
pub fn stroke_connectors(frame: &mut Frame, list: &[Connector]) {
    for c in list {
        let r = &c.route;
        let color = with_alpha(c.color, c.alpha);
        let curve = Path::new(|b| {
            b.move_to(Point::new(r.x1, r.y1));
            b.bezier_curve_to(Point::new(r.cx1, r.y1), Point::new(r.cx2, r.y2), Point::new(r.x2, r.y2));
        });
        let mut line = Stroke::default()
            .with_color(color)
            .with_width(2.0)
            .with_line_cap(canvas::LineCap::Round);
        if c.dashed {
            line.line_dash = canvas::LineDash { segments: &[5.0, 5.0], offset: 0 };
        }
        frame.stroke(&curve, line);

        frame.fill(&Path::circle(Point::new(r.x1, r.y1), SOURCE_DOT_RADIUS), color);

        let ring = Path::circle(Point::new(r.x2, r.y2), TARGET_RING_RADIUS);
        frame.fill(&ring, with_alpha(t::bg_secondary(), c.alpha));
        frame.stroke(&ring, Stroke::default().with_color(color).with_width(1.5));

        let dir = if r.arrives_rightward { 1.0 } else { -1.0 };
        let at = |dx: f32, dy: f32| Point::new(r.x2 + dir * dx, r.y2 + dy);
        let chevron = Path::new(|b| {
            b.move_to(at(-1.25, -2.5));
            b.line_to(at(1.5, 0.0));
            b.line_to(at(-1.25, 2.5));
        });
        frame.stroke(
            &chevron,
            Stroke::default()
                .with_color(color)
                .with_width(1.5)
                .with_line_cap(canvas::LineCap::Round)
                .with_line_join(canvas::LineJoin::Round),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32) -> Rectangle {
        Rectangle { x, y, width: 100.0, height: 40.0 }
    }

    fn edge(kind: ConnectorKind) -> EdgeSpec {
        EdgeSpec { from_id: "a".into(), to_id: "b".into(), kind }
    }

    #[test]
    fn beside_runs_right_edge_to_left_edge() {
        let r = route(rect(0.0, 0.0), rect(300.0, 100.0), 28.0);
        assert_eq!((r.x1, r.y1, r.x2, r.y2), (100.0, 20.0, 300.0, 120.0));
        assert_eq!((r.cx1, r.cx2), (200.0, 200.0)); // offset = max(40, 100)
        assert!(r.arrives_rightward);
    }

    #[test]
    fn beside_uses_min_control_offset() {
        let r = route(rect(0.0, 0.0), rect(120.0, 0.0), 28.0);
        assert_eq!((r.x1, r.x2), (100.0, 120.0));
        assert_eq!((r.cx1, r.cx2), (140.0, 80.0));
    }

    #[test]
    fn within_tolerance_still_counts_as_beside() {
        let r = route(rect(0.0, 0.0), rect(88.0, 0.0), 28.0);
        assert!(r.arrives_rightward);
        assert_eq!(r.x1, 100.0);
    }

    #[test]
    fn same_column_bows_out_of_the_left_edge() {
        let r = route(rect(50.0, 0.0), rect(60.0, 100.0), 28.0);
        assert_eq!((r.x1, r.x2), (50.0, 60.0));
        assert_eq!((r.cx1, r.cx2), (22.0, 22.0));
        assert!(r.arrives_rightward);
        let board = route(rect(50.0, 0.0), rect(60.0, 100.0), BOARD_BOW);
        assert_eq!(board.cx1, 38.0);
    }

    #[test]
    fn target_left_of_source_is_mirrored() {
        let r = route(rect(300.0, 0.0), rect(0.0, 100.0), 28.0);
        assert_eq!((r.x1, r.x2), (300.0, 100.0));
        assert_eq!((r.cx1, r.cx2), (200.0, 200.0));
        assert!(!r.arrives_rightward);
    }

    #[test]
    fn colors_by_kind_and_focus() {
        assert_eq!(color_for(ConnectorKind::Blocks, false), t::dep_blocks());
        assert_eq!(color_for(ConnectorKind::Blocks, true), t::dep_blocked_by());
        assert_eq!(color_for(ConnectorKind::Relates, false), t::dep_relates());
        assert_eq!(color_for(ConnectorKind::Relates, true), t::dep_relates());
        assert_eq!(color_for(ConnectorKind::Duplicates, true), t::dep_duplicates());
    }

    #[test]
    fn connector_applies_focus_only_when_target_is_focus() {
        let (s, tg) = (rect(0.0, 0.0), rect(300.0, 0.0));
        let e = edge(ConnectorKind::Blocks);
        let focus_b = DrawOptions { focus_id: Some("b".into()), ..Default::default() };
        let focus_a = DrawOptions { focus_id: Some("a".into()), ..Default::default() };
        assert_eq!(connector(&e, s, tg, &focus_b).color, t::dep_blocked_by());
        assert_eq!(connector(&e, s, tg, &focus_a).color, t::dep_blocks());
        assert_eq!(connector(&e, s, tg, &DrawOptions::default()).color, t::dep_blocks());
    }

    #[test]
    fn duplicates_are_dashed() {
        let (s, tg) = (rect(0.0, 0.0), rect(300.0, 0.0));
        let o = DrawOptions::default();
        assert!(connector(&edge(ConnectorKind::Duplicates), s, tg, &o).dashed);
        assert!(!connector(&edge(ConnectorKind::Blocks), s, tg, &o).dashed);
    }

    #[test]
    fn dims_edges_not_touching_the_highlight() {
        let e = edge(ConnectorKind::Blocks);
        assert_eq!(alpha_for(&e, None), 1.0);
        assert_eq!(alpha_for(&e, Some("a")), 1.0);
        assert_eq!(alpha_for(&e, Some("b")), 1.0);
        assert_eq!(alpha_for(&e, Some("z")), DIMMED_ALPHA);
    }

    #[test]
    fn connectors_skip_edges_with_a_missing_rect() {
        let edges = vec![
            edge(ConnectorKind::Blocks),
            EdgeSpec { from_id: "a".into(), to_id: "c".into(), kind: ConnectorKind::Relates },
        ];
        let rects = |id: &str| match id {
            "a" => Some(rect(0.0, 0.0)),
            "b" => Some(rect(300.0, 0.0)),
            _ => None,
        };
        let out = connectors(&edges, rects, &DrawOptions::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, ConnectorKind::Blocks);
    }
}
