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

/// "Jan 05" from an RFC 3339 timestamp or `YYYY-MM-DD` date (Rails list-row
/// format); falls back to the raw date prefix when unparseable.
pub fn short_date(raw: &str) -> String {
    let day = raw.get(..10).unwrap_or(raw);
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| d.format("%b %d").to_string())
        .unwrap_or_else(|_| day.to_string())
}

/// "3pt" / "2.5pt" estimate label.
pub fn format_estimate(e: f64) -> String {
    if e.fract() == 0.0 {
        format!("{}pt", e as i64)
    } else {
        format!("{e:.1}pt")
    }
}

#[cfg(test)]
mod date_tests {
    use super::*;

    #[test]
    fn short_date_formats_and_falls_back() {
        assert_eq!(short_date("2026-01-05T10:00:00Z"), "Jan 05");
        assert_eq!(short_date("2026-03-09"), "Mar 09");
        assert_eq!(short_date("bad"), "bad");
    }

    #[test]
    fn estimate_label() {
        assert_eq!(format_estimate(3.0), "3pt");
        assert_eq!(format_estimate(2.5), "2.5pt");
    }
}
