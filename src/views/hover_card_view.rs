//! Draws a hover card (JRST-299): a fixed-width panel in the app's top-level
//! stack, hung just under the span it describes.
//!
//! Add a field: draw it in `issue_card` (the data comes from
//! `body_hover::content_for`). Placement is `card_placement::place`, called from
//! `overlay`; keep `estimated_height` in step with the card's rows.

use iced::widget::{column, container, row};
use crate::views::text;
use iced::{alignment, Border, Element, Length, Padding, Rectangle, Size, Theme};

use crate::app::Message;
use crate::body_hover::{CardContent, IssueCard, MemberCard, ProjectCard};
use crate::card_placement::{self, Vertical};
use crate::theme as t;

/// Card width in logical pixels.
pub const WIDTH: f32 = 320.0;

/// A transparent full-window layer holding the card at `anchor` (window
/// coordinates). Clicks outside the card pass through.
pub fn overlay<'a>(content: &CardContent, anchor: Rectangle) -> Element<'a, Message> {
    let content = content.clone();
    let height = estimated_height(&content);
    // `responsive` supplies the window size at layout time, with no app message.
    iced::widget::responsive(move |window| {
        let placed = card_placement::place(anchor, Size::new(WIDTH, height), window);
        let (top, bottom, valign) = match placed.vertical {
            Vertical::Top(y) => (y, 0.0, alignment::Vertical::Top),
            Vertical::Bottom(gap) => (0.0, gap, alignment::Vertical::Bottom),
        };
        let card = match &content {
            CardContent::Issue(issue) => issue_card(issue),
            CardContent::Member(member) => member_card(member),
            CardContent::Project(project) => project_card(project),
        };
        // The card reports the pointer so moving from the anchor onto it keeps it open.
        let card = iced::widget::mouse_area(card)
            .on_enter(Message::HoverCardEntered)
            .on_exit(Message::HoverCardLeft);
        container(card)
            .padding(Padding { top, bottom, left: placed.left, right: 0.0 })
            .align_y(valign)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    })
    .into()
}

/// Average glyph width as a fraction of the font size, for wrap estimates.
const GLYPH: f32 = 0.55;

/// Rough card height for the flip decision: padding plus every row of
/// `issue_card`, with each row that can wrap (title, excerpt, metadata)
/// counted in lines. Update when `issue_card` gains rows.
fn estimated_height(content: &CardContent) -> f32 {
    match content {
        CardContent::Issue(issue) => {
            let inner = WIDTH - 2.0 * t::space::S3;
            let xs_line = t::type_scale::XS * t::type_scale::LH_XS;
            let lines = |chars: usize, size: f32| {
                let per_line = (inner / (size * GLYPH)).floor().max(1.0);
                (chars as f32 / per_line).ceil().max(1.0)
            };
            // Identifier and lane share one line.
            let mut height = xs_line;
            height += t::space::S1
                + lines(issue.title.chars().count(), t::type_scale::SM)
                    * t::type_scale::SM
                    * t::type_scale::LH_SM;
            if let Some(excerpt) = &issue.excerpt {
                let n = lines(excerpt.chars().count(), t::type_scale::XS).min(EXCERPT_LINES);
                height += t::space::S1 + n * xs_line;
            }
            let rows = meta_lines(&meta_widths(issue), inner, t::space::S3);
            height += t::space::S1 + rows as f32 * xs_line + (rows - 1) as f32 * t::space::S3;
            2.0 * t::space::S3 + height
        }
        CardContent::Project(project) => {
            let inner = WIDTH - 2.0 * t::space::S3;
            let xs_line = t::type_scale::XS * t::type_scale::LH_XS;
            // Name/status line, progress label + bar, lead/due line.
            let mut height = t::type_scale::SM * t::type_scale::LH_SM;
            if let Some(excerpt) = &project.excerpt {
                let per_line = (inner / (t::type_scale::XS * GLYPH)).floor().max(1.0);
                let n = (excerpt.chars().count() as f32 / per_line).ceil().clamp(1.0, EXCERPT_LINES);
                height += t::space::S2 + n * xs_line;
            }
            height += t::space::S2 + xs_line + t::space::S1 + PROGRESS_HEIGHT;
            if project.lead.is_some() || project.due.is_some() {
                height += t::space::S2 + xs_line.max(16.0);
            }
            2.0 * t::space::S3 + height
        }
        // Name, email and count: three single lines.
        CardContent::Member(_) => {
            2.0 * t::space::S3
                + t::type_scale::SM * t::type_scale::LH_SM
                + 2.0 * (t::space::S1 + t::type_scale::XS * t::type_scale::LH_XS)
        }
    }
}

/// Lines the excerpt is clamped to.
const EXCERPT_LINES: f32 = 3.0;

/// Estimated widths of the metadata groups, in the order `issue_card` draws
/// them: a glyph or badge (about 16), a gap, then the label.
fn meta_widths(issue: &IssueCard) -> Vec<f32> {
    let label = |s: &str| s.chars().count() as f32 * t::type_scale::XS * GLYPH;
    let group = |s: &str| 16.0 + t::space::S1 + label(s);
    let mut widths = vec![group(issue.priority_label)];
    for text in [&issue.assignee, &issue.project, &issue.due].into_iter().flatten() {
        widths.push(group(text));
    }
    widths
}

/// How many lines a wrapping row of items with `widths` takes in `available`
/// width, `spacing` between items. Always at least one.
fn meta_lines(widths: &[f32], available: f32, spacing: f32) -> usize {
    let mut lines = 1;
    let mut used = 0.0;
    for &w in widths {
        let need = if used == 0.0 { w } else { used + spacing + w };
        if used > 0.0 && need > available {
            lines += 1;
            used = w;
        } else {
            used = need;
        }
    }
    lines
}

fn member_card<'a>(member: &MemberCard) -> Element<'a, Message> {
    let body = column![
        text(member.name.clone()).size(t::type_scale::SM).color(t::text_primary()),
        text(member.email.clone()).size(t::type_scale::XS).color(t::text_secondary()),
        text(match member.open_issues {
            1 => "1 open issue".to_string(),
            n => format!("{n} open issues"),
        })
        .size(t::type_scale::XS)
        .color(t::text_secondary()),
    ]
    .spacing(t::space::S1);
    panel(body.into())
}

/// Height of the project card's progress bar (the web's `h-1.5`).
const PROGRESS_HEIGHT: f32 = 6.0;

/// The web's status label: the status, capitalized, or "No status".
fn status_label(status: Option<crate::db::models::ProjectStatus>) -> String {
    let Some(status) = status else { return "No status".to_string() };
    let raw = status.as_str();
    let mut chars = raw.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn project_card<'a>(project: &ProjectCard) -> Element<'a, Message> {
    let status_color = project.status.map(t::project_status_color).unwrap_or_else(t::text_muted);
    let top = row![
        text("◆").size(t::type_scale::XS).color(status_color),
        container(text(project.name.clone()).size(t::type_scale::SM).color(t::text_primary()))
            .width(Length::Fill)
            .max_height(t::type_scale::SM * t::type_scale::LH_SM)
            .clip(true),
        muted(status_label(project.status)),
    ]
    .spacing(t::space::S2)
    .align_y(iced::Alignment::Center);

    let mut body = column![top].spacing(t::space::S2);
    if let Some(excerpt) = &project.excerpt {
        body = body.push(
            container(
                text(excerpt.clone())
                    .size(t::type_scale::XS)
                    .line_height(t::type_scale::LH_XS)
                    .color(t::text_muted()),
            )
            .max_height(t::type_scale::XS * t::type_scale::LH_XS * EXCERPT_LINES)
            .clip(true),
        );
    }

    let pct = project.percent.clamp(0, 100) as u16;
    let bar = row![]
        .height(Length::Fixed(PROGRESS_HEIGHT))
        .push_maybe((pct > 0).then(|| {
            container(text(""))
                .width(Length::FillPortion(pct))
                .height(Length::Fixed(PROGRESS_HEIGHT))
                .style(|_theme: &Theme| container::Style {
                    background: Some(iced::Background::Color(t::accent())),
                    border: Border { radius: t::radius::FULL.into(), ..Default::default() },
                    ..Default::default()
                })
        }))
        .push_maybe((pct < 100).then(|| {
            container(text("")).width(Length::FillPortion(100 - pct)).height(Length::Fixed(PROGRESS_HEIGHT))
        }));
    let bar = container(bar).width(Length::Fill).clip(true).style(|_theme: &Theme| container::Style {
        background: Some(iced::Background::Color(t::border())),
        border: Border { radius: t::radius::FULL.into(), ..Default::default() },
        ..Default::default()
    });
    let progress = column![
        row![
            muted("Progress"),
            iced::widget::horizontal_space(),
            muted(format!("{}/{} · {}%", project.completed, project.total, project.percent)),
        ],
        bar,
    ]
    .spacing(t::space::S1);
    body = body.push(progress);

    let mut facts = Vec::new();
    if let Some(name) = &project.lead {
        facts.push(fact(vec![initials_badge(name), muted(name.clone())]));
    }
    if let Some(due) = &project.due {
        facts.push(fact(vec![muted("◷"), muted(format!("Due {due}"))]));
    }
    if !facts.is_empty() {
        body = body.push(
            iced::widget::Row::with_children(facts)
                .spacing(t::space::S3)
                .align_y(iced::Alignment::Center)
                .wrap(),
        );
    }
    panel(body.into())
}

fn issue_card<'a>(issue: &IssueCard) -> Element<'a, Message> {
    let mut top = row![text(issue.identifier.clone()).size(t::type_scale::XS).color(t::text_secondary())]
        .spacing(t::space::S2)
        .align_y(iced::Alignment::Center);
    if let Some(lane) = &issue.lane {
        top = top
            .push(iced::widget::horizontal_space())
            .push(crate::views::lane_icon::view(lane.icon))
            .push(text(lane.name.clone()).size(t::type_scale::XS).color(t::text_muted()));
    }
    // Groups of facts that wrap onto further lines when the card is narrow.
    let mut facts = vec![fact(vec![
        text("●").size(t::type_scale::XS).color(t::priority_color(issue.priority)).into(),
        muted(issue.priority_label),
    ])];
    if let Some(name) = &issue.assignee {
        facts.push(fact(vec![initials_badge(name), muted(name.clone())]));
    }
    if let Some(name) = &issue.project {
        facts.push(fact(vec![muted("▣"), muted(name.clone())]));
    }
    if let Some(due) = &issue.due {
        facts.push(fact(vec![muted("◷"), muted(due.clone())]));
    }
    let meta = iced::widget::Row::with_children(facts)
        .spacing(t::space::S3)
        .align_y(iced::Alignment::Center)
        .wrap();
    let body = column![
        top,
        text(issue.title.clone()).size(t::type_scale::SM).color(t::text_primary()),
    ]
    .spacing(t::space::S1);
    let body = match &issue.excerpt {
        Some(excerpt) => body.push(
            // About three lines: iced has no line clamp, so cut the height.
            container(
                text(excerpt.clone())
                    .size(t::type_scale::XS)
                    .line_height(t::type_scale::LH_XS)
                    .color(t::text_muted()),
            )
                .max_height(t::type_scale::XS * t::type_scale::LH_XS * EXCERPT_LINES)
                .clip(true),
        ),
        None => body,
    };
    let body = body.push(meta);
    panel(body.into())
}

fn panel<'a>(body: Element<'a, Message>) -> Element<'a, Message> {
    container(body)
        .padding(t::space::S3)
        .width(WIDTH)
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_modal())),
            border: Border { color: t::border(), width: 1.0, radius: t::radius::LG.into() },
            ..Default::default()
        })
        .into()
}

/// One icon-and-label group in the meta row.
fn fact<'a>(parts: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    iced::widget::Row::with_children(parts)
        .spacing(t::space::S1)
        .align_y(iced::Alignment::Center)
        .into()
}

fn muted<'a>(label: impl Into<String>) -> Element<'a, Message> {
    text(label.into()).size(t::type_scale::XS).color(t::text_muted()).into()
}

/// A small circle with the person's initials.
fn initials_badge<'a>(name: &str) -> Element<'a, Message> {
    container(text(crate::views::projects::lead_initials(name)).size(9).color(t::text_primary()))
        .center_x(Length::Fixed(16.0))
        .center_y(Length::Fixed(16.0))
        .style(|_theme: &Theme| container::Style {
            background: Some(iced::Background::Color(t::bg_card())),
            border: Border { color: t::border(), width: 1.0, radius: t::radius::FULL.into() },
            ..Default::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body_hover::LaneInfo;
    use crate::db::models::Priority;
    use crate::lane_icon::LaneIcon;

    fn card() -> IssueCard {
        IssueCard {
            identifier: "A-1".into(),
            title: "t".into(),
            lane: Some(LaneInfo { name: "Done".into(), icon: LaneIcon::Completed }),
            priority: Priority::Low,
            priority_label: "Low",
            assignee: None,
            project: None,
            due: None,
            excerpt: None,
        }
    }

    fn height(card: IssueCard) -> f32 {
        estimated_height(&CardContent::Issue(card))
    }

    #[test]
    fn status_labels_match_the_web() {
        use crate::db::models::ProjectStatus;
        assert_eq!(status_label(Some(ProjectStatus::Started)), "Started");
        assert_eq!(status_label(Some(ProjectStatus::Cancelled)), "Cancelled");
        assert_eq!(status_label(None), "No status");
    }

    #[test]
    fn meta_row_wraps_greedily() {
        let rows = [
            (vec![], 1),
            (vec![100.0], 1),
            (vec![100.0, 100.0], 1),
            (vec![100.0, 100.0, 100.0], 2), // 100+12+100+12+100 > 296
            (vec![200.0, 200.0, 200.0], 3),
        ];
        for (widths, want) in rows {
            assert_eq!(meta_lines(&widths, 296.0, 12.0), want, "{widths:?}");
        }
    }

    #[test]
    fn every_row_adds_height_and_wrapping_rows_add_per_line() {
        let base = height(card());
        let mut with_excerpt = card();
        with_excerpt.excerpt = Some("short".into());
        let one = height(with_excerpt);
        assert!(one > base, "excerpt row counts");
        let mut long = card();
        long.excerpt = Some("word ".repeat(34));
        let three = height(long);
        assert!(three > one, "more excerpt lines count");
        let mut huge = card();
        huge.excerpt = Some("z".repeat(170));
        assert!((height(huge) - three).abs() < 20.0, "excerpt clamps at three lines");

        let mut wide = card();
        wide.assignee = Some("A very long assignee name indeed".into());
        wide.project = Some("An equally long project name here".into());
        wide.due = Some("Dec 25".into());
        assert!(height(wide) > base, "wrapped metadata adds lines");

        let mut titled = card();
        titled.title = "w".repeat(120);
        assert!(height(titled) > base, "long titles wrap");
    }
}
