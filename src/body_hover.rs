//! Hover cards over references in rendered bodies (JRST-299).
//!
//! Glue between three things: the `hover_rich` widget (reports which span the
//! pointer is on), the `hover_card` state machine (show and hide timing) and
//! the card's content. Pure, so the traps are table-tested.
//!
//! Vocabulary: an [`Anchor`] is the thing that has a card. Its equality is
//! what the state machine uses for "same card", so it carries identity only.
//! Where the card goes is separate (`BodyHover::current`'s rectangle).
//!
//! To add a card field: add it to [`IssueCard`], fill it in [`content_for`],
//! draw it in `views/hover_card_view.rs`. To add an anchor source: a new
//! [`Anchor`] variant, one arm in [`Anchor::from_link`] (the span's
//! `LinkTarget`) and one in [`content_for`]; the widget and machine need no
//! change. A source outside body text (a row, say) needs its own owner id and
//! window rectangle, then calls [`BodyHover::body_hover`] the same way.

use iced::{Point, Rectangle};

use crate::db::models::{Issue, Lane, Member, Priority, Project, Team};
use crate::hover_card::{Effect, Event, HoverCard};
use crate::lane_icon::{icon_for, LaneIcon};
use crate::mentions::LinkTarget;

/// What a hover card is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    /// Local issue UUID.
    Issue(String),
    /// Mention text including the `@`. No card yet (JRST-310).
    Mention(String),
}

impl Anchor {
    /// The anchor for a span's link, if that kind of link has a card.
    pub fn from_link(link: &LinkTarget) -> Option<Anchor> {
        match link {
            LinkTarget::Issue(id) => Some(Anchor::Issue(id.clone())),
            LinkTarget::Mention(text) => Some(Anchor::Mention(text.clone())),
            LinkTarget::External(_) => None,
        }
    }
}

/// Card state plus the bookkeeping that body widgets need.
#[derive(Debug, Default)]
pub struct BodyHover {
    card: HoverCard<Anchor>,
    /// The widget whose span the current anchor came from.
    owner: Option<u64>,
    /// Where the current anchor sits, in window pixels.
    rect: Option<Rectangle>,
}

impl BodyHover {
    pub fn new() -> Self {
        Self::default()
    }

    /// The card to draw and the window rectangle of the span it hangs off.
    pub fn current(&self) -> Option<(&Anchor, Rectangle)> {
        self.card.current().zip(self.rect)
    }

    /// A body widget reported a hover change. `span` is the hovered span's
    /// anchor and window rectangle; `None` = the pointer left the widget.
    ///
    /// Each paragraph is its own widget, so moving between two can deliver
    /// `Some` from the new one before `None` from the old one. A `None` only
    /// counts when it comes from the widget that owns the current anchor.
    /// The same holds for a `Some` without an anchor (plain text): it ends
    /// the hover only if its widget owns the anchor.
    pub fn body_hover(&mut self, owner: u64, span: Option<(Option<Anchor>, Rectangle)>) -> Option<Effect> {
        match span {
            Some((Some(anchor), rect)) => {
                self.owner = Some(owner);
                self.rect = Some(rect);
                self.card.update(Event::EnteredAnchor(anchor))
            }
            _ if self.owner == Some(owner) => {
                self.owner = None;
                self.card.update(Event::LeftAnchor)
            }
            _ => None,
        }
    }

    /// True while a card is showing (including during its hide delay).
    pub fn is_open(&self) -> bool {
        self.card.current().is_some()
    }

    /// Any other state-machine event (timer, click, scroll, escape...).
    pub fn event(&mut self, event: Event<Anchor>) -> Option<Effect> {
        let effect = self.card.update(event);
        if self.card.current().is_none() {
            self.rect = None;
        }
        effect
    }
}

/// What the card shows. One variant per anchor kind that has a card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardContent {
    Issue(IssueCard),
}

/// The fields of an issue card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueCard {
    /// `IDENT-n`, or `#n` when the team isn't loaded.
    pub identifier: String,
    pub title: String,
    /// The issue's lane; `None` when the lane isn't in the records.
    pub lane: Option<LaneInfo>,
    pub priority: Priority,
    /// `Urgent`..`Low`, or `No priority`; always present, like the web's.
    pub priority_label: &'static str,
    /// The assignee's name; `None` when unassigned or not in the records.
    pub assignee: Option<String>,
    /// The project's name; `None` when there is none or it isn't in the records.
    pub project: Option<String>,
    /// `Jul 6`, as the web's `%b %-d`; `None` when unset or unparseable.
    pub due: Option<String>,
    /// Plain-text start of the description; `None` when empty.
    pub excerpt: Option<String>,
}

/// Longest excerpt, ellipsis included (the web's `truncate(.., length: 170)`).
pub const EXCERPT_CHARS: usize = 170;

/// The description as a plain-text excerpt of at most [`EXCERPT_CHARS`]
/// characters, ending in `...` when cut. `None` for an empty description.
pub fn excerpt(description: Option<&str>) -> Option<String> {
    let text = crate::document::parse(description?).plain_text();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= EXCERPT_CHARS {
        return Some(text);
    }
    let cut: String = text.chars().take(EXCERPT_CHARS - 3).collect();
    Some(format!("{cut}..."))
}

/// `YYYY-MM-DD` (or an RFC 3339 timestamp) as `Jul 6`.
pub fn due_label(raw: &str) -> Option<String> {
    let day = raw.get(..10)?;
    let date = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
    Some(date.format("%b %-d").to_string())
}

/// A lane as the card shows it: icon beside the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneInfo {
    pub name: String,
    pub icon: LaneIcon,
}

/// Owned records for one card, read from the local database when it opens.
#[derive(Debug, Clone, Default)]
pub struct OwnedRecords {
    pub issues: Vec<Issue>,
    pub teams: Vec<Team>,
    pub lanes: Vec<Lane>,
    pub members: Vec<Member>,
    pub projects: Vec<Project>,
}

impl OwnedRecords {
    pub fn records(&self) -> Records<'_> {
        Records {
            issues: &self.issues,
            teams: &self.teams,
            lanes: &self.lanes,
            members: &self.members,
            projects: &self.projects,
        }
    }
}

/// The local records a card is built from. Slices, so the caller decides
/// where they come from.
#[derive(Debug, Clone, Copy, Default)]
pub struct Records<'a> {
    pub issues: &'a [Issue],
    pub teams: &'a [Team],
    pub lanes: &'a [Lane],
    pub members: &'a [Member],
    pub projects: &'a [Project],
}

/// Card content for an anchor from local data; `None` draws no card.
pub fn content_for(anchor: &Anchor, records: &Records) -> Option<CardContent> {
    let Records { issues, teams, lanes, members, projects } = *records;
    match anchor {
        Anchor::Issue(id) => {
            let issue = issues.iter().find(|i| i.id == *id)?;
            let lane = lanes
                .iter()
                .find(|l| l.id == issue.lane_id)
                .map(|l| LaneInfo { name: l.name.clone(), icon: icon_for(l.category.as_deref()) });
            let number = issue.team_number.unwrap_or(0);
            let identifier = match teams.iter().find(|t| t.id == issue.team_id) {
                Some(team) => format!("{}-{}", team.identifier, number),
                None => format!("#{number}"),
            };
            let priority = issue.priority_enum();
            let assignee = issue
                .assignee_id
                .as_ref()
                .and_then(|id| members.iter().find(|m| m.id == *id))
                .map(|m| m.name.clone());
            let project = issue
                .project_id
                .as_ref()
                .and_then(|id| projects.iter().find(|p| p.id == *id))
                .map(|p| p.name.clone());
            let due = issue.due_date.as_deref().and_then(due_label);
            Some(CardContent::Issue(IssueCard {
                identifier,
                title: issue.title.clone(),
                lane,
                priority,
                priority_label: crate::display::PRIORITY_TITLES[priority.to_i32() as usize],
                assignee,
                project,
                due,
                excerpt: excerpt(issue.description.as_deref()),
            }))
        }
        Anchor::Mention(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hover_card::SHOW_DELAY;
    use iced::Size;

    fn rect() -> Rectangle {
        Rectangle::new(Point::new(10.0, 20.0), Size::new(30.0, 14.0))
    }

    fn on(id: &str) -> Option<(Option<Anchor>, Rectangle)> {
        Some((Some(Anchor::Issue(id.into())), rect()))
    }

    fn plain() -> Option<(Option<Anchor>, Rectangle)> {
        Some((None, rect()))
    }

    fn open(h: &mut BodyHover, owner: u64, id: &str) {
        let e = h.body_hover(owner, on(id)).unwrap();
        h.event(Event::TimerFired(e.generation));
        assert!(h.current().is_some());
    }

    fn shown(h: &BodyHover) -> Option<Anchor> {
        h.current().map(|(a, _)| a.clone())
    }

    fn issue_anchor(id: &str) -> Option<Anchor> {
        Some(Anchor::Issue(id.into()))
    }

    #[test]
    fn link_kinds_map_to_anchors() {
        let rows = [
            (LinkTarget::Issue("i".into()), Some(Anchor::Issue("i".into()))),
            (LinkTarget::Mention("@ann".into()), Some(Anchor::Mention("@ann".into()))),
            (LinkTarget::External("https://x.y".into()), None),
        ];
        for (link, want) in rows {
            assert_eq!(Anchor::from_link(&link), want);
        }
    }

    #[test]
    fn shows_after_delay_and_hides_when_owner_leaves() {
        let mut h = BodyHover::new();
        let e = h.body_hover(1, on("a")).unwrap();
        assert_eq!(e.delay, SHOW_DELAY);
        assert_eq!(shown(&h), None);
        h.event(Event::TimerFired(e.generation));
        assert_eq!(shown(&h), issue_anchor("a"));
        let e = h.body_hover(1, None).unwrap();
        assert_eq!(shown(&h), issue_anchor("a"), "visible during hide delay");
        h.event(Event::TimerFired(e.generation));
        assert_eq!(shown(&h), None);
    }

    #[test]
    fn late_none_from_previous_paragraph_does_not_cancel_the_new_anchor() {
        let mut h = BodyHover::new();
        open(&mut h, 1, "a");
        // Pointer moves paragraph 1 -> paragraph 2: Some(new) then None(old).
        let e = h.body_hover(2, on("b")).unwrap();
        assert_eq!(h.body_hover(1, None), None);
        h.event(Event::TimerFired(e.generation));
        assert_eq!(shown(&h), issue_anchor("b"));
    }

    #[test]
    fn none_before_some_also_works() {
        let mut h = BodyHover::new();
        open(&mut h, 1, "a");
        h.body_hover(1, None);
        let e = h.body_hover(2, on("b")).unwrap();
        h.event(Event::TimerFired(e.generation));
        assert_eq!(shown(&h), issue_anchor("b"));
    }

    #[test]
    fn plain_span_in_another_widget_does_not_clear_the_card() {
        let mut h = BodyHover::new();
        open(&mut h, 1, "a");
        assert_eq!(h.body_hover(2, plain()), None);
        assert_eq!(shown(&h), issue_anchor("a"));
    }

    #[test]
    fn plain_span_in_the_owning_widget_leaves() {
        let mut h = BodyHover::new();
        open(&mut h, 1, "a");
        assert!(h.body_hover(1, plain()).is_some());
    }

    #[test]
    fn next_span_with_same_issue_keeps_the_card() {
        let mut h = BodyHover::new();
        open(&mut h, 1, "a");
        h.body_hover(1, on("a"));
        assert_eq!(shown(&h), issue_anchor("a"));
    }

    #[test]
    fn card_enter_and_leave_keep_and_then_hide() {
        let mut h = BodyHover::new();
        open(&mut h, 1, "a");
        h.body_hover(1, None);
        h.event(Event::EnteredCard);
        assert_eq!(shown(&h), issue_anchor("a"));
        let e = h.event(Event::LeftCard).unwrap();
        assert_eq!(shown(&h), issue_anchor("a"), "visible during hide delay");
        // Back on the anchor before the timer: stays open, old timer is stale.
        h.body_hover(1, on("a"));
        h.event(Event::TimerFired(e.generation));
        assert_eq!(shown(&h), issue_anchor("a"));
        h.body_hover(1, None);
        h.event(Event::EnteredCard);
        let e = h.event(Event::LeftCard).unwrap();
        h.event(Event::TimerFired(e.generation));
        assert_eq!(shown(&h), None);
        assert_eq!(h.current(), None);
    }

    #[test]
    fn click_scroll_escape_and_view_change_close_open_and_pending() {
        for ev in [Event::Clicked, Event::Scrolled, Event::Escape, Event::ViewChanged] {
            let mut h = BodyHover::new();
            open(&mut h, 1, "a");
            h.event(ev.clone());
            assert_eq!(shown(&h), None, "{ev:?} closes an open card");
            assert!(!h.is_open());

            let mut h = BodyHover::new();
            let pending = h.body_hover(1, on("a")).unwrap();
            h.event(ev.clone());
            h.event(Event::TimerFired(pending.generation));
            assert_eq!(shown(&h), None, "{ev:?} cancels a pending card");
        }
    }

    fn issue(id: &str, team: &str, n: Option<i32>, title: &str) -> Issue {
        Issue {
            id: id.into(),
            title: title.into(),
            description: None,
            priority: 0,
            team_number: n,
            due_date: None,
            estimate: None,
            started_at: None,
            completed_at: None,
            canceled_at: None,
            lane_id: "l".into(),
            team_id: team.into(),
            project_id: None,
            parent_issue_id: None,
            assignee_id: None,
            creator_id: None,
            archived_at: None,
            remote_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn issue_card_content_from_local_data() {
        let team = Team {
            id: "t".into(),
            name: "Proj".into(),
            identifier: "PROJ".into(),
            remote_id: None,
            archived_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        let mut issues = [issue("a", "t", Some(7), "Fix it"), issue("b", "gone", Some(3), "Orphan")];
        for i in &mut issues {
            i.priority = 0;
        }
        let teams = [team];
        let records = Records { issues: &issues, teams: &teams, ..Default::default() };
        let rows = [
            (Anchor::Issue("a".into()), Some(("PROJ-7", "Fix it"))),
            (Anchor::Issue("b".into()), Some(("#3", "Orphan"))),
            (Anchor::Issue("missing".into()), None),
            (Anchor::Mention("@ann".into()), None),
        ];
        for (anchor, want) in rows {
            let got = content_for(&anchor, &records);
            let want = want.map(|(i, t)| {
                CardContent::Issue(IssueCard {
                    identifier: i.into(),
                    title: t.into(),
                    lane: None,
                    priority: Priority::Urgent,
                    priority_label: "Urgent",
                    assignee: None,
                    project: None,
                    due: None,
                    excerpt: None,
                })
            });
            assert_eq!(got, want, "{anchor:?}");
        }
    }

    fn lane(id: &str, name: &str, category: Option<&str>) -> Lane {
        Lane {
            id: id.into(),
            name: name.into(),
            color: None,
            position: 0,
            team_id: "t".into(),
            remote_id: None,
            category: category.map(String::from),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn card_carries_the_lane_name_and_icon() {
        let issues = [issue("a", "t", Some(1), "x")];
        let rows = [
            (vec![lane("l", "In Progress", Some("started"))], Some(("In Progress", LaneIcon::Started))),
            (vec![lane("l", "Odd", None)], Some(("Odd", LaneIcon::Plain))),
            (vec![lane("other", "Done", Some("completed"))], None),
            (vec![], None),
        ];
        for (lanes, want) in rows {
            let records = Records { issues: &issues, lanes: &lanes, ..Default::default() };
            let Some(CardContent::Issue(card)) = content_for(&Anchor::Issue("a".into()), &records) else {
                panic!("card expected");
            };
            let want = want.map(|(name, icon)| LaneInfo { name: name.into(), icon });
            assert_eq!(card.lane, want);
        }
    }

    fn member(id: &str, name: &str) -> Member {
        Member {
            id: id.into(),
            remote_id: None,
            team_id: "t".into(),
            name: name.into(),
            email: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn card_carries_priority_and_assignee() {
        let members = [member("m", "Ann Lee")];
        let rows = [
            (0, Some("m"), "Urgent", Some("Ann Lee")),
            (3, None, "Low", None),
            (4, Some("gone"), "No priority", None),
            (99, Some("m"), "No priority", Some("Ann Lee")),
        ];
        for (priority, assignee, label, name) in rows {
            let mut i = issue("a", "t", Some(1), "x");
            i.priority = priority;
            i.assignee_id = assignee.map(String::from);
            let issues = [i];
            let records = Records { issues: &issues, members: &members, ..Default::default() };
            let Some(CardContent::Issue(card)) = content_for(&Anchor::Issue("a".into()), &records) else {
                panic!("card expected");
            };
            assert_eq!(card.priority_label, label);
            assert_eq!(card.assignee.as_deref(), name);
        }
    }

    fn project(id: &str, name: &str) -> Project {
        Project {
            id: id.into(),
            remote_id: None,
            team_id: "t".into(),
            name: name.into(),
            description: None,
            status: None,
            priority: 0,
            lead_id: None,
            start_date: None,
            due_date: None,
            roadmap_commitment: None,
            velocity_score: 0,
            completed_issues_count: 0,
            total_issues_count: 0,
            progress_percentage: 0,
            behind_schedule: false,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn card_carries_project_and_due_date() {
        let projects = [project("p", "Launch")];
        let rows = [
            (Some("p"), Some("2026-07-06"), Some("Launch"), Some("Jul 6")),
            (Some("p"), Some("2026-12-25T10:00:00Z"), Some("Launch"), Some("Dec 25")),
            (Some("gone"), Some("not a date"), None, None),
            (None, None, None, None),
            (Some("p"), Some("2026-13-45"), Some("Launch"), None),
        ];
        for (project_id, due, project, label) in rows {
            let mut i = issue("a", "t", Some(1), "x");
            i.project_id = project_id.map(String::from);
            i.due_date = due.map(String::from);
            let issues = [i];
            let records = Records { issues: &issues, projects: &projects, ..Default::default() };
            let Some(CardContent::Issue(card)) = content_for(&Anchor::Issue("a".into()), &records) else {
                panic!("card expected");
            };
            assert_eq!(card.project.as_deref(), project, "{project_id:?}");
            assert_eq!(card.due.as_deref(), label, "{due:?}");
        }
    }

    #[test]
    fn excerpt_is_plain_and_capped() {
        let exact = "x".repeat(170);
        let long = "y".repeat(171);
        let multibyte = "é".repeat(200);
        let rows: [(Option<&str>, Option<String>); 7] = [
            (None, None),
            (Some(""), None),
            (Some("  \n "), None),
            (Some("# Hi\n\nsome **bold** text"), Some("Hi some bold text".into())),
            (Some(&exact), Some(exact.clone())),
            (Some(&long), Some(format!("{}...", "y".repeat(167)))),
            (Some(&multibyte), Some(format!("{}...", "é".repeat(167)))),
        ];
        for (input, want) in rows {
            assert_eq!(excerpt(input), want, "{input:?}");
        }
        assert_eq!(excerpt(Some("![pic](https://x.y/a.png)")), None, "image-only leaves nothing");
    }

    #[test]
    fn card_carries_the_excerpt() {
        let mut i = issue("a", "t", Some(1), "x");
        i.description = Some("- `code` item".into());
        let issues = [i];
        let records = Records { issues: &issues, ..Default::default() };
        let Some(CardContent::Issue(card)) = content_for(&Anchor::Issue("a".into()), &records) else {
            panic!("card expected");
        };
        assert_eq!(card.excerpt.as_deref(), Some("code item"));
    }
}
