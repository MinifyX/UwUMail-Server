//! Which snapshots stay: the newest of each of the last days, weeks and months that have one.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Retention {
    pub daily: usize,
    pub weekly: usize,
    pub monthly: usize,
}

impl Default for Retention {
    fn default() -> Self {
        Retention { daily: 7, weekly: 4, monthly: 6 }
    }
}

/// Days since 1970-01-01 as (year, month).
fn year_month(days: i64) -> (i64, i64) {
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month)
}

/// Which day, week or month a time falls into.
type Bucket = fn(i64) -> i64;

/// The indexes of the snapshots to keep, given their creation times (Unix seconds, UTC). The newest
/// snapshot always stays.
pub fn keep(times: &[i64], retention: Retention) -> BTreeSet<usize> {
    let mut order: Vec<usize> = (0..times.len()).collect();
    order.sort_by_key(|&index| std::cmp::Reverse(times[index]));
    let mut kept = BTreeSet::new();
    if let Some(&newest) = order.first() {
        kept.insert(newest);
    }
    let buckets: [(usize, Bucket); 3] = [
        (retention.daily, |time| time.div_euclid(86_400)),
        // Weeks start on Monday; 1970-01-01 was a Thursday.
        (retention.weekly, |time| (time.div_euclid(86_400) + 3).div_euclid(7)),
        (retention.monthly, |time| {
            let (year, month) = year_month(time.div_euclid(86_400));
            year * 12 + month
        }),
    ];
    for (count, bucket_of) in buckets {
        let mut seen = BTreeSet::new();
        for &index in &order {
            if seen.len() >= count {
                break;
            }
            if seen.insert(bucket_of(times[index])) {
                kept.insert(index);
            }
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    #[test]
    fn days_weeks_and_months_each_keep_their_newest() {
        // 2026-01-01 00:00 UTC, then a snapshot every 6 hours for 400 days.
        let start = 1_767_225_600;
        let times: Vec<i64> = (0..400 * 4).map(|i| start + i * 6 * 3600).collect();
        let kept = keep(&times, Retention::default());
        let newest = *times.last().unwrap();
        assert!(kept.contains(&(times.len() - 1)));
        let ages: Vec<i64> = kept.iter().map(|&index| (newest - times[index]) / DAY).collect();
        assert!(ages.iter().filter(|age| **age < 7).count() >= 7, "a week of days: {ages:?}");
        assert!(ages.iter().any(|age| *age >= 120), "five months back, the newest of the sixth: {ages:?}");
        assert!(kept.len() <= 7 + 4 + 6, "{ages:?}");

        assert_eq!(keep(&[], Retention::default()).len(), 0);
        let none = Retention { daily: 0, weekly: 0, monthly: 0 };
        assert_eq!(keep(&[5, 9, 7], none), BTreeSet::from([1]), "the newest stays anyway");
        assert_eq!(year_month(0), (1970, 1));
        assert_eq!(year_month(20_454), (2026, 1));
    }
}
