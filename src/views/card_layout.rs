//! Board card layout (web `issues/_issue_card.html.erb`).
//!
//! A card is an id row (identifier, comment count on the right), a title
//! clamped to two lines (`line-clamp-2`), then a footer of property pills that
//! wraps onto at most two rows (`flex-wrap max-h-[3.5rem]`) with the assignee
//! avatar on the right. The card is as tall as that content, so a one-line
//! title gives a shorter card than a two-line one.
//!
//! Scroll-to-focus needs every card's height without asking the widget tree,
//! and iced 0.13 has no flex-wrap, so the title's line count and each pill's
//! width are measured here with the same font, size and width the card draws
//! with. `issue_card` draws exactly the rows `CardLayout` packs, so the height
//! computed here is the height drawn.

use std::cell::RefCell;
use std::collections::HashMap;

use iced::advanced::graphics::text::Paragraph;
use iced::advanced::text::{Paragraph as _, Text};
use iced::alignment::{Horizontal, Vertical};
use iced::widget::text::{LineHeight, Wrapping};
use iced::Size;

use crate::db::models::{Issue, Label, Member, Priority};
use crate::db::queries::BlockCounts;
use crate::display::{CardProperty, GroupBy};
use crate::scroll;
use crate::theme::{space, type_scale};
use crate::views::board::CardData;
use crate::views::util;

/// Card padding (web `px-3 py-2.5`).
pub const PAD_X: f32 = space::S3;
pub const PAD_Y: f32 = space::S2_5;
/// One `XS` line (the id row and pill text).
pub const XS_LINE: f32 = type_scale::XS * type_scale::LH_XS;
/// One `SM` title line.
pub const TITLE_LINE: f32 = type_scale::SM * type_scale::LH_SM;
/// Lines the title is clamped to (web `line-clamp-2`).
pub const TITLE_MAX_LINES: usize = 2;
/// Gap under the id row (web `mb-2`) and under the title (web `mb-3`).
pub const ID_GAP: f32 = space::S2;
pub const TITLE_GAP: f32 = space::S3;
/// Pill padding (web `px-1.5 py-0.5`): an `XS` line plus 2px above and below.
pub const PILL_PAD_X: f32 = space::S1_5;
pub const PILL_PAD_Y: f32 = space::S0_5;
pub const PILL_H: f32 = XS_LINE + 2.0 * PILL_PAD_Y;
/// Gap between pills, both across and between rows (web `gap-1.5`).
pub const PILL_GAP: f32 = space::S1_5;
/// Rows the footer pills wrap onto before the rest is cut (web `max-h-[3.5rem]`).
pub const FOOTER_MAX_ROWS: usize = 2;
/// Pill icons (web `w-3 h-3`) and the gap after them (web `gap-0.5` for
/// priority, `gap-1` otherwise).
pub const PILL_ICON: f32 = space::S3;
pub const PRIORITY_ICON_GAP: f32 = space::S0_5;
pub const PILL_ICON_GAP: f32 = space::S1;
/// Label colour dot (web `w-1.5 h-1.5`).
pub const LABEL_DOT: f32 = space::S1_5;
/// Labels shown before the "+N" (web `labels.first(2)`).
pub const MAX_LABELS: usize = 2;
/// Assignee avatar (web `user_avatar size: "sm"`) and its gap (web `gap-1`).
pub const AVATAR: f32 = space::S5;
pub const AVATAR_GAP: f32 = space::S1;

/// The id row's block icon (web `w-3 h-3`) and the gap before it (web `gap-1.5`).
pub const BLOCK_ICON_GAP: f32 = space::S1_5;

/// Width the card's content lays out in: the fixed column minus padding.
pub const CONTENT_W: f32 = scroll::BOARD_COLUMN_W - 2.0 * PAD_X;

/// One footer pill, in web order.
pub enum Pill<'a> {
    /// Triangle + web priority number (urgent 4 … low 1).
    Priority(u8),
    Label(&'a Label),
    /// "+N" more labels; plain text, no frame.
    More(usize),
    /// A framed text pill with an optional leading icon (project).
    Fact { icon: Option<&'static str>, text: String },
}

impl Pill<'_> {
    /// The text the pill draws.
    pub fn text(&self) -> String {
        match self {
            Pill::Priority(n) => n.to_string(),
            Pill::Label(l) => l.name.clone(),
            Pill::More(n) => format!("+{n}"),
            Pill::Fact { text, .. } => text.clone(),
        }
    }

    /// Laid-out width for text `text_w` wide: frame padding plus any icon/dot.
    fn width(&self, text_w: f32) -> f32 {
        let leading = match self {
            Pill::Priority(_) => PILL_ICON + PRIORITY_ICON_GAP,
            Pill::Label(_) => LABEL_DOT + PILL_ICON_GAP,
            Pill::More(_) => return text_w,
            Pill::Fact { icon: Some(_), .. } => PILL_ICON + PILL_ICON_GAP,
            Pill::Fact { icon: None, .. } => 0.0,
        };
        (2.0 * PILL_PAD_X + leading + text_w).ceil()
    }
}

/// A pill with the width it is drawn at.
pub struct Placed<'a> {
    pub pill: Pill<'a>,
    pub width: f32,
}

/// Everything `issue_card` draws, already measured.
pub struct CardLayout<'a> {
    pub show_id: bool,
    pub comments: usize,
    /// `blocks` edges on each side; drawn as an icon on the id row.
    pub blocks: BlockCounts,
    pub title_lines: usize,
    /// Footer rows (at most `FOOTER_MAX_ROWS`); empty means the web's "···"
    /// placeholder.
    pub rows: Vec<Vec<Placed<'a>>>,
    pub assignee: Option<&'a Member>,
}

impl<'a> CardLayout<'a> {
    pub fn new(issue: &Issue, data: &CardData<'a>) -> Self {
        let d = data.display;
        let assignee = if d.shows(CardProperty::Assignee) {
            issue
                .assignee_id
                .as_ref()
                .and_then(|id| data.members.iter().find(|m| m.id == *id))
        } else {
            None
        };
        let avail = if assignee.is_some() {
            CONTENT_W - AVATAR - AVATAR_GAP
        } else {
            CONTENT_W
        };
        let placed = pills(issue, data)
            .into_iter()
            .map(|pill| {
                let width = pill.width(text_width(&pill.text())).min(avail);
                Placed { pill, width }
            })
            .collect();
        CardLayout {
            show_id: d.shows(CardProperty::Id),
            comments: data.comment_counts.get(&issue.id).copied().unwrap_or(0),
            blocks: data.block_counts.get(&issue.id).copied().unwrap_or_default(),
            title_lines: title_lines(&issue.title),
            rows: pack(placed, avail),
            assignee,
        }
    }

    /// Height of the footer: its pill rows, or one row for the avatar / "···".
    pub fn footer_h(&self) -> f32 {
        footer_height(self.rows.len())
    }

    /// The card's full height (border-box: padding plus content).
    pub fn height(&self) -> f32 {
        card_height(self.show_id, self.title_lines, self.rows.len())
    }
}

/// Footer height for `rows` pill rows; an empty footer still holds one row
/// (the avatar or the "···" placeholder).
pub fn footer_height(rows: usize) -> f32 {
    let rows = rows.max(1) as f32;
    rows * PILL_H + (rows - 1.0) * PILL_GAP
}

/// Card height from its measured parts.
pub fn card_height(show_id: bool, title_lines: usize, footer_rows: usize) -> f32 {
    let mut h = 2.0 * PAD_Y;
    if show_id {
        h += XS_LINE + ID_GAP;
    }
    h += title_lines as f32 * TITLE_LINE + TITLE_GAP;
    h + footer_height(footer_rows)
}

/// The footer pills in web order: priority, labels (first two, then "+N"),
/// due date, estimate, project, status (unless grouped by it), created,
/// updated.
fn pills<'a>(issue: &Issue, data: &CardData<'a>) -> Vec<Pill<'a>> {
    let d = data.display;
    let mut out = Vec::new();
    if d.shows(CardProperty::Priority) {
        let n = match issue.priority_enum() {
            Priority::Urgent => Some(4),
            Priority::High => Some(3),
            Priority::Medium => Some(2),
            Priority::Low => Some(1),
            Priority::None => None,
        };
        out.extend(n.map(Pill::Priority));
    }
    if d.shows(CardProperty::Labels) {
        if let Some(labels) = data.issue_labels.get(&issue.id) {
            out.extend(labels.iter().take(MAX_LABELS).map(Pill::Label));
            if labels.len() > MAX_LABELS {
                out.push(Pill::More(labels.len() - MAX_LABELS));
            }
        }
    }
    let fact = |text: String| Pill::Fact { icon: None, text };
    if d.shows(CardProperty::DueDate) {
        if let Some(due) = issue.due_date.as_deref() {
            out.push(fact(util::short_date(due)));
        }
    }
    if d.shows(CardProperty::Estimate) {
        if let Some(e) = issue.estimate {
            out.push(fact(util::format_estimate(e)));
        }
    }
    if d.shows(CardProperty::Project) {
        if let Some(p) = issue
            .project_id
            .as_ref()
            .and_then(|id| data.projects.iter().find(|p| p.id == *id))
        {
            out.push(Pill::Fact { icon: Some(ICON_PROJECT), text: p.name.clone() });
        }
    }
    if d.shows(CardProperty::Status) && d.group_by != GroupBy::Status {
        if let Some(l) = data.lanes.iter().find(|l| l.id == issue.lane_id) {
            out.push(fact(l.name.clone()));
        }
    }
    if d.shows(CardProperty::Created) {
        out.push(fact(util::short_date(&issue.created_at)));
    }
    if d.shows(CardProperty::Updated) {
        out.push(fact(util::short_date(&issue.updated_at)));
    }
    out
}

/// Greedy flex-wrap: fill each row left to right within `avail`, start a new
/// row when the next pill doesn't fit, and drop what falls past the last row.
fn pack(pills: Vec<Placed<'_>>, avail: f32) -> Vec<Vec<Placed<'_>>> {
    let mut rows: Vec<Vec<Placed<'_>>> = Vec::new();
    let mut used = 0.0;
    for p in pills {
        let fits = rows.last().is_some_and(|_| used + PILL_GAP + p.width <= avail);
        if fits {
            used += PILL_GAP + p.width;
            rows.last_mut().expect("checked").push(p);
        } else {
            if rows.len() == FOOTER_MAX_ROWS {
                break;
            }
            used = p.width;
            rows.push(vec![p]);
        }
    }
    rows
}

pub const ICON_PROJECT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" fill="none" stroke="#000" viewBox="0 0 24 24"><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M20 7l-8-4-8 4m16 0l-8 4m8-4v10l-8 4m0-10L4 7m8 4v10M4 7v10l8 4"/></svg>"##;

thread_local! {
    /// Measured pill text widths and title line counts. The font and column
    /// width are fixed for the app's life, so the text alone is the key.
    static PILL_WIDTHS: RefCell<HashMap<String, f32>> = RefCell::new(HashMap::new());
    static TITLE_LINES: RefCell<HashMap<String, usize>> = RefCell::new(HashMap::new());
}

/// Entries a cache holds before it is cleared (titles change as issues do).
const CACHE_CAP: usize = 50_000;

fn cached<V: Copy>(
    cache: &'static std::thread::LocalKey<RefCell<HashMap<String, V>>>,
    key: &str,
    compute: impl FnOnce() -> V,
) -> V {
    cache.with(|c| {
        if let Some(v) = c.borrow().get(key) {
            return *v;
        }
        let v = compute();
        let mut c = c.borrow_mut();
        if c.len() >= CACHE_CAP {
            c.clear();
        }
        c.insert(key.to_string(), v);
        v
    })
}

/// Width of `content` as one `XS` line.
fn text_width(content: &str) -> f32 {
    cached(&PILL_WIDTHS, content, || {
        measure(content, type_scale::XS, type_scale::LH_XS, f32::INFINITY, Wrapping::None).width
    })
}

/// Lines `title` wraps to across the card (1 or 2; the title is clamped).
fn title_lines(title: &str) -> usize {
    cached(&TITLE_LINES, title, || {
        let h = measure(title, type_scale::SM, type_scale::LH_SM, CONTENT_W, Wrapping::WordOrGlyph)
            .height;
        ((h / TITLE_LINE).round() as usize).clamp(1, TITLE_MAX_LINES)
    })
}

/// Lay `content` out the way `views::text` draws it, in the app font.
fn measure(content: &str, size: f32, line_height: f32, width: f32, wrapping: Wrapping) -> Size {
    Paragraph::with_text(Text {
        content,
        bounds: Size::new(width, f32::INFINITY),
        size: size.into(),
        line_height: LineHeight::Relative(line_height),
        font: crate::fonts::active(),
        horizontal_alignment: Horizontal::Left,
        vertical_alignment: Vertical::Top,
        shaping: crate::views::text::shaping_for(content),
        wrapping,
    })
    .min_bounds()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(w: f32) -> Placed<'static> {
        Placed { pill: Pill::Fact { icon: None, text: String::new() }, width: w }
    }

    fn widths(rows: &[Vec<Placed<'_>>]) -> Vec<Vec<f32>> {
        rows.iter().map(|r| r.iter().map(|p| p.width).collect()).collect()
    }

    #[test]
    fn pills_wrap_onto_a_new_row_when_the_next_one_does_not_fit() {
        // 40 + 6 + 40 = 86 fits in 100; another 6 + 40 does not.
        let rows = pack(vec![fact(40.0), fact(40.0), fact(40.0)], 100.0);
        assert_eq!(widths(&rows), vec![vec![40.0, 40.0], vec![40.0]]);
    }

    #[test]
    fn pills_past_the_second_row_are_dropped() {
        let rows = pack(vec![fact(90.0), fact(90.0), fact(90.0)], 100.0);
        assert_eq!(widths(&rows), vec![vec![90.0], vec![90.0]]);
    }

    #[test]
    fn a_pill_that_exactly_fills_the_row_stays_on_it() {
        let rows = pack(vec![fact(47.0), fact(47.0)], 100.0);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn no_pills_means_no_rows() {
        assert!(pack(Vec::new(), 100.0).is_empty());
    }

    #[test]
    fn card_height_follows_the_web_card_spacing() {
        // padding 2 * 10; id line 16 + 8; title lines * 20 + 12; footer rows of
        // 20 with 6 between (an empty footer still holds one row).
        assert_eq!(card_height(false, 1, 0), 20.0 + 20.0 + 12.0 + 20.0);
        assert_eq!(card_height(true, 1, 1), 20.0 + 24.0 + 20.0 + 12.0 + 20.0);
        assert_eq!(card_height(true, 2, 1), 20.0 + 24.0 + 40.0 + 12.0 + 20.0);
        assert_eq!(card_height(true, 2, 2), 20.0 + 24.0 + 40.0 + 12.0 + 46.0);
    }

    #[test]
    fn pill_width_adds_padding_and_the_leading_mark() {
        assert_eq!(Pill::Fact { icon: None, text: String::new() }.width(30.0), 42.0);
        assert_eq!(Pill::Priority(4).width(8.0), 12.0 + 12.0 + 2.0 + 8.0);
        assert_eq!(Pill::More(3).width(14.5), 14.5);
    }
}
