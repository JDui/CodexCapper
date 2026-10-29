//! Reset-time formatting shared by the native taskbar rows.
//! Keep the thresholds and formats in step with `src/lib/utils.ts`.

use chrono::{DateTime, Duration, Local, Utc};

fn uses_countdown(style: &str, label: &str, left: Duration) -> bool {
    style == "countdown"
        || (style != "absolute" && left <= Duration::hours(if label == "7d" { 12 } else { 1 }))
}

pub fn format(
    reset: Option<DateTime<Utc>>,
    label: &str,
    style: &str,
    now: DateTime<Utc>,
    compact: bool,
) -> String {
    let Some(reset) = reset else {
        return String::new();
    };
    let left = reset - now;
    if !uses_countdown(style, label, left) {
        let local = reset.with_timezone(&Local);
        let pattern = if local.date_naive() == now.with_timezone(&Local).date_naive() {
            "%H:%M"
        } else {
            "%m/%d %H:%M"
        };
        return local.format(pattern).to_string();
    }
    if left <= Duration::zero() {
        return if compact { "0m" } else { "即将重置" }.into();
    }
    // Ceiling in milliseconds keeps the display correct just above a minute.
    let minutes = (left.num_milliseconds() + 59_999) / 60_000;
    let days = minutes / 1440;
    let hours = minutes % 1440 / 60;
    let mins = minutes % 60;
    if compact && days >= 100 {
        format!("{days}d")
    } else if compact && days > 0 {
        format!("{days}d {hours}h")
    } else if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_thresholds_include_the_exact_boundary_for_each_window() {
        for (label, hours) in [("5h", 1), ("7d", 12)] {
            let limit = Duration::hours(hours);
            assert!(uses_countdown("auto", label, limit));
            assert!(uses_countdown(
                "auto",
                label,
                limit - Duration::milliseconds(1)
            ));
            assert!(!uses_countdown(
                "auto",
                label,
                limit + Duration::milliseconds(1)
            ));
        }
    }

    #[test]
    fn explicit_styles_override_auto_and_unknown_styles_use_auto() {
        assert!(uses_countdown("countdown", "5h", Duration::days(2)));
        assert!(!uses_countdown("absolute", "7d", Duration::minutes(1)));
        assert!(uses_countdown("", "7d", Duration::hours(11)));
        assert!(!uses_countdown("", "5h", Duration::hours(2)));
    }

    #[test]
    fn countdown_rounds_up_and_handles_missing_and_expired_resets() {
        let now = Utc::now();
        assert_eq!(format(None, "7d", "auto", now, true), "");
        assert_eq!(format(Some(now), "7d", "auto", now, true), "0m");
        assert_eq!(format(Some(now), "7d", "auto", now, false), "即将重置");
        assert_eq!(
            format(
                Some(now + Duration::milliseconds(60_001)),
                "5h",
                "auto",
                now,
                true
            ),
            "2m"
        );
        assert_eq!(
            format(Some(now + Duration::hours(12)), "7d", "auto", now, true),
            "12h 0m"
        );
    }

    #[test]
    fn absolute_reset_keeps_the_clock_and_adds_a_date_outside_today() {
        let now = Utc::now();
        assert_eq!(
            format(Some(now), "5h", "absolute", now, true),
            now.with_timezone(&Local).format("%H:%M").to_string()
        );
        let later = now + Duration::days(3);
        assert_eq!(
            format(Some(later), "7d", "auto", now, true),
            later
                .with_timezone(&Local)
                .format("%m/%d %H:%M")
                .to_string()
        );
    }
}
