//! Pure results function for the Command palette. The view only renders what
//! this returns, so it is the main test seam. New target kinds (team, project,
//! issue) are added as a `PaletteKind` variant, a `PaletteNav` variant, and a
//! `push_*` function called from `palette_results`.

use crate::app::View;
use crate::db::models::{Issue, Team};
use crate::issue_match::IssueQuery;

/// What a row is, shown as a label so rows can be scanned by kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteKind {
    View,
    Issue,
}

impl PaletteKind {
    pub fn label(self) -> &'static str {
        match self {
            PaletteKind::View => "View",
            PaletteKind::Issue => "Issue",
        }
    }
}

/// Where choosing a row goes.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteNav {
    /// Switch view for the current team (`Message::NavigateToView`).
    View(View),
    /// Open this issue's detail (`App::open_issue_detail`).
    Issue(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaletteRow {
    pub kind: PaletteKind,
    pub primary: String,
    pub secondary: String,
    pub nav: PaletteNav,
}

/// Local data the palette searches. Later kinds add their fields here.
#[derive(Debug, Clone, Copy, Default)]
pub struct PaletteData<'a> {
    /// The current team's issues (archived ones are filtered out here).
    pub issues: &'a [Issue],
    /// The current team; `None` or archived yields no issue rows.
    pub team: Option<&'a Team>,
}

/// Most rows the palette ever shows.
pub const MAX_ROWS: usize = 20;

/// Keep the first `max` rows; call after ordering so the best matches survive.
fn cap_rows(mut rows: Vec<PaletteRow>, max: usize) -> Vec<PaletteRow> {
    rows.truncate(max);
    rows
}

/// Ordered palette rows for `query`. An empty (or blank) query lists every
/// navigation target.
pub fn palette_results(query: &str, data: &PaletteData<'_>) -> Vec<PaletteRow> {
    let needle = query.trim().to_lowercase();
    let mut rows = Vec::new();
    push_issues(query, data, &mut rows);
    push_views(&needle, data, &mut rows);
    cap_rows(rows, MAX_ROWS)
}

/// Case-insensitive substring match; `needle` is already trimmed and lowercased.
fn name_matches(needle: &str, name: &str) -> bool {
    name.to_lowercase().contains(needle)
}

/// Current-team issues matching `query` via the shared matcher: highest team
/// number first, unsynced (no number) issues after the numbered ones.
fn push_issues(query: &str, data: &PaletteData<'_>, rows: &mut Vec<PaletteRow>) {
    let Some(team) = data.team.filter(|t| t.archived_at.is_none()) else {
        return;
    };
    let query = IssueQuery::parse(query);
    let mut found: Vec<&Issue> = data
        .issues
        .iter()
        .filter(|i| i.archived_at.is_none() && query.matches(i, &team.identifier))
        .collect();
    // Stable: ties (e.g. several unnumbered issues) keep their input order.
    found.sort_by_key(|i| std::cmp::Reverse(i.team_number));
    for issue in found {
        let identifier = match issue.team_number {
            Some(n) => format!("{}-{}", team.identifier, n),
            None => "Unsynced".to_string(),
        };
        rows.push(PaletteRow {
            kind: PaletteKind::Issue,
            primary: identifier,
            secondary: issue.title.clone(),
            nav: PaletteNav::Issue(issue.id.clone()),
        });
    }
}

fn push_views(needle: &str, _data: &PaletteData<'_>, rows: &mut Vec<PaletteRow>) {
    let views = [
        ("Board", "Current team", View::Board),
        ("Projects", "Current team", View::ProjectList),
        ("Roadmap", "Current team", View::Roadmap),
        ("My Issues", "All teams", View::MyIssues),
    ];
    for (name, scope, view) in views {
        if name_matches(needle, name) {
            rows.push(PaletteRow {
                kind: PaletteKind::View,
                primary: name.to_string(),
                secondary: scope.to_string(),
                nav: PaletteNav::View(view),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(archived: bool) -> Team {
        Team {
            id: "t".into(),
            name: "Eng".into(),
            identifier: "ENG".into(),
            remote_id: None,
            archived_at: archived.then(|| "2026-01-01".to_string()),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn issue(id: &str, num: Option<i32>, title: &str, archived: bool) -> Issue {
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
            team_id: "t".into(),
            project_id: None,
            parent_issue_id: None,
            assignee_id: None,
            creator_id: None,
            archived_at: archived.then(|| "2026-01-01".to_string()),
            remote_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn issue_ids(query: &str, team: &Team, issues: &[Issue]) -> Vec<String> {
        let data = PaletteData { issues, team: Some(team) };
        palette_results(query, &data)
            .into_iter()
            .filter_map(|r| match r.nav {
                PaletteNav::Issue(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn identifier_and_number_find_current_team_issue() {
        let t = team(false);
        let issues = [issue("a", Some(42), "Fix", false), issue("b", Some(7), "Other", false)];
        for q in ["ENG-42", "eng-42", "42"] {
            assert_eq!(issue_ids(q, &t, &issues), ["a"], "query {q}");
        }
        assert!(issue_ids("OPS-42", &t, &issues).is_empty());
    }

    #[test]
    fn title_matches_highest_number_first_unsynced_last() {
        let t = team(false);
        let issues = [
            issue("u", None, "login local", false),
            issue("a", Some(3), "Login bug", false),
            issue("b", Some(9), "login page", false),
            issue("c", Some(5), "unrelated", false),
        ];
        assert_eq!(issue_ids("login", &t, &issues), ["b", "a", "u"]);
    }

    #[test]
    fn archived_issues_and_archived_team_are_left_out() {
        let issues = [issue("a", Some(1), "thing", true), issue("b", Some(2), "thing", false)];
        assert_eq!(issue_ids("thing", &team(false), &issues), ["b"]);
        assert!(issue_ids("thing", &team(true), &issues).is_empty());
    }

    #[test]
    fn issues_come_before_views_and_empty_query_has_none() {
        let t = team(false);
        let issues = [issue("a", Some(1), "board fix", false)];
        let data = PaletteData { issues: &issues, team: Some(&t) };
        let rows = palette_results("board", &data);
        assert_eq!(rows[0].kind, PaletteKind::Issue);
        assert_eq!(rows[0].primary, "ENG-1");
        assert_eq!(rows[0].secondary, "board fix");
        assert_eq!(rows[1].kind, PaletteKind::View);
        assert!(palette_results("", &data).iter().all(|r| r.kind == PaletteKind::View));
    }

    fn names(query: &str) -> Vec<String> {
        palette_results(query, &PaletteData::default())
            .into_iter()
            .map(|r| r.primary)
            .collect()
    }

    #[test]
    fn empty_query_lists_all_views_in_order() {
        assert_eq!(names(""), ["Board", "Projects", "Roadmap", "My Issues"]);
    }

    #[test]
    fn blank_query_is_like_empty() {
        assert_eq!(names("   "), names(""));
    }

    #[test]
    fn partial_query_narrows_ignoring_case() {
        assert_eq!(names("proj"), ["Projects"]);
        assert_eq!(names("ROAD"), ["Roadmap"]);
    }

    #[test]
    fn leading_and_trailing_spaces_are_ignored() {
        assert_eq!(names("  road "), ["Roadmap"]);
    }

    #[test]
    fn substring_matches_inside_name() {
        assert_eq!(names("issues"), ["My Issues"]);
    }

    #[test]
    fn non_matching_query_returns_nothing() {
        assert!(names("zzz").is_empty());
    }

    fn view_row(name: &str) -> PaletteRow {
        PaletteRow {
            kind: PaletteKind::View,
            primary: name.to_string(),
            secondary: String::new(),
            nav: PaletteNav::View(View::Board),
        }
    }

    #[test]
    fn cap_keeps_the_first_rows_in_order() {
        let rows: Vec<_> = ["a", "b", "c", "d"].into_iter().map(view_row).collect();
        let capped: Vec<_> = cap_rows(rows, 2).into_iter().map(|r| r.primary).collect();
        assert_eq!(capped, ["a", "b"]);
    }

    #[test]
    fn cap_leaves_short_lists_alone() {
        let rows = vec![view_row("a")];
        assert_eq!(cap_rows(rows, 20).len(), 1);
    }

    #[test]
    fn results_never_exceed_the_cap() {
        assert!(palette_results("", &PaletteData::default()).len() <= MAX_ROWS);
    }

    #[test]
    fn rows_carry_kind_and_navigation() {
        let rows = palette_results("my", &PaletteData::default());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, PaletteKind::View);
        assert_eq!(rows[0].kind.label(), "View");
        assert_eq!(rows[0].nav, PaletteNav::View(View::MyIssues));
    }
}
