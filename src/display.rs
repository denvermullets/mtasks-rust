//! Board display options (grouping, ordering, visible card properties) and
//! issue filters, plus the pure logic that turns a flat issue list into board
//! columns. Persisted in `Config` (see `config.rs`); the UI lives in
//! `views/display_panel.rs` and `views/board.rs`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::db::models::{Issue, Label, Lane, Member, Project, Team};
use crate::lane_icon::{icon_for, LaneIcon};

/// Key used for the "none" bucket of an axis (unassigned, no project, ...).
const NONE_KEY: &str = "";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    Status,
    Priority,
    Assignee,
    Project,
    Label,
    Parent,
    None,
}

impl GroupBy {
    pub const ALL: [(GroupBy, &'static str); 7] = [
        (GroupBy::Status, "Status"),
        (GroupBy::Priority, "Priority"),
        (GroupBy::Assignee, "Assignee"),
        (GroupBy::Project, "Project"),
        (GroupBy::Label, "Label"),
        (GroupBy::Parent, "Parent"),
        (GroupBy::None, "None"),
    ];

    fn axis(self) -> Option<Axis> {
        match self {
            GroupBy::Status => Some(Axis::Status),
            GroupBy::Priority => Some(Axis::Priority),
            GroupBy::Assignee => Some(Axis::Assignee),
            GroupBy::Project => Some(Axis::Project),
            GroupBy::Label => Some(Axis::Label),
            GroupBy::Parent => Some(Axis::Parent),
            GroupBy::None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubGroup {
    None,
    Priority,
    Assignee,
    Project,
    Status,
}

impl SubGroup {
    pub const ALL: [(SubGroup, &'static str); 5] = [
        (SubGroup::None, "None"),
        (SubGroup::Priority, "Priority"),
        (SubGroup::Assignee, "Assignee"),
        (SubGroup::Project, "Project"),
        (SubGroup::Status, "Status"),
    ];

    fn axis(self) -> Option<Axis> {
        match self {
            SubGroup::None => None,
            SubGroup::Priority => Some(Axis::Priority),
            SubGroup::Assignee => Some(Axis::Assignee),
            SubGroup::Project => Some(Axis::Project),
            SubGroup::Status => Some(Axis::Status),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderBy {
    /// Source order from the DB (priority, then creation time).
    Manual,
    Priority,
    DueDate,
    Created,
    Updated,
}

impl OrderBy {
    pub const ALL: [(OrderBy, &'static str); 5] = [
        (OrderBy::Manual, "Manual"),
        (OrderBy::Priority, "Priority"),
        (OrderBy::DueDate, "Due date"),
        (OrderBy::Created, "Created"),
        (OrderBy::Updated, "Updated"),
    ];
}

/// What to show of completed/canceled issues: Rails' "Completed issues"
/// dropdown. `Hide` hides all of them; the windows keep those completed or
/// canceled recently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompletedFilter {
    Hide,
    PastDay,
    PastWeek,
    PastMonth,
    #[default]
    All,
}

impl CompletedFilter {
    pub const ALL: [(CompletedFilter, &'static str); 5] = [
        (CompletedFilter::Hide, "Hide"),
        (CompletedFilter::PastDay, "Past day"),
        (CompletedFilter::PastWeek, "Past week"),
        (CompletedFilter::PastMonth, "Past month"),
        (CompletedFilter::All, "All"),
    ];

    fn window(self) -> Option<chrono::Duration> {
        match self {
            CompletedFilter::PastDay => Some(chrono::Duration::days(1)),
            CompletedFilter::PastWeek => Some(chrono::Duration::weeks(1)),
            CompletedFilter::PastMonth => Some(chrono::Duration::days(30)),
            _ => None,
        }
    }
}

/// Window within which a completed issue still shows while `show_completed`
/// is off (kept separate so older configs with only `show_completed` load).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletedWithin {
    #[default]
    Nothing,
    Day,
    Week,
    Month,
}

/// Optional pieces of a board card (title is always shown).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardProperty {
    Id,
    Priority,
    Labels,
    Assignee,
    Project,
    DueDate,
    Status,
    Estimate,
    Created,
    Updated,
}

impl CardProperty {
    pub const ALL: [(CardProperty, &'static str); 10] = [
        (CardProperty::Id, "ID"),
        (CardProperty::Priority, "Priority"),
        (CardProperty::Labels, "Labels"),
        (CardProperty::Assignee, "Assignee"),
        (CardProperty::Project, "Project"),
        (CardProperty::DueDate, "Due date"),
        (CardProperty::Status, "Status"),
        (CardProperty::Estimate, "Estimate"),
        (CardProperty::Created, "Created"),
        (CardProperty::Updated, "Updated"),
    ];
}

/// How issues are laid out: kanban columns or a vertical list of rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    #[default]
    Board,
    List,
}

impl ViewMode {
    pub const ALL: [(ViewMode, &'static str); 2] =
        [(ViewMode::Board, "Board"), (ViewMode::List, "List")];
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayOptions {
    /// Board (columns) or List (rows under section headers).
    #[serde(default)]
    pub view_mode: ViewMode,
    #[serde(default = "default_group_by")]
    pub group_by: GroupBy,
    #[serde(default = "default_sub_group")]
    pub sub_group: SubGroup,
    #[serde(default = "default_order_by")]
    pub order_by: OrderBy,
    /// Whether completed/canceled issues are shown.
    #[serde(default = "default_true")]
    pub show_completed: bool,
    /// When `show_completed` is off, completed issues newer than this still show.
    #[serde(default)]
    pub completed_within: CompletedWithin,
    /// Whether issues with a parent are listed (off = top-level issues only).
    #[serde(default = "default_true")]
    pub show_sub_issues: bool,
    /// Whether every bucket is shown even when empty. Status and priority
    /// grouping always keep their empty buckets (so lanes stay visible);
    /// this extends that to the other group-by axes.
    #[serde(default)]
    pub show_empty_groups: bool,
    #[serde(default = "default_properties")]
    pub properties: Vec<CardProperty>,
}

fn default_group_by() -> GroupBy {
    GroupBy::Status
}
fn default_sub_group() -> SubGroup {
    SubGroup::None
}
fn default_order_by() -> OrderBy {
    OrderBy::Manual
}
fn default_true() -> bool {
    true
}
fn default_properties() -> Vec<CardProperty> {
    vec![
        CardProperty::Id,
        CardProperty::Priority,
        CardProperty::Labels,
    ]
}

impl Default for DisplayOptions {
    fn default() -> Self {
        Self {
            view_mode: ViewMode::Board,
            group_by: default_group_by(),
            sub_group: default_sub_group(),
            order_by: default_order_by(),
            show_completed: true,
            completed_within: CompletedWithin::Nothing,
            show_sub_issues: true,
            show_empty_groups: false,
            properties: default_properties(),
        }
    }
}

impl DisplayOptions {
    /// What dashboard rows always show: identifier, priority, status, project,
    /// assignee, labels and due date. Independent of the board's own settings.
    pub fn dashboard_rows() -> &'static DisplayOptions {
        static ROWS: std::sync::OnceLock<DisplayOptions> = std::sync::OnceLock::new();
        ROWS.get_or_init(|| DisplayOptions {
            properties: vec![
                CardProperty::Id,
                CardProperty::Priority,
                CardProperty::Status,
                CardProperty::Project,
                CardProperty::Assignee,
                CardProperty::Labels,
                CardProperty::DueDate,
            ],
            ..DisplayOptions::default()
        })
    }

    pub fn shows(&self, p: CardProperty) -> bool {
        self.properties.contains(&p)
    }

    pub fn completed_filter(&self) -> CompletedFilter {
        if self.show_completed {
            return CompletedFilter::All;
        }
        match self.completed_within {
            CompletedWithin::Nothing => CompletedFilter::Hide,
            CompletedWithin::Day => CompletedFilter::PastDay,
            CompletedWithin::Week => CompletedFilter::PastWeek,
            CompletedWithin::Month => CompletedFilter::PastMonth,
        }
    }

    pub fn set_completed_filter(&mut self, f: CompletedFilter) {
        self.show_completed = f == CompletedFilter::All;
        self.completed_within = match f {
            CompletedFilter::PastDay => CompletedWithin::Day,
            CompletedFilter::PastWeek => CompletedWithin::Week,
            CompletedFilter::PastMonth => CompletedWithin::Month,
            _ => CompletedWithin::Nothing,
        };
    }

    /// Whether `sub` would repeat the current group-by axis (pointless, so the
    /// panel disables it).
    pub fn sub_group_conflicts(&self, sub: SubGroup) -> bool {
        sub_group_conflicts(self.group_by, sub)
    }

    pub fn toggle_property(&mut self, p: CardProperty) {
        if let Some(pos) = self.properties.iter().position(|x| *x == p) {
            self.properties.remove(pos);
        } else {
            self.properties.push(p);
        }
    }
}

/// Whether sub-group `sub` is the same axis as group-by `group`.
pub fn sub_group_conflicts(group: GroupBy, sub: SubGroup) -> bool {
    matches!(
        (group, sub),
        (GroupBy::Priority, SubGroup::Priority)
            | (GroupBy::Assignee, SubGroup::Assignee)
            | (GroupBy::Project, SubGroup::Project)
            | (GroupBy::Status, SubGroup::Status)
    )
}

/// List-mode j/k: move the (column, issue) focus one row (`delta` >= 0 is
/// down, < 0 is up) through the flattened list, crossing section boundaries
/// and skipping empty sections. Stays put at the first/last row.
pub fn list_step(counts: &[usize], lane: usize, issue: usize, delta: i32) -> (usize, usize) {
    if delta >= 0 {
        if issue + 1 < counts.get(lane).copied().unwrap_or(0) {
            return (lane, issue + 1);
        }
        match (lane + 1..counts.len()).find(|&l| counts[l] > 0) {
            Some(l) => (l, 0),
            None => (lane, issue),
        }
    } else {
        if issue > 0 {
            return (lane, issue - 1);
        }
        match (0..lane.min(counts.len())).rev().find(|&l| counts[l] > 0) {
            Some(l) => (l, counts[l] - 1),
            None => (lane, issue),
        }
    }
}

/// List-mode h/l: first row of the next (`forward`) or previous non-empty
/// section; unchanged lane when there is none.
pub fn list_jump_section(counts: &[usize], lane: usize, forward: bool) -> usize {
    let found = if forward {
        (lane + 1..counts.len()).find(|&l| counts[l] > 0)
    } else {
        (0..lane.min(counts.len())).rev().find(|&l| counts[l] > 0)
    };
    found.unwrap_or(lane)
}

/// Which multi-select a filter toggle targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterField {
    Assignee,
    Priority,
    Label,
    Status,
    Project,
}

/// Multi-select filters. Within a field values are OR'd; fields are AND'd.
/// Ids are local UUIDs; priority is the numeric value as a string; an empty
/// string means "none" (unassigned / no project).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Filters {
    #[serde(default)]
    pub assignees: Vec<String>,
    #[serde(default)]
    pub priorities: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub statuses: Vec<String>,
    #[serde(default)]
    pub projects: Vec<String>,
}

impl Filters {
    fn list(&self, f: FilterField) -> &Vec<String> {
        match f {
            FilterField::Assignee => &self.assignees,
            FilterField::Priority => &self.priorities,
            FilterField::Label => &self.labels,
            FilterField::Status => &self.statuses,
            FilterField::Project => &self.projects,
        }
    }

    fn list_mut(&mut self, f: FilterField) -> &mut Vec<String> {
        match f {
            FilterField::Assignee => &mut self.assignees,
            FilterField::Priority => &mut self.priorities,
            FilterField::Label => &mut self.labels,
            FilterField::Status => &mut self.statuses,
            FilterField::Project => &mut self.projects,
        }
    }

    pub fn is_selected(&self, f: FilterField, value: &str) -> bool {
        self.list(f).iter().any(|v| v == value)
    }

    pub fn toggle(&mut self, f: FilterField, value: &str) {
        let list = self.list_mut(f);
        if let Some(pos) = list.iter().position(|v| v == value) {
            list.remove(pos);
        } else {
            list.push(value.to_string());
        }
    }

    /// Total number of selected values (drives the filter-button badge).
    pub fn count(&self) -> usize {
        self.assignees.len()
            + self.priorities.len()
            + self.labels.len()
            + self.statuses.len()
            + self.projects.len()
    }

    pub fn clear(&mut self) {
        *self = Filters::default();
    }

    /// Drop selected ids that no longer exist in the loaded data (ids are
    /// per-team, so a set seeded from another team's goes stale).
    /// Priorities and the "none" values (empty string) are always valid.
    /// Returns whether anything was removed.
    pub fn prune(
        &mut self,
        members: &[Member],
        labels: &[Label],
        lanes: &[Lane],
        projects: &[Project],
    ) -> bool {
        let before = self.count();
        let keep = |list: &mut Vec<String>, valid: HashSet<&str>| {
            list.retain(|v| v.is_empty() || valid.contains(v.as_str()));
        };
        keep(
            &mut self.assignees,
            members.iter().map(|m| m.id.as_str()).collect(),
        );
        keep(
            &mut self.labels,
            labels.iter().map(|l| l.id.as_str()).collect(),
        );
        keep(
            &mut self.statuses,
            lanes.iter().map(|l| l.id.as_str()).collect(),
        );
        keep(
            &mut self.projects,
            projects.iter().map(|p| p.id.as_str()).collect(),
        );
        self.count() != before
    }

    /// Whether `issue` passes every non-empty field.
    pub fn matches(&self, issue: &Issue, issue_labels: &[Label]) -> bool {
        let one = |list: &[String], key: &str| list.is_empty() || list.iter().any(|v| v == key);
        one(
            &self.assignees,
            issue.assignee_id.as_deref().unwrap_or(NONE_KEY),
        ) && one(&self.priorities, &issue.priority.to_string())
            && one(&self.statuses, &issue.lane_id)
            && one(
                &self.projects,
                issue.project_id.as_deref().unwrap_or(NONE_KEY),
            )
            && (self.labels.is_empty() || issue_labels.iter().any(|l| self.labels.contains(&l.id)))
    }
}

/// Whether an issue counts as completed (done or canceled).
pub fn is_completed(issue: &Issue) -> bool {
    issue.completed_at.is_some() || issue.canceled_at.is_some()
}

/// Whether `issue` survives the completed-issues setting at time `now`.
/// Unparseable timestamps count as recent.
pub fn passes_completed(issue: &Issue, opts: &DisplayOptions, now: DateTime<Utc>) -> bool {
    if !is_completed(issue) {
        return true;
    }
    let filter = opts.completed_filter();
    match filter {
        CompletedFilter::All => true,
        CompletedFilter::Hide => false,
        _ => {
            let Some(window) = filter.window() else {
                return true;
            };
            let cutoff = now - window;
            [&issue.completed_at, &issue.canceled_at]
                .into_iter()
                .flatten()
                .any(|ts| {
                    DateTime::parse_from_rfc3339(ts)
                        .map(|d| d.with_timezone(&Utc) >= cutoff)
                        .unwrap_or(true)
                })
        }
    }
}

/// Sort `issues` in place by `order`. Stable, so `Manual` keeps source order.
pub fn order_issues(issues: &mut [&Issue], order: OrderBy) {
    match order {
        OrderBy::Manual => {}
        OrderBy::Priority => issues.sort_by_key(|i| i.priority),
        // Soonest first; undated issues last.
        OrderBy::DueDate => issues.sort_by(|a, b| match (&a.due_date, &b.due_date) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }),
        // Newest first.
        OrderBy::Created => issues.sort_by(|a, b| b.created_at.cmp(&a.created_at)),
        OrderBy::Updated => issues.sort_by(|a, b| b.updated_at.cmp(&a.updated_at)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Status,
    Priority,
    Assignee,
    Project,
    Label,
    Parent,
}

struct Bucket {
    key: String,
    title: String,
    /// Set for lane (status) buckets.
    lane_icon: Option<LaneIcon>,
}

/// A sub-group inside a column: `issues[start..end]` of the column.
#[derive(Debug, Clone)]
pub struct Section {
    pub title: String,
    pub start: usize,
    pub end: usize,
}

/// One board column. `issues` is flattened in display order (section by
/// section), so a flat index is the focus index within the column.
#[derive(Debug, Clone)]
pub struct Column<'a> {
    pub title: String,
    /// Lane category icon; `None` unless the column is a lane.
    pub lane_icon: Option<LaneIcon>,
    pub issues: Vec<&'a Issue>,
    /// Empty when no sub-group is set.
    pub sections: Vec<Section>,
}

/// Everything `build_columns` reads, borrowed from `App`.
pub struct Data<'a> {
    /// The single team being shown, or `None` for a cross-team view.
    pub team: Option<&'a Team>,
    /// All teams, used to resolve an issue's own team in cross-team views.
    pub teams: &'a [Team],
    /// Cross-team views merge same-named lanes (case-insensitive) into one
    /// Status bucket, since each team has its own lane rows.
    pub merge_lanes_by_name: bool,
    pub lanes: &'a [Lane],
    pub issues: &'a [Issue],
    pub members: &'a [Member],
    pub projects: &'a [Project],
    pub labels: &'a [Label],
    pub issue_labels: &'a HashMap<String, Vec<Label>>,
}

impl Data<'_> {
    /// The team `issue` belongs to: the fixed team in single-team views,
    /// otherwise looked up by `issue.team_id`.
    pub fn team_of(&self, issue: &Issue) -> Option<&Team> {
        self.team
            .or_else(|| self.teams.iter().find(|t| t.id == issue.team_id))
    }

    fn lane_key(&self, lane_id: &str) -> String {
        if !self.merge_lanes_by_name {
            return lane_id.to_string();
        }
        match self.lanes.iter().find(|l| l.id == lane_id) {
            Some(l) => l.name.to_lowercase(),
            None => lane_id.to_string(),
        }
    }
}

/// Local member ids (one per team) that are the current user, matched by the
/// Rails user id. Empty when the current user isn't known yet.
pub fn my_member_ids(members: &[Member], current_user_remote_id: Option<i64>) -> Vec<String> {
    let Some(rid) = current_user_remote_id else {
        return vec![];
    };
    members
        .iter()
        .filter(|m| m.remote_id == Some(rid))
        .map(|m| m.id.clone())
        .collect()
}

/// `base` with the assignee field forced to `my_ids` (My Issues view). Other
/// fields are kept. With no known member ids nothing can match, so a sentinel
/// that no issue carries is used rather than leaving the filter empty (which
/// would show everything). Selected statuses are expanded to every lane with
/// the same (case-insensitive) name, since each team has its own lane rows and
/// the cross-team columns merge them.
pub fn my_issues_filters(base: &Filters, my_ids: &[String], lanes: &[Lane]) -> Filters {
    let mut f = base.clone();
    f.assignees = if my_ids.is_empty() {
        vec!["\u{0}no-member".to_string()]
    } else {
        my_ids.to_vec()
    };
    for id in &base.statuses {
        if let Some(sel) = lanes.iter().find(|l| l.id == *id) {
            let name = sel.name.to_lowercase();
            for l in lanes.iter().filter(|l| l.name.to_lowercase() == name) {
                if !f.statuses.contains(&l.id) {
                    f.statuses.push(l.id.clone());
                }
            }
        }
    }
    f
}

/// Status filter options for the cross-team view: one `(lane id, name)` per
/// distinct lane name (first lane by position is the stored value), in column
/// order. Matches how `buckets` merges lanes.
pub fn merged_status_options(lanes: &[Lane]) -> Vec<(String, String)> {
    let mut sorted: Vec<&Lane> = lanes.iter().collect();
    sorted.sort_by_key(|l| l.position);
    let mut seen = HashSet::new();
    sorted
        .into_iter()
        .filter(|l| seen.insert(l.name.to_lowercase()))
        .map(|l| (l.id.clone(), l.name.clone()))
        .collect()
}

/// Message for the My Issues view when it has nothing to list: `Some` only when
/// the current user isn't cached yet, or when there are no assigned issues at
/// all (no filters/search narrowing the result).
pub fn my_issues_empty_message(
    my_ids_known: bool,
    visible_count: usize,
    narrowed: bool,
) -> Option<&'static str> {
    if !my_ids_known {
        Some("Sync once to load your issues")
    } else if visible_count == 0 && !narrowed {
        Some("No issues assigned to you")
    } else {
        None
    }
}

pub const PRIORITY_TITLES: [&str; 5] = ["Urgent", "High", "Medium", "Low", "No priority"];

/// Issues that survive completed-hiding, sub-issue hiding, filters and the
/// search needle, in the requested order.
pub fn visible_issues<'a>(
    data: &Data<'a>,
    opts: &DisplayOptions,
    filters: &Filters,
    needle: &str,
) -> Vec<&'a Issue> {
    visible_from(data.issues.iter(), data, opts, filters, needle)
}

/// `visible_issues` over an arbitrary candidate set (e.g. one project's issues).
pub fn visible_from<'a>(
    candidates: impl Iterator<Item = &'a Issue>,
    data: &Data<'a>,
    opts: &DisplayOptions,
    filters: &Filters,
    needle: &str,
) -> Vec<&'a Issue> {
    let now = Utc::now();
    let mut out: Vec<&Issue> = candidates
        .filter(|i| passes_completed(i, opts, now))
        .filter(|i| opts.show_sub_issues || i.parent_issue_id.is_none())
        .filter(|i| {
            let labels = data
                .issue_labels
                .get(&i.id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            filters.matches(i, labels)
        })
        .filter(|i| crate::search_match::issue_matches(i, data.team_of(i), needle))
        .collect();
    order_issues(&mut out, opts.order_by);
    out
}

fn parent_title(issue: &Issue, team: Option<&Team>) -> String {
    let ident = match (team, issue.team_number) {
        (Some(t), Some(n)) => format!("{}-{} ", t.identifier, n),
        _ => String::new(),
    };
    format!("{}{}", ident, issue.title)
}

/// Canonical ordered buckets of an axis, including empty ones. `Parent`
/// buckets come from the parents of `visible`.
fn buckets(axis: Axis, data: &Data<'_>, visible: &[&Issue]) -> Vec<Bucket> {
    let b = |key: &str, title: &str| Bucket {
        key: key.to_string(),
        title: title.to_string(),
        lane_icon: None,
    };
    let lane_b = |key: &str, l: &Lane| Bucket {
        lane_icon: Some(icon_for(l.category.as_deref())),
        ..b(key, &l.name)
    };
    match axis {
        Axis::Status if data.merge_lanes_by_name => {
            let mut sorted: Vec<&Lane> = data.lanes.iter().collect();
            sorted.sort_by_key(|l| l.position); // stable: ties keep source order
            let mut seen = HashSet::new();
            sorted
                .into_iter()
                .filter(|l| seen.insert(l.name.to_lowercase()))
                .map(|l| lane_b(&l.name.to_lowercase(), l))
                .collect()
        }
        Axis::Status => data.lanes.iter().map(|l| lane_b(&l.id, l)).collect(),
        Axis::Priority => PRIORITY_TITLES
            .iter()
            .enumerate()
            .map(|(i, t)| b(&i.to_string(), t))
            .collect(),
        Axis::Assignee => {
            let mut ms: Vec<&Member> = data.members.iter().collect();
            ms.sort_by_key(|m| m.name.to_lowercase());
            let mut v: Vec<Bucket> = ms.iter().map(|m| b(&m.id, &m.name)).collect();
            v.push(b(NONE_KEY, "Unassigned"));
            v
        }
        Axis::Project => {
            let mut ps: Vec<&Project> = data.projects.iter().collect();
            ps.sort_by_key(|p| p.name.to_lowercase());
            let mut v: Vec<Bucket> = ps.iter().map(|p| b(&p.id, &p.name)).collect();
            v.push(b(NONE_KEY, "No project"));
            v
        }
        Axis::Label => {
            let mut ls: Vec<&Label> = data.labels.iter().collect();
            ls.sort_by_key(|l| l.name.to_lowercase());
            let mut v: Vec<Bucket> = ls.iter().map(|l| b(&l.id, &l.name)).collect();
            v.push(b(NONE_KEY, "No label"));
            v
        }
        Axis::Parent => {
            let parent_ids: HashSet<&str> = visible
                .iter()
                .filter_map(|i| i.parent_issue_id.as_deref())
                .collect();
            let mut ps: Vec<&Issue> = data
                .issues
                .iter()
                .filter(|i| parent_ids.contains(i.id.as_str()))
                .collect();
            ps.sort_by_key(|i| i.team_number.unwrap_or(0));
            let mut v: Vec<Bucket> = ps
                .iter()
                .map(|i| b(&i.id, &parent_title(i, data.team_of(i))))
                .collect();
            v.push(b(NONE_KEY, "No parent"));
            v
        }
    }
}

/// Bucket keys `issue` belongs to on `axis` (labels can be several, or none).
fn keys_for(issue: &Issue, axis: Axis, data: &Data<'_>) -> Vec<String> {
    match axis {
        Axis::Status => vec![data.lane_key(&issue.lane_id)],
        Axis::Priority => vec![issue.priority.clamp(0, 4).to_string()],
        Axis::Assignee => vec![issue.assignee_id.clone().unwrap_or_default()],
        Axis::Project => vec![issue.project_id.clone().unwrap_or_default()],
        Axis::Parent => vec![issue.parent_issue_id.clone().unwrap_or_default()],
        Axis::Label => {
            let ls = data
                .issue_labels
                .get(&issue.id)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            if ls.is_empty() {
                vec![NONE_KEY.to_string()]
            } else {
                ls.iter().map(|l| l.id.clone()).collect()
            }
        }
    }
}

/// Split `issues` by `axis` into non-empty `(title, issues)` sections in
/// canonical order.
fn split_sections<'a>(
    axis: Axis,
    data: &Data<'_>,
    issues: &[&'a Issue],
) -> Vec<(String, Vec<&'a Issue>)> {
    buckets(axis, data, issues)
        .into_iter()
        .filter_map(|bk| {
            let members: Vec<&Issue> = issues
                .iter()
                .copied()
                .filter(|i| keys_for(i, axis, data).contains(&bk.key))
                .collect();
            (!members.is_empty()).then_some((bk.title, members))
        })
        .collect()
}

/// Build the board columns for the given options. Status and priority grouping
/// always show every bucket (so empty lanes stay droppable-looking); other axes
/// show only non-empty buckets to avoid a wall of empty columns.
pub fn build_columns<'a>(
    data: &Data<'a>,
    opts: &DisplayOptions,
    filters: &Filters,
    needle: &str,
) -> Vec<Column<'a>> {
    let visible = visible_issues(data, opts, filters, needle);
    let sub_axis = opts.sub_group.axis();

    let finish = |title: String,
                  lane_icon: Option<LaneIcon>,
                  issues: Vec<&'a Issue>|
     -> Column<'a> {
        match sub_axis {
            None => Column {
                title,
                lane_icon,
                issues,
                sections: vec![],
            },
            Some(axis) => {
                let mut flat = Vec::new();
                let mut sections = Vec::new();
                for (stitle, group) in split_sections(axis, data, &issues) {
                    let start = flat.len();
                    flat.extend(group);
                    sections.push(Section {
                        title: stitle,
                        start,
                        end: flat.len(),
                    });
                }
                Column {
                    title,
                    lane_icon,
                    issues: flat,
                    sections,
                }
            }
        }
    };

    let Some(axis) = opts.group_by.axis() else {
        return vec![finish("All issues".to_string(), None, visible)];
    };

    let keep_empty = opts.show_empty_groups || matches!(axis, Axis::Status | Axis::Priority);
    buckets(axis, data, &visible)
        .into_iter()
        .filter_map(|bk| {
            let members: Vec<&Issue> = visible
                .iter()
                .copied()
                .filter(|i| keys_for(i, axis, data).contains(&bk.key))
                .collect();
            (keep_empty || !members.is_empty()).then(|| finish(bk.title, bk.lane_icon, members))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_rows_show_every_story_12_column() {
        let d = DisplayOptions::dashboard_rows();
        for p in [
            CardProperty::Id,
            CardProperty::Priority,
            CardProperty::Status,
            CardProperty::Project,
            CardProperty::Assignee,
            CardProperty::Labels,
            CardProperty::DueDate,
        ] {
            assert!(d.shows(p), "{p:?}");
        }
    }

    pub(super) fn issue(id: &str, lane: &str, priority: i32) -> Issue {
        Issue {
            id: id.into(),
            title: format!("Issue {id}"),
            description: None,
            priority,
            team_number: Some(1),
            due_date: None,
            estimate: None,
            started_at: None,
            completed_at: None,
            canceled_at: None,
            lane_id: lane.into(),
            team_id: "t".into(),
            project_id: None,
            parent_issue_id: None,
            assignee_id: None,
            creator_id: None,
            remote_id: None,
            created_at: format!("2026-01-0{id}T00:00:00Z"),
            updated_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    pub(super) fn lane(id: &str, name: &str, pos: i32) -> Lane {
        Lane {
            id: id.into(),
            name: name.into(),
            color: None,
            position: pos,
            team_id: "t".into(),
            remote_id: None,
            category: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn label(id: &str) -> Label {
        Label {
            id: id.into(),
            remote_id: None,
            team_id: "t".into(),
            name: id.into(),
            color: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    pub(super) fn data<'a>(
        lanes: &'a [Lane],
        issues: &'a [Issue],
        labels: &'a [Label],
        il: &'a HashMap<String, Vec<Label>>,
    ) -> Data<'a> {
        Data {
            team: None,
            teams: &[],
            merge_lanes_by_name: false,
            lanes,
            issues,
            members: &[],
            projects: &[],
            labels,
            issue_labels: il,
        }
    }

    #[test]
    fn group_by_status_keeps_empty_lanes() {
        let lanes = [lane("a", "Todo", 0), lane("b", "Done", 1)];
        let issues = [issue("1", "a", 2)];
        let il = HashMap::new();
        let d = data(&lanes, &issues, &[], &il);
        let cols = build_columns(&d, &DisplayOptions::default(), &Filters::default(), "");
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].issues.len(), 1);
        assert!(cols[1].issues.is_empty());
    }

    #[test]
    fn group_by_priority_and_none() {
        let lanes = [lane("a", "Todo", 0)];
        let issues = [issue("1", "a", 0), issue("2", "a", 3)];
        let il = HashMap::new();
        let d = data(&lanes, &issues, &[], &il);
        let mut o = DisplayOptions {
            group_by: GroupBy::Priority,
            ..Default::default()
        };
        let cols = build_columns(&d, &o, &Filters::default(), "");
        assert_eq!(cols.len(), 5);
        assert_eq!(cols[0].issues.len(), 1);
        assert_eq!(cols[3].issues.len(), 1);
        o.group_by = GroupBy::None;
        let cols = build_columns(&d, &o, &Filters::default(), "");
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].issues.len(), 2);
    }

    #[test]
    fn group_by_label_multi_and_unlabeled() {
        let lanes = [lane("a", "Todo", 0)];
        let issues = [issue("1", "a", 2), issue("2", "a", 2)];
        let labels = [label("bug"), label("ui")];
        let mut il = HashMap::new();
        il.insert("1".to_string(), vec![label("bug"), label("ui")]);
        let d = data(&lanes, &issues, &labels, &il);
        let o = DisplayOptions {
            group_by: GroupBy::Label,
            ..Default::default()
        };
        let cols = build_columns(&d, &o, &Filters::default(), "");
        let titles: Vec<_> = cols.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["bug", "ui", "No label"]);
        assert_eq!(cols[0].issues.len(), 1);
        assert_eq!(cols[2].issues.len(), 1);
    }

    #[test]
    fn sub_groups_flatten_in_order() {
        let lanes = [lane("a", "Todo", 0)];
        let issues = [issue("1", "a", 3), issue("2", "a", 0), issue("3", "a", 3)];
        let il = HashMap::new();
        let d = data(&lanes, &issues, &[], &il);
        let o = DisplayOptions {
            sub_group: SubGroup::Priority,
            ..Default::default()
        };
        let cols = build_columns(&d, &o, &Filters::default(), "");
        let c = &cols[0];
        assert_eq!(c.sections.len(), 2);
        assert_eq!(c.sections[0].title, "Urgent");
        assert_eq!((c.sections[1].start, c.sections[1].end), (1, 3));
        assert_eq!(c.issues[0].id, "2");
    }

    #[test]
    fn ordering() {
        let mut a = issue("1", "a", 3);
        let mut b = issue("2", "a", 0);
        a.due_date = Some("2026-02-01".into());
        b.due_date = None;
        let mut v = vec![&b, &a];
        order_issues(&mut v, OrderBy::DueDate);
        assert_eq!(v[0].id, "1");
        order_issues(&mut v, OrderBy::Priority);
        assert_eq!(v[0].id, "2");
        order_issues(&mut v, OrderBy::Created);
        assert_eq!(v[0].id, "2"); // created 2026-01-02, newest first
    }

    #[test]
    fn completed_hidden() {
        let lanes = [lane("a", "Todo", 0)];
        let mut done = issue("1", "a", 2);
        done.completed_at = Some("x".into());
        let issues = [done, issue("2", "a", 2)];
        let il = HashMap::new();
        let d = data(&lanes, &issues, &[], &il);
        let o = DisplayOptions {
            show_completed: false,
            ..Default::default()
        };
        let cols = build_columns(&d, &o, &Filters::default(), "");
        assert_eq!(cols[0].issues.len(), 1);
        assert_eq!(cols[0].issues[0].id, "2");
    }

    #[test]
    fn filters_and_count() {
        let mut f = Filters::default();
        f.toggle(FilterField::Priority, "0");
        f.toggle(FilterField::Priority, "1");
        f.toggle(FilterField::Status, "a");
        assert_eq!(f.count(), 3);
        let i = issue("1", "a", 1);
        assert!(f.matches(&i, &[]));
        assert!(!f.matches(&issue("2", "b", 1), &[]));
        assert!(!f.matches(&issue("3", "a", 3), &[]));
        f.toggle(FilterField::Priority, "1");
        assert_eq!(f.count(), 2);
        f.toggle(FilterField::Label, "bug");
        assert!(!f.matches(&issue("4", "a", 0), &[]));
        assert!(f.matches(&issue("4", "a", 0), &[label("bug")]));
        f.clear();
        assert_eq!(f.count(), 0);
    }

    #[test]
    fn unassigned_filter() {
        let mut f = Filters::default();
        f.toggle(FilterField::Assignee, "");
        let mut i = issue("1", "a", 1);
        assert!(f.matches(&i, &[]));
        i.assignee_id = Some("m".into());
        assert!(!f.matches(&i, &[]));
    }

    #[test]
    fn toggle_property() {
        let mut o = DisplayOptions::default();
        assert!(o.shows(CardProperty::Id));
        o.toggle_property(CardProperty::Id);
        assert!(!o.shows(CardProperty::Id));
        o.toggle_property(CardProperty::DueDate);
        assert!(o.shows(CardProperty::DueDate));
    }

    pub(super) fn member(id: &str, team: &str, remote: Option<i64>) -> Member {
        Member {
            id: id.into(),
            remote_id: remote,
            team_id: team.into(),
            name: id.into(),
            email: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn team(id: &str, ident: &str) -> Team {
        Team {
            id: id.into(),
            name: ident.into(),
            identifier: ident.into(),
            remote_id: None,
            archived_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn my_member_ids_spans_teams() {
        let ms = [
            member("a1", "t1", Some(7)),
            member("a2", "t2", Some(7)),
            member("b1", "t1", Some(8)),
            member("c1", "t1", None),
        ];
        assert_eq!(my_member_ids(&ms, Some(7)), ["a1", "a2"]);
        assert!(my_member_ids(&ms, Some(99)).is_empty());
        assert!(my_member_ids(&ms, None).is_empty());
    }

    #[test]
    fn my_issues_aggregates_across_teams() {
        let mut l2 = lane("l2", "todo", 0);
        l2.team_id = "t2".into();
        let lanes = [lane("l1", "Todo", 0), l2, lane("l3", "Done", 1)];
        let mut i1 = issue("1", "l1", 1);
        i1.assignee_id = Some("a1".into());
        let mut i2 = issue("2", "l2", 1);
        i2.team_id = "t2".into();
        i2.assignee_id = Some("a2".into());
        let mut i3 = issue("3", "l1", 1);
        i3.assignee_id = Some("b1".into());
        let i4 = issue("4", "l1", 1);
        let issues = [i1, i2, i3, i4];
        let teams = [team("t", "AAA"), team("t2", "BBB")];
        let il = HashMap::new();
        let d = Data {
            teams: &teams,
            merge_lanes_by_name: true,
            ..data(&lanes, &issues, &[], &il)
        };
        let ids = vec!["a1".to_string(), "a2".to_string()];
        let f = my_issues_filters(&Filters::default(), &ids, &lanes);
        let cols = build_columns(&d, &DisplayOptions::default(), &f, "");
        // "Todo" and "todo" merge; "Done" stays.
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].issues.len(), 2);
        assert!(cols[1].issues.is_empty());
        assert_eq!(d.team_of(cols[0].issues[1]).unwrap().identifier, "BBB");
        // Search resolves the issue's own team identifier.
        let cols = build_columns(&d, &DisplayOptions::default(), &f, "bbb-");
        assert_eq!(cols[0].issues.len(), 1);
        // Unknown current user matches nothing.
        let f = my_issues_filters(&Filters::default(), &[], &lanes);
        let cols = build_columns(&d, &DisplayOptions::default(), &f, "");
        assert!(cols.iter().all(|c| c.issues.is_empty()));
        // A user-saved assignee filter is overridden, other fields kept.
        let mut base = Filters::default();
        base.toggle(FilterField::Assignee, "zzz");
        base.toggle(FilterField::Priority, "1");
        let f = my_issues_filters(&base, &ids, &lanes);
        assert_eq!(f.assignees, ids);
        assert_eq!(f.priorities, ["1"]);
    }

    #[test]
    fn my_issues_status_filter_merges_by_name() {
        let mut l2 = lane("l2", "todo", 0);
        l2.team_id = "t2".into();
        let lanes = [lane("l1", "Todo", 0), l2, lane("l3", "Done", 1)];
        let opts = merged_status_options(&lanes);
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0], ("l1".to_string(), "Todo".to_string()));
        let ids = vec!["a".to_string()];
        let mut base = Filters::default();
        base.toggle(FilterField::Status, "l1");
        let f = my_issues_filters(&base, &ids, &lanes);
        assert_eq!(f.statuses, ["l1", "l2"]);
        // Selecting via the other team's lane id expands the same way.
        let mut base = Filters::default();
        base.toggle(FilterField::Status, "l2");
        let f = my_issues_filters(&base, &ids, &lanes);
        assert_eq!(f.statuses, ["l2", "l1"]);
    }

    #[test]
    fn my_issues_empty_messages() {
        assert_eq!(
            my_issues_empty_message(false, 0, false),
            Some("Sync once to load your issues")
        );
        assert_eq!(
            my_issues_empty_message(true, 0, false),
            Some("No issues assigned to you")
        );
        assert_eq!(my_issues_empty_message(true, 0, true), None);
        assert_eq!(my_issues_empty_message(true, 3, false), None);
    }

    #[test]
    fn cross_team_group_by_parent() {
        let lanes = [lane("l1", "Todo", 0)];
        // Parent lives in team t2 (BBB, #7); my children are in t and t2.
        let mut parent = issue("1", "l1", 1);
        parent.team_id = "t2".into();
        parent.team_number = Some(7);
        let mut c1 = issue("2", "l1", 1);
        c1.parent_issue_id = Some("1".into());
        c1.assignee_id = Some("a1".into());
        let mut c2 = issue("3", "l1", 1);
        c2.team_id = "t2".into();
        c2.parent_issue_id = Some("1".into());
        c2.assignee_id = Some("a2".into());
        let mut c3 = issue("4", "l1", 1);
        c3.assignee_id = Some("a1".into());
        let issues = [parent, c1, c2, c3];
        let teams = [team("t", "AAA"), team("t2", "BBB")];
        let il = HashMap::new();
        let d = Data {
            teams: &teams,
            merge_lanes_by_name: true,
            ..data(&lanes, &issues, &[], &il)
        };
        let opts = DisplayOptions {
            group_by: GroupBy::Parent,
            ..DisplayOptions::default()
        };
        let ids = vec!["a1".to_string(), "a2".to_string()];
        let f = my_issues_filters(&Filters::default(), &ids, &lanes);
        let cols = build_columns(&d, &opts, &f, "");
        assert_eq!(cols.len(), 2);
        // Parent title resolves the parent's own team identifier.
        assert_eq!(cols[0].title, "BBB-7 Issue 1");
        assert_eq!(cols[0].issues.len(), 2);
        assert_eq!(cols[1].title, "No parent");
        assert_eq!(cols[1].issues.len(), 1);
    }

    #[test]
    fn list_step_crosses_sections_and_skips_empty() {
        let counts = [2, 0, 1];
        assert_eq!(list_step(&counts, 0, 0, 1), (0, 1));
        assert_eq!(list_step(&counts, 0, 1, 1), (2, 0));
        assert_eq!(list_step(&counts, 2, 0, 1), (2, 0));
        assert_eq!(list_step(&counts, 2, 0, -1), (0, 1));
        assert_eq!(list_step(&counts, 0, 0, -1), (0, 0));
        assert_eq!(list_step(&[], 0, 0, 1), (0, 0));
    }

    #[test]
    fn list_jump_section_skips_empty() {
        let counts = [2, 0, 1];
        assert_eq!(list_jump_section(&counts, 0, true), 2);
        assert_eq!(list_jump_section(&counts, 2, false), 0);
        assert_eq!(list_jump_section(&counts, 2, true), 2);
    }

    #[test]
    fn view_mode_defaults_to_board_when_missing() {
        let o: DisplayOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(o.view_mode, ViewMode::Board);
        let o: DisplayOptions = serde_json::from_str(r#"{"view_mode":"list"}"#).unwrap();
        assert_eq!(o.view_mode, ViewMode::List);
    }
}

#[cfg(test)]
mod property_tests {
    use super::tests::{data, issue, lane, member};
    use super::*;

    #[test]
    fn new_properties_toggle_and_roundtrip() {
        let mut o = DisplayOptions::default();
        for p in [
            CardProperty::Estimate,
            CardProperty::Created,
            CardProperty::Updated,
        ] {
            assert!(!o.shows(p));
            o.toggle_property(p);
            assert!(o.shows(p));
        }
        let json = serde_json::to_string(&o).unwrap();
        assert!(json.contains("estimate") && json.contains("created") && json.contains("updated"));
        let back: DisplayOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back, o);
        assert_eq!(CardProperty::ALL.len(), 10);
    }

    #[test]
    fn prune_drops_foreign_ids_keeps_none_values() {
        let mut f = Filters::default();
        f.toggle(FilterField::Assignee, "");
        f.toggle(FilterField::Assignee, "m1");
        f.toggle(FilterField::Assignee, "other-team-m");
        f.toggle(FilterField::Status, "a");
        f.toggle(FilterField::Status, "gone");
        f.toggle(FilterField::Label, "gone");
        f.toggle(FilterField::Project, "");
        f.toggle(FilterField::Priority, "1");
        let lanes = [lane("a", "Todo", 0)];
        let members = [member("m1", "t", None)];
        assert!(f.prune(&members, &[], &lanes, &[]));
        assert_eq!(f.assignees, vec!["".to_string(), "m1".to_string()]);
        assert_eq!(f.statuses, vec!["a".to_string()]);
        assert!(f.labels.is_empty());
        assert_eq!(f.projects, vec!["".to_string()]);
        assert_eq!(f.priorities, vec!["1".to_string()]);
        assert!(!f.prune(&members, &[], &lanes, &[]));
    }

    #[test]
    fn sub_group_conflict_matches_axis() {
        assert!(sub_group_conflicts(GroupBy::Status, SubGroup::Status));
        assert!(sub_group_conflicts(GroupBy::Priority, SubGroup::Priority));
        assert!(!sub_group_conflicts(GroupBy::Status, SubGroup::Priority));
        assert!(!sub_group_conflicts(GroupBy::Label, SubGroup::Project));
        assert!(!sub_group_conflicts(GroupBy::None, SubGroup::None));
    }

    #[test]
    fn completed_filter_round_trips_and_windows() {
        let mut o = DisplayOptions::default();
        assert_eq!(o.completed_filter(), CompletedFilter::All);
        for (f, _) in CompletedFilter::ALL {
            o.set_completed_filter(f);
            assert_eq!(o.completed_filter(), f);
        }
        let now = DateTime::parse_from_rfc3339("2026-03-10T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut done = issue("1", "a", 2);
        done.completed_at = Some("2026-03-09T12:00:00Z".into());
        let mut old = issue("2", "a", 2);
        old.canceled_at = Some("2026-01-01T00:00:00Z".into());
        let open = issue("3", "a", 2);
        o.set_completed_filter(CompletedFilter::Hide);
        assert!(!passes_completed(&done, &o, now) && passes_completed(&open, &o, now));
        o.set_completed_filter(CompletedFilter::PastDay);
        assert!(passes_completed(&done, &o, now) && !passes_completed(&old, &o, now));
        o.set_completed_filter(CompletedFilter::PastMonth);
        assert!(passes_completed(&done, &o, now) && !passes_completed(&old, &o, now));
        o.set_completed_filter(CompletedFilter::All);
        assert!(passes_completed(&old, &o, now));
    }

    #[test]
    fn hides_sub_issues_and_shows_empty_groups() {
        let lanes = [lane("a", "Todo", 0)];
        let mut child = issue("2", "a", 2);
        child.parent_issue_id = Some("1".into());
        let issues = [issue("1", "a", 2), child];
        let il = HashMap::new();
        let d = data(&lanes, &issues, &[], &il);
        let mut o = DisplayOptions::default();
        assert_eq!(visible_issues(&d, &o, &Filters::default(), "").len(), 2);
        o.show_sub_issues = false;
        assert_eq!(visible_issues(&d, &o, &Filters::default(), "").len(), 1);

        let mut o = DisplayOptions {
            group_by: GroupBy::Project,
            ..Default::default()
        };
        let hidden = build_columns(&d, &o, &Filters::default(), "");
        o.show_empty_groups = true;
        let shown = build_columns(&d, &o, &Filters::default(), "");
        assert_eq!(hidden.len(), 1); // only "No project"
        assert!(shown.len() >= hidden.len());
    }
}
