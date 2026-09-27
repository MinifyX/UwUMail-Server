//! The statistics under Server → Statistics: the last 30 days, or the last 12 months.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::StatsDay;

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::health::unix_now;
use crate::session::Admin;

const DAY: i64 = 86_400;

#[derive(Deserialize)]
pub struct Range {
    range: Option<String>,
}

/// `2026-09-27` for a Unix time.
fn day_of(at: i64) -> String {
    uwumail_jmap::dates::format(at)[..10].to_owned()
}

/// The month before `2026-01`: `2025-12`.
fn month_before(month: &str) -> String {
    let (year, number) = month.split_once('-').unwrap_or(("1970", "01"));
    let (year, number): (i64, i64) = (year.parse().unwrap_or(1970), number.parse().unwrap_or(1));
    if number == 1 { format!("{:04}-12", year - 1) } else { format!("{year:04}-{:02}", number - 1) }
}

/// Days (or months) in order, each with every value it has; empty periods are there too, so a
/// chart has a bar or a gap for each. Counters are summed into months, gauges keep their last value.
pub(crate) fn periods(days: Vec<StatsDay>, now: i64, months: bool) -> Vec<(String, BTreeMap<String, i64>)> {
    if !months {
        let mut by_day: BTreeMap<String, BTreeMap<String, i64>> =
            days.into_iter().map(|day| (day.day, day.values)).collect();
        return (0..30)
            .rev()
            .map(|back| {
                let day = day_of(now - back * DAY);
                let values = by_day.remove(&day).unwrap_or_default();
                (day, values)
            })
            .collect();
    }
    let mut names = vec![day_of(now)[..7].to_owned()];
    while names.len() < 12 {
        let before = month_before(names.last().expect("never empty"));
        names.push(before);
    }
    names.reverse();
    let mut by_month: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    // Oldest day first, so a gauge ends with the month's last reading.
    for day in days {
        let values = by_month.entry(day.day[..7].to_owned()).or_default();
        for (key, value) in day.values {
            if key.starts_with("gauge.") {
                values.insert(key, value);
            } else {
                *values.entry(key).or_default() += value;
            }
        }
    }
    names.into_iter().map(|month| (month.clone(), by_month.remove(&month).unwrap_or_default())).collect()
}

pub async fn show(State(web): State<Web>, _admin: Admin, Query(range): Query<Range>) -> ApiResult<Json<Value>> {
    let months = match range.range.as_deref() {
        None | Some("days") => false,
        Some("months") => true,
        Some(other) => return Err(ApiError::Invalid(format!("unknown range {other}"))),
    };
    let now = unix_now();
    let days = web.store().stats_days(if months { 366 } else { 30 }).await?;
    let periods = periods(days, now, months);
    let mut totals: BTreeMap<String, i64> = BTreeMap::new();
    for (_, values) in &periods {
        for (key, value) in values {
            if !key.starts_with("gauge.") {
                *totals.entry(key.clone()).or_default() += value;
            }
        }
    }
    let periods: Vec<Value> =
        periods.into_iter().map(|(period, values)| json!({ "period": period, "values": values })).collect();
    Ok(Json(json!({
        "range": if months { "months" } else { "days" },
        "periods": periods,
        "totals": totals,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-27 12:00 UTC.
    const NOON: i64 = 1_790_510_400;

    fn day(day: &str, values: &[(&str, i64)]) -> StatsDay {
        StatsDay { day: day.into(), values: values.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect() }
    }

    #[test]
    fn days_and_months_are_complete_and_add_up() {
        let days = vec![
            day("2026-08-31", &[("mail.received", 2), ("gauge.accounts", 3)]),
            day("2026-09-01", &[("mail.received", 5), ("gauge.accounts", 4)]),
            day("2026-09-27", &[("mail.received", 1), ("gauge.accounts", 5)]),
        ];
        let by_day = periods(days.clone(), NOON, false);
        assert_eq!(by_day.len(), 30);
        assert_eq!(by_day[0].0, "2026-08-29");
        assert_eq!(by_day[29].0, "2026-09-27");
        assert_eq!(by_day[29].1["mail.received"], 1);
        assert!(by_day[1].1.is_empty());

        let by_month = periods(days, NOON, true);
        assert_eq!(by_month.len(), 12);
        assert_eq!(by_month[0].0, "2025-10");
        assert_eq!(by_month[11].0, "2026-09");
        assert_eq!(by_month[11].1["mail.received"], 6);
        assert_eq!(by_month[11].1["gauge.accounts"], 5, "the last reading of the month");
        assert_eq!(by_month[10].1["mail.received"], 2);
        assert_eq!(month_before("2026-01"), "2025-12");
    }
}
