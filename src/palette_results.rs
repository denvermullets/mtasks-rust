//! Pure results function for the Command palette. The view only renders what
//! this returns, so it is the main test seam. New target kinds (team, project,
//! issue) are added as a `PaletteKind` variant, a `PaletteNav` variant, and a
//! `push_*` function called from `palette_results`.

use crate::app::View;

/// What a row is, shown as a label so rows can be scanned by kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteKind {
    View,
}

impl PaletteKind {
    pub fn label(self) -> &'static str {
        match self {
            PaletteKind::View => "View",
        }
    }
}

/// Where choosing a row goes.
#[derive(Debug, Clone, PartialEq)]
pub enum PaletteNav {
    /// Switch view for the current team (`Message::NavigateToView`).
    View(View),
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
pub struct PaletteData {}

/// Most rows the palette ever shows.
pub const MAX_ROWS: usize = 20;

/// Keep the first `max` rows; call after ordering so the best matches survive.
fn cap_rows(mut rows: Vec<PaletteRow>, max: usize) -> Vec<PaletteRow> {
    rows.truncate(max);
    rows
}

/// Ordered palette rows for `query`. An empty (or blank) query lists every
/// navigation target.
pub fn palette_results(query: &str, data: &PaletteData) -> Vec<PaletteRow> {
    let needle = query.trim().to_lowercase();
    let mut rows = Vec::new();
    push_views(&needle, data, &mut rows);
    cap_rows(rows, MAX_ROWS)
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
