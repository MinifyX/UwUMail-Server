//! Counters for the admin panel's statistics and the Prometheus metrics (docs/metrics.md).
//!
//! Whatever happens (a message arrives, a login fails) is counted in memory with one atomic
//! increment, from any crate that has the store at hand. Every minute, and once more when the
//! server stops, what came in since the last time is added to the day's row in `stats_daily`. The
//! counters themselves only ever grow while the server runs, which is what Prometheus expects.

use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde::Serialize;

use crate::{Result, Store, now};

/// How many days of statistics are kept.
pub const STATS_RETENTION_DAYS: i64 = 400;

/// Something worth counting. The keys are stable: they are stored per day and become metric labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stat {
    /// Mail from other servers (or fetched from other providers) that was taken.
    Received,
    /// Of those, delivered into Junk for at least one recipient.
    Junk,
    /// Refused at the door: a recipient that does not exist here.
    RefusedUnknownRecipient,
    /// Refused as spam by its score.
    RefusedSpam,
    /// Refused because the virus scanner found something.
    RefusedVirus,
    /// Refused by a policy: DMARC, a blocked sender, relaying.
    RefusedPolicy,
    /// Asked to come back later (greylisting).
    RefusedGreylisted,
    /// Sent by our own people (SMTP submission or JMAP).
    Submitted,
    /// Handed to another server.
    Delivered,
    /// Another server did not take it this time; it is tried again.
    Deferred,
    /// Given up on: refused for good or out of retries.
    Bounced,
    LoginFailedSmtp,
    LoginFailedImap,
    LoginFailedJmap,
    LoginFailedDav,
    LoginFailedManageSieve,
    LoginFailedPortal,
    LoginFailedOther,
}

impl Stat {
    pub const ALL: [Stat; 18] = [
        Stat::Received,
        Stat::Junk,
        Stat::RefusedUnknownRecipient,
        Stat::RefusedSpam,
        Stat::RefusedVirus,
        Stat::RefusedPolicy,
        Stat::RefusedGreylisted,
        Stat::Submitted,
        Stat::Delivered,
        Stat::Deferred,
        Stat::Bounced,
        Stat::LoginFailedSmtp,
        Stat::LoginFailedImap,
        Stat::LoginFailedJmap,
        Stat::LoginFailedDav,
        Stat::LoginFailedManageSieve,
        Stat::LoginFailedPortal,
        Stat::LoginFailedOther,
    ];

    /// The name in `stats_daily` and in the portal.
    pub fn key(self) -> &'static str {
        match self {
            Stat::Received => "mail.received",
            Stat::Junk => "mail.junk",
            Stat::RefusedUnknownRecipient => "refused.unknownRecipient",
            Stat::RefusedSpam => "refused.spam",
            Stat::RefusedVirus => "refused.virus",
            Stat::RefusedPolicy => "refused.policy",
            Stat::RefusedGreylisted => "refused.greylisted",
            Stat::Submitted => "mail.submitted",
            Stat::Delivered => "mail.delivered",
            Stat::Deferred => "mail.deferred",
            Stat::Bounced => "mail.bounced",
            Stat::LoginFailedSmtp => "loginFailed.smtp",
            Stat::LoginFailedImap => "loginFailed.imap",
            Stat::LoginFailedJmap => "loginFailed.jmap",
            Stat::LoginFailedDav => "loginFailed.dav",
            Stat::LoginFailedManageSieve => "loginFailed.managesieve",
            Stat::LoginFailedPortal => "loginFailed.portal",
            Stat::LoginFailedOther => "loginFailed.other",
        }
    }

    /// The counter for a failed login over `protocol`, as the protocols name themselves.
    pub fn login_failed(protocol: &str) -> Stat {
        match protocol {
            "smtp" | "submission" => Stat::LoginFailedSmtp,
            "imap" => Stat::LoginFailedImap,
            "jmap" => Stat::LoginFailedJmap,
            "dav" | "caldav" | "carddav" => Stat::LoginFailedDav,
            "managesieve" | "sieve" => Stat::LoginFailedManageSieve,
            "portal" | "web" => Stat::LoginFailedPortal,
            _ => Stat::LoginFailedOther,
        }
    }

    fn index(self) -> usize {
        Stat::ALL.iter().position(|stat| *stat == self).expect("every stat is listed")
    }
}

/// The in-memory counters. Cheap to bump from anywhere; [`Store::flush_stats`] writes them down.
pub struct Stats {
    since_start: [AtomicU64; Stat::ALL.len()],
    /// What of `since_start` is already in the database. The async lock also keeps two flushes
    /// from adding the same numbers twice.
    flushed: tokio::sync::Mutex<[u64; Stat::ALL.len()]>,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            since_start: std::array::from_fn(|_| AtomicU64::new(0)),
            flushed: tokio::sync::Mutex::new([0; Stat::ALL.len()]),
        }
    }
}

impl Stats {
    pub fn count(&self, stat: Stat) {
        self.add(stat, 1);
    }

    pub fn add(&self, stat: Stat, amount: u64) {
        self.since_start[stat.index()].fetch_add(amount, Ordering::Relaxed);
    }

    /// Everything counted since the server started, for Prometheus.
    pub fn since_start(&self) -> Vec<(Stat, u64)> {
        Stat::ALL.iter().map(|stat| (*stat, self.since_start[stat.index()].load(Ordering::Relaxed))).collect()
    }

    fn snapshot(&self) -> [u64; Stat::ALL.len()] {
        std::array::from_fn(|index| self.since_start[index].load(Ordering::Relaxed))
    }
}

/// One day of statistics: counters summed up, gauges as last read.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct StatsDay {
    /// The UTC date, `2026-09-27`.
    pub day: String,
    pub values: std::collections::BTreeMap<String, i64>,
}

impl Store {
    pub fn stats(&self) -> &Stats {
        &self.inner.stats
    }

    /// Adds what was counted since the last flush to today's row, records today's gauges and
    /// forgets days older than [`STATS_RETENTION_DAYS`].
    pub async fn flush_stats(&self) -> Result<()> {
        self.flush_stats_at(now()).await
    }

    pub(crate) async fn flush_stats_at(&self, at: i64) -> Result<()> {
        let mut flushed = self.inner.stats.flushed.lock().await;
        let snapshot = self.inner.stats.snapshot();
        let deltas: Vec<(&'static str, i64)> = Stat::ALL
            .iter()
            .map(|stat| (stat.key(), snapshot[stat.index()].saturating_sub(flushed[stat.index()]) as i64))
            .filter(|(_, delta)| *delta > 0)
            .collect();
        self.write(move |tx| {
            for (key, delta) in &deltas {
                tx.execute(
                    "INSERT INTO stats_daily (day, key, value) VALUES (date(?1, 'unixepoch'), ?2, ?3)
                     ON CONFLICT (day, key) DO UPDATE SET value = value + excluded.value",
                    params![at, key, delta],
                )?;
            }
            for (key, value) in gauges(tx)? {
                tx.execute(
                    "INSERT INTO stats_daily (day, key, value) VALUES (date(?1, 'unixepoch'), ?2, ?3)
                     ON CONFLICT (day, key) DO UPDATE SET value = excluded.value",
                    params![at, key, value],
                )?;
            }
            tx.execute(
                "DELETE FROM stats_daily WHERE day < date(?1, 'unixepoch', ?2)",
                params![at, format!("-{STATS_RETENTION_DAYS} days")],
            )?;
            Ok(())
        })
        .await?;
        *flushed = snapshot;
        Ok(())
    }

    /// The days from `days - 1` days ago up to today, oldest first, with what is counted but not
    /// written down yet added to today. Days without anything are missing.
    pub async fn stats_days(&self, days: i64) -> Result<Vec<StatsDay>> {
        self.stats_days_at(now(), days).await
    }

    pub(crate) async fn stats_days_at(&self, at: i64, days: i64) -> Result<Vec<StatsDay>> {
        let pending: Vec<(&'static str, i64)> = {
            let flushed = self.inner.stats.flushed.lock().await;
            let snapshot = self.inner.stats.snapshot();
            Stat::ALL
                .iter()
                .map(|stat| (stat.key(), snapshot[stat.index()].saturating_sub(flushed[stat.index()]) as i64))
                .filter(|(_, delta)| *delta > 0)
                .collect()
        };
        let back = format!("-{} days", days.clamp(1, STATS_RETENTION_DAYS) - 1);
        self.read(move |conn| {
            let today: String = conn.query_row("SELECT date(?1, 'unixepoch')", [at], |row| row.get(0))?;
            let mut stmt = conn.prepare(
                "SELECT day, key, value FROM stats_daily WHERE day >= date(?1, 'unixepoch', ?2) ORDER BY day, key",
            )?;
            let rows = stmt.query_map(params![at, back], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
            })?;
            let mut result: Vec<StatsDay> = Vec::new();
            for row in rows {
                let (day, key, value) = row?;
                match result.last_mut() {
                    Some(last) if last.day == day => {
                        last.values.insert(key, value);
                    }
                    _ => result.push(StatsDay { day, values: [(key, value)].into_iter().collect() }),
                }
            }
            if !pending.is_empty() {
                if result.last().is_none_or(|last| last.day != today) {
                    result.push(StatsDay { day: today, values: Default::default() });
                }
                let last = result.last_mut().expect("today was just added");
                for (key, delta) in pending {
                    *last.values.entry(key.to_owned()).or_default() += delta;
                }
            }
            Ok(result)
        })
        .await
    }
}

/// The readings kept per day: how big the server is right now.
fn gauges(conn: &rusqlite::Connection) -> Result<Vec<(&'static str, i64)>> {
    let count = |sql: &str| -> rusqlite::Result<i64> { conn.query_row(sql, [], |row| row.get(0)) };
    Ok(vec![
        ("gauge.accounts", count("SELECT COUNT(*) FROM accounts WHERE deleted_at IS NULL")?),
        ("gauge.domains", count("SELECT COUNT(*) FROM domains")?),
        ("gauge.storageBytes", count("SELECT COALESCE(SUM(used_bytes), 0) FROM accounts")?),
        ("gauge.queueRecipients", count("SELECT COUNT(*) FROM queue_recipients WHERE status = 'pending'")?),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;

    const DAY: i64 = 86_400;
    /// 2026-09-27 12:00 UTC.
    const NOON: i64 = 1_790_510_400;

    #[tokio::test]
    async fn counters_are_added_to_their_day_and_read_back() {
        let (store, _dir) = store().await;
        store.stats().count(Stat::Received);
        store.stats().add(Stat::Received, 2);
        store.stats().count(Stat::login_failed("imap"));
        store.flush_stats_at(NOON - DAY).await.unwrap();
        // Nothing new: a second flush adds nothing twice.
        store.flush_stats_at(NOON - DAY).await.unwrap();

        store.stats().count(Stat::RefusedSpam);
        store.flush_stats_at(NOON).await.unwrap();
        // Counted, not written down yet: still part of today.
        store.stats().count(Stat::RefusedSpam);

        let days = store.stats_days_at(NOON, 30).await.unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].day, "2026-09-26");
        assert_eq!(days[0].values["mail.received"], 3);
        assert_eq!(days[0].values["loginFailed.imap"], 1);
        assert_eq!(days[0].values["gauge.accounts"], 0);
        assert_eq!(days[1].day, "2026-09-27");
        assert_eq!(days[1].values["refused.spam"], 2);
        assert!(!days[1].values.contains_key("mail.received"));

        // Since the start, for Prometheus, nothing is ever taken away.
        let totals: std::collections::HashMap<_, _> = store.stats().since_start().into_iter().collect();
        assert_eq!(totals[&Stat::Received], 3);
        assert_eq!(totals[&Stat::RefusedSpam], 2);

        // Only the asked-for days, and old ones are forgotten when a flush comes by.
        assert_eq!(store.stats_days_at(NOON, 1).await.unwrap().len(), 1);
        store.flush_stats_at(NOON + (STATS_RETENTION_DAYS + 1) * DAY).await.unwrap();
        let later = store.stats_days_at(NOON + (STATS_RETENTION_DAYS + 1) * DAY, STATS_RETENTION_DAYS).await.unwrap();
        assert_eq!(later.len(), 1, "only the day of that flush is left");
        assert_eq!(later[0].values["refused.spam"], 1);
    }

    #[test]
    fn protocols_have_their_own_counters() {
        assert_eq!(Stat::login_failed("imap"), Stat::LoginFailedImap);
        assert_eq!(Stat::login_failed("smtp"), Stat::LoginFailedSmtp);
        assert_eq!(Stat::login_failed("pop3"), Stat::LoginFailedOther);
        let keys: std::collections::HashSet<_> = Stat::ALL.iter().map(|stat| stat.key()).collect();
        assert_eq!(keys.len(), Stat::ALL.len());
    }
}
