//! The one issue matcher, with Rails `matching_search` semantics. Used by `#`
//! suggestions and the Command palette; there is no second copy of this rule.
//!
//! - The query is trimmed; a blank query matches nothing.
//! - `^(?:([A-Za-z0-9]{3,4})-)?(\d+)$` (case-insensitive) is an identifier
//!   search: exact team number, and when a prefix is given only in the team
//!   whose identifier equals it.
//! - Anything else is a case-insensitive substring match on the title only.

use crate::db::models::Issue;

/// A parsed query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueQuery {
    /// Blank query: matches nothing.
    Blank,
    /// Identifier search: optional lowercased team prefix and the number.
    Identifier { prefix: Option<String>, number: i64 },
    /// Lowercased title substring.
    Title(String),
}

impl IssueQuery {
    pub fn parse(raw: &str) -> Self {
        let q = raw.trim();
        if q.is_empty() {
            return Self::Blank;
        }
        let (prefix, digits) = match q.split_once('-') {
            Some((p, d))
                if (3..=4).contains(&p.chars().count())
                    && p.chars().all(|c| c.is_ascii_alphanumeric()) =>
            {
                (Some(p.to_lowercase()), d)
            }
            _ => (None, q),
        };
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            // A digit string too large for any team number matches nothing.
            let number = digits.parse::<i64>().unwrap_or(-1);
            return Self::Identifier { prefix, number };
        }
        Self::Title(q.to_lowercase())
    }

    /// Does `issue` (belonging to the team with `team_identifier`) match?
    pub fn matches(&self, issue: &Issue, team_identifier: &str) -> bool {
        match self {
            Self::Blank => false,
            Self::Identifier { prefix, number } => {
                issue.team_number.is_some_and(|n| i64::from(n) == *number)
                    && prefix
                        .as_deref()
                        .is_none_or(|p| team_identifier.to_lowercase() == p)
            }
            Self::Title(t) => issue.title.to_lowercase().contains(t),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue_matches(issue: &Issue, team_identifier: &str, query: &str) -> bool {
        IssueQuery::parse(query).matches(issue, team_identifier)
    }

    fn issue(num: Option<i32>, title: &str, desc: Option<&str>) -> Issue {
        Issue {
            id: "i".into(),
            title: title.into(),
            description: desc.map(String::from),
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
            archived_at: None,
            remote_id: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn identifier_forms_match_exact_number_only() {
        let i = issue(Some(12), "x", None);
        for q in ["ENG-12", "eng-12", "12", " 12 "] {
            assert!(issue_matches(&i, "ENG", q), "{q}");
        }
        assert!(!issue_matches(&issue(Some(120), "x", None), "ENG", "12"));
        assert!(!issue_matches(&issue(Some(212), "x", None), "ENG", "ENG-12"));
    }

    #[test]
    fn other_prefix_does_not_match() {
        let i = issue(Some(12), "x", None);
        assert!(!issue_matches(&i, "ENG", "OPS-12"));
    }

    #[test]
    fn unnumbered_issue_never_matches_identifier() {
        assert!(!issue_matches(&issue(None, "x", None), "ENG", "1"));
    }

    #[test]
    fn title_matches_case_insensitive_anywhere_not_description() {
        let i = issue(Some(1), "Fix Login bug", Some("secret sauce"));
        assert!(issue_matches(&i, "ENG", "login"));
        assert!(issue_matches(&i, "ENG", "  BUG "));
        assert!(!issue_matches(&i, "ENG", "sauce"));
    }

    #[test]
    fn non_identifier_shapes_fall_back_to_title() {
        // Prefix too short / too long / extra text: title search.
        let i = issue(Some(1), "see ab-1 and abcde-1 and 12-x", None);
        assert!(issue_matches(&i, "ENG", "ab-1"));
        assert!(issue_matches(&i, "ENG", "abcde-1"));
        assert!(issue_matches(&i, "ENG", "12-x"));
        assert!(!issue_matches(&issue(Some(1), "t", None), "ENG", "ab-1"));
    }

    #[test]
    fn blank_matches_nothing() {
        let i = issue(Some(1), "", None);
        assert!(!issue_matches(&i, "ENG", ""));
        assert!(!issue_matches(&i, "ENG", "   "));
    }

    #[test]
    fn huge_number_matches_nothing() {
        let i = issue(Some(1), "t", None);
        assert!(!issue_matches(&i, "ENG", "99999999999999999999999"));
    }
}
