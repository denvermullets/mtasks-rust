//! Command palette logic with no rendering: the results function and the
//! palette's state (query, rows, highlight). The view only renders a
//! `PaletteState`, so this module is the main test seam. A new target kind
//! is a `PaletteNav` variant (plus its `label`) and a `push_*` function
//! called from `palette_results`.

use crate::app::View;
use crate::db::models::Team;
use crate::db::queries::{SearchableIssue, SearchableProject};
use crate::issue_match::IssueQuery;

/// Where choosing a row goes; also says what kind of row it is.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteNav {
    /// Switch view for the current team (`Message::NavigateToView`).
    View(View),
    /// Open this team's Board (`Message::SelectTeamView(id, View::Board)`).
    Team(String),
    /// Open this issue's detail (`App::open_issue_detail`).
    Issue(String),
    /// Open a project's detail, switching to `team_id` first when needed.
    Project { project_id: String, team_id: String },
    /// Open a Saved view by local id, switching team when needed
    /// (`Message::OpenSavedView`).
    SavedView(String),
}

impl PaletteNav {
    /// The kind label shown on a row, so rows can be scanned by kind.
    pub fn label(&self) -> &'static str {
        match self {
            PaletteNav::View(_) => "View",
            PaletteNav::Team(_) => "Team",
            PaletteNav::Issue(_) => "Issue",
            PaletteNav::Project { .. } => "Project",
            PaletteNav::SavedView(_) => "View",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaletteRow {
    pub primary: String,
    pub secondary: String,
    pub nav: PaletteNav,
}

/// A Saved view as the palette searches it.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchableSavedView {
    pub id: String,
    pub team_id: String,
    pub team_name: String,
    pub name: String,
}

/// Local data the palette searches, owned so `PaletteState` can keep it and
/// recompute rows on every keystroke. Archived issues, teams, and the
/// projects of archived teams are already left out when this is loaded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaletteData {
    pub issues: Vec<SearchableIssue>,
    /// The selected team; its issues rank first. `None` ranks none first.
    pub current_team_id: Option<String>,
    pub teams: Vec<Team>,
    pub projects: Vec<SearchableProject>,
    pub saved_views: Vec<SearchableSavedView>,
}

/// Most rows the palette ever shows.
pub const MAX_ROWS: usize = 20;

/// Ordered palette rows for `query`, capped at `MAX_ROWS`. An empty (or
/// blank) query lists every navigation target.
pub fn palette_results(query: &str, data: &PaletteData) -> Vec<PaletteRow> {
    let needle = query.trim().to_lowercase();
    let mut rows = Vec::new();
    push_issues(query, data, &mut rows);
    push_views(&needle, &mut rows);
    push_teams(&needle, data, &mut rows);
    push_projects(&needle, data, &mut rows);
    push_saved_views(&needle, data, &mut rows);
    // After ordering, so the best matches survive.
    rows.truncate(MAX_ROWS);
    rows
}

/// An open palette: the query, its rows and the highlighted row.
#[derive(Debug, Clone, PartialEq)]
pub struct PaletteState {
    pub query: String,
    pub rows: Vec<PaletteRow>,
    /// Index of the highlighted row; always 0 when there are no rows.
    pub selected: usize,
    data: PaletteData,
}

impl PaletteState {
    pub fn new(data: PaletteData) -> Self {
        let mut state = PaletteState {
            query: String::new(),
            rows: Vec::new(),
            data,
            selected: 0,
        };
        state.set_query(String::new());
        state
    }

    /// Store the query and recompute the rows.
    pub fn set_query(&mut self, query: String) {
        self.rows = palette_results(&query, &self.data);
        self.query = query;
        self.selected = 0;
    }

    /// Move the highlight by `delta` rows, wrapping at both ends.
    pub fn move_selection(&mut self, delta: i32) {
        let n = self.rows.len() as i32;
        if n > 0 {
            self.selected = (self.selected as i32 + delta).rem_euclid(n) as usize;
        }
    }

    /// Where Enter goes: the highlighted row's target, if any.
    pub fn chosen(&self) -> Option<PaletteNav> {
        self.rows.get(self.selected).map(|r| r.nav.clone())
    }
}

/// Case-insensitive substring match; `needle` is already trimmed and lowercased.
fn name_matches(needle: &str, name: &str) -> bool {
    name.to_lowercase().contains(needle)
}

/// Issues from every team matching `query` via the shared matcher. The
/// current team's come first; within a team the highest number first, with
/// unsynced (no number) issues after the numbered ones.
fn push_issues(query: &str, data: &PaletteData, rows: &mut Vec<PaletteRow>) {
    let query = IssueQuery::parse(query);
    let mut found: Vec<&SearchableIssue> = data
        .issues
        .iter()
        .filter(|s| query.matches(&s.issue, &s.team_identifier))
        .collect();
    // Stable: ties (e.g. several unnumbered issues) keep their input order.
    found.sort_by_key(|s| {
        let other_team = data.current_team_id.as_deref() != Some(s.issue.team_id.as_str());
        (other_team, std::cmp::Reverse(s.issue.team_number))
    });
    for s in found {
        let number = s
            .issue
            .team_number
            .map_or_else(|| "\u{2014}".to_string(), |n| n.to_string());
        rows.push(PaletteRow {
            primary: format!("{}-{number}", s.team_identifier),
            secondary: format!("{} \u{b7} {}", s.issue.title, s.team_name),
            nav: PaletteNav::Issue(s.issue.id.clone()),
        });
    }
}

fn push_views(needle: &str, rows: &mut Vec<PaletteRow>) {
    let views = [
        ("Board", "Current team", View::Board),
        ("Projects", "Current team", View::ProjectList),
        ("Roadmap", "Current team", View::Roadmap),
        ("Recurring issues", "Current team", View::RecurringIssues),
        ("My Issues", "All teams", View::MyIssues),
        ("Inbox", "Notifications", View::Inbox),
    ];
    for (name, scope, view) in views {
        if name_matches(needle, name) {
            rows.push(PaletteRow {
                primary: name.to_string(),
                secondary: scope.to_string(),
                nav: PaletteNav::View(view),
            });
        }
    }
}

fn push_teams(needle: &str, data: &PaletteData, rows: &mut Vec<PaletteRow>) {
    for team in &data.teams {
        if name_matches(needle, &team.name) || name_matches(needle, &team.identifier) {
            rows.push(PaletteRow {
                primary: team.name.clone(),
                secondary: team.identifier.clone(),
                nav: PaletteNav::Team(team.id.clone()),
            });
        }
    }
}

fn push_projects(needle: &str, data: &PaletteData, rows: &mut Vec<PaletteRow>) {
    for p in &data.projects {
        if name_matches(needle, &p.name) {
            rows.push(PaletteRow {
                primary: p.name.clone(),
                secondary: p.team_name.clone(),
                nav: PaletteNav::Project {
                    project_id: p.id.clone(),
                    team_id: p.team_id.clone(),
                },
            });
        }
    }
}

fn push_saved_views(needle: &str, data: &PaletteData, rows: &mut Vec<PaletteRow>) {
    for v in &data.saved_views {
        if name_matches(needle, &v.name) {
            rows.push(PaletteRow {
                primary: v.name.clone(),
                secondary: v.team_name.clone(),
                nav: PaletteNav::SavedView(v.id.clone()),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::Issue;

    fn team(id: &str, name: &str, ident: &str) -> Team {
        Team {
            id: id.into(),
            name: name.into(),
            identifier: ident.into(),
            remote_id: None,
            archived_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn data() -> PaletteData {
        PaletteData {
            teams: vec![
                team("t1", "JAIT App", "JRST"),
                team("t2", "Rails API", "RAIL"),
            ],
            ..PaletteData::default()
        }
    }

    fn issue(id: &str, team: &str, num: Option<i32>, title: &str) -> Issue {
        Issue {
            id: id.into(),
            title: title.into(),
            description: None,
            priority: 0,
            team_number: num,
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

    /// Issue "ENG" team is "t-eng" named "ENG team"; identifier derives from id.
    fn si(id: &str, team_ident: &str, num: Option<i32>, title: &str) -> SearchableIssue {
        SearchableIssue {
            issue: issue(id, &format!("t-{}", team_ident.to_lowercase()), num, title),
            team_identifier: team_ident.into(),
            team_name: format!("{team_ident} team"),
        }
    }

    fn issue_data(current: Option<&str>, issues: Vec<SearchableIssue>) -> PaletteData {
        PaletteData {
            issues,
            current_team_id: current.map(String::from),
            ..PaletteData::default()
        }
    }

    fn issue_ids(query: &str, current: Option<&str>, issues: Vec<SearchableIssue>) -> Vec<String> {
        palette_results(query, &issue_data(current, issues))
            .into_iter()
            .filter_map(|r| match r.nav {
                PaletteNav::Issue(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn identifier_and_number_find_current_team_issue() {
        let issues = || vec![si("a", "ENG", Some(42), "Fix"), si("b", "ENG", Some(7), "Other")];
        for q in ["ENG-42", "eng-42", "42"] {
            assert_eq!(issue_ids(q, Some("t-eng"), issues()), ["a"], "query {q}");
        }
        assert!(issue_ids("OPS-42", Some("t-eng"), issues()).is_empty());
    }

    #[test]
    fn bare_number_matches_every_team_current_first() {
        let issues = vec![
            si("o", "OPS", Some(42), "ops"),
            si("e", "ENG", Some(42), "eng"),
            si("x", "ENG", Some(420), "no"),
        ];
        assert_eq!(issue_ids("42", Some("t-eng"), issues.clone()), ["e", "o"]);
        assert_eq!(issue_ids("42", Some("t-ops"), issues), ["o", "e"]);
    }

    #[test]
    fn prefixed_number_matches_only_that_team() {
        let issues = vec![si("o", "OPS", Some(42), "ops"), si("e", "ENG", Some(42), "eng")];
        assert_eq!(issue_ids("ops-42", Some("t-eng"), issues), ["o"]);
    }

    #[test]
    fn other_teams_title_matches_rank_after_current_team() {
        let issues = vec![
            si("o9", "OPS", Some(9), "login ops"),
            si("e1", "ENG", Some(1), "login eng"),
            si("e3", "ENG", Some(3), "Login bug"),
            si("o2", "OPS", Some(2), "login again"),
            si("u", "ENG", None, "login local"),
            si("n", "ENG", Some(5), "unrelated"),
        ];
        assert_eq!(
            issue_ids("login", Some("t-eng"), issues),
            ["e3", "e1", "u", "o9", "o2"]
        );
    }

    #[test]
    fn without_a_current_team_issues_still_match() {
        let issues = vec![si("a", "AAA", Some(1), "x"), si("b", "BBB", Some(1), "x")];
        assert_eq!(issue_ids("x", None, issues).len(), 2);
    }

    #[test]
    fn issue_row_shows_identifier_title_and_team() {
        let data = issue_data(Some("t-eng"), vec![si("a", "OPS", Some(42), "Fix it")]);
        let rows = palette_results("fix", &data);
        assert_eq!(rows[0].primary, "OPS-42");
        assert_eq!(rows[0].secondary, "Fix it \u{b7} OPS team");
    }

    #[test]
    fn unsynced_issue_row_shows_a_dash_for_its_number() {
        let data = issue_data(Some("t-eng"), vec![si("u", "OPS", None, "Fix new")]);
        assert_eq!(palette_results("fix", &data)[0].primary, "OPS-\u{2014}");
    }

    #[test]
    fn issues_come_before_views_and_empty_query_has_none() {
        let data = issue_data(Some("t-eng"), vec![si("a", "ENG", Some(1), "board fix")]);
        let rows = palette_results("board", &data);
        assert_eq!(rows[0].nav.label(), "Issue");
        assert_eq!(rows[0].primary, "ENG-1");
        assert_eq!(rows[1].nav.label(), "View");
        assert!(palette_results("", &data).iter().all(|r| r.nav.label() == "View"));
    }

    #[test]
    fn rows_are_capped_keeping_the_best_matches() {
        let issues = (1..=25)
            .map(|n| si(&format!("i{n}"), "ENG", Some(n), "login"))
            .collect();
        let rows = palette_results("login", &issue_data(Some("t-eng"), issues));
        assert_eq!(rows.len(), MAX_ROWS);
        assert_eq!(rows[0].primary, "ENG-25");
        assert_eq!(rows[MAX_ROWS - 1].primary, "ENG-6");
    }

    fn names(query: &str) -> Vec<String> {
        palette_results(query, &data())
            .into_iter()
            .map(|r| r.primary)
            .collect()
    }

    #[test]
    fn empty_query_lists_views_then_teams_in_order() {
        assert_eq!(
            names(""),
            ["Board", "Projects", "Roadmap", "Recurring issues", "My Issues", "Inbox", "JAIT App", "Rails API"]
        );
    }

    #[test]
    fn team_matches_name_substring_ignoring_case() {
        assert_eq!(names("RAILS"), ["Rails API"]);
        assert_eq!(names("app"), ["JAIT App"]);
    }

    #[test]
    fn team_matches_identifier() {
        assert_eq!(names("jrst"), ["JAIT App"]);
        assert_eq!(names("rail"), ["Rails API"]);
    }

    #[test]
    fn team_row_navigates_to_its_board() {
        let rows = palette_results("rails", &data());
        assert_eq!(rows[0].nav.label(), "Team");
        assert_eq!(rows[0].secondary, "RAIL");
        assert_eq!(rows[0].nav, PaletteNav::Team("t2".into()));
    }

    #[test]
    fn blank_query_is_like_empty() {
        assert_eq!(names("   "), names(""));
    }

    #[test]
    fn partial_query_narrows_ignoring_case() {
        assert_eq!(names("proj"), ["Projects"]);
        assert_eq!(names("recur"), ["Recurring issues"]);
        assert_eq!(names("ROAD"), ["Roadmap"]);
    }

    #[test]
    fn leading_and_trailing_spaces_are_ignored() {
        assert_eq!(names("  road "), ["Roadmap"]);
    }

    #[test]
    fn substring_matches_inside_name() {
        assert_eq!(names("issues"), ["Recurring issues", "My Issues"]);
    }

    #[test]
    fn inbox_row_navigates_to_the_inbox() {
        let rows = palette_results("inbox", &data());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].nav, PaletteNav::View(View::Inbox));
    }

    #[test]
    fn non_matching_query_returns_nothing() {
        assert!(names("zzz").is_empty());
    }

    fn project(id: &str, name: &str, team: &str) -> SearchableProject {
        SearchableProject {
            id: id.into(),
            name: name.into(),
            team_id: format!("t-{team}"),
            team_name: team.into(),
        }
    }

    fn project_data() -> PaletteData {
        PaletteData {
            projects: vec![
                project("p1", "Billing revamp", "Alpha"),
                project("p2", "Onboarding", "Beta"),
            ],
            ..PaletteData::default()
        }
    }

    fn project_rows(query: &str) -> Vec<PaletteRow> {
        palette_results(query, &project_data())
            .into_iter()
            .filter(|r| r.nav.label() == "Project")
            .collect()
    }

    #[test]
    fn project_matches_substring_ignoring_case_across_teams() {
        let rows = project_rows("ONBO");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "Onboarding");
        assert_eq!(rows[0].secondary, "Beta");
        assert_eq!(
            rows[0].nav,
            PaletteNav::Project { project_id: "p2".into(), team_id: "t-Beta".into() }
        );
    }

    #[test]
    fn empty_query_lists_projects_after_views() {
        let labels: Vec<_> = palette_results("", &project_data())
            .iter()
            .map(|r| r.nav.label())
            .collect();
        assert_eq!(
            labels,
            ["View", "View", "View", "View", "View", "View", "Project", "Project"]
        );
    }

    #[test]
    fn rows_carry_kind_and_navigation() {
        let rows = palette_results("my", &data());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].nav.label(), "View");
        assert_eq!(rows[0].nav, PaletteNav::View(View::MyIssues));
    }

    fn sv(id: &str, team: &str, team_name: &str, name: &str) -> SearchableSavedView {
        SearchableSavedView {
            id: id.into(),
            team_id: team.into(),
            team_name: team_name.into(),
            name: name.into(),
        }
    }

    fn saved_view_data() -> PaletteData {
        PaletteData {
            saved_views: vec![
                sv("v1", "t1", "JAIT App", "Bugs this week"),
                sv("v2", "t2", "Rails API", "Bugs this week"),
            ],
            ..PaletteData::default()
        }
    }

    #[test]
    fn saved_view_matches_on_name_and_shows_team_name() {
        let rows = palette_results("BUGS", &saved_view_data());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].nav.label(), "View");
        assert_eq!(rows[0].primary, "Bugs this week");
        assert_eq!(rows[0].secondary, "JAIT App");
        assert_eq!(rows[1].secondary, "Rails API");
        assert!(palette_results("zzz", &saved_view_data()).is_empty());
    }

    #[test]
    fn same_named_saved_views_open_their_own_view() {
        let mut state = PaletteState::new(saved_view_data());
        state.set_query("bugs".into());
        assert_eq!(state.chosen(), Some(PaletteNav::SavedView("v1".into())));
        state.move_selection(1);
        assert_eq!(state.chosen(), Some(PaletteNav::SavedView("v2".into())));
    }

    #[test]
    fn saved_views_do_not_match_on_team_name() {
        assert!(palette_results("rails", &saved_view_data()).is_empty());
    }

    #[test]
    fn palette_starts_with_empty_query() {
        assert_eq!(PaletteState::new(PaletteData::default()).query, "");
    }

    #[test]
    fn typing_recomputes_rows() {
        let mut state = PaletteState::new(PaletteData::default());
        assert_eq!(state.rows.len(), 6);
        state.set_query("road".into());
        assert_eq!(state.rows.len(), 1);
    }

    #[test]
    fn first_row_highlighted_on_open_and_after_typing() {
        let mut state = PaletteState::new(PaletteData::default());
        assert_eq!(state.selected, 0);
        state.move_selection(1);
        state.set_query("o".into());
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn down_wraps_to_first_and_up_wraps_to_last() {
        let mut state = PaletteState::new(PaletteData::default());
        state.move_selection(-1);
        assert_eq!(state.selected, 5);
        state.move_selection(1);
        assert_eq!(state.selected, 0);
        state.move_selection(1);
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn move_with_no_rows_stays_put() {
        let mut state = PaletteState::new(PaletteData::default());
        state.set_query("zzz".into());
        state.move_selection(1);
        state.move_selection(-1);
        assert_eq!(state.selected, 0);
        assert_eq!(state.chosen(), None);
    }

    #[test]
    fn chosen_is_the_highlighted_rows_target() {
        let mut state = PaletteState::new(PaletteData::default());
        assert_eq!(state.chosen(), Some(PaletteNav::View(View::Board)));
        state.move_selection(-1);
        assert_eq!(state.chosen(), Some(PaletteNav::View(View::Inbox)));
    }
}
