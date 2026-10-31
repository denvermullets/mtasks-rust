//! Pure results function for the Command palette. The view only renders what
//! this returns, so it is the main test seam. New target kinds (team, project,
//! issue) are added as a `PaletteKind` variant, a `PaletteNav` variant, and a
//! `push_*` function called from `palette_results`.

use crate::app::View;

/// What a row is, shown as a label so rows can be scanned by kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteKind {
    View,
    Project,
}

impl PaletteKind {
    pub fn label(self) -> &'static str {
        match self {
            PaletteKind::View => "View",
            PaletteKind::Project => "Project",
        }
    }
}

/// Where choosing a row goes.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteNav {
    /// Switch view for the current team (`Message::NavigateToView`).
    View(View),
    /// Open a project's detail, switching to `team_id` first when needed.
    Project { project_id: String, team_id: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaletteRow {
    pub kind: PaletteKind,
    pub primary: String,
    pub secondary: String,
    pub nav: PaletteNav,
}

/// A project from any team, with the team facts the palette needs.
#[derive(Debug, Clone, PartialEq)]
pub struct PaletteProject {
    pub id: String,
    pub name: String,
    pub team_id: String,
    pub team_name: String,
    pub team_archived: bool,
}

/// Local data the palette searches. Later kinds add their fields here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PaletteData {
    /// Projects of every team, loaded when the palette opens.
    pub projects: Vec<PaletteProject>,
}

impl PaletteData {
    /// Load the palette's data from the local database.
    pub fn load(conn: &rusqlite::Connection) -> Self {
        let mut projects = Vec::new();
        for team in crate::db::queries::all_teams(conn).unwrap_or_default() {
            for p in crate::db::queries::projects_for_team(conn, &team.id).unwrap_or_default() {
                projects.push(PaletteProject {
                    id: p.id,
                    name: p.name,
                    team_id: team.id.clone(),
                    team_name: team.name.clone(),
                    team_archived: team.archived_at.is_some(),
                });
            }
        }
        PaletteData { projects }
    }
}

/// Ordered palette rows for `query`. An empty (or blank) query lists every
/// navigation target.
pub fn palette_results(query: &str, data: &PaletteData) -> Vec<PaletteRow> {
    let needle = query.trim().to_lowercase();
    let mut rows = Vec::new();
    push_views(&needle, data, &mut rows);
    push_projects(&needle, data, &mut rows);
    rows
}

/// Case-insensitive substring match; `needle` is already trimmed and lowercased.
fn name_matches(needle: &str, name: &str) -> bool {
    name.to_lowercase().contains(needle)
}

fn push_views(needle: &str, _data: &PaletteData, rows: &mut Vec<PaletteRow>) {
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

fn push_projects(needle: &str, data: &PaletteData, rows: &mut Vec<PaletteRow>) {
    for p in data.projects.iter().filter(|p| !p.team_archived) {
        if name_matches(needle, &p.name) {
            rows.push(PaletteRow {
                kind: PaletteKind::Project,
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

#[cfg(test)]
mod tests {
    use super::*;

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

    fn project(id: &str, name: &str, team: &str, archived: bool) -> PaletteProject {
        PaletteProject {
            id: id.into(),
            name: name.into(),
            team_id: format!("t-{team}"),
            team_name: team.into(),
            team_archived: archived,
        }
    }

    fn project_data() -> PaletteData {
        PaletteData {
            projects: vec![
                project("p1", "Billing revamp", "Alpha", false),
                project("p2", "Onboarding", "Beta", false),
                project("p3", "Billing legacy", "Old", true),
            ],
        }
    }

    fn project_rows(query: &str) -> Vec<PaletteRow> {
        palette_results(query, &project_data())
            .into_iter()
            .filter(|r| r.kind == PaletteKind::Project)
            .collect()
    }

    #[test]
    fn project_matches_substring_ignoring_case_across_teams() {
        let rows = project_rows("ONBO");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "Onboarding");
        assert_eq!(rows[0].secondary, "Beta");
        assert_eq!(rows[0].kind.label(), "Project");
        assert_eq!(
            rows[0].nav,
            PaletteNav::Project { project_id: "p2".into(), team_id: "t-Beta".into() }
        );
    }

    #[test]
    fn projects_in_archived_teams_are_left_out() {
        let rows = project_rows("billing");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "Billing revamp");
        assert!(project_rows("legacy").is_empty());
    }

    #[test]
    fn empty_query_lists_projects_after_views() {
        let rows = palette_results("", &project_data());
        let kinds: Vec<_> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [
                PaletteKind::View,
                PaletteKind::View,
                PaletteKind::View,
                PaletteKind::View,
                PaletteKind::Project,
                PaletteKind::Project,
            ]
        );
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
