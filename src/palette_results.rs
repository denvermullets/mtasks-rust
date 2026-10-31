//! Pure results function for the Command palette. The view only renders what
//! this returns, so it is the main test seam. New target kinds (team, project,
//! issue) are added as a `PaletteKind` variant, a `PaletteNav` variant, and a
//! `push_*` function called from `palette_results`.

use crate::app::View;
use crate::db::models::Team;

/// What a row is, shown as a label so rows can be scanned by kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteKind {
    View,
    Team,
}

impl PaletteKind {
    pub fn label(self) -> &'static str {
        match self {
            PaletteKind::View => "View",
            PaletteKind::Team => "Team",
        }
    }
}

/// Where choosing a row goes.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteNav {
    /// Switch view for the current team (`Message::NavigateToView`).
    View(View),
    /// Open this team's Board (`Message::SelectTeamView(id, View::Board)`).
    Team(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaletteRow {
    pub kind: PaletteKind,
    pub primary: String,
    pub secondary: String,
    pub nav: PaletteNav,
}

/// Local data the palette searches. Later kinds add their fields here.
#[derive(Debug, Clone, Default)]
pub struct PaletteData {
    pub teams: Vec<Team>,
}

/// Ordered palette rows for `query`. An empty (or blank) query lists every
/// navigation target.
pub fn palette_results(query: &str, data: &PaletteData) -> Vec<PaletteRow> {
    let needle = query.trim().to_lowercase();
    let mut rows = Vec::new();
    push_views(&needle, data, &mut rows);
    push_teams(&needle, data, &mut rows);
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

fn push_teams(needle: &str, data: &PaletteData, rows: &mut Vec<PaletteRow>) {
    for team in data.teams.iter().filter(|t| t.archived_at.is_none()) {
        if name_matches(needle, &team.name) || name_matches(needle, &team.identifier) {
            rows.push(PaletteRow {
                kind: PaletteKind::Team,
                primary: team.name.clone(),
                secondary: team.identifier.clone(),
                nav: PaletteNav::Team(team.id.clone()),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(id: &str, name: &str, ident: &str, archived: bool) -> Team {
        Team {
            id: id.into(),
            name: name.into(),
            identifier: ident.into(),
            remote_id: None,
            archived_at: archived.then(|| "2026-01-01T00:00:00.000Z".into()),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn data() -> PaletteData {
        PaletteData {
            teams: vec![
                team("t1", "JAIT App", "JRST", false),
                team("t2", "Rails API", "RAIL", false),
                team("t3", "Old Stuff", "OLD", true),
            ],
        }
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
            ["Board", "Projects", "Roadmap", "My Issues", "JAIT App", "Rails API"]
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
    fn archived_teams_never_appear() {
        assert!(names("old").is_empty());
        assert!(!names("").contains(&"Old Stuff".to_string()));
    }

    #[test]
    fn team_row_navigates_to_its_board() {
        let rows = palette_results("rails", &data());
        assert_eq!(rows[0].kind, PaletteKind::Team);
        assert_eq!(rows[0].kind.label(), "Team");
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

    #[test]
    fn rows_carry_kind_and_navigation() {
        let rows = palette_results("my", &data());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, PaletteKind::View);
        assert_eq!(rows[0].kind.label(), "View");
        assert_eq!(rows[0].nav, PaletteNav::View(View::MyIssues));
    }
}
