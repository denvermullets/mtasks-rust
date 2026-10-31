pub mod client;
pub mod dto;
pub mod error;

pub mod comments;
pub mod deleted;
pub mod dependencies;
pub mod issues;
pub mod labels;
pub mod lanes;
pub mod members;
pub mod notifications;
pub mod projects;
pub mod saved_views;
pub mod teams;
pub mod users;

pub use client::{ApiClient, Conditional, Page};
pub use comments::{CommentParams, CommentTarget};
pub use dependencies::{Direction, IssueDependencyResponse};
pub use dto::{
    CommentDto, IssueDependencyDto, IssueDto, IssueRef, LabelDto, LabelRef, LaneDto, MemberDto,
    ParentIssueRef, ProjectDto, RecordRef, TeamDto, UserRef,
};
pub use error::ApiError;
pub use issues::IssueParams;
pub use labels::LabelParams;
pub use projects::ProjectParams;
pub use users::{HomePageDto, MeDto, MeTokenDto};

/// Percent-encode a query value (timestamps carry `:` and possibly `+`).
pub(crate) fn encode_query_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `?updated_since=<encoded>` for an optional cursor, or an empty string.
pub(crate) fn updated_since_query(updated_since: Option<&str>) -> String {
    match updated_since {
        Some(t) => format!("?updated_since={}", encode_query_value(t)),
        None => String::new(),
    }
}
