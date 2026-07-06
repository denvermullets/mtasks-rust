//! Small view-layer formatting helpers shared across views.

use chrono::{DateTime, Utc};

/// Coarse "N{s,m,h,d} ago" label for a past instant. Clamps future instants to
/// `0s ago`.
pub fn format_age(then: DateTime<Utc>) -> String {
    let secs = (Utc::now() - then).num_seconds().max(0);
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Relative label for an RFC3339 timestamp string (as stored on models). Falls
/// back to the raw string if it can't be parsed.
pub fn relative_time(rfc3339: &str) -> String {
    match DateTime::parse_from_rfc3339(rfc3339) {
        Ok(dt) => format_age(dt.with_timezone(&Utc)),
        Err(_) => rfc3339.to_string(),
    }
}
