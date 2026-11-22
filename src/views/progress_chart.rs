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
    /// The day the chart was computed for; drawn as a "Today" marker.
    pub today: Option<NaiveDate>,
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
    let dates = issue_dates(project, issues);
    if dates.is_empty() {
        return ChartData::default();
    }
    let due_date = opt_date(&project.due_date);
    let effective_start = effective_start(project, &dates);

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

    ChartData {
        snapshots,
        expected,
        behind: behind_from_dates(project, &dates, today),
        today: Some(today),
    }
}

/// Whether `project` is behind schedule, worked out from its local issues so
/// the answer moves with the calendar. Use this, not `project.behind_schedule`:
/// that field is Rails' answer as of the project's last pull, and a project is
/// only re-pulled when its own row changes, so finishing issues (or days
/// passing) leaves it stale. It's the fallback only when none of the project's
/// issues are mirrored locally.
pub fn behind_schedule(project: &Project, issues: &[Issue], today: NaiveDate) -> bool {
    let dates = issue_dates(project, issues);
    if dates.is_empty() {
        return project.behind_schedule;
    }
    behind_from_dates(project, &dates, today)
}

fn behind_from_dates(project: &Project, dates: &[IssueDates], today: NaiveDate) -> bool {
    let completed = dates.iter().filter(|d| d.completed.is_some()).count() as f32;
    is_behind(
        effective_start(project, dates),
        opt_date(&project.due_date),
        today,
        dates.len() as f32,
        completed,
    )
}

fn issue_dates(project: &Project, issues: &[Issue]) -> Vec<IssueDates> {
    issues
        .iter()
        .filter(|i| i.project_id.as_deref() == Some(project.id.as_str()))
        .map(|i| IssueDates {
            created: parse_date(&i.created_at),
            started: opt_date(&i.started_at),
            completed: [opt_date(&i.completed_at), opt_date(&i.canceled_at)]
                .into_iter()
                .flatten()
                .min(),
        })
        .collect()
}

/// Mirrors `Project#effective_start_date`: the start date, else the earliest
/// issue start.
fn effective_start(project: &Project, dates: &[IssueDates]) -> Option<NaiveDate> {
    opt_date(&project.start_date).or_else(|| dates.iter().filter_map(|d| d.started).min())
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

/// Indices of the samples to label on the x axis: first, last, and evenly
/// spaced ones in between, at most `max_labels` total.
pub fn axis_label_indices(n: usize, max_labels: usize) -> Vec<usize> {
    match n {
        0 => return Vec::new(),
        1 => return vec![0],
        _ => {}
    }
    let m = max_labels.clamp(2, n);
    let mut out: Vec<usize> = (0..m)
        .map(|k| ((k as f32) * (n - 1) as f32 / (m - 1) as f32).round() as usize)
        .collect();
    out.dedup();
    out
}

/// Fractional sample index of `date` (samples may skip days), or `None`
/// when it falls outside the sampled range.
pub fn date_position(snaps: &[Snapshot], date: NaiveDate) -> Option<f32> {
    let (first, last) = (snaps.first()?, snaps.last()?);
    if date < first.date || date > last.date {
        return None;
    }
    let i = snaps.windows(2).position(|w| date <= w[1].date)?;
    let (a, b) = (snaps[i].date, snaps[i + 1].date);
    let span = (b - a).num_days().max(1) as f32;
    Some(i as f32 + (date - a).num_days() as f32 / span)
}

/// Nearest sample index for a cursor at `px`, or `None` when the cursor is
/// outside the plot's horizontal extent (with a little slack at the edges).
pub fn hover_index(px: f32, pl: f32, w: f32, n: usize) -> Option<usize> {
    if n == 0 || px < pl - 8.0 || px > pl + w + 8.0 {
        return None;
    }
    if n == 1 {
        return Some(0);
    }
    let f = ((px - pl) / w).clamp(0.0, 1.0) * (n - 1) as f32;
    Some((f.round() as usize).min(n - 1))
}

/// Hover card (date + one row per series), placed right of the guide line,
/// flipping left when it would run off the chart -- Rails' chart tooltip.
fn draw_tooltip(
    frame: &mut Frame,
    bounds: Rectangle,
    hx: f32,
    top: f32,
    date: NaiveDate,
    rows: &[(Color, &str, String)],
) {
    let (pad, row_h, tw) = (10.0, 18.0, 150.0);
    let th = pad * 2.0 + row_h * (rows.len() as f32 + 1.0);
    let tx = if hx + 12.0 + tw <= bounds.width {
        hx + 12.0
    } else {
        (hx - 12.0 - tw).max(0.0)
    };
    let ty = top;
    let card = Path::rounded_rectangle(Point::new(tx, ty), Size::new(tw, th), 6.0.into());
    frame.fill(&card, t::bg_menu());
    frame.stroke(
        &card,
        Stroke::default().with_color(t::border()).with_width(1.0),
    );
    frame.fill_text(canvas::Text {
        content: date.format("%b %-d").to_string(),
        position: Point::new(tx + pad, ty + pad),
        color: t::text_muted(),
        size: 11.0.into(),
        ..Default::default()
    });
    for (k, (color, label, value)) in rows.iter().enumerate() {
        let ry = ty + pad + row_h * (k as f32 + 1.0);
        frame.fill(
            &Path::circle(Point::new(tx + pad + 4.0, ry + 7.0), 4.0),
            *color,
        );
        frame.fill_text(canvas::Text {
            content: label.to_string(),
            position: Point::new(tx + pad + 14.0, ry),
            color: t::text_secondary(),
            size: 12.0.into(),
            ..Default::default()
        });
        frame.fill_text(canvas::Text {
            content: value.clone(),
            position: Point::new(tx + tw - pad, ry),
            color: t::text_strong(),
            size: 12.0.into(),
            horizontal_alignment: iced::alignment::Horizontal::Right,
            ..Default::default()
        });
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
        cursor: mouse::Cursor,
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
        let xf = |f: f32| pl + (f / n.saturating_sub(1).max(1) as f32) * w;
        let x = |i: usize| xf(i as f32);
        let y = |v: f32| pt + h - (v / max_val) * h;
        let base = y(0.0);

        if !self.mini {
            for v in ticks {
                let line = Path::line(Point::new(pl, y(v)), Point::new(pl + w, y(v)));
                let width = if v == 0.0 { 1.0 } else { 0.75 };
                frame.stroke(
                    &line,
                    Stroke::default().with_color(t::border()).with_width(width),
                );
                frame.fill_text(canvas::Text {
                    content: format!("{}", v.round() as i64),
                    position: Point::new(pl - 10.0, y(v) - 6.0),
                    color: t::text_muted(),
                    size: 11.0.into(),
                    horizontal_alignment: iced::alignment::Horizontal::Right,
                    ..Default::default()
                });
            }
            // ~1 date label per 80px of plot width, always incl. start/end.
            let max_labels = ((w / 80.0).floor() as usize).max(2);
            for i in axis_label_indices(n, max_labels) {
                let date = snaps[i].date;
                let content = if Some(date) == self.data.today {
                    "Today".to_string()
                } else {
                    date.format("%b %-d").to_string()
                };
                frame.fill_text(canvas::Text {
                    content,
                    position: Point::new(x(i), bounds.height - 17.0),
                    color: t::text_muted(),
                    size: 11.0.into(),
                    horizontal_alignment: iced::alignment::Horizontal::Center,
                    ..Default::default()
                });
            }
        }

        // "Today" marker inside the range (an endpoint is labelled on the axis).
        if !self.mini {
            if let Some(f) = self
                .data
                .today
                .and_then(|today| date_position(snaps, today))
                .filter(|f| *f > 0.0 && *f < (n - 1) as f32)
            {
                let tx = xf(f);
                frame.stroke(
                    &Path::line(Point::new(tx, pt), Point::new(tx, base)),
                    Stroke {
                        line_dash: canvas::LineDash {
                            segments: &[2.0, 3.0],
                            offset: 0,
                        },
                        ..Stroke::default()
                            .with_color(t::text_secondary())
                            .with_width(1.0)
                    },
                );
                frame.fill_text(canvas::Text {
                    content: "Today".to_string(),
                    position: Point::new(tx, 1.0),
                    color: t::text_secondary(),
                    size: 10.0.into(),
                    horizontal_alignment: iced::alignment::Horizontal::Center,
                    ..Default::default()
                });
            }
        }

        // Top-of-gradient fill opacities = Rails SERIES fillOpacity (0.22/0.22/0.3).
        let series: [(Color, Getter, f32); 3] = [
            (t::chart_scope(), |s| s.scope, 0.22),
            (t::chart_started(), |s| s.started, 0.22),
            (t::chart_completed(), |s| s.completed, 0.3),
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
                t::danger()
            } else {
                Color {
                    a: 0.7,
                    ..t::danger()
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
                frame.fill(&Path::circle(Point::new(ex, *ey), 6.5), t::bg_primary());
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
                    color: t::text_primary(),
                    size: 11.0.into(),
                    horizontal_alignment: iced::alignment::Horizontal::Center,
                    ..Default::default()
                });
            }
        }

        if !self.mini {
            if let Some(p) = cursor.position_in(bounds) {
                if let Some(i) = hover_index(p.x, pl, w, n) {
                    let snap = &snaps[i];
                    let expected = self.data.expected.as_ref().map(|e| e[i]);
                    let hx = x(i);
                    // Dashed guide at the hovered sample.
                    frame.stroke(
                        &Path::line(Point::new(hx, pt), Point::new(hx, base)),
                        Stroke {
                            line_dash: canvas::LineDash {
                                segments: &[3.0, 3.0],
                                offset: 0,
                            },
                            ..Stroke::default()
                                .with_color(t::text_muted())
                                .with_width(1.0)
                        },
                    );
                    let mut rows: Vec<(Color, &str, String)> = Vec::new();
                    for (color, get, _) in series {
                        frame.fill(
                            &Path::circle(Point::new(hx, y(get(snap) as f32)), 5.5),
                            t::bg_primary(),
                        );
                        frame.fill(
                            &Path::circle(Point::new(hx, y(get(snap) as f32)), 3.5),
                            color,
                        );
                    }
                    rows.push((t::chart_scope(), "Scope", snap.scope.to_string()));
                    rows.push((t::chart_started(), "Started", snap.started.to_string()));
                    rows.push((
                        t::chart_completed(),
                        "Completed",
                        snap.completed.to_string(),
                    ));
                    if let Some(v) = expected {
                        rows.push((t::danger(), "Expected", format!("{}", v.round() as i64)));
                    }
                    draw_tooltip(&mut frame, bounds, hx, pt, snap.date, &rows);
                }
            }
        }

        // Behind schedule on a mini chart: red marker bar on the left edge
        // (desktop-only affordance; Rails' sparkline has no behind indicator).
        if self.mini && self.data.behind {
            frame.fill_rectangle(Point::ORIGIN, Size::new(2.0, bounds.height), t::danger());
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
            status: Some(ProjectStatus::Started),
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
    fn stale_synced_flag_does_not_override_local_answer() {
        // Rails said "behind" at the last pull; since then the work got done.
        let mut p = project(Some("2026-01-01"), Some("2026-01-11"));
        p.behind_schedule = true;
        let issues = [
            issue("2026-01-01", Some("2026-01-02"), Some("2026-01-03")),
            issue("2026-01-01", None, None),
        ];
        let today = d("2026-01-04"); // 3/10 elapsed -> expect 1 of 2 done
        assert!(!compute(&p, &issues, today).behind);
        assert!(!behind_schedule(&p, &issues, today));
    }

    #[test]
    fn synced_flag_is_fallback_without_local_issues() {
        let mut p = project(Some("2026-01-01"), Some("2026-01-11"));
        p.behind_schedule = true;
        assert!(behind_schedule(&p, &[], d("2026-01-04")));
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
    fn hover_index_snaps_to_nearest_sample() {
        assert_eq!(hover_index(10.0, 32.0, 100.0, 5), None);
        assert_eq!(hover_index(32.0, 32.0, 100.0, 5), Some(0));
        assert_eq!(hover_index(70.0, 32.0, 100.0, 5), Some(2));
        assert_eq!(hover_index(136.0, 32.0, 100.0, 5), Some(4));
        assert_eq!(hover_index(50.0, 32.0, 100.0, 1), Some(0));
    }

    #[test]
    fn axis_labels_include_ends_and_spread_evenly() {
        assert!(axis_label_indices(0, 5).is_empty());
        assert_eq!(axis_label_indices(1, 5), vec![0]);
        assert_eq!(axis_label_indices(2, 5), vec![0, 1]);
        assert_eq!(axis_label_indices(9, 5), vec![0, 2, 4, 6, 8]);
        assert_eq!(axis_label_indices(60, 2), vec![0, 59]);
        let idx = axis_label_indices(60, 6);
        assert_eq!((idx[0], *idx.last().unwrap(), idx.len()), (0, 59, 6));
    }

    #[test]
    fn today_position_interpolates_between_samples() {
        let snap = |s: &str| Snapshot {
            date: d(s),
            scope: 0,
            started: 0,
            completed: 0,
        };
        let snaps = [snap("2026-01-01"), snap("2026-01-05"), snap("2026-01-09")];
        assert_eq!(date_position(&snaps, d("2026-01-01")), Some(0.0));
        assert_eq!(date_position(&snaps, d("2026-01-03")), Some(0.5));
        assert_eq!(date_position(&snaps, d("2026-01-05")), Some(1.0));
        assert_eq!(date_position(&snaps, d("2026-01-09")), Some(2.0));
        assert_eq!(date_position(&snaps, d("2025-12-31")), None);
        assert_eq!(date_position(&snaps, d("2026-01-10")), None);
    }

    #[test]
    fn compute_records_today() {
        let issues = [issue("2026-01-01", None, None)];
        let c = compute(&project(None, Some("2026-01-20")), &issues, d("2026-01-05"));
        assert_eq!(c.today, Some(d("2026-01-05")));
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
