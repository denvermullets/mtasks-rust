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
        let (pl, pr, pt, pb) = if self.mini {
            (3.0, 3.0, 3.0, 3.0)
        } else {
            (30.0, 40.0, 12.0, 22.0)
        };
        let w = (bounds.width - pl - pr).max(1.0);
        let h = (bounds.height - pt - pb).max(1.0);
        let max_val = snaps
            .iter()
            .map(|s| s.scope as f32)
            .chain(self.data.expected.iter().flatten().copied())
            .fold(1.0_f32, f32::max);
        let n = snaps.len();
        let x = |i: usize| pl + (i as f32 / n.saturating_sub(1).max(1) as f32) * w;
        let y = |v: f32| pt + h - (v / max_val) * h;
        let base = y(0.0);

        if !self.mini {
            for k in 0..=4 {
                let v = max_val * k as f32 / 4.0;
                let line = Path::line(Point::new(pl, y(v)), Point::new(pl + w, y(v)));
                frame.stroke(
                    &line,
                    Stroke::default().with_color(t::BORDER).with_width(1.0),
                );
                frame.fill_text(canvas::Text {
                    content: format!("{}", v.round() as i64),
                    position: Point::new(pl - 6.0, y(v) - 6.0),
                    color: t::TEXT_MUTED,
                    size: 10.0.into(),
                    horizontal_alignment: iced::alignment::Horizontal::Right,
                    ..Default::default()
                });
            }
            for (i, align) in [
                (0, iced::alignment::Horizontal::Left),
                (n - 1, iced::alignment::Horizontal::Right),
            ] {
                frame.fill_text(canvas::Text {
                    content: snaps[i].date.format("%b %-d").to_string(),
                    position: Point::new(x(i), bounds.height - 14.0),
                    color: t::TEXT_MUTED,
                    size: 10.0.into(),
                    horizontal_alignment: align,
                    ..Default::default()
                });
            }
        }

        let series: [(Color, Getter, f32); 3] = [
            (t::CHART_SCOPE, |s| s.scope, 0.12),
            (t::CHART_STARTED, |s| s.started, 0.12),
            (t::CHART_COMPLETED, |s| s.completed, 0.2),
        ];
        let width = if self.mini { 1.2 } else { 2.0 };
        for (color, get, alpha) in series {
            if !self.mini {
                let area = Path::new(|b| {
                    b.move_to(Point::new(x(0), base));
                    for (i, s) in snaps.iter().enumerate() {
                        b.line_to(Point::new(x(i), y(get(s) as f32)));
                    }
                    b.line_to(Point::new(x(n - 1), base));
                    b.close();
                });
                frame.fill(&area, Color { a: alpha, ..color });
            }
            let line = Path::new(|b| {
                for (i, s) in snaps.iter().enumerate() {
                    let p = Point::new(x(i), y(get(s) as f32));
                    if i == 0 {
                        b.move_to(p);
                    } else {
                        b.line_to(p);
                    }
                }
            });
            frame.stroke(&line, Stroke::default().with_color(color).with_width(width));
            if !self.mini {
                let last = get(&snaps[n - 1]);
                frame.fill(
                    &Path::circle(Point::new(x(n - 1), y(last as f32)), 3.5),
                    color,
                );
                frame.fill_text(canvas::Text {
                    content: last.to_string(),
                    position: Point::new(x(n - 1) + 8.0, y(last as f32) - 6.0),
                    color,
                    size: 11.0.into(),
                    ..Default::default()
                });
            }
        }

        if let Some(exp) = &self.data.expected {
            let color = if self.data.behind {
                t::DANGER
            } else {
                Color {
                    a: 0.7,
                    ..t::DANGER
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

        // Behind schedule on a mini chart: red marker bar on the left edge.
        if self.mini && self.data.behind {
            frame.fill_rectangle(Point::ORIGIN, Size::new(2.0, bounds.height), t::DANGER);
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
}
