//! Dashboard evaluation: a pure port of Rails `DashboardIssuesQuery` that runs
//! over the local mirror. No Iced and no SQL. `evaluate` takes the dashboard
//! config, the mirrored data, the current user and today's date, and returns
//! each group in order with its sorted issues.
//!
//! Later tickets slot in without reshaping the types: project and label sources
//! extend `pool` and `dropped`, filter tabs / sort / search / Mine / pickers add
//! fields to `Header` (and variants to `Filter` / `Sort`), counts and
//! source-team ids add fields to `Evaluation`.
use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;

use crate::db::dashboards::{DashboardGroup, GroupSource, SourceType};
use crate::db::models::{Issue, Label, Lane, Member, Project, Team};

/// One group with its sources, as stored in the mirror.
#[derive(Debug, Clone)]
pub struct GroupConfig {
    pub group: DashboardGroup,
    pub sources: Vec<GroupSource>,
}

/// The mirrored workspace data evaluation reads, borrowed from `App`.
#[derive(Clone, Copy)]
pub struct Mirror<'a> {
    pub teams: &'a [Team],
    pub projects: &'a [Project],
    pub labels: &'a [Label],
    pub lanes: &'a [Lane],
    pub members: &'a [Member],
    pub issues: &'a [Issue],
    /// Labels per issue local id.
    pub issue_labels: &'a HashMap<String, Vec<Label>>,
}

/// The lens over each group's base set. Today is the default, as on the web.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    /// Effective due date on or before today (overdue included).
    #[default]
    Today,
    /// Effective due date on or before today + 6.
    Week,
    /// Urgent or high priority, whatever the date.
    Hot,
    /// No date or priority condition.
    Open,
}

impl Filter {
    pub const ALL: [Filter; 4] = [Filter::Today, Filter::Week, Filter::Hot, Filter::Open];

    pub fn label(self) -> &'static str {
        match self {
            Filter::Today => "Today",
            Filter::Week => "Week",
            Filter::Hot => "Hot",
            Filter::Open => "Open",
        }
    }
}

/// Row order within a group. Only Due exists so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Due,
}

/// The header state. Held in memory per dashboard; not persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Header {
    pub filter: Filter,
    pub sort: Sort,
}

pub struct Input<'a> {
    pub groups: &'a [GroupConfig],
    pub mirror: Mirror<'a>,
    /// Rails user id of the configured current user.
    pub current_user_remote_id: Option<i64>,
    /// Machine-local date.
    pub today: NaiveDate,
    pub header: Header,
}

pub struct GroupResult<'a> {
    pub group: &'a DashboardGroup,
    /// Every source was dropped as unreachable. Not widened to show everything.
    #[allow(dead_code)]
    pub inaccessible: bool,
    /// Sorted issues.
    pub issues: Vec<&'a Issue>,
}

pub struct Evaluation<'a> {
    /// In the order of `Input::groups`.
    pub groups: Vec<GroupResult<'a>>,
    /// Distinct issues across all groups with effective due = today. Computed
    /// from the refined base sets, so it ignores the filter tab (and search).
    pub due_today: usize,
    /// Distinct issues across all groups with effective due < today.
    pub overdue: usize,
}

pub fn evaluate<'a>(input: &Input<'a>) -> Evaluation<'a> {
    let m = &input.mirror;
    let accessible: HashSet<&str> = m
        .teams
        .iter()
        .filter(|t| t.archived_at.is_none())
        .map(|t| t.id.as_str())
        .collect();
    let my_members: HashSet<&str> = m
        .members
        .iter()
        .filter(|mem| {
            input.current_user_remote_id.is_some() && mem.remote_id == input.current_user_remote_id
        })
        .map(|mem| mem.id.as_str())
        .collect();

    // Ids across all groups' refined base sets, deduplicated for the counts.
    let mut counted: HashSet<&str> = HashSet::new();
    let groups = input
        .groups
        .iter()
        .map(|cfg| {
            let pool = pool(cfg, m, &accessible, &my_members);
            let pool = apply_label_sources(cfg, m, &accessible, pool);
            // The refined base set: counts read this, rows narrow it further.
            let base = base_set(input, &pool);
            counted.extend(base.iter().map(|i| i.id.as_str()));
            let mut issues: Vec<&Issue> = base
                .into_iter()
                .filter(|i| passes_filter(i, input.header.filter, input.today, m.projects))
                .collect();
            sort(&mut issues, input.header.sort, m.projects);
            GroupResult {
                group: &cfg.group,
                inaccessible: !cfg.sources.is_empty()
                    && cfg.sources.iter().all(|s| dropped(s, m, &accessible)),
                issues,
            }
        })
        .collect();
    let (due_today, overdue) = due_counts(&counted, m, input.today);
    Evaluation { groups, due_today, overdue }
}

/// A group's pool narrowed to the starting set rows and counts both read.
/// Refinements (Mine, team, assignee, label, status) plug in here, so rows and
/// counts narrow together. The status picker swaps `is_active` for a
/// lane-name match in this function.
fn base_set<'a>(input: &Input<'a>, pool: &HashSet<&'a str>) -> Vec<&'a Issue> {
    let m = &input.mirror;
    m.issues
        .iter()
        .filter(|i| pool.contains(i.id.as_str()) && is_active(i, m.lanes))
        .collect()
}

/// (due today, overdue) over distinct issue ids, by effective due date.
fn due_counts(ids: &HashSet<&str>, m: &Mirror<'_>, today: NaiveDate) -> (usize, usize) {
    let (mut due_today, mut overdue) = (0, 0);
    for i in m.issues.iter().filter(|i| ids.contains(i.id.as_str())) {
        match effective_due(i, m.projects) {
            Some(d) if d == today => due_today += 1,
            Some(d) if d < today => overdue += 1,
            _ => {}
        }
    }
    (due_today, overdue)
}

/// The local project behind a project source, if mirrored and on an accessible team.
fn project_of_source<'a>(
    s: &GroupSource,
    m: &Mirror<'a>,
    accessible: &HashSet<&str>,
) -> Option<&'a Project> {
    m.projects
        .iter()
        .find(|p| p.remote_id == Some(s.remote_id))
        .filter(|p| accessible.contains(p.team_id.as_str()))
}

/// The local team behind a team source, if mirrored.
fn team_of_source<'a>(s: &GroupSource, m: &Mirror<'a>) -> Option<&'a Team> {
    m.teams.iter().find(|t| t.remote_id == Some(s.remote_id))
}

/// A source pointing at something outside the accessible teams.
fn dropped(s: &GroupSource, m: &Mirror<'_>, accessible: &HashSet<&str>) -> bool {
    match s.source_type {
        SourceType::Team => {
            !team_of_source(s, m).is_some_and(|t| accessible.contains(t.id.as_str()))
        }
        SourceType::Label => label_of_source(s, m, accessible).is_none(),
        SourceType::Project => project_of_source(s, m, accessible).is_none(),
    }
}

/// The mirrored label behind a label source, if its team is accessible.
fn label_of_source<'a>(
    s: &GroupSource,
    m: &Mirror<'a>,
    accessible: &HashSet<&str>,
) -> Option<&'a Label> {
    m.labels
        .iter()
        .find(|l| l.remote_id == Some(s.remote_id))
        .filter(|l| accessible.contains(l.team_id.as_str()))
}

/// Applies a group's label sources to its team/project pool. With team or
/// project sources, labels narrow the pool to issues carrying any listed label.
/// With only label sources, the pool is every issue on the labels' teams that
/// carries any of them. Label sources on inaccessible teams are dropped.
fn apply_label_sources<'a>(
    cfg: &GroupConfig,
    m: &Mirror<'a>,
    accessible: &HashSet<&str>,
    base: HashSet<&'a str>,
) -> HashSet<&'a str> {
    let label_sources: Vec<&GroupSource> =
        cfg.sources.iter().filter(|s| s.source_type == SourceType::Label).collect();
    if label_sources.is_empty() {
        return base;
    }
    let labels: Vec<&Label> =
        label_sources.iter().filter_map(|s| label_of_source(s, m, accessible)).collect();
    let carries_any = |issue_id: &str| {
        m.issue_labels
            .get(issue_id)
            .is_some_and(|ls| ls.iter().any(|l| labels.iter().any(|w| w.id == l.id)))
    };
    let narrowing = cfg.sources.iter().any(|s| s.source_type != SourceType::Label);
    if narrowing {
        return base.into_iter().filter(|id| carries_any(id)).collect();
    }
    let teams: HashSet<&str> = labels.iter().map(|l| l.team_id.as_str()).collect();
    m.issues
        .iter()
        .filter(|i| teams.contains(i.team_id.as_str()) && carries_any(&i.id))
        .map(|i| i.id.as_str())
        .collect()
}

/// Ids of the issues a group's sources pull in, before the active-set rule.
fn pool<'a>(
    cfg: &GroupConfig,
    m: &Mirror<'a>,
    accessible: &HashSet<&str>,
    my_members: &HashSet<&str>,
) -> HashSet<&'a str> {
    let mut out = HashSet::new();
    for s in cfg.sources.iter().filter(|s| s.source_type == SourceType::Team) {
        let Some(team) = team_of_source(s, m).filter(|t| accessible.contains(t.id.as_str()))
        else {
            continue;
        };
        for i in m.issues.iter().filter(|i| i.team_id == team.id) {
            let mine = i.assignee_id.as_deref().is_some_and(|a| my_members.contains(a));
            if s.include_all || mine {
                out.insert(i.id.as_str());
            }
        }
    }
    add_project_sources(cfg, m, accessible, my_members, &mut out);
    out
}

/// Project sources: every issue in the project when include-all, else only
/// those assigned to the current user. Unions into `out`.
fn add_project_sources<'a>(
    cfg: &GroupConfig,
    m: &Mirror<'a>,
    accessible: &HashSet<&str>,
    my_members: &HashSet<&str>,
    out: &mut HashSet<&'a str>,
) {
    for s in cfg.sources.iter().filter(|s| s.source_type == SourceType::Project) {
        let Some(project) = project_of_source(s, m, accessible) else {
            continue;
        };
        for i in m.issues.iter().filter(|i| i.project_id.as_deref() == Some(project.id.as_str())) {
            let mine = i.assignee_id.as_deref().is_some_and(|a| my_members.contains(a));
            if s.include_all || mine {
                out.insert(i.id.as_str());
            }
        }
    }
}

/// The filter tab's rule. Undated issues never match Today or Week.
fn passes_filter(i: &Issue, filter: Filter, today: NaiveDate, projects: &[Project]) -> bool {
    match filter {
        Filter::Open => true,
        Filter::Hot => i.priority <= 1,
        Filter::Today => effective_due(i, projects).is_some_and(|d| d <= today),
        Filter::Week => {
            effective_due(i, projects).is_some_and(|d| d <= today + chrono::Days::new(6))
        }
    }
}

/// Not archived, completed or canceled, and not in a completed/canceled lane.
fn is_active(i: &Issue, lanes: &[Lane]) -> bool {
    if i.archived_at.is_some() || i.completed_at.is_some() || i.canceled_at.is_some() {
        return false;
    }
    let closed_lane = lanes
        .iter()
        .find(|l| l.id == i.lane_id)
        .and_then(|l| l.category.as_deref())
        .is_some_and(|c| c == "completed" || c == "canceled");
    !closed_lane
}

/// The issue's due date, else its project's.
pub fn effective_due(i: &Issue, projects: &[Project]) -> Option<NaiveDate> {
    let own = i.due_date.as_deref().and_then(parse_day);
    own.or_else(|| {
        let pid = i.project_id.as_deref()?;
        projects
            .iter()
            .find(|p| p.id == pid)?
            .due_date
            .as_deref()
            .and_then(parse_day)
    })
}

fn parse_day(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

fn sort(issues: &mut [&Issue], sort: Sort, projects: &[Project]) {
    match sort {
        // Dated first (false < true), earliest first, then priority (urgent
        // first), then id for a stable order.
        Sort::Due => issues.sort_by_cached_key(|i| {
            let due = effective_due(i, projects);
            (due.is_none(), due, i.priority, i.id.clone())
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(id: &str, remote: i64, archived: bool) -> Team {
        Team {
            id: id.into(),
            name: id.into(),
            identifier: id.to_uppercase(),
            remote_id: Some(remote),
            archived_at: archived.then(|| "2026-01-01".to_string()),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn lane(id: &str, team: &str, category: &str) -> Lane {
        Lane {
            id: id.into(),
            name: id.into(),
            color: None,
            position: 0,
            team_id: team.into(),
            remote_id: None,
            category: Some(category.into()),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn member(id: &str, team: &str, remote: i64) -> Member {
        Member {
            id: id.into(),
            remote_id: Some(remote),
            team_id: team.into(),
            name: id.into(),
            email: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn project(id: &str, team: &str, due: Option<&str>) -> Project {
        Project {
            id: id.into(),
            remote_id: None,
            team_id: team.into(),
            name: id.into(),
            description: None,
            status: None,
            priority: 0,
            lead_id: None,
            start_date: None,
            due_date: due.map(String::from),
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

    fn issue(id: &str, team: &str, lane: &str) -> Issue {
        Issue {
            id: id.into(),
            title: id.into(),
            description: None,
            priority: 2,
            team_number: Some(1),
            due_date: None,
            estimate: None,
            started_at: None,
            completed_at: None,
            canceled_at: None,
            lane_id: lane.into(),
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

    fn group(id: &str) -> DashboardGroup {
        DashboardGroup {
            id: id.into(),
            remote_id: None,
            dashboard_id: "d".into(),
            name: id.into(),
            description: String::new(),
            color: "#6366f1".into(),
            position: 0,
        }
    }

    fn team_src(remote: i64, all: bool) -> GroupSource {
        GroupSource { source_type: SourceType::Team, remote_id: remote, include_all: all }
    }

    fn cfg(id: &str, sources: Vec<GroupSource>) -> GroupConfig {
        GroupConfig { group: group(id), sources }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    struct Fx {
        teams: Vec<Team>,
        projects: Vec<Project>,
        lanes: Vec<Lane>,
        members: Vec<Member>,
        issues: Vec<Issue>,
        labels: Vec<Label>,
        issue_labels: HashMap<String, Vec<Label>>,
    }

    impl Fx {
        fn new() -> Self {
            Fx {
                teams: vec![team("a", 1, false), team("b", 2, false), team("arch", 3, true)],
                projects: vec![],
                lanes: vec![
                    lane("la", "a", "started"),
                    lane("lb", "b", "started"),
                    lane("done", "a", "completed"),
                    lane("canc", "a", "canceled"),
                    lane("larch", "arch", "started"),
                ],
                members: vec![member("ma", "a", 99), member("mb", "b", 99), member("other", "a", 5)],
                issues: vec![],
                labels: vec![],
                issue_labels: HashMap::new(),
            }
        }

        fn eval<'a>(&'a self, groups: &'a [GroupConfig], user: Option<i64>) -> Evaluation<'a> {
            self.eval_with(groups, user, Header { filter: Filter::Open, ..Header::default() })
        }

        fn eval_with<'a>(
            &'a self,
            groups: &'a [GroupConfig],
            user: Option<i64>,
            header: Header,
        ) -> Evaluation<'a> {
            evaluate(&Input {
                groups,
                mirror: Mirror {
                    teams: &self.teams,
                    projects: &self.projects,
                    labels: &self.labels,
                    lanes: &self.lanes,
                    members: &self.members,
                    issues: &self.issues,
                    issue_labels: &self.issue_labels,
                },
                current_user_remote_id: user,
                today: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
                header,
            })
        }

        /// Issue ids per group, in order.
        fn run(&self, groups: Vec<GroupConfig>, user: Option<i64>) -> Vec<Vec<String>> {
            self.eval(&groups, user)
                .groups
                .iter()
                .map(|g| g.issues.iter().map(|i| i.id.clone()).collect())
                .collect()
        }
    }

    #[test]
    fn include_all_pulls_every_team_issue_and_no_other_team() {
        let mut fx = Fx::new();
        fx.issues = vec![issue("i1", "a", "la"), issue("i2", "a", "la"), issue("i3", "b", "lb")];
        assert_eq!(
            fx.run(vec![cfg("g", vec![team_src(1, true)])], Some(99)),
            vec![ids(&["i1", "i2"])]
        );
    }

    #[test]
    fn assigned_source_pulls_only_my_issues_across_member_rows() {
        let mut fx = Fx::new();
        let mut mine = issue("mine", "a", "la");
        mine.assignee_id = Some("ma".into());
        let mut theirs = issue("theirs", "a", "la");
        theirs.assignee_id = Some("other".into());
        let mut mine_b = issue("mine_b", "b", "lb");
        mine_b.assignee_id = Some("mb".into());
        fx.issues = vec![mine, theirs, issue("none", "a", "la"), mine_b];
        let got = fx.run(vec![cfg("g", vec![team_src(1, false), team_src(2, false)])], Some(99));
        assert_eq!(got, vec![ids(&["mine", "mine_b"])]);
        // Unknown user matches nothing.
        assert_eq!(fx.run(vec![cfg("g", vec![team_src(1, false)])], None), vec![ids(&[])]);
    }

    #[test]
    fn archived_and_unmirrored_teams_are_dropped() {
        let mut fx = Fx::new();
        fx.issues = vec![issue("i1", "a", "la"), issue("x", "arch", "larch")];
        let groups = vec![
            cfg("mixed", vec![team_src(1, true), team_src(3, true)]),
            cfg("only_archived", vec![team_src(3, true)]),
            cfg("unmirrored", vec![team_src(77, true)]),
            cfg("no_sources", vec![]),
        ];
        let ev = fx.eval(&groups, Some(99));
        let got: Vec<Vec<&str>> = ev
            .groups
            .iter()
            .map(|g| g.issues.iter().map(|i| i.id.as_str()).collect())
            .collect();
        assert_eq!(got, vec![vec!["i1"], vec![], vec![], vec![]]);
        let flags: Vec<bool> = ev.groups.iter().map(|g| g.inaccessible).collect();
        assert_eq!(flags, [false, true, true, false]);
    }

    #[test]
    fn only_active_issues_show() {
        let mut fx = Fx::new();
        let mut archived = issue("archived", "a", "la");
        archived.archived_at = Some("2026-01-01".into());
        let mut completed = issue("completed", "a", "la");
        completed.completed_at = Some("2026-01-01".into());
        let mut canceled = issue("canceled", "a", "la");
        canceled.canceled_at = Some("2026-01-01".into());
        fx.issues = vec![
            issue("live", "a", "la"),
            archived,
            completed,
            canceled,
            issue("in_done_lane", "a", "done"),
            issue("in_canceled_lane", "a", "canc"),
            issue("unknown_lane", "a", "ghost"),
        ];
        assert_eq!(
            fx.run(vec![cfg("g", vec![team_src(1, true)])], None),
            vec![ids(&["live", "unknown_lane"])]
        );
    }

    #[test]
    fn issue_in_two_groups_appears_in_both_but_once_per_group() {
        let mut fx = Fx::new();
        let mut i = issue("i1", "a", "la");
        i.assignee_id = Some("ma".into());
        fx.issues = vec![i];
        let got = fx.run(
            vec![
                cfg("g1", vec![team_src(1, true)]),
                cfg("g2", vec![team_src(1, false)]),
                cfg("g3", vec![team_src(1, true), team_src(1, false)]),
            ],
            Some(99),
        );
        assert_eq!(got, vec![ids(&["i1"]), ids(&["i1"]), ids(&["i1"])]);
    }

    #[test]
    fn groups_keep_input_order() {
        let mut fx = Fx::new();
        fx.issues = vec![issue("i1", "a", "la")];
        let got = fx.run(vec![cfg("z", vec![]), cfg("a", vec![team_src(1, true)])], None);
        assert_eq!(got, vec![ids(&[]), ids(&["i1"])]);
    }

    #[test]
    fn sorted_by_effective_due_then_priority_then_id() {
        let mut fx = Fx::new();
        fx.projects = vec![project("p_early", "a", Some("2026-11-02")), project("p_none", "a", None)];
        let mk = |id: &str, due: Option<&str>, prio: i32, proj: Option<&str>| {
            let mut i = issue(id, "a", "la");
            i.due_date = due.map(String::from);
            i.priority = prio;
            i.project_id = proj.map(String::from);
            i
        };
        fx.issues = vec![
            mk("undated_urgent", None, 0, None),
            mk("late", Some("2026-12-01"), 0, None),
            mk("own_beats_project", Some("2026-11-10T09:00:00Z"), 2, Some("p_early")),
            mk("via_project", None, 3, Some("p_early")),
            mk("via_project_hi", None, 1, Some("p_early")),
            mk("project_undated", None, 4, Some("p_none")),
            mk("tie_b", Some("2026-11-05"), 2, None),
            mk("tie_a", Some("2026-11-05"), 2, None),
        ];
        assert_eq!(
            fx.run(vec![cfg("g", vec![team_src(1, true)])], None),
            vec![ids(&[
                "via_project_hi",
                "via_project",
                "tie_a",
                "tie_b",
                "own_beats_project",
                "late",
                "undated_urgent",
                "project_undated",
            ])]
        );
    }

    #[test]
    fn effective_due_prefers_own_date() {
        let projects = vec![project("p", "a", Some("2026-02-02"))];
        let mut i = issue("i", "a", "la");
        i.project_id = Some("p".into());
        assert_eq!(effective_due(&i, &projects), NaiveDate::from_ymd_opt(2026, 2, 2));
        i.due_date = Some("2026-01-01".into());
        assert_eq!(effective_due(&i, &projects), NaiveDate::from_ymd_opt(2026, 1, 1));
    }

    fn filter_ids(fx: &Fx, filter: Filter) -> Vec<String> {
        let groups = vec![cfg("g", vec![team_src(1, true)])];
        let header = Header { filter, ..Header::default() };
        let mut v: Vec<String> = fx.eval_with(&groups, None, header).groups[0]
            .issues
            .iter()
            .map(|i| i.id.clone())
            .collect();
        v.sort();
        v
    }

    fn dated(id: &str, due: Option<&str>, prio: i32) -> Issue {
        let mut i = issue(id, "a", "la");
        i.due_date = due.map(String::from);
        i.priority = prio;
        i
    }

    // Fixture today is 2026-11-01.
    fn filter_fixture() -> Fx {
        let mut fx = Fx::new();
        fx.projects = vec![project("p_due", "a", Some("2026-11-03"))];
        let mut via_project = dated("via_project", None, 3);
        via_project.project_id = Some("p_due".into());
        fx.issues = vec![
            dated("yesterday", Some("2026-10-31"), 3),
            dated("today", Some("2026-11-01"), 3),
            dated("plus6", Some("2026-11-07"), 3),
            dated("plus7", Some("2026-11-08"), 3),
            dated("undated", None, 3),
            dated("hot_undated", None, 1),
            dated("urgent_far", Some("2027-01-01"), 0),
            via_project,
        ];
        fx
    }

    fn label(id: &str, team: &str, remote: i64) -> Label {
        Label {
            id: id.into(),
            remote_id: Some(remote),
            team_id: team.into(),
            name: id.into(),
            color: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn label_src(remote: i64) -> GroupSource {
        GroupSource { source_type: SourceType::Label, remote_id: remote, include_all: false }
    }

    fn label_fx() -> Fx {
        let mut fx = Fx::new();
        fx.labels = vec![
            label("bug_a", "a", 10),
            label("ui_a", "a", 11),
            label("bug_b", "b", 12),
            label("bug_arch", "arch", 13),
        ];
        let mut mine = issue("mine", "a", "la");
        mine.assignee_id = Some("ma".into());
        fx.issues = vec![
            mine,
            issue("theirs", "a", "la"),
            issue("plain", "a", "la"),
            issue("both", "a", "la"),
            issue("on_b", "b", "lb"),
            issue("on_arch", "arch", "larch"),
        ];
        fx.issue_labels.insert("mine".into(), vec![fx.labels[0].clone()]);
        fx.issue_labels.insert("theirs".into(), vec![fx.labels[1].clone()]);
        fx.issue_labels.insert("both".into(), vec![fx.labels[0].clone(), fx.labels[1].clone()]);
        fx.issue_labels.insert("on_b".into(), vec![fx.labels[2].clone()]);
        fx.issue_labels.insert("on_arch".into(), vec![fx.labels[3].clone()]);
        fx
    }

    fn proj_src(remote: i64, all: bool) -> GroupSource {
        GroupSource { source_type: SourceType::Project, remote_id: remote, include_all: all }
    }

    /// Fixture with projects p1 (team a, remote 10) and p2 (team arch, remote 11).
    fn project_fx() -> Fx {
        let mut fx = Fx::new();
        let mut p1 = project("p1", "a", None);
        p1.remote_id = Some(10);
        let mut p2 = project("p2", "arch", None);
        p2.remote_id = Some(11);
        fx.projects = vec![p1, p2];
        let mut mine = issue("mine", "a", "la");
        mine.project_id = Some("p1".into());
        mine.assignee_id = Some("ma".into());
        let mut theirs = issue("theirs", "a", "la");
        theirs.project_id = Some("p1".into());
        theirs.assignee_id = Some("other".into());
        let mut outside = issue("outside", "a", "la");
        outside.assignee_id = Some("ma".into());
        let mut archived_proj = issue("ap", "arch", "larch");
        archived_proj.project_id = Some("p2".into());
        fx.issues = vec![mine, theirs, outside, archived_proj];
        fx
    }

    #[test]
    fn today_tab_is_the_default_and_includes_overdue() {
        assert_eq!(Header::default().filter, Filter::Today);
        assert_eq!(filter_ids(&filter_fixture(), Filter::Today), ids(&["today", "yesterday"]));
    }

    #[test]
    fn week_tab_stops_at_today_plus_six() {
        assert_eq!(
            filter_ids(&filter_fixture(), Filter::Week),
            ids(&["plus6", "today", "via_project", "yesterday"])
        );
    }

    #[test]
    fn hot_tab_is_urgent_and_high_whatever_the_date() {
        assert_eq!(filter_ids(&filter_fixture(), Filter::Hot), ids(&["hot_undated", "urgent_far"]));
    }

    #[test]
    fn open_tab_has_no_date_or_priority_condition() {
        assert_eq!(filter_ids(&filter_fixture(), Filter::Open).len(), 8);
    }

    #[test]
    fn project_due_date_fallback_counts_for_week_but_not_today() {
        let ids_today = filter_ids(&filter_fixture(), Filter::Today);
        assert!(!ids_today.contains(&"via_project".to_string()));
        assert!(filter_ids(&filter_fixture(), Filter::Week).contains(&"via_project".to_string()));
    }

    #[test]
    fn labels_only_group_pulls_every_labeled_issue_on_label_teams() {
        let fx = label_fx();
        let got = fx.run(vec![cfg("g", vec![label_src(10), label_src(11)])], Some(99));
        assert_eq!(got, vec![ids(&["both", "mine", "theirs"])]);
    }

    #[test]
    fn labels_narrow_team_sources_to_issues_carrying_any_label() {
        let fx = label_fx();
        let got = fx.run(vec![cfg("g", vec![team_src(1, true), label_src(10)])], Some(99));
        assert_eq!(got, vec![ids(&["both", "mine"])]);
        // Assigned-to-me team source: only my labeled issues.
        let srcs = vec![team_src(1, false), label_src(10), label_src(11)];
        assert_eq!(fx.run(vec![cfg("g", srcs)], Some(99)), vec![ids(&["mine"])]);
    }

    #[test]
    fn issue_with_several_listed_labels_appears_once() {
        let fx = label_fx();
        let got = fx.run(vec![cfg("g", vec![label_src(10), label_src(11)])], Some(99));
        assert_eq!(got[0].iter().filter(|i| *i == "both").count(), 1);
        let srcs = vec![team_src(1, true), label_src(10), label_src(11)];
        let got = fx.run(vec![cfg("g", srcs)], Some(99));
        assert_eq!(got[0].iter().filter(|i| *i == "both").count(), 1);
    }

    #[test]
    fn label_on_inaccessible_team_or_unmirrored_is_dropped() {
        let fx = label_fx();
        let groups = vec![
            cfg("only_arch", vec![label_src(13)]),
            cfg("unknown", vec![label_src(999)]),
            cfg("mixed", vec![label_src(13), label_src(12)]),
        ];
        let ev = fx.eval(&groups, Some(99));
        assert!(ev.groups[0].inaccessible && ev.groups[0].issues.is_empty());
        assert!(ev.groups[1].inaccessible && ev.groups[1].issues.is_empty());
        assert!(!ev.groups[2].inaccessible);
        let got: Vec<&str> = ev.groups[2].issues.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(got, vec!["on_b"]);
    }

    #[test]
    fn include_all_project_source_pulls_every_project_issue() {
        let fx = project_fx();
        let got = fx.run(vec![cfg("g", vec![proj_src(10, true)])], Some(99));
        let mut g = got[0].clone();
        g.sort();
        assert_eq!(g, ids(&["mine", "theirs"]));
    }

    #[test]
    fn assigned_project_source_pulls_only_my_project_issues() {
        let fx = project_fx();
        let got = fx.run(vec![cfg("g", vec![proj_src(10, false)])], Some(99));
        assert_eq!(got, vec![ids(&["mine"])]);
    }

    #[test]
    fn project_on_inaccessible_team_is_dropped_and_group_flagged() {
        let fx = project_fx();
        let groups = vec![cfg("g", vec![proj_src(11, true)]), cfg("h", vec![proj_src(999, true)])];
        let ev = fx.eval(&groups, Some(99));
        assert!(ev.groups.iter().all(|g| g.issues.is_empty() && g.inaccessible));
    }

    #[test]
    fn project_and_team_sources_union() {
        let fx = project_fx();
        // Team a assigned-to-me gives mine + outside; project all adds theirs.
        let got = fx.run(vec![cfg("g", vec![team_src(1, false), proj_src(10, true)])], Some(99));
        let mut g = got[0].clone();
        g.sort();
        assert_eq!(g, ids(&["mine", "outside", "theirs"]));
    }

    #[test]
    fn dropped_project_alongside_live_team_source_is_not_inaccessible() {
        let fx = project_fx();
        let groups = vec![cfg("g", vec![team_src(1, true), proj_src(11, true)])];
        let ev = fx.eval(&groups, Some(99));
        assert!(!ev.groups[0].inaccessible);
        assert_eq!(ev.groups[0].issues.len(), 3);
    }

    #[test]
    fn project_source_with_label_source_narrows_instead_of_labels_only() {
        let mut fx = project_fx();
        fx.labels = vec![label("bug_a", "a", 20)];
        fx.issue_labels.insert("mine".into(), vec![fx.labels[0].clone()]);
        // "outside" carries the label too but is not in project p1: a
        // labels-only group would pull it in, a narrowing one must not.
        fx.issue_labels.insert("outside".into(), vec![fx.labels[0].clone()]);
        let got = fx.run(vec![cfg("g", vec![proj_src(10, true), label_src(20)])], Some(99));
        assert_eq!(got, vec![ids(&["mine"])]);
    }

    fn counts(fx: &Fx, groups: Vec<GroupConfig>, header: Header) -> (usize, usize) {
        let e = fx.eval_with(&groups, Some(99), header);
        (e.due_today, e.overdue)
    }

    fn count_fx() -> Fx {
        let mut fx = Fx::new();
        let mut a = dated("today", Some("2026-11-01"), 3);
        let mut b = dated("late", Some("2026-10-30"), 3);
        let mut c = dated("soon", Some("2026-11-03"), 3);
        let d = dated("none", None, 3);
        for i in [&mut a, &mut b, &mut c] {
            i.team_id = "a".into();
            i.lane_id = "la".into();
        }
        let mut d = d;
        d.team_id = "a".into();
        d.lane_id = "la".into();
        a.title = "findme".into();
        fx.issues = vec![a, b, c, d];
        fx
    }

    #[test]
    fn counts_split_today_and_overdue_by_effective_due() {
        let fx = count_fx();
        let g = vec![cfg("g", vec![team_src(1, true)])];
        assert_eq!(counts(&fx, g, Header::default()), (1, 1));
    }

    #[test]
    fn counts_dedupe_issue_in_several_groups() {
        let fx = count_fx();
        let g = vec![cfg("g1", vec![team_src(1, true)]), cfg("g2", vec![team_src(1, true)])];
        assert_eq!(counts(&fx, g, Header::default()), (1, 1));
    }

    #[test]
    fn counts_ignore_the_filter_tab() {
        let fx = count_fx();
        for filter in Filter::ALL {
            let g = vec![cfg("g", vec![team_src(1, true)])];
            let h = Header { filter, ..Header::default() };
            assert_eq!(counts(&fx, g, h), (1, 1), "{filter:?}");
        }
        // Hot shows no rows (all priority 3) yet the counts stay.
        let g = vec![cfg("g", vec![team_src(1, true)])];
        let e = fx.eval_with(&g, Some(99), Header { filter: Filter::Hot, ..Header::default() });
        assert!(e.groups[0].issues.is_empty());
        assert_eq!(e.due_today, 1);
    }

    #[test]
    fn counts_skip_closed_issues() {
        let mut fx = count_fx();
        fx.issues[0].lane_id = "done".into();
        let g = vec![cfg("g", vec![team_src(1, true)])];
        assert_eq!(counts(&fx, g, Header::default()), (0, 1));
    }
}
