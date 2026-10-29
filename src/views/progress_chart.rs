//! Project progress / burndown chart (JAIT-217). Mirrors Rails
//! `Project#progress_chart_data` / `#expected_progress_line` /
//! `#behind_schedule?`: scope / started / completed counts per sampled day plus
//! an expected-progress line. Series math is pure; drawing uses iced `canvas`.

use chrono::NaiveDate;
use iced::mouse;
use iced::widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Size, Theme};

use crate::app::Message;
use crate::db::models::{Issue, Project};
use crate::theme as t;

const MAX_SAMPLES: usize = 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub date: NaiveDate,
    pub scope: u32,
    pub started: u32,
    pub completed: u32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChartData {
    pub snapshots: Vec<Snapshot>,
    /// Same length as `snapshots` when present.
    pub expected: Option<Vec<f32>>,
    pub behind: bool,
}

/// "YYYY-MM-DD" or an RFC3339 timestamp -> date (first 10 chars).
fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

fn opt_date(s: &Option<String>) -> Option<NaiveDate> {
    s.as_deref().and_then(parse_date)
}

struct IssueDates {
    created: Option<NaiveDate>,
    started: Option<NaiveDate>,
    /// Earliest of completed/canceled (cancelled work is resolved).
    completed: Option<NaiveDate>,
}

fn sample_days(days: Vec<NaiveDate>, target: usize) -> Vec<NaiveDate> {
    if days.len() <= target || target < 2 {
        return days;
    }
    let step = (days.len() - 1) as f64 / (target - 1) as f64;
    let mut out: Vec<NaiveDate> = (0..target)
        .map(|i| days[((i as f64) * step).round() as usize])
        .collect();
    out.dedup();
    out
}

/// Compute the chart series for `project` from the issues belonging to it.
pub fn compute(project: &Project, issues: &[Issue], today: NaiveDate) -> ChartData {
    let scoped: Vec<&Issue> = issues
        .iter()
        .filter(|i| i.project_id.as_deref() == Some(project.id.as_str()) && i.archived_at.is_none())
        .collect();
    if scoped.is_empty() {
        return ChartData::default();
    }
    let dates: Vec<IssueDates> = scoped
        .iter()
        .map(|i| IssueDates {
            created: parse_date(&i.created_at),
            started: opt_date(&i.started_at),
            completed: [opt_date(&i.completed_at), opt_date(&i.canceled_at)]
                .into_iter()
                .flatten()
                .min(),
        })
        .collect();

    let start_date = opt_date(&project.start_date);
    let due_date = opt_date(&project.due_date);
    let effective_start = start_date.or_else(|| dates.iter().filter_map(|d| d.started).min());

    let chart_start = effective_start
        .or_else(|| dates.iter().filter_map(|d| d.created).min())
        .or_else(|| parse_date(&project.created_at))
        .unwrap_or(today);
    let chart_end = due_date.map_or(today, |d| d.max(today));
    if chart_end < chart_start {
        return ChartData::default();
    }
    let mut days = Vec::new();
    let mut d = chart_start;
    while d <= chart_end {
        days.push(d);
        d += chrono::Duration::days(1);
    }
    let days = sample_days(days, MAX_SAMPLES);

    let count = |f: &dyn Fn(&IssueDates) -> Option<NaiveDate>, day: NaiveDate| -> u32 {
        dates
            .iter()
            .filter(|x| f(x).is_some_and(|v| v <= day))
            .count() as u32
    };
    let snapshots: Vec<Snapshot> = days
        .iter()
        .map(|&day| Snapshot {
            date: day,
            scope: count(&|x| x.created, day),
            started: count(&|x| x.started, day),
            completed: count(&|x| x.completed, day),
        })
        .collect();

    let total = scoped.len() as f32;
    let expected = match (effective_start, due_date) {
        (Some(s), Some(due)) if (due - s).num_days() > 0 => {
            let total_days = (due - s).num_days() as f32;
            Some(
                snapshots
                    .iter()
                    .map(|snap| {
                        let frac =
                            ((snap.date - s).num_days() as f32).clamp(0.0, total_days) / total_days;
                        snap.scope as f32 * frac
                    })
                    .collect(),
            )
        }
        _ => None,
    };

    let completed_now = dates.iter().filter(|d| d.completed.is_some()).count() as f32;
    let behind = is_behind(effective_start, due_date, today, total, completed_now);
    ChartData {
        snapshots,
        expected,
        behind: behind || project.behind_schedule,
    }
}

/// Mirrors `Project#behind_schedule?`.
fn is_behind(
    start: Option<NaiveDate>,
    due: Option<NaiveDate>,
    today: NaiveDate,
    total: f32,
    completed: f32,
) -> bool {
    let (Some(s), Some(due)) = (start, due) else {
        return false;
    };
    if due <= s {
        return false;
    }
    let total_days = (due - s).num_days() as f32;
    let elapsed = (today.min(due) - s).num_days() as f32;
    if elapsed <= 0.0 {
        return false;
    }
    completed < (elapsed / total_days * total).round()
}

pub fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

/// Bezier control points `(c1, c2)` for each segment of a monotone cubic
/// (Fritsch-Carlson) curve through `pts` -- port of `smoothPath` in Rails'
/// `progress_chart_controller.js`, so curves never overshoot the data (e.g.
/// dip below 0). Returns `pts.len() - 1` segments; empty for < 2 points.
pub fn monotone_controls(pts: &[(f32, f32)]) -> Vec<((f32, f32), (f32, f32))> {
    let n = pts.len();
    if n < 2 {
        return Vec::new();
    }
    let slopes: Vec<f32> = pts
        .windows(2)
        .map(|w| {
            let dx = w[1].0 - w[0].0;
            if dx == 0.0 {
                0.0
            } else {
                (w[1].1 - w[0].1) / dx
            }
        })
        .collect();
    let mut tan = vec![slopes[0]];
    for i in 1..n - 1 {
        tan.push(if slopes[i - 1] * slopes[i] <= 0.0 {
            0.0
        } else {
            (slopes[i - 1] + slopes[i]) / 2.0
        });
    }
    tan.push(slopes[n - 2]);
    for i in 0..n - 1 {
        if slopes[i] == 0.0 {
            tan[i] = 0.0;
            tan[i + 1] = 0.0;
            continue;
        }
        let a = tan[i] / slopes[i];
        let b = tan[i + 1] / slopes[i];
        let h = a.hypot(b);
        if h > 3.0 {
            tan[i] = (3.0 / h) * a * slopes[i];
            tan[i + 1] = (3.0 / h) * b * slopes[i];
        }
    }
    (0..n - 1)
        .map(|i| {
            let (x0, y0) = pts[i];
            let (x1, y1) = pts[i + 1];
            let dx = (x1 - x0) / 3.0;
            ((x0 + dx, y0 + tan[i] * dx), (x1 - dx, y1 - tan[i + 1] * dx))
        })
        .collect()
}

/// Round the y-axis top up to 3 even steps (17 -> max 18, ticks 0/6/12/18).
/// Mirrors Rails `niceScale`.
pub fn nice_scale(max_val: f32) -> (f32, [f32; 4]) {
    let raw = (max_val / 3.0).max(f32::MIN_POSITIVE);
    // Counts are integers: floor the magnitude at 1 (Rails would give 0.4/0.8/1.2 ticks).
    let magnitude = 10f32.powf(raw.log10().floor()).max(1.0);
    let step = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0]
        .iter()
        .map(|m| m * magnitude)
        .find(|s| *s >= raw - 1e-6)
        .unwrap_or(10.0 * magnitude);
    (step * 3.0, [0.0, step, step * 2.0, step * 3.0])
}

/// Push end-of-line labels apart (min `gap` px) so close values don't overlap.
/// Takes y positions, returns label y's in the same order. Mirrors Rails
/// `spreadLabels`.
pub fn spread_labels(ys: &[f32], gap: f32, min_y: f32, max_y: f32) -> Vec<f32> {
    let mut idx: Vec<usize> = (0..ys.len()).collect();
    idx.sort_by(|&a, &b| ys[a].total_cmp(&ys[b]));
    let mut l: Vec<f32> = idx.iter().map(|&i| ys[i]).collect();
    for i in 1..l.len() {
        l[i] = l[i].max(l[i - 1] + gap);
    }
    if let Some(&last) = l.last() {
        let overflow = last - max_y;
        if overflow > 0.0 {
            l.iter_mut().for_each(|v| *v -= overflow);
        }
    }
    for i in (0..l.len().saturating_sub(1)).rev() {
        l[i] = l[i].min(l[i + 1] - gap);
    }
    l.iter_mut().for_each(|v| *v = v.max(min_y));
    let mut out = vec![0.0; ys.len()];
    for (k, &i) in idx.iter().enumerate() {
        out[i] = l[k];
    }
    out
}

/// Add a smooth (monotone cubic) curve through `pts` to the path builder.
fn smooth_to(b: &mut canvas::path::Builder, pts: &[(f32, f32)]) {
    if pts.is_empty() {
        return;
    }
    b.move_to(Point::new(pts[0].0, pts[0].1));
    if pts.len() < 3 {
        for p in &pts[1..] {
            b.line_to(Point::new(p.0, p.1));
        }
        return;
    }
    for (i, (c1, c2)) in monotone_controls(pts).into_iter().enumerate() {
        let e = pts[i + 1];
        b.bezier_curve_to(
            Point::new(c1.0, c1.1),
            Point::new(c2.0, c2.1),
            Point::new(e.0, e.1),
        );
    }
}

struct ProgressChart {
    data: ChartData,
    mini: bool,
}

type Getter = fn(&Snapshot) -> u32;

impl canvas::Program<Message> for ProgressChart {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let snaps = &self.data.snapshots;
        if snaps.is_empty() {
            return vec![frame.into_geometry()];
        }
        // Full padding = Rails renderFull `padding` {top 16, right 52, bottom 30, left 32}.
        let (pl, pr, pt, pb) = if self.mini {
            (3.0, 3.0, 3.0, 3.0)
        } else {
            (32.0, 52.0, 16.0, 30.0)
        };
        let w = (bounds.width - pl - pr).max(1.0);
        let h = (bounds.height - pt - pb).max(1.0);
        let raw_max = snaps
            .iter()
            .map(|s| s.scope as f32)
            .chain(self.data.expected.iter().flatten().copied())
            .fold(1.0_f32, f32::max);
        let (max_val, ticks) = if self.mini {
            (raw_max, [0.0; 4])
        } else {
            nice_scale(raw_max)
        };
        let n = snaps.len();
        let x = |i: usize| pl + (i as f32 / n.saturating_sub(1).max(1) as f32) * w;
        let y = |v: f32| pt + h - (v / max_val) * h;
        let base = y(0.0);

        if !self.mini {
            for v in ticks {
                let line = Path::line(Point::new(pl, y(v)), Point::new(pl + w, y(v)));
                let width = if v == 0.0 { 1.0 } else { 0.75 };
                frame.stroke(
                    &line,
                    Stroke::default().with_color(t::BORDER()).with_width(width),
                );
                frame.fill_text(canvas::Text {
                    content: format!("{}", v.round() as i64),
                    position: Point::new(pl - 10.0, y(v) - 6.0),
                    color: t::TEXT_MUTED(),
                    size: 11.0.into(),
                    horizontal_alignment: iced::alignment::Horizontal::Right,
                    ..Default::default()
                });
            }
            for i in [0, n - 1] {
                frame.fill_text(canvas::Text {
                    content: snaps[i].date.format("%b %-d").to_string(),
                    position: Point::new(x(i), bounds.height - 17.0),
                    color: t::TEXT_MUTED(),
                    size: 11.0.into(),
                    horizontal_alignment: iced::alignment::Horizontal::Center,
                    ..Default::default()
                });
            }
        }

        // Top-of-gradient fill opacities = Rails SERIES fillOpacity (0.22/0.22/0.3).
        let series: [(Color, Getter, f32); 3] = [
            (t::CHART_SCOPE(), |s| s.scope, 0.22),
            (t::CHART_STARTED(), |s| s.started, 0.22),
            (t::CHART_COMPLETED(), |s| s.completed, 0.3),
        ];
        let width = if self.mini { 1.2 } else { 2.0 };
        let mut ends: Vec<(Color, u32, f32)> = Vec::new();
        for (color, get, alpha) in series {
            let pts: Vec<(f32, f32)> = snaps
                .iter()
                .enumerate()
                .map(|(i, s)| (x(i), y(get(s) as f32)))
                .collect();
            if !self.mini {
                let area = Path::new(|b| {
                    smooth_to(b, &pts);
                    b.line_to(Point::new(x(n - 1), base));
                    b.line_to(Point::new(x(0), base));
                    b.close();
                });
                // Vertical gradient alpha -> 0.02, as the Rails linearGradient.
                let grad =
                    canvas::gradient::Linear::new(Point::new(0.0, pt), Point::new(0.0, base))
                        .add_stop(0.0, Color { a: alpha, ..color })
                        .add_stop(1.0, Color { a: 0.02, ..color });
                frame.fill(&area, canvas::Fill::from(canvas::Gradient::Linear(grad)));
            }
            let line = Path::new(|b| smooth_to(b, &pts));
            frame.stroke(&line, Stroke::default().with_color(color).with_width(width));
            ends.push((color, get(&snaps[n - 1]), pts[n - 1].1));
        }

        if let Some(exp) = &self.data.expected {
            // Rails draws the expected line piecewise (straight) in #f87171.
            let color = if self.data.behind {
                t::DANGER()
            } else {
                Color {
                    a: 0.7,
                    ..t::DANGER()
                }
            };
            let line = Path::new(|b| {
                for (i, v) in exp.iter().enumerate() {
                    let p = Point::new(x(i), y(*v));
                    if i == 0 {
                        b.move_to(p);
                    } else {
                        b.line_to(p);
                    }
                }
            });
            let dash: &[f32] = &[7.0, 5.0];
            frame.stroke(
                &line,
                Stroke {
                    line_dash: canvas::LineDash {
                        segments: dash,
                        offset: 0,
                    },
                    ..Stroke::default().with_color(color).with_width(width)
                },
            );
        }

        if !self.mini {
            // End dots (r 4.5 + background ring) and tinted value badges, spread
            // so close values (17 / 16) don't overlap -- Rails endDots/endLabels.
            let ex = x(n - 1);
            let ys: Vec<f32> = ends.iter().map(|e| e.2).collect();
            let label_ys = spread_labels(&ys, 20.0, pt + 8.0, base);
            for ((color, last, ey), ly) in ends.iter().zip(label_ys) {
                frame.fill(&Path::circle(Point::new(ex, *ey), 6.5), t::BG_PRIMARY());
                frame.fill(&Path::circle(Point::new(ex, *ey), 4.5), *color);
                let bw = 14.0 + last.to_string().len() as f32 * 7.0;
                frame.fill_rectangle(
                    Point::new(ex + 10.0, ly - 10.0),
                    Size::new(bw, 20.0),
                    Color { a: 0.14, ..*color },
                );
                frame.fill_text(canvas::Text {
                    content: last.to_string(),
                    position: Point::new(ex + 10.0 + bw / 2.0, ly - 6.0),
                    color: t::TEXT_PRIMARY(),
                    size: 11.0.into(),
                    horizontal_alignment: iced::alignment::Horizontal::Center,
                    ..Default::default()
                });
            }
        }

        // Behind schedule on a mini chart: red marker bar on the left edge
        // (desktop-only affordance; Rails' sparkline has no behind indicator).
        if self.mini && self.data.behind {
            frame.fill_rectangle(Point::ORIGIN, Size::new(2.0, bounds.height), t::DANGER());
        }
        vec![frame.into_geometry()]
    }
}

/// Full-size chart, fixed height.
pub fn view<'a>(data: ChartData) -> Element<'a, Message> {
    Canvas::new(ProgressChart { data, mini: false })
        .width(Length::Fill)
        .height(Length::Fixed(220.0))
        .into()
}

/// Mini sparkline for table rows.
pub fn sparkline<'a>(data: ChartData, width: f32) -> Element<'a, Message> {
    Canvas::new(ProgressChart { data, mini: true })
        .width(Length::Fixed(width))
        .height(Length::Fixed(24.0))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::ProjectStatus;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn project(start: Option<&str>, due: Option<&str>) -> Project {
        Project {
            id: "p1".into(),
            remote_id: None,
            team_id: "t".into(),
            name: "P".into(),
            description: None,
            status: ProjectStatus::Started,
            priority: 0,
            lead_id: None,
            start_date: start.map(String::from),
            due_date: due.map(String::from),
            roadmap_commitment: None,
            velocity_score: 0,
            completed_issues_count: 0,
            total_issues_count: 0,
            progress_percentage: 0,
            behind_schedule: false,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    fn ts(d: &str) -> String {
        format!("{d}T10:00:00.000Z")
    }

    fn issue(created: &str, started: Option<&str>, done: Option<&str>) -> Issue {
        Issue {
            id: "i".into(),
            title: "t".into(),
            description: None,
            priority: 0,
            team_number: None,
            due_date: None,
            estimate: None,
            started_at: started.map(ts),
            completed_at: done.map(ts),
            canceled_at: None,
            lane_id: "l".into(),
            team_id: "t".into(),
            project_id: Some("p1".into()),
            parent_issue_id: None,
            assignee_id: None,
            creator_id: None,
            archived_at: None,
            remote_id: None,
            created_at: ts(created),
            updated_at: ts(created),
        }
    }

    #[test]
    fn empty_project_has_no_data() {
        let c = compute(&project(None, None), &[], d("2026-02-01"));
        assert!(c.snapshots.is_empty());
        assert!(!c.behind);
    }

    #[test]
    fn no_dates_has_no_expected_line() {
        let issues = [issue("2026-01-01", None, None)];
        let c = compute(&project(None, None), &issues, d("2026-01-05"));
        assert_eq!(c.snapshots.len(), 5);
        assert!(c.expected.is_none());
        assert!(!c.behind);
        assert_eq!(c.snapshots[4].scope, 1);
    }

    #[test]
    fn all_complete_is_not_behind() {
        let issues = [
            issue("2026-01-01", Some("2026-01-02"), Some("2026-01-03")),
            issue("2026-01-01", Some("2026-01-02"), Some("2026-01-04")),
        ];
        let c = compute(
            &project(Some("2026-01-01"), Some("2026-01-11")),
            &issues,
            d("2026-01-10"),
        );
        assert!(!c.behind);
        let last = c.snapshots.last().unwrap();
        assert_eq!((last.scope, last.started, last.completed), (2, 2, 2));
        assert_eq!(c.expected.as_ref().unwrap().len(), c.snapshots.len());
    }

    #[test]
    fn past_due_incomplete_is_behind() {
        let issues = [
            issue("2026-01-01", None, None),
            issue("2026-01-01", None, Some("2026-01-02")),
        ];
        let c = compute(
            &project(Some("2026-01-01"), Some("2026-01-11")),
            &issues,
            d("2026-02-01"),
        );
        assert!(c.behind);
        let exp = c.expected.unwrap();
        assert!((exp.last().unwrap() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn samples_are_capped() {
        let issues = [issue("2025-01-01", None, None)];
        let c = compute(&project(None, None), &issues, d("2026-01-01"));
        assert!(c.snapshots.len() <= MAX_SAMPLES);
        assert_eq!(c.snapshots.last().unwrap().date, d("2026-01-01"));
    }

    #[test]
    fn monotone_never_overshoots() {
        let pts = [(0.0, 10.0), (1.0, 10.0), (2.0, 0.0), (3.0, 0.0)];
        let segs = monotone_controls(&pts);
        assert_eq!(segs.len(), 3);
        for (c1, c2) in &segs {
            assert!(c1.1 >= -1e-5 && c1.1 <= 10.0 + 1e-5);
            assert!(c2.1 >= -1e-5 && c2.1 <= 10.0 + 1e-5);
        }
        // flat segment stays flat
        assert_eq!(segs[0].0 .1, 10.0);
        assert_eq!(segs[0].1 .1, 10.0);
    }

    #[test]
    fn monotone_linear_is_linear() {
        let segs = monotone_controls(&[(0.0, 0.0), (3.0, 3.0), (6.0, 6.0)]);
        assert!((segs[0].0 .1 - 1.0).abs() < 1e-5);
        assert!((segs[0].1 .1 - 2.0).abs() < 1e-5);
        assert!(monotone_controls(&[(0.0, 1.0)]).is_empty());
    }

    #[test]
    fn nice_scale_matches_rails() {
        assert_eq!(nice_scale(17.0), (18.0, [0.0, 6.0, 12.0, 18.0]));
        assert_eq!(nice_scale(1.0).0, 3.0);
        assert_eq!(nice_scale(100.0).0, 120.0);
    }

    #[test]
    fn spread_labels_separates_and_clamps() {
        let out = spread_labels(&[100.0, 101.0, 102.0], 20.0, 10.0, 200.0);
        assert!(out[1] - out[0] >= 19.99 && out[2] - out[1] >= 19.99);
        let out = spread_labels(&[195.0, 196.0], 20.0, 10.0, 200.0);
        assert!(out.iter().all(|v| *v <= 200.0 && *v >= 10.0));
        assert!(out[1] - out[0] >= 19.99);
    }
}
