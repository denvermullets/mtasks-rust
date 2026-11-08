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
    /// Open a dashboard by local id (`Message::OpenDashboard`).
    Dashboard(String),
    /// Attach files to the open issue (`Message::PickAttachmentFiles`).
    AttachFiles,
    /// Open the open issue's dependency map (`Message::OpenDependencyMap`).
    OpenDependencyMap,
    /// Open the open project's dependency map (`Message::OpenProjectDependencyMap`).
    OpenProjectDependencyMap,
    /// Give the keyboard to the Terminal (`TerminalMessage::FocusTerminal`).
    FocusTerminal,
    /// Give the keyboard back to the app (`TerminalMessage::FocusApp`).
    FocusApp,
    /// Hide the Terminal panel (`TerminalMessage::Hide`).
    HidePanel,
    /// Show or hide the project page's Properties sidebar
    /// (`Message::ToggleProjectProperties`).
    ToggleProjectProperties,
    /// Switch the project page between always-stacked cards and side by side
    /// when roomy (`Message::ToggleProjectCardsStacked`).
    ToggleProjectCardsStacked,
    /// Dock the Terminal panel at the bottom or down the right
    /// (`Message::ToggleTerminalDock`).
    ToggleTerminalDock,
    /// Focus the open Tab at this position (`TabMessage::Focus`).
    Tab(usize),
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
            PaletteNav::Dashboard(_) => "Dashboard",
            PaletteNav::AttachFiles
            | PaletteNav::OpenDependencyMap
            | PaletteNav::OpenProjectDependencyMap
            | PaletteNav::FocusTerminal
            | PaletteNav::FocusApp
            | PaletteNav::HidePanel
            | PaletteNav::ToggleProjectProperties
            | PaletteNav::ToggleProjectCardsStacked
            | PaletteNav::ToggleTerminalDock => "Command",
            PaletteNav::Tab(_) => "Tab",
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

/// A dashboard as the palette searches it.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchableDashboard {
    pub id: String,
    pub name: String,
}

/// An open Tab as the palette searches it: its bar label, in bar order.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchableTab {
    pub label: String,
    pub active: bool,
}

/// Where the keyboard is, as far as the focus commands care.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TerminalFocus {
    /// This page has no panel (a cross-team page): no focus commands.
    #[default]
    Unavailable,
    /// The app has the keyboard; a Terminal can take it.
    App,
    /// A Terminal has the keyboard (the palette opened over it).
    Terminal,
}

/// Local data the palette searches, owned so `PaletteState` can keep it and
/// recompute rows on every keystroke. Issues and projects of archived teams,
/// and those teams themselves, are already left out when this is loaded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaletteData {
    pub issues: Vec<SearchableIssue>,
    /// The selected team; its issues rank first. `None` ranks none first.
    pub current_team_id: Option<String>,
    pub teams: Vec<Team>,
    pub projects: Vec<SearchableProject>,
    pub saved_views: Vec<SearchableSavedView>,
    pub dashboards: Vec<SearchableDashboard>,
    /// Open Tabs in bar order; the position is what choosing a row focuses.
    pub tabs: Vec<SearchableTab>,
    /// An issue detail is open, so issue-only commands are offered.
    pub on_issue_detail: bool,
    /// A project page is open, so its properties toggle is offered.
    pub on_project_detail: bool,
    pub terminal_focus: TerminalFocus,
    /// The Terminal panel is on screen, so it can be hidden.
    pub terminal_panel_showing: bool,
    /// The project page stacks its cards at every width.
    pub project_cards_stacked: bool,
    /// The Terminal panel is docked right (else at the bottom).
    pub terminal_dock_right: bool,
}

/// Most rows the palette ever shows.
pub const MAX_ROWS: usize = 20;

/// Ordered palette rows for `query`, capped at `MAX_ROWS`. An empty (or
/// blank) query lists every navigation target.
pub fn palette_results(query: &str, data: &PaletteData) -> Vec<PaletteRow> {
    let needle = query.trim().to_lowercase();
    let mut rows = Vec::new();
    push_issues(query, data, &mut rows);
    push_commands(&needle, data, &mut rows);
    push_tabs(&needle, data, &mut rows);
    push_views(&needle, &mut rows);
    push_teams(&needle, data, &mut rows);
    push_projects(&needle, data, &mut rows);
    push_saved_views(&needle, data, &mut rows);
    push_dashboards(&needle, data, &mut rows);
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

fn push_commands(needle: &str, data: &PaletteData, rows: &mut Vec<PaletteRow>) {
    if data.on_issue_detail && name_matches(needle, "Attach file") {
        rows.push(PaletteRow {
            primary: "Attach file".to_string(),
            secondary: "Shortcut: a".to_string(),
            nav: PaletteNav::AttachFiles,
        });
    }
    if data.on_issue_detail && name_matches(needle, "Open dependency map") {
        rows.push(PaletteRow {
            primary: "Open dependency map".to_string(),
            secondary: "Blockers and sub-issues around this issue".to_string(),
            nav: PaletteNav::OpenDependencyMap,
        });
    }
    if data.on_project_detail && name_matches(needle, "Open project dependency map") {
        rows.push(PaletteRow {
            primary: "Open project dependency map".to_string(),
            secondary: "Blockers across this project's issues".to_string(),
            nav: PaletteNav::OpenProjectDependencyMap,
        });
    }
    if data.on_project_detail && name_matches(needle, "Toggle project properties") {
        rows.push(PaletteRow {
            primary: "Toggle project properties".to_string(),
            secondary: "Shortcut: /".to_string(),
            nav: PaletteNav::ToggleProjectProperties,
        });
    }
    if data.on_project_detail {
        let (name, secondary) = if data.project_cards_stacked {
            ("Side-by-side project cards", "Burndown and Issues side by side when there's room")
        } else {
            ("Stack project cards", "Burndown above Issues at every width")
        };
        if name_matches(needle, name) {
            rows.push(PaletteRow {
                primary: name.to_string(),
                secondary: secondary.to_string(),
                nav: PaletteNav::ToggleProjectCardsStacked,
            });
        }
    }
    if data.terminal_focus != TerminalFocus::Unavailable {
        let name = if data.terminal_dock_right {
            "Dock Terminal panel at the bottom"
        } else {
            "Dock Terminal panel on the right"
        };
        if name_matches(needle, name) {
            rows.push(PaletteRow {
                primary: name.to_string(),
                secondary: "Every Tab's panel moves".to_string(),
                nav: PaletteNav::ToggleTerminalDock,
            });
        }
    }
    match data.terminal_focus {
        TerminalFocus::App if name_matches(needle, "Focus Terminal") => {
            rows.push(PaletteRow {
                primary: "Focus Terminal".to_string(),
                secondary: "Shortcut: Cmd+J".to_string(),
                nav: PaletteNav::FocusTerminal,
            });
        }
        TerminalFocus::Terminal if name_matches(needle, "Focus app") => {
            rows.push(PaletteRow {
                primary: "Focus app".to_string(),
                secondary: "Shortcut: Cmd+J".to_string(),
                nav: PaletteNav::FocusApp,
            });
        }
        _ => {}
    }
    if data.terminal_panel_showing && name_matches(needle, "Hide Terminal panel") {
        rows.push(PaletteRow {
            primary: "Hide Terminal panel".to_string(),
            secondary: "Shortcut: Cmd+Shift+J · Terminals keep running".to_string(),
            nav: PaletteNav::HidePanel,
        });
    }
}

/// Switch-Tab commands: every open Tab but the active one, by label.
fn push_tabs(needle: &str, data: &PaletteData, rows: &mut Vec<PaletteRow>) {
    for (i, tab) in data.tabs.iter().enumerate() {
        if !tab.active && name_matches(needle, &tab.label) {
            rows.push(PaletteRow {
                primary: tab.label.clone(),
                secondary: "Switch to this Tab".to_string(),
                nav: PaletteNav::Tab(i),
            });
        }
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

fn push_dashboards(needle: &str, data: &PaletteData, rows: &mut Vec<PaletteRow>) {
    for d in &data.dashboards {
        if name_matches(needle, &d.name) {
            rows.push(PaletteRow {
                primary: d.name.clone(),
                secondary: String::new(),
                nav: PaletteNav::Dashboard(d.id.clone()),
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
            role: None,
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
    fn focus_commands_appear_only_where_they_apply() {
        let with = |terminal_focus| PaletteData { terminal_focus, ..PaletteData::default() };
        let navs = |d: &PaletteData| -> Vec<PaletteNav> {
            palette_results("focus", d).into_iter().map(|r| r.nav).collect()
        };
        assert!(navs(&with(TerminalFocus::Unavailable)).is_empty());
        assert_eq!(navs(&with(TerminalFocus::App)), vec![PaletteNav::FocusTerminal]);
        assert_eq!(navs(&with(TerminalFocus::Terminal)), vec![PaletteNav::FocusApp]);
        // Unfiltered, the commands are listed too.
        let all = palette_results("", &with(TerminalFocus::App));
        assert!(all.iter().any(|r| r.nav == PaletteNav::FocusTerminal));
    }

    #[test]
    fn hide_panel_is_offered_only_while_the_panel_shows() {
        let hide = |terminal_panel_showing| {
            let d = PaletteData { terminal_panel_showing, ..PaletteData::default() };
            palette_results("hide", &d).into_iter().any(|r| r.nav == PaletteNav::HidePanel)
        };
        assert!(hide(true));
        assert!(!hide(false));
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

    #[test]
    fn attach_command_only_on_issue_detail() {
        let on = PaletteData { on_issue_detail: true, ..PaletteData::default() };
        let rows = palette_results("attach", &on);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].nav, PaletteNav::AttachFiles);
        assert!(palette_results("attach", &PaletteData::default()).is_empty());
    }

    #[test]
    fn dependency_map_command_only_on_issue_detail() {
        let on = PaletteData { on_issue_detail: true, ..PaletteData::default() };
        let rows = palette_results("dependency map", &on);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "Open dependency map");
        assert_eq!(rows[0].nav, PaletteNav::OpenDependencyMap);
        assert_eq!(rows[0].nav.label(), "Command");
        assert!(palette_results("dependency map", &PaletteData::default()).is_empty());
    }

    #[test]
    fn project_dependency_map_command_only_on_the_project_page() {
        let on = PaletteData { on_project_detail: true, ..PaletteData::default() };
        let rows = palette_results("project dependency", &on);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].nav, PaletteNav::OpenProjectDependencyMap);
        assert!(palette_results("project dependency", &PaletteData::default()).is_empty());
    }

    #[test]
    fn toggle_properties_command_only_on_the_project_page() {
        let on = PaletteData { on_project_detail: true, ..PaletteData::default() };
        let rows = palette_results("toggle project", &on);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "Toggle project properties");
        assert_eq!(rows[0].secondary, "Shortcut: /");
        assert_eq!(rows[0].nav, PaletteNav::ToggleProjectProperties);
        assert_eq!(rows[0].nav.label(), "Command");
        assert!(palette_results("toggle project", &PaletteData::default()).is_empty());
    }

    #[test]
    fn stack_cards_command_only_on_the_project_page_and_names_the_other_layout() {
        let side = PaletteData { on_project_detail: true, ..PaletteData::default() };
        let rows = palette_results("project cards", &side);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "Stack project cards");
        assert_eq!(rows[0].nav, PaletteNav::ToggleProjectCardsStacked);
        let stacked = PaletteData { project_cards_stacked: true, ..side };
        assert_eq!(palette_results("project cards", &stacked)[0].primary, "Side-by-side project cards");
        assert!(palette_results("project cards", &PaletteData::default()).is_empty());
    }

    #[test]
    fn dock_command_offers_the_other_side_wherever_a_panel_can_show() {
        let bottom = PaletteData { terminal_focus: TerminalFocus::App, ..PaletteData::default() };
        let rows = palette_results("dock", &bottom);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "Dock Terminal panel on the right");
        assert_eq!(rows[0].nav, PaletteNav::ToggleTerminalDock);
        let right = PaletteData { terminal_dock_right: true, ..bottom };
        assert_eq!(palette_results("dock", &right)[0].primary, "Dock Terminal panel at the bottom");
        assert!(palette_results("dock", &PaletteData::default()).is_empty());
    }

    fn dashboard_data() -> PaletteData {
        PaletteData {
            dashboards: vec![
                SearchableDashboard { id: "d1".into(), name: "Morning Triage".into() },
                SearchableDashboard { id: "d2".into(), name: "Hot list".into() },
            ],
            ..PaletteData::default()
        }
    }

    #[test]
    fn dashboard_matches_name_ignoring_case_and_is_labelled() {
        let rows = palette_results("TRIAGE", &dashboard_data());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "Morning Triage");
        assert_eq!(rows[0].nav, PaletteNav::Dashboard("d1".into()));
        assert_eq!(rows[0].nav.label(), "Dashboard");
        assert!(palette_results("zzz", &dashboard_data()).is_empty());
    }

    #[test]
    fn empty_query_lists_dashboards_after_other_targets() {
        let mut data = dashboard_data();
        data.saved_views = vec![sv("v1", "t1", "JAIT App", "Bugs")];
        let rows = palette_results("", &data);
        let labels: Vec<_> = rows.iter().map(|r| r.nav.label()).collect();
        assert!(labels.starts_with(&["View"; 5][..]));
        assert_eq!(&labels[labels.len() - 2..], ["Dashboard", "Dashboard"]);
        assert_eq!(rows[rows.len() - 2].primary, "Morning Triage");
    }

    #[test]
    fn choosing_a_dashboard_row_yields_its_local_id() {
        let mut state = PaletteState::new(dashboard_data());
        state.set_query("hot".into());
        assert_eq!(state.chosen(), Some(PaletteNav::Dashboard("d2".into())));
    }

    fn tab(label: &str, active: bool) -> SearchableTab {
        SearchableTab { label: label.into(), active }
    }

    #[test]
    fn open_tabs_appear_by_name_with_their_bar_position() {
        let data = PaletteData {
            tabs: vec![tab("JRST JAIT App", true), tab("RAIL Rails API", false)],
            ..PaletteData::default()
        };
        let rows = palette_results("rails", &data);
        let tabs: Vec<_> = rows.iter().filter(|r| r.nav.label() == "Tab").collect();
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].nav, PaletteNav::Tab(1));
        assert_eq!(tabs[0].primary, "RAIL Rails API");
    }

    #[test]
    fn the_active_tab_is_not_offered_and_empty_query_lists_the_rest() {
        let data = PaletteData {
            tabs: vec![tab("A", false), tab("B", true), tab("C", false)],
            ..PaletteData::default()
        };
        let navs: Vec<_> = palette_results("", &data)
            .into_iter()
            .map(|r| r.nav)
            .filter(|n| matches!(n, PaletteNav::Tab(_)))
            .collect();
        assert_eq!(navs, vec![PaletteNav::Tab(0), PaletteNav::Tab(2)]);
        assert!(palette_results("zzz", &data).is_empty());
    }
}
