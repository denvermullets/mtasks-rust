//! Board and dashboard search matching: no UI. A query is normalized to a
//! lowercase needle, then matched against an issue's title or identifier.
//! (The palette and `#` suggestions use the stricter `issue_match` instead.)

use crate::db::models::{Issue, Team};

/// Normalize a raw search query into the lowercase needle the matchers expect.
/// An empty needle matches everything.
pub fn needle(query: &str) -> String {
    query.trim().to_lowercase()
}

/// Whether `issue` matches `needle` by title or identifier (e.g. "JAIT-111").
pub fn issue_matches(issue: &Issue, team: Option<&Team>, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let ident = match team {
        Some(tm) => format!("{}-{}", tm.identifier, issue.team_number.unwrap_or(0)),
        None => format!("#{}", issue.team_number.unwrap_or(0)),
    };
    ident.to_lowercase().contains(needle) || issue.title.to_lowercase().contains(needle)
}
