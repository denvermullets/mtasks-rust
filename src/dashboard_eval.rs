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
    // Read by the label-source and label-picker tickets.
    #[allow(dead_code)]
    pub labels: &'a [Label],
    pub lanes: &'a [Lane],
    pub members: &'a [Member],
    pub issues: &'a [Issue],
    /// Labels per issue local id.
    #[allow(dead_code)]
    pub issue_labels: &'a HashMap<String, Vec<Label>>,
}

/// The lens over each group's base set. Only Open exists so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Open,
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
    // Read by the filter-tab ticket.
    #[allow(dead_code)]
    pub filter: Filter,
    pub sort: Sort,
}

pub struct Input<'a> {
    pub groups: &'a [GroupConfig],
    pub mirror: Mirror<'a>,
    /// Rails user id of the configured current user.
    pub current_user_remote_id: Option<i64>,
    /// Machine-local date. Read by the date-based filter tabs and counts.
    #[allow(dead_code)]
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

    let groups = input
        .groups
        .iter()
        .map(|cfg| {
            let pool = pool(cfg, m, &accessible, &my_members);
            let mut issues: Vec<&Issue> = m
                .issues
                .iter()
                .filter(|i| pool.contains(i.id.as_str()) && is_active(i, m.lanes))
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
    Evaluation { groups }
}

/// The local team behind a team source, if mirrored.
fn team_of_source<'a>(s: &GroupSource, m: &Mirror<'a>) -> Option<&'a Team> {
    m.teams.iter().find(|t| t.remote_id == Some(s.remote_id))
}

/// A source pointing at something outside the accessible teams. Only team
/// sources are judged so far; project and label sources arrive with their tickets.
fn dropped(s: &GroupSource, m: &Mirror<'_>, accessible: &HashSet<&str>) -> bool {
    match s.source_type {
        SourceType::Team => {
            !team_of_source(s, m).is_some_and(|t| accessible.contains(t.id.as_str()))
        }
        SourceType::Project | SourceType::Label => false,
    }
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
    out
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
                header: Header::default(),
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
}
