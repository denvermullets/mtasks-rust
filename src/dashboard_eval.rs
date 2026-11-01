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
            Filter::Today => "Due today",
            Filter::Week => "This week",
            Filter::Hot => "Urgent / High",
            Filter::Open => "All open",
        }
    }
}

/// Row order within a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Due,
    Priority,
    Updated,
    Created,
}

impl Sort {
    pub const ALL: [Sort; 4] = [Sort::Due, Sort::Priority, Sort::Updated, Sort::Created];
}

impl std::fmt::Display for Sort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Sort::Due => "Sort: Due date",
            Sort::Priority => "Sort: Priority",
            Sort::Updated => "Sort: Recently updated",
            Sort::Created => "Sort: Newest",
        })
    }
}

/// The header state. Held in memory per dashboard; not persisted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Header {
    pub filter: Filter,
    pub sort: Sort,
    /// Raw view-search text. Narrows rows only (never counts); matched like
    /// the other lists: title, or team identifier plus number.
    pub search: String,
    /// Local id of the team the picker narrows to. A value that isn't one of
    /// the dashboard's source teams is ignored (see `source_teams`).
    pub team: Option<String>,
    /// Mine: only issues assigned to the current user (any of their member rows).
    pub mine: bool,
    /// Label name the picker narrows to, matched across the source teams. A
    /// name not in `Evaluation::label_names` is ignored.
    pub label: Option<String>,
    /// Lane name the status picker matches across the source teams. A name
    /// no source team has a lane for is ignored (see `status_options`).
    pub status: Option<String>,
    /// The assignee picker: one person (by Rails user id, across teams) or
    /// the unowned work.
    pub assignee: Option<AssigneeFilter>,
}

/// What the assignee picker narrows to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssigneeFilter {
    Unassigned,
    /// Rails user id.
    User(i64),
}

/// A person the assignee picker offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssigneeOption {
    pub remote_id: i64,
    pub name: String,
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
    pub inaccessible: bool,
    /// The group has no sources at all.
    pub no_sources: bool,
    /// Sorted issues.
    pub issues: Vec<&'a Issue>,
}

pub struct Evaluation<'a> {
    /// The date the evaluation ran for, so rows and counts agree on "today".
    pub today: NaiveDate,
    /// In the order of `Input::groups`.
    pub groups: Vec<GroupResult<'a>>,
    /// Distinct issues across all groups with effective due = today. Computed
    /// from the refined base sets, so it ignores the filter tab (and search).
    pub due_today: usize,
    /// Distinct issues across all groups with effective due < today.
    pub overdue: usize,
    /// The accessible teams behind the dashboard's sources (team sources,
    /// project sources' teams, label sources' teams), in mirror (sidebar)
    /// order. The team, assignee, label and status pickers read this.
    pub source_teams: Vec<&'a Team>,
    /// Distinct lane names on the source teams, in board order (teams in
    /// sidebar order, lanes by position). The status picker lists these.
    pub status_options: Vec<String>,
    /// Members of the source teams, deduplicated by Rails user id and sorted
    /// by name. The assignee picker adds "Unassigned" itself.
    pub assignees: Vec<AssigneeOption>,
    /// Distinct, sorted label names on the source teams. The label picker reads this.
    pub label_names: Vec<String>,
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

    let source_teams = source_teams(input.groups, m, &accessible);
    let assignees = assignee_options(&source_teams, m.members);
    let valid_team = input
        .header
        .team
        .as_deref()
        .filter(|id| source_teams.iter().any(|t| t.id == *id))
        .map(str::to_string);
    let label_names = label_names(m, &source_teams);
    let valid_label = input
        .header
        .label
        .as_deref()
        .filter(|n| label_names.iter().any(|x| x == n))
        .map(str::to_string);
    let status_options = status_options(&source_teams, m.lanes);
    let valid_status = input
        .header
        .status
        .as_deref()
        .filter(|s| status_options.iter().any(|o| o == s))
        .map(str::to_string);
    let input = &Input {
        groups: input.groups,
        mirror: input.mirror,
        current_user_remote_id: input.current_user_remote_id,
        today: input.today,
        header: Header {
            team: valid_team,
            label: valid_label,
            status: valid_status,
            ..input.header.clone()
        },
    };
    let source_team_ids: HashSet<&str> = source_teams.iter().map(|t| t.id.as_str()).collect();
    // Ids across all groups' refined base sets, deduplicated for the counts.
    let mut counted: HashSet<&str> = HashSet::new();
    let needle = crate::search_match::needle(&input.header.search);
    let groups = input
        .groups
        .iter()
        .map(|cfg| {
            let pool = pool(cfg, m, &accessible, &my_members);
            let pool = apply_label_sources(cfg, m, &accessible, pool);
            // The refined base set: counts read this, rows narrow it further.
            let base = base_set(input, &pool, &my_members, &source_team_ids);
            counted.extend(base.iter().map(|i| i.id.as_str()));
            let mut issues: Vec<&Issue> = base
                .into_iter()
                .filter(|i| passes_filter(i, input.header.filter, input.today, m.projects))
                .filter(|i| matches_search(i, m.teams, &needle))
                .collect();
            sort(&mut issues, input.header.sort, m.projects);
            GroupResult {
                group: &cfg.group,
                inaccessible: is_inaccessible(cfg, m, &accessible),
                no_sources: cfg.sources.is_empty(),
                issues,
            }
        })
        .collect();
    let (due_today, overdue) = due_counts(&counted, m, input.today);
    Evaluation { today: input.today, groups, due_today, overdue, source_teams, assignees, label_names, status_options }
}

/// A group's pool narrowed to the starting set rows and counts both read.
/// Refinements (Mine, team, assignee, label, status) plug in here, so rows and
/// counts narrow together. The status picker swaps `is_active` for a
/// lane-name match in this function.
fn base_set<'a>(
    input: &Input<'a>,
    pool: &HashSet<&'a str>,
    my_members: &HashSet<&str>,
    source_team_ids: &HashSet<&str>,
) -> Vec<&'a Issue> {
    let m = &input.mirror;
    m.issues
        .iter()
        .filter(|i| pool.contains(i.id.as_str()))
        .filter(|i| passes_start(i, input.header.status.as_deref(), m.lanes, source_team_ids))
        .filter(|i| passes_mine(i, input.header.mine, my_members))
        .filter(|i| passes_team(i, input.header.team.as_deref()))
        .filter(|i| passes_assignee(i, input.header.assignee, m.members))
        .filter(|i| passes_label(i, input.header.label.as_deref(), m, source_team_ids))
        .collect()
}


/// The starting set. Without a status: active issues. With one: non-archived
/// issues in any lane of that name on the source teams, closed lanes included.
fn passes_start(
    i: &Issue,
    status: Option<&str>,
    lanes: &[Lane],
    source_team_ids: &HashSet<&str>,
) -> bool {
    let Some(name) = status else { return is_active(i, lanes) };
    i.archived_at.is_none()
        && lanes.iter().any(|l| {
            l.id == i.lane_id && l.name == name && source_team_ids.contains(l.team_id.as_str())
        })
}

/// Distinct lane names on the source teams, in board order.
fn status_options(source_teams: &[&Team], lanes: &[Lane]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in source_teams {
        let mut ls: Vec<&Lane> = lanes.iter().filter(|l| l.team_id == t.id).collect();
        ls.sort_by_key(|l| l.position);
        for l in ls {
            if !out.contains(&l.name) {
                out.push(l.name.clone());
            }
        }
    }
    out
}

/// Mine off keeps everything; on keeps issues assigned to one of my member rows.
fn passes_mine(i: &Issue, mine: bool, my_members: &HashSet<&str>) -> bool {
    !mine || i.assignee_id.as_deref().is_some_and(|a| my_members.contains(a))
}


/// The team picker: no team means no narrowing. `evaluate` has already
/// dropped a value that isn't a source team.
fn passes_team(i: &Issue, team: Option<&str>) -> bool {
    team.is_none_or(|id| i.team_id == id)
}

/// The assignee picker: a person matches issues assigned to any of their
/// member rows (any team); Unassigned matches issues with no assignee.
fn passes_assignee(i: &Issue, pick: Option<AssigneeFilter>, members: &[Member]) -> bool {
    match pick {
        None => true,
        Some(AssigneeFilter::Unassigned) => i.assignee_id.is_none(),
        Some(AssigneeFilter::User(uid)) => i.assignee_id.as_deref().is_some_and(|a| {
            members.iter().any(|m| m.id == a && m.remote_id == Some(uid))
        }),
    }
}

/// Members of the source teams, one per Rails user id, sorted by name.
fn assignee_options(teams: &[&Team], members: &[Member]) -> Vec<AssigneeOption> {
    let mut out: Vec<AssigneeOption> = Vec::new();
    for m in members.iter().filter(|m| teams.iter().any(|t| t.id == m.team_id)) {
        let Some(remote_id) = m.remote_id else { continue };
        if !out.iter().any(|o| o.remote_id == remote_id) {
            out.push(AssigneeOption { remote_id, name: m.name.clone() });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.remote_id.cmp(&b.remote_id)));
    out
}

/// Distinct label names on the source teams, sorted.
fn label_names(m: &Mirror<'_>, source_teams: &[&Team]) -> Vec<String> {
    let mut names: Vec<String> = m
        .labels
        .iter()
        .filter(|l| source_teams.iter().any(|t| t.id == l.team_id))
        .map(|l| l.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The label picker: keeps issues carrying a label of that name on a source
/// team. No name means no narrowing. A bool, so an issue is never repeated.
fn passes_label(i: &Issue, name: Option<&str>, m: &Mirror<'_>, source_ids: &HashSet<&str>) -> bool {
    name.is_none_or(|n| {
        m.issue_labels.get(&i.id).is_some_and(|ls| {
            ls.iter().any(|l| l.name == n && source_ids.contains(l.team_id.as_str()))
        })
    })
}

/// Accessible teams behind the sources of every group, in mirror order.
fn source_teams<'a>(
    groups: &[GroupConfig],
    m: &Mirror<'a>,
    accessible: &HashSet<&str>,
) -> Vec<&'a Team> {
    let mut ids: HashSet<&str> = HashSet::new();
    for s in groups.iter().flat_map(|g| g.sources.iter()) {
        match s.source_type {
            SourceType::Team => ids.extend(team_of_source(s, m).map(|t| t.id.as_str())),
            SourceType::Project => {
                ids.extend(project_of_source(s, m, accessible).map(|p| p.team_id.as_str()))
            }
            SourceType::Label => {
                ids.extend(label_of_source(s, m, accessible).map(|l| l.team_id.as_str()))
            }
        }
    }
    m.teams.iter().filter(|t| accessible.contains(t.id.as_str()) && ids.contains(t.id.as_str())).collect()
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

/// The view search: title text, or a team identifier plus number.
fn matches_search(i: &Issue, teams: &[Team], needle: &str) -> bool {
    let team = teams.iter().find(|t| t.id == i.team_id);
    crate::search_match::issue_matches(i, team, needle)
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

/// A group is inaccessible when, for either kind (team/project; label), it has
/// sources of that kind and every one was dropped. A kind with none doesn't count.
fn is_inaccessible(cfg: &GroupConfig, m: &Mirror<'_>, accessible: &HashSet<&str>) -> bool {
    let all_dropped = |label: bool| {
        let kind: Vec<&GroupSource> = cfg
            .sources
            .iter()
            .filter(|s| (s.source_type == SourceType::Label) == label)
            .collect();
        !kind.is_empty() && kind.iter().all(|s| dropped(s, m, accessible))
    };
    all_dropped(false) || all_dropped(true)
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

/// A dashboard row's due cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueLabel {
    pub text: String,
    pub overdue: bool,
    /// The date came from the issue's project, not the issue.
    pub inherited: bool,
}

/// The due cell for `i` (web `dashboard_due_label`), or `None` without a date.
pub fn due_label(i: &Issue, projects: &[Project], today: NaiveDate) -> Option<DueLabel> {
    let due = effective_due(i, projects)?;
    let (text, overdue) = if due == today {
        ("Today".to_string(), false)
    } else if due < today {
        (format!("Overdue \u{b7} {}d", (today - due).num_days()), true)
    } else {
        (due.format("%a %b %-d").to_string(), false)
    };
    let inherited = i.due_date.as_deref().and_then(parse_day).is_none();
    Some(DueLabel { text, overdue, inherited })
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
        // Urgent first, then effective due (undated last), then id.
        Sort::Priority => issues.sort_by_cached_key(|i| {
            let due = effective_due(i, projects);
            (i.priority, due.is_none(), due, i.id.clone())
        }),
        // Newest first, then id.
        Sort::Updated => issues
            .sort_by_cached_key(|i| (std::cmp::Reverse(i.updated_at.clone()), i.id.clone())),
        Sort::Created => issues
            .sort_by_cached_key(|i| (std::cmp::Reverse(i.created_at.clone()), i.id.clone())),
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

    #[test]
    fn due_label_matches_the_web() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let projects = [project("p", "t", Some("2026-11-02"))];
        let label = |due: Option<&str>, project: Option<&str>| {
            let mut i = issue("i", "t", "l");
            i.due_date = due.map(str::to_string);
            i.project_id = project.map(str::to_string);
            due_label(&i, &projects, today)
        };
        assert_eq!(label(None, None), None);
        let overdue = label(Some("2026-10-31"), None).unwrap();
        assert_eq!((overdue.text.as_str(), overdue.overdue, overdue.inherited), ("Overdue \u{b7} 2d", true, false));
        let upcoming = label(Some("2026-11-09"), None).unwrap();
        assert_eq!((upcoming.text.as_str(), upcoming.overdue), ("Mon Nov 9", false));
        let inherited = label(None, Some("p")).unwrap();
        assert_eq!((inherited.text.as_str(), inherited.inherited), ("Today", true));
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

    fn run_sorted(fx: &Fx, sort: Sort) -> Vec<String> {
        let groups = vec![cfg("g", vec![team_src(1, true)])];
        let ev = evaluate(&Input {
            groups: &groups,
            mirror: Mirror {
                teams: &fx.teams,
                projects: &fx.projects,
                labels: &fx.labels,
                lanes: &fx.lanes,
                members: &fx.members,
                issues: &fx.issues,
                issue_labels: &fx.issue_labels,
            },
            current_user_remote_id: None,
            today: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            header: Header { filter: Filter::Open, sort, ..Header::default() },
        });
        ev.groups[0].issues.iter().map(|i| i.id.clone()).collect()
    }

    fn sort_fx() -> Fx {
        let mut fx = Fx::new();
        fx.projects = vec![project("p", "a", Some("2026-11-03"))];
        let mk = |id: &str, due: Option<&str>, pri: i32, proj: Option<&str>, c: &str, u: &str| {
            let mut i = issue(id, "a", "la");
            i.due_date = due.map(String::from);
            i.priority = pri;
            i.project_id = proj.map(String::from);
            i.created_at = c.into();
            i.updated_at = u.into();
            i
        };
        fx.issues = vec![
            mk("late_urgent", Some("2026-11-09"), 0, None, "2026-01-03", "2026-02-01"),
            mk("soon_low", Some("2026-11-02"), 3, None, "2026-01-01", "2026-02-04"),
            mk("soon_high", Some("2026-11-02"), 1, None, "2026-01-02", "2026-02-02"),
            mk("via_proj", None, 2, Some("p"), "2026-01-05", "2026-02-03"),
            mk("undated_urgent", None, 0, None, "2026-01-04", "2026-02-05"),
            mk("undated_low", None, 3, None, "2026-01-06", "2026-02-06"),
        ];
        fx
    }

    #[test]
    fn sort_due_is_dated_first_then_priority() {
        assert_eq!(
            run_sorted(&sort_fx(), Sort::Due),
            ids(&["soon_high", "soon_low", "via_proj", "late_urgent", "undated_urgent", "undated_low"])
        );
    }

    #[test]
    fn sort_priority_then_due_then_id() {
        let mut fx = sort_fx();
        let mut tie = issue("a_tie", "a", "la");
        tie.priority = 0;
        tie.due_date = Some("2026-11-09".into());
        fx.issues.push(tie);
        assert_eq!(
            run_sorted(&fx, Sort::Priority),
            ids(&[
                "a_tie",
                "late_urgent",
                "undated_urgent",
                "soon_high",
                "via_proj",
                "soon_low",
                "undated_low"
            ])
        );
    }

    #[test]
    fn sort_updated_and_created_newest_first_then_id() {
        let mut fx = sort_fx();
        assert_eq!(
            run_sorted(&fx, Sort::Updated),
            ids(&["undated_low", "undated_urgent", "soon_low", "via_proj", "soon_high", "late_urgent"])
        );
        assert_eq!(
            run_sorted(&fx, Sort::Created),
            ids(&["undated_low", "via_proj", "undated_urgent", "late_urgent", "soon_high", "soon_low"])
        );
        for i in &mut fx.issues {
            i.updated_at = "same".into();
            i.created_at = "same".into();
        }
        let by_id =
            ids(&["late_urgent", "soon_high", "soon_low", "undated_low", "undated_urgent", "via_proj"]);
        assert_eq!(run_sorted(&fx, Sort::Updated), by_id);
        assert_eq!(run_sorted(&fx, Sort::Created), by_id);
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

    /// Issue ids per group with `search` typed into the header.
    fn run_search(fx: &Fx, groups: &[GroupConfig], search: &str) -> Vec<Vec<String>> {
        let header = Header { filter: Filter::Open, search: search.into(), ..Header::default() };
        evaluate(&Input {
            groups,
            mirror: Mirror {
                teams: &fx.teams,
                projects: &fx.projects,
                labels: &fx.labels,
                lanes: &fx.lanes,
                members: &fx.members,
                issues: &fx.issues,
                issue_labels: &fx.issue_labels,
            },
            current_user_remote_id: Some(99),
            today: NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            header,
        })
        .groups
        .iter()
        .map(|g| g.issues.iter().map(|i| i.id.clone()).collect())
        .collect()
    }

    fn search_fixture() -> (Fx, Vec<GroupConfig>) {
        let mut fx = Fx::new();
        let mut a12 = issue("a12", "a", "la");
        a12.team_number = Some(12);
        a12.title = "Fix login".into();
        let mut a3 = issue("a3", "a", "la");
        a3.team_number = Some(3);
        a3.title = "Login page".into();
        let mut b12 = issue("b12", "b", "lb");
        b12.team_number = Some(12);
        b12.title = "Other".into();
        fx.issues = vec![a12, a3, b12];
        let groups = vec![cfg("g", vec![team_src(1, true), team_src(2, true)])];
        (fx, groups)
    }

    #[test]
    fn search_matches_title_across_teams() {
        let (fx, groups) = search_fixture();
        assert_eq!(run_search(&fx, &groups, "LOGIN"), vec![ids(&["a12", "a3"])]);
    }

    #[test]
    fn search_matches_team_identifier_and_number() {
        let (fx, groups) = search_fixture();
        assert_eq!(run_search(&fx, &groups, "a-12"), vec![ids(&["a12"])]);
        assert_eq!(run_search(&fx, &groups, " B-12 "), vec![ids(&["b12"])]);
        assert_eq!(run_search(&fx, &groups, "-12"), vec![ids(&["a12", "b12"])]);
    }

    #[test]
    fn empty_search_restores_all_rows_and_applies_to_every_group() {
        let (fx, mut groups) = search_fixture();
        groups.push(cfg("g2", vec![team_src(1, true)]));
        assert_eq!(run_search(&fx, &groups, "a-3"), vec![ids(&["a3"]), ids(&["a3"])]);
        assert_eq!(run_search(&fx, &groups, "  "), vec![ids(&["a12", "a3", "b12"]), ids(&["a12", "a3"])]);
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

    #[test]
    fn inaccessible_when_one_kind_lost_access_even_with_other_kind() {
        let fx = label_fx();
        let groups = vec![
            // Live team source, but its only label source is on an archived team.
            cfg("labels_lost", vec![team_src(1, true), label_src(13)]),
            // Live label source, but its only team source is unmirrored.
            cfg("teams_lost", vec![team_src(77, true), label_src(10)]),
            cfg("all_live", vec![team_src(1, true), label_src(10)]),
        ];
        let ev = fx.eval(&groups, Some(99));
        let flags: Vec<bool> = ev.groups.iter().map(|g| g.inaccessible).collect();
        assert_eq!(flags, [true, true, false]);
        assert!(ev.groups[0].issues.is_empty());
    }

    #[test]
    fn project_kind_lost_with_live_label_is_inaccessible() {
        let mut fx = project_fx();
        fx.labels = vec![label("bug_a", "a", 20)];
        let groups = vec![cfg("g", vec![proj_src(11, true), label_src(20)])];
        assert!(fx.eval(&groups, Some(99)).groups[0].inaccessible);
    }

    #[test]
    fn no_sources_and_reachable_but_empty_are_distinct_states() {
        let fx = Fx::new();
        let groups = vec![cfg("none", vec![]), cfg("empty", vec![team_src(1, true)])];
        let ev = fx.eval(&groups, Some(99));
        assert!(ev.groups.iter().all(|g| !g.inaccessible && g.issues.is_empty()));
        assert!(ev.groups[0].no_sources);
        assert!(!ev.groups[1].no_sources);
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

    fn status_fx() -> Fx {
        let mut fx = Fx::new();
        fx.lanes = vec![
            named_lane("la", "a", "Todo", 0, "started"),
            named_lane("done", "a", "Done", 1, "completed"),
            named_lane("lb", "b", "Todo", 0, "started"),
            named_lane("bdone", "b", "Done", 1, "completed"),
            named_lane("larch", "arch", "Todo", 0, "started"),
        ];
        let mut ia = issue("ia", "a", "la");
        ia.due_date = Some("2026-10-30".into());
        let mut id = issue("id", "a", "done");
        id.due_date = Some("2026-11-01".into());
        let mut ib = issue("ib", "b", "bdone");
        ib.due_date = Some("2026-10-01".into());
        let mut ar = issue("iarch", "a", "done");
        ar.archived_at = Some("2026-10-01".into());
        fx.issues = vec![ia, id, ib, ar];
        fx
    }

    fn named_lane(id: &str, team: &str, name: &str, pos: i32, cat: &str) -> Lane {
        Lane { name: name.into(), position: pos, ..lane(id, team, cat) }
    }

    fn status_ids(fx: &Fx, status: &str, srcs: Vec<GroupSource>) -> (Vec<String>, usize, usize) {
        let groups = vec![cfg("g", srcs)];
        let header = Header {
            filter: Filter::Open,
            status: Some(status.into()),
            ..Header::default()
        };
        let ev = fx.eval_with(&groups, Some(99), header);
        let ids = ev.groups[0].issues.iter().map(|i| i.id.clone()).collect();
        (ids, ev.due_today, ev.overdue)
    }

    #[test]
    fn status_options_list_distinct_names_in_board_order() {
        let fx = status_fx();
        let groups = vec![cfg("g", vec![team_src(1, true), team_src(2, true)])];
        let ev = fx.eval(&groups, Some(99));
        assert_eq!(ev.status_options, ["Todo", "Done"]);
    }

    #[test]
    fn status_open_lane_matches_active_issues_in_it() {
        let fx = status_fx();
        let (ids, _, overdue) = status_ids(&fx, "Todo", vec![team_src(1, true)]);
        assert_eq!(ids, ["ia"]);
        assert_eq!(overdue, 1);
    }

    #[test]
    fn status_closed_lane_shows_finished_work_in_rows_and_counts() {
        let fx = status_fx();
        let (ids, due_today, overdue) = status_ids(&fx, "Done", vec![team_src(1, true)]);
        // Archived is still excluded.
        assert_eq!(ids, ["id"]);
        assert_eq!((due_today, overdue), (1, 0));
    }

    #[test]
    fn status_matches_by_name_across_teams() {
        let fx = status_fx();
        let (mut ids, _, overdue) =
            status_ids(&fx, "Done", vec![team_src(1, true), team_src(2, true)]);
        ids.sort();
        assert_eq!(ids, ["ib", "id"]);
        assert_eq!(overdue, 1);
    }

    #[test]
    fn unknown_status_is_ignored() {
        let fx = status_fx();
        let (ids, _, _) = status_ids(&fx, "Nope", vec![team_src(1, true)]);
        assert_eq!(ids, ["ia"]);
    }

    #[test]
    fn team_picker_narrows_rows_and_counts() {
        let mut fx = Fx::new();
        let mut a = issue("ia", "a", "la");
        a.due_date = Some("2026-10-30".into());
        let mut b = issue("ib", "b", "lb");
        b.due_date = Some("2026-10-30".into());
        fx.issues = vec![a, b];
        let groups = vec![cfg("g", vec![team_src(1, true), team_src(2, true)])];
        let header =
            Header { filter: Filter::Open, team: Some("b".into()), ..Header::default() };
        let ev = fx.eval_with(&groups, Some(99), header);
        let got: Vec<&str> = ev.groups[0].issues.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(got, ["ib"]);
        assert_eq!(ev.overdue, 1);
    }

    #[test]
    fn team_picker_ignores_a_bad_value() {
        let mut fx = Fx::new();
        fx.issues = vec![issue("ia", "a", "la"), issue("ib", "b", "lb")];
        // "b" exists but isn't behind this dashboard; "zzz" doesn't exist.
        let groups = vec![cfg("g", vec![team_src(1, true)])];
        for bad in ["b", "zzz"] {
            let header =
                Header { filter: Filter::Open, team: Some(bad.into()), ..Header::default() };
            let ev = fx.eval_with(&groups, Some(99), header);
            let got: Vec<&str> = ev.groups[0].issues.iter().map(|i| i.id.as_str()).collect();
            assert_eq!(got, ["ia"], "{bad}");
        }
    }

    fn assignee_fx() -> Fx {
        let mut fx = Fx::new();
        // User 7 is a member of both teams; user 99 is me.
        fx.members.push(member("m7a", "a", 7));
        fx.members.push(member("m7b", "b", 7));
        let mut i1 = issue("i1", "a", "la");
        i1.assignee_id = Some("m7a".into());
        let mut i2 = issue("i2", "b", "lb");
        i2.assignee_id = Some("m7b".into());
        let mut i3 = issue("i3", "a", "la");
        i3.assignee_id = Some("ma".into());
        let i4 = issue("i4", "b", "lb");
        fx.issues = vec![i1, i2, i3, i4];
        fx
    }

    fn assignee_ids(fx: &Fx, pick: AssigneeFilter) -> Vec<String> {
        let groups = vec![cfg("g", vec![team_src(1, true), team_src(2, true)])];
        let header = Header {
            filter: Filter::Open,
            assignee: Some(pick),
            ..Header::default()
        };
        let ev = fx.eval_with(&groups, Some(99), header);
        ev.groups[0].issues.iter().map(|i| i.id.clone()).collect()
    }

    #[test]
    fn assignee_picker_matches_a_person_across_teams() {
        let fx = assignee_fx();
        let mut got = assignee_ids(&fx, AssigneeFilter::User(7));
        got.sort();
        assert_eq!(got, ["i1", "i2"]);
    }

    #[test]
    fn assignee_picker_unassigned_narrows_rows_and_counts() {
        let mut fx = assignee_fx();
        fx.issues[3].due_date = Some("2026-10-30".into());
        assert_eq!(assignee_ids(&fx, AssigneeFilter::Unassigned), ["i4"]);
        let groups = vec![cfg("g", vec![team_src(1, true), team_src(2, true)])];
        let header = Header {
            assignee: Some(AssigneeFilter::Unassigned),
            ..Header::default()
        };
        assert_eq!(fx.eval_with(&groups, Some(99), header).overdue, 1);
    }

    #[test]
    fn assignee_options_dedupe_by_user_and_sort_by_name() {
        let mut fx = assignee_fx();
        for m in fx.members.iter_mut() {
            m.name = match m.id.as_str() {
                "m7a" | "m7b" => "Zed".into(),
                "ma" | "mb" => "bob".into(),
                _ => "Amy".into(),
            };
        }
        let groups = vec![cfg("g", vec![team_src(1, true), team_src(2, true)])];
        let ev = fx.eval(&groups, Some(99));
        let got: Vec<(i64, &str)> =
            ev.assignees.iter().map(|a| (a.remote_id, a.name.as_str())).collect();
        assert_eq!(got, [(5, "Amy"), (99, "bob"), (7, "Zed")]);
    }

    #[test]
    #[test]
    fn source_teams_cover_all_source_kinds_in_mirror_order() {
        let mut fx = Fx::new();
        fx.labels = vec![label("l1", "b", 50)];
        let groups = vec![
            cfg("g1", vec![team_src(3, true), team_src(1, true)]),
            cfg("g2", vec![label_src(50)]),
        ];
        let ev = fx.eval(&groups, Some(99));
        let ids: Vec<&str> = ev.source_teams.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }

    fn mine_fx() -> Fx {
        let mut fx = Fx::new();
        let mut mine_due = issue("mine", "a", "la");
        mine_due.assignee_id = Some("ma".into());
        mine_due.due_date = Some("2026-11-01".into());
        let mut other_due = issue("other", "a", "la");
        other_due.assignee_id = Some("other".into());
        other_due.due_date = Some("2026-10-30".into());
        let mut mine_b = issue("mine_b", "b", "lb");
        mine_b.assignee_id = Some("mb".into());
        fx.issues = vec![mine_due, other_due, mine_b];
        fx
    }

    #[test]
    fn mine_off_shows_everything_and_counts_all() {
        let fx = mine_fx();
        let groups = vec![cfg("g", vec![team_src(1, true), team_src(2, true)])];
        let e = fx.eval(&groups, Some(99));
        let got: Vec<&str> = e.groups[0].issues.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(got.len(), 3);
        assert_eq!((e.due_today, e.overdue), (1, 1));
    }

    #[test]
    fn mine_on_narrows_rows_across_teams_and_counts() {
        let fx = mine_fx();
        let groups = vec![cfg("g", vec![team_src(1, true), team_src(2, true)])];
        let h = Header { filter: Filter::Open, mine: true, ..Header::default() };
        let e = fx.eval_with(&groups, Some(99), h);
        let mut got: Vec<&str> = e.groups[0].issues.iter().map(|i| i.id.as_str()).collect();
        got.sort();
        assert_eq!(got, vec!["mine", "mine_b"]);
        assert_eq!((e.due_today, e.overdue), (1, 0));
    }

    #[test]
    fn mine_without_a_current_user_shows_nothing() {
        let fx = mine_fx();
        let groups = vec![cfg("g", vec![team_src(1, true)])];
        let h = Header { filter: Filter::Open, mine: true, ..Header::default() };
        assert!(fx.eval_with(&groups, None, h).groups[0].issues.is_empty());
    }
    fn named_label(id: &str, team: &str, name: &str) -> Label {
        Label { name: name.into(), ..label(id, team, 0) }
    }

    fn label_pick_fx() -> Fx {
        let mut fx = Fx::new();
        let (a_bug, b_bug) = (named_label("la_bug", "a", "bug"), named_label("lb_bug", "b", "bug"));
        let a_ui = named_label("la_ui", "a", "ui");
        let arch_bug = named_label("larch_bug", "arch", "archived-only");
        let mut one = issue("one", "a", "la");
        one.due_date = Some("2026-10-30".into());
        let two = issue("two", "b", "lb");
        let three = issue("three", "a", "la");
        let four = issue("four", "a", "la");
        fx.issue_labels.insert("one".into(), vec![a_bug.clone()]);
        fx.issue_labels.insert("two".into(), vec![b_bug.clone()]);
        fx.issue_labels.insert("three".into(), vec![a_ui.clone()]);
        fx.issue_labels.insert("four".into(), vec![a_bug.clone(), a_ui.clone()]);
        fx.labels = vec![a_bug, b_bug, a_ui, arch_bug];
        fx.issues = vec![one, two, three, four];
        fx
    }

    #[test]
    fn label_names_are_distinct_sorted_and_from_source_teams() {
        let fx = label_pick_fx();
        let groups = vec![cfg("g", vec![team_src(1, true), team_src(2, true)])];
        assert_eq!(fx.eval(&groups, Some(99)).label_names, ["bug", "ui"]);
        // Only team b is behind the dashboard: just its labels.
        let groups = vec![cfg("g", vec![team_src(2, true)])];
        assert_eq!(fx.eval(&groups, Some(99)).label_names, ["bug"]);
    }

    #[test]
    fn label_picker_matches_by_name_across_teams_rows_and_counts() {
        let fx = label_pick_fx();
        let groups = vec![cfg("g", vec![team_src(1, true), team_src(2, true)])];
        let h = Header { filter: Filter::Open, label: Some("bug".into()), ..Header::default() };
        let ev = fx.eval_with(&groups, Some(99), h);
        let mut got: Vec<&str> = ev.groups[0].issues.iter().map(|i| i.id.as_str()).collect();
        got.sort();
        assert_eq!(got, ["four", "one", "two"]);
        assert_eq!(ev.overdue, 1);
    }

    #[test]
    fn label_picker_lists_an_issue_once_with_several_matching_labels() {
        let mut fx = label_pick_fx();
        let extra = named_label("la_bug2", "a", "bug");
        fx.issue_labels.get_mut("four").unwrap().push(extra.clone());
        fx.labels.push(extra);
        let groups = vec![cfg("g", vec![team_src(1, true)])];
        let h = Header { filter: Filter::Open, label: Some("bug".into()), ..Header::default() };
        let ev = fx.eval_with(&groups, Some(99), h);
        let fours = ev.groups[0].issues.iter().filter(|i| i.id == "four").count();
        assert_eq!(fours, 1);
    }

    #[test]
    fn label_picker_ignores_a_name_not_on_the_source_teams() {
        let fx = label_pick_fx();
        let groups = vec![cfg("g", vec![team_src(1, true)])];
        let h = Header {
            filter: Filter::Open,
            label: Some("archived-only".into()),
            ..Header::default()
        };
        assert_eq!(fx.eval_with(&groups, Some(99), h).groups[0].issues.len(), 3);
    }
}
