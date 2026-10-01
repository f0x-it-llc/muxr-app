//! The one coarse age wording, shared by every feature that says how long ago
//! something happened. Plain functions over a [`Duration`] — framework-free,
//! so a `domain` entity may call it.
//!
//! Bands: "just now" under a minute, "N minutes ago", "N hours ago",
//! "yesterday" (one day), "N days ago", "last week" (one week), then
//! "N weeks ago". A count of one reads in the singular.

use std::time::Duration;

const SECS_PER_MINUTE: u64 = 60;
const MINUTES_PER_HOUR: u64 = 60;
const HOURS_PER_DAY: u64 = 24;
const DAYS_PER_WEEK: u64 = 7;

/// The age wording for `elapsed`.
pub fn age_label(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / SECS_PER_MINUTE;
    if minutes < 1 {
        return "just now".to_string();
    }
    let hours = minutes / MINUTES_PER_HOUR;
    if hours < 1 {
        return plural(minutes, "minute");
    }
    let days = hours / HOURS_PER_DAY;
    if days < 1 {
        return plural(hours, "hour");
    }
    if days < 2 {
        return "yesterday".to_string();
    }
    let weeks = days / DAYS_PER_WEEK;
    if weeks < 1 {
        return plural(days, "day");
    }
    if weeks < 2 {
        return "last week".to_string();
    }
    plural(weeks, "week")
}

/// `"1 hour ago"` / `"3 hours ago"` — the one place the plural `s` is decided.
fn plural(count: u64, unit: &str) -> String {
    if count == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{count} {unit}s ago")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(secs: u64) -> String {
        age_label(Duration::from_secs(secs))
    }

    #[test]
    fn each_band_boundary_reads_as_documented() {
        assert_eq!(label(59), "just now");
        assert_eq!(label(60), "1 minute ago");
        assert_eq!(label(59 * 60), "59 minutes ago");
        assert_eq!(label(3600), "1 hour ago");
        assert_eq!(label(23 * 3600), "23 hours ago");
        assert_eq!(label(24 * 3600), "yesterday");
        assert_eq!(label(2 * 24 * 3600), "2 days ago");
        assert_eq!(label(7 * 24 * 3600), "last week");
        assert_eq!(label(14 * 24 * 3600), "2 weeks ago");
    }
}
