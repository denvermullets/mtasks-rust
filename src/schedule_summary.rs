//! Human-readable schedule wording for recurring issues (JRST-158).
//!
//! Ports `RecurringIssue#schedule_summary` from the Rails app so offline
//! creates/edits read the same as the web: "Every day", "Every 2 weeks on
//! Wednesday", "Every month on the 1st". Pure, no UI wiring.

// Unused until JRST-161 / JRST-172 wire it in; remove this allow then.
#![allow(dead_code)]

const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// English ordinal ("1st", "12th", "22nd"), as Rails' `Integer#ordinalize`.
fn ordinalize(n: i64) -> String {
    let suffix = match (n.abs() % 100, n.abs() % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// `frequency` is "daily" | "weekly" | "monthly"; `weekday` is 0 = Sunday.
/// A missing/out-of-range weekday or day of month drops the "on ..." clause;
/// an unknown frequency is treated as daily.
pub fn schedule_summary(
    frequency: &str,
    interval: i64,
    weekday: Option<i64>,
    day_of_month: Option<i64>,
) -> String {
    let unit = match frequency {
        "weekly" => "week",
        "monthly" => "month",
        _ => "day",
    };
    let every = if interval == 1 {
        format!("Every {unit}")
    } else {
        format!("Every {interval} {unit}s")
    };
    match frequency {
        "weekly" => match weekday.and_then(|w| DAY_NAMES.get(usize::try_from(w).ok()?)) {
            Some(name) => format!("{every} on {name}"),
            None => every,
        },
        "monthly" => match day_of_month {
            Some(d) if (1..=31).contains(&d) => format!("{every} on the {}", ordinalize(d)),
            _ => every,
        },
        _ => every,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily() {
        assert_eq!(schedule_summary("daily", 1, None, None), "Every day");
        assert_eq!(schedule_summary("daily", 3, None, None), "Every 3 days");
    }

    #[test]
    fn weekly() {
        assert_eq!(
            schedule_summary("weekly", 1, Some(3), None),
            "Every week on Wednesday"
        );
        assert_eq!(
            schedule_summary("weekly", 2, Some(3), None),
            "Every 2 weeks on Wednesday"
        );
    }

    #[test]
    fn monthly() {
        assert_eq!(
            schedule_summary("monthly", 1, None, Some(1)),
            "Every month on the 1st"
        );
        assert_eq!(
            schedule_summary("monthly", 6, None, Some(31)),
            "Every 6 months on the 31st"
        );
    }

    #[test]
    fn every_weekday_name() {
        let names = [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ];
        for (i, name) in names.iter().enumerate() {
            assert_eq!(
                schedule_summary("weekly", 1, Some(i as i64), None),
                format!("Every week on {name}")
            );
        }
    }

    #[test]
    fn ordinals() {
        let cases = [
            (1, "1st"),
            (2, "2nd"),
            (3, "3rd"),
            (4, "4th"),
            (11, "11th"),
            (12, "12th"),
            (13, "13th"),
            (21, "21st"),
            (22, "22nd"),
            (23, "23rd"),
            (31, "31st"),
        ];
        for (d, ord) in cases {
            assert_eq!(
                schedule_summary("monthly", 1, None, Some(d)),
                format!("Every month on the {ord}")
            );
        }
    }

    #[test]
    fn missing_or_invalid_detail_drops_clause() {
        assert_eq!(schedule_summary("weekly", 1, None, None), "Every week");
        assert_eq!(schedule_summary("weekly", 1, Some(7), None), "Every week");
        assert_eq!(schedule_summary("weekly", 1, Some(-1), None), "Every week");
        assert_eq!(schedule_summary("monthly", 2, None, Some(0)), "Every 2 months");
    }
}
