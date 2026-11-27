//! Durable schedules use a five-field cron expression in UTC.
//!
//! Fields are minute, hour, day of month, month, and day of week. Each field
//! accepts `*`, a number, a list, an inclusive range, and an optional step
//! (`*/15`, `1-5/2`). Weekdays are 0 for Sunday through 6 for Saturday.
//! Day-of-month and day-of-week follow the traditional cron OR rule when both
//! are restricted. Schedules are evaluated in UTC so a host and its clients
//! agree on the next run without a timezone database.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc};

#[derive(Clone, Debug)]
pub struct CronSchedule {
    minutes: Field,
    hours: Field,
    days: Field,
    months: Field,
    weekdays: Field,
}

#[derive(Clone, Debug)]
struct Field {
    allowed: Vec<bool>,
    min: u32,
    max: u32,
    wildcard: bool,
}

impl Field {
    fn parse(spec: &str, label: &str, min: u32, max: u32) -> Result<Field> {
        let wildcard = spec == "*";
        let mut allowed = vec![false; (max - min + 1) as usize];
        for part in spec.split(',') {
            if part.is_empty() {
                bail!("cron {label} field has an empty list item");
            }
            let (base, step) = match part.split_once('/') {
                Some((base, step)) => {
                    let step: u32 = step
                        .parse()
                        .with_context(|| format!("cron {label} step {step:?} is not a number"))?;
                    if step == 0 {
                        bail!("cron {label} step must be at least 1");
                    }
                    (base, Some(step))
                }
                None => (part, None),
            };
            let (start, end) = if base == "*" {
                (min, max)
            } else if let Some((start, end)) = base.split_once('-') {
                let start = number(start, label, min, max)?;
                let end = number(end, label, min, max)?;
                if start > end {
                    bail!("cron {label} range {start}-{end} is reversed");
                }
                (start, end)
            } else {
                let value = number(base, label, min, max)?;
                (value, if step.is_some() { max } else { value })
            };
            let step = step.unwrap_or(1);
            let mut value = start;
            while value <= end {
                allowed[(value - min) as usize] = true;
                value += step;
            }
        }
        if !allowed.iter().any(|selected| *selected) {
            bail!("cron {label} field selects no values");
        }
        Ok(Field {
            allowed,
            min,
            max,
            wildcard,
        })
    }

    fn matches(&self, value: u32) -> bool {
        value >= self.min && value <= self.max && self.allowed[(value - self.min) as usize]
    }
}

fn number(text: &str, label: &str, min: u32, max: u32) -> Result<u32> {
    let value: u32 = text
        .parse()
        .with_context(|| format!("cron {label} value {text:?} is not a number"))?;
    if value < min || value > max {
        bail!("cron {label} value {value} is outside {min}-{max}");
    }
    Ok(value)
}

pub fn parse(expression: &str) -> Result<CronSchedule> {
    let parts: Vec<&str> = expression.split_whitespace().collect();
    if parts.len() != 5 {
        bail!("cron expression needs five fields: minute hour day month weekday");
    }
    Ok(CronSchedule {
        minutes: Field::parse(parts[0], "minute", 0, 59)?,
        hours: Field::parse(parts[1], "hour", 0, 23)?,
        days: Field::parse(parts[2], "day", 1, 31)?,
        months: Field::parse(parts[3], "month", 1, 12)?,
        weekdays: Field::parse(parts[4], "weekday", 0, 6)?,
    })
}

pub fn next_after(schedule: &CronSchedule, after: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let mut candidate = (after + Duration::minutes(1))
        .with_second(0)
        .and_then(|time| time.with_nanosecond(0))
        .ok_or_else(|| anyhow::anyhow!("cannot round the cron candidate to a minute"))?;
    let deadline = after + Duration::days(366 * 5);
    while candidate <= deadline {
        if !schedule.months.matches(candidate.month()) {
            candidate = first_of_next_month(candidate)?;
            continue;
        }
        if !day_matches(schedule, candidate) {
            candidate = next_day_start(candidate)?;
            continue;
        }
        if !schedule.hours.matches(candidate.hour()) {
            candidate = next_hour_start(candidate)?;
            continue;
        }
        if !schedule.minutes.matches(candidate.minute()) {
            candidate += Duration::minutes(1);
            continue;
        }
        return Ok(candidate);
    }
    bail!("cron expression has no occurrence in the next five years")
}

fn day_matches(schedule: &CronSchedule, time: DateTime<Utc>) -> bool {
    let day = schedule.days.matches(time.day());
    let weekday = schedule
        .weekdays
        .matches(time.weekday().num_days_from_sunday());
    match (schedule.days.wildcard, schedule.weekdays.wildcard) {
        (true, true) => true,
        (true, false) => weekday,
        (false, true) => day,
        (false, false) => day || weekday,
    }
}

fn first_of_next_month(time: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let (year, month) = if time.month() == 12 {
        (time.year() + 1, 1)
    } else {
        (time.year(), time.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| naive.and_utc())
        .ok_or_else(|| anyhow::anyhow!("cron candidate crossed the supported date range"))
}

fn next_day_start(time: DateTime<Utc>) -> Result<DateTime<Utc>> {
    time.date_naive()
        .succ_opt()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| naive.and_utc())
        .ok_or_else(|| anyhow::anyhow!("cron candidate crossed the supported date range"))
}

fn next_hour_start(time: DateTime<Utc>) -> Result<DateTime<Utc>> {
    time.with_minute(0)
        .and_then(|time| time.with_second(0))
        .map(|time| time + Duration::hours(1))
        .ok_or_else(|| anyhow::anyhow!("cannot round the cron candidate to an hour"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn next(expression: &str, after: &str) -> DateTime<Utc> {
        next_after(&parse(expression).unwrap(), at(after)).unwrap()
    }

    #[test]
    fn steps_run_within_the_hour() {
        assert_eq!(
            next("*/15 * * * *", "2026-11-27T10:07:30Z").to_rfc3339(),
            "2026-11-27T10:15:00+00:00"
        );
        assert_eq!(
            next("*/15 * * * *", "2026-11-27T10:45:00Z").to_rfc3339(),
            "2026-11-27T11:00:00+00:00"
        );
    }

    #[test]
    fn weekdays_skip_the_weekend() {
        assert_eq!(
            next("0 9 * * 1-5", "2026-11-27T10:00:00Z").to_rfc3339(),
            "2026-11-30T09:00:00+00:00"
        );
    }

    #[test]
    fn monthly_runs_roll_into_the_next_month() {
        assert_eq!(
            next("30 2 1 * *", "2026-11-02T00:00:00Z").to_rfc3339(),
            "2026-12-01T02:30:00+00:00"
        );
    }

    #[test]
    fn leap_day_is_found() {
        assert_eq!(
            next("0 0 29 2 *", "2026-03-01T00:00:00Z").to_rfc3339(),
            "2028-02-29T00:00:00+00:00"
        );
    }

    #[test]
    fn an_impossible_date_reports_itself() {
        let error = next_after(&parse("0 0 31 2 *").unwrap(), at("2026-01-01T00:00:00Z"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no occurrence"), "{error}");
    }

    #[test]
    fn lists_and_ranges_select_values() {
        assert_eq!(
            next("15,45 * * * *", "2026-11-27T10:20:00Z").to_rfc3339(),
            "2026-11-27T10:45:00+00:00"
        );
        assert_eq!(
            next("0 0 * * 0", "2026-11-27T10:00:00Z").to_rfc3339(),
            "2026-11-29T00:00:00+00:00"
        );
    }

    #[test]
    fn day_of_month_or_weekday_matches_the_traditional_rule() {
        let result = next("0 0 13 * 5", "2026-11-01T00:00:00Z");
        assert_eq!(result.format("%H:%M:%S").to_string(), "00:00:00");
        assert!(
            result.day() == 13 || result.weekday().num_days_from_sunday() == 5,
            "{result}"
        );
        assert!(result <= at("2026-11-14T00:00:00Z"));
    }

    #[test]
    fn invalid_expressions_are_refused() {
        for expression in [
            "",
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 7",
            "5-1 * * * *",
            "*/0 * * * *",
            "a * * * *",
            "1,,2 * * * *",
        ] {
            assert!(parse(expression).is_err(), "{expression}");
        }
    }
}
