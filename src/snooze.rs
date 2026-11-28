//! Thread snooze: presets, the wake check, and the label shown in lists.
//!
//! A snooze is one pair of timestamps on a thread: when it was set and when it
//! ends. Nothing schedules a wake; every surface compares the end time with the
//! current time, so a closed laptop or a restarted TUI cannot leave a thread
//! hidden past its timer.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Datelike, Duration, Utc};

pub const INDEFINITE_UNTIL: &str = "9999-12-31T23:59:59.000Z";

fn indefinite_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(INDEFINITE_UNTIL)
        .expect("the indefinite snooze timestamp is valid")
        .with_timezone(&Utc)
}

pub fn is_indefinite_time(until: DateTime<Utc>) -> bool {
    until.date_naive().year() >= 9999
}

pub fn is_indefinite(value: &str) -> bool {
    parse_timestamp(value).is_some_and(is_indefinite_time)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnoozePreset {
    OneHour,
    FourHours,
    Tomorrow,
    Week,
    Indefinite,
}

pub const PRESETS: [SnoozePreset; 5] = [
    SnoozePreset::OneHour,
    SnoozePreset::FourHours,
    SnoozePreset::Tomorrow,
    SnoozePreset::Week,
    SnoozePreset::Indefinite,
];

impl SnoozePreset {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "1h" | "hour" => Some(Self::OneHour),
            "4h" => Some(Self::FourHours),
            "tomorrow" | "1d" => Some(Self::Tomorrow),
            "week" | "7d" => Some(Self::Week),
            "forever" | "indefinite" | "indefinitely" => Some(Self::Indefinite),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OneHour => "1h",
            Self::FourHours => "4h",
            Self::Tomorrow => "tomorrow",
            Self::Week => "week",
            Self::Indefinite => "forever",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::OneHour => "in 1 hour",
            Self::FourHours => "in 4 hours",
            Self::Tomorrow => "tomorrow",
            Self::Week => "next week",
            Self::Indefinite => "until woken",
        }
    }

    pub fn until(self, now: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Self::OneHour => now + Duration::hours(1),
            Self::FourHours => now + Duration::hours(4),
            Self::Tomorrow => now + Duration::days(1),
            Self::Week => now + Duration::days(7),
            Self::Indefinite => indefinite_time(),
        }
    }
}

pub fn parse_preset(value: &str) -> Result<SnoozePreset> {
    SnoozePreset::parse(value).ok_or_else(|| {
        anyhow!("unknown snooze preset \"{value}\"; use 1h, 4h, tomorrow, week, forever, or off")
    })
}

/// The end time of an active snooze, or None when the thread is awake.
pub fn active_until(snoozed_until: Option<&str>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let until = snoozed_until.and_then(parse_timestamp)?;
    (until > now).then_some(until)
}

pub fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

/// `14:32`, or `Nov 27 09:00` when the wake is not today. Local time, because
/// the person reading it is the one who will come back to the thread.
pub fn wake_label(until: DateTime<Utc>, now: DateTime<Utc>) -> String {
    if is_indefinite_time(until) {
        return "until woken".to_string();
    }
    let local = until.with_timezone(&chrono::Local);
    let local_now = now.with_timezone(&chrono::Local);
    if local.date_naive() == local_now.date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%b %e %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn presets_move_the_wake_forward_by_their_span() {
        let now = at("2026-11-26T12:00:00Z");
        assert_eq!(SnoozePreset::OneHour.until(now), at("2026-11-26T13:00:00Z"));
        assert_eq!(
            SnoozePreset::FourHours.until(now),
            at("2026-11-26T16:00:00Z")
        );
        assert_eq!(
            SnoozePreset::Tomorrow.until(now),
            at("2026-11-27T12:00:00Z")
        );
        assert_eq!(SnoozePreset::Week.until(now), at("2026-12-03T12:00:00Z"));
        assert_eq!(SnoozePreset::Indefinite.until(now), at(INDEFINITE_UNTIL));
    }

    #[test]
    fn parse_accepts_every_offered_spelling() {
        for preset in PRESETS {
            assert_eq!(SnoozePreset::parse(preset.as_str()), Some(preset));
        }
        assert_eq!(SnoozePreset::parse("hour"), Some(SnoozePreset::OneHour));
        assert_eq!(
            SnoozePreset::parse("indefinitely"),
            Some(SnoozePreset::Indefinite)
        );
        assert!(SnoozePreset::parse("3h").is_none());
        let error = parse_preset("3h").unwrap_err().to_string();
        assert!(error.contains("forever"), "{error}");
        assert!(error.contains("off"), "{error}");
    }

    #[test]
    fn an_active_snooze_ends_at_its_timestamp() {
        let now = at("2026-11-26T12:00:00Z");
        assert_eq!(
            active_until(Some("2026-11-26T13:00:00Z"), now),
            Some(at("2026-11-26T13:00:00Z"))
        );
        assert_eq!(active_until(Some("2026-11-26T11:59:59Z"), now), None);
        assert_eq!(active_until(None, now), None);
        assert_eq!(active_until(Some("not a timestamp"), now), None);
    }

    #[test]
    fn wake_label_switches_from_time_to_date_outside_today() {
        let now = at("2026-11-26T12:00:00Z");
        let later = wake_label(at("2026-11-26T23:00:00Z"), now);
        assert!(later.contains(':'), "{later}");
        let next_week = wake_label(at("2026-12-03T12:00:00Z"), now);
        assert!(next_week.contains("Dec"), "{next_week}");
        assert_eq!(wake_label(at(INDEFINITE_UNTIL), now), "until woken");
    }

    #[test]
    fn the_indefinite_sentinel_is_recognized_and_stays_active() {
        let now = at("2026-11-26T12:00:00Z");
        assert!(is_indefinite(INDEFINITE_UNTIL));
        assert!(!is_indefinite("2026-11-26T13:00:00Z"));
        assert_eq!(
            active_until(Some(INDEFINITE_UNTIL), now),
            Some(at(INDEFINITE_UNTIL))
        );
    }
}
