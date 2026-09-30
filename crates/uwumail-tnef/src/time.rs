//! Dates of the Windows world as Unix seconds, and the calendar arithmetic iCalendar needs.

/// Seconds between 1601-01-01 (FILETIME and MAPI minutes) and 1970-01-01.
const EPOCH_1601: i64 = 11_644_473_600;

/// A FILETIME (100 ns since 1601) as Unix seconds.
pub fn filetime(value: u64) -> i64 {
    (value / 10_000_000) as i64 - EPOCH_1601
}

/// Minutes since 1601, as recurrence patterns count them, as (local) Unix seconds.
pub fn minutes_1601(value: u32) -> i64 {
    i64::from(value) * 60 - EPOCH_1601
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's `days_from_civil`).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date of a day count since 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// 0 for Sunday … 6 for Saturday.
pub fn weekday(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

fn days_in_month(year: i64, month: u32) -> u32 {
    let next = if month == 12 { days_from_civil(year + 1, 1, 1) } else { days_from_civil(year, month + 1, 1) };
    (next - days_from_civil(year, month, 1)) as u32
}

/// The day of month of the `week`th (1–4, 5 = last) `weekday` (0 = Sunday) of a month.
pub fn nth_weekday(year: i64, month: u32, week: u32, weekday_wanted: u32) -> u32 {
    let first = days_from_civil(year, month, 1);
    let offset = (weekday_wanted + 7 - weekday(first)) % 7;
    let mut day = 1 + offset + 7 * (week.clamp(1, 5) - 1);
    let last = days_in_month(year, month);
    while day > last {
        day -= 7;
    }
    day
}

/// `YYYYMMDDTHHMMSS` (with `Z` when `utc`).
pub fn ical_datetime(secs: i64, utc: bool) -> String {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}{}",
        y.clamp(0, 9999),
        m,
        d,
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        if utc { "Z" } else { "" }
    )
}

/// `YYYYMMDD`.
pub fn ical_date(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{:04}{:02}{:02}", y.clamp(0, 9999), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2026, 10, 25)), (2026, 10, 25));
        assert_eq!(civil_from_days(days_from_civil(1601, 1, 1)), (1601, 1, 1));
        assert_eq!(weekday(days_from_civil(2026, 9, 30)), 3);
        // Last Sunday of October 2026, second Sunday of March 2026.
        assert_eq!(nth_weekday(2026, 10, 5, 0), 25);
        assert_eq!(nth_weekday(2026, 3, 2, 0), 8);
        assert_eq!(filetime(116_444_736_000_000_000), 0);
        assert_eq!(minutes_1601(0), -EPOCH_1601);
        assert_eq!(ical_datetime(0, true), "19700101T000000Z");
        assert_eq!(ical_date(86_400 * 365), "19710101");
    }
}
