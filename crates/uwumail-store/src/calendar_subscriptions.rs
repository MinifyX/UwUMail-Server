//! Subscribed calendars (migration 0039, docs/calendar-import.md): a feed in iCalendar format that
//! fills one calendar of the account, fetched again every so often. The fetching is in the DAV
//! crate; what is kept here is the address, sealed, and how the last runs went.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::dav::{
    ChangeLog, DavCollection, DavKind, NewDavCollection, delete_collection, insert_collection, own_collection,
};
use crate::fetch::{seal, unseal};
use crate::{Result, Store, StoreError, now};

/// Feeds one person may subscribe to. Each is a calendar of their 100.
pub const MAX_CALENDAR_SUBSCRIPTIONS: usize = 20;
/// More often than every 15 minutes is not polite to a publisher; less than weekly is not useful.
pub const MIN_SUBSCRIPTION_INTERVAL_SECS: i64 = 900;
pub const MAX_SUBSCRIPTION_INTERVAL_SECS: i64 = 7 * 86_400;
pub const DEFAULT_SUBSCRIPTION_INTERVAL_SECS: i64 = 3600;
/// A failing feed is asked less and less often, but at least once a day.
const MAX_BACKOFF_SECS: i64 = 86_400;
/// "Refresh now" once a minute at most.
pub const SUBSCRIPTION_REFRESH_PAUSE_SECS: i64 = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarSubscription {
    pub id: i64,
    pub account_id: i64,
    pub collection_id: i64,
    /// The feed's host, for showing; the address itself stays sealed.
    pub source: String,
    pub interval_secs: i64,
    pub keep_alarms: bool,
    pub enabled: bool,
    #[serde(skip)]
    pub validator: Option<String>,
    pub next_run_at: i64,
    pub last_run_at: Option<i64>,
    pub last_ok_at: Option<i64>,
    pub last_error: String,
    pub failures: i64,
    pub entries: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewCalendarSubscription {
    pub collection: NewDavCollection,
    pub url: String,
    pub interval_secs: i64,
    pub keep_alarms: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CalendarSubscriptionUpdate {
    pub url: Option<String>,
    pub interval_secs: Option<i64>,
    pub keep_alarms: Option<bool>,
    pub enabled: Option<bool>,
}

/// How a run went: the feed's validator and the entries now in the calendar, or what failed.
pub type SubscriptionRun = std::result::Result<(Option<String>, usize), String>;

const COLUMNS: &str = "id, account_id, collection_id, url_shown, interval_secs, keep_alarms, enabled, validator, \
     next_run_at, last_run_at, last_ok_at, last_error, failures, entries, created_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<CalendarSubscription> {
    Ok(CalendarSubscription {
        id: row.get(0)?,
        account_id: row.get(1)?,
        collection_id: row.get(2)?,
        source: row.get(3)?,
        interval_secs: row.get(4)?,
        keep_alarms: row.get(5)?,
        enabled: row.get(6)?,
        validator: row.get(7)?,
        next_run_at: row.get(8)?,
        last_run_at: row.get(9)?,
        last_ok_at: row.get(10)?,
        last_error: row.get(11)?,
        failures: row.get(12)?,
        entries: row.get(13)?,
        created_at: row.get(14)?,
    })
}

/// What a feed's address shows of itself: the host, and that there is more.
pub fn shown_url(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host).to_ascii_lowercase();
    if rest.len() > authority.len() { format!("{host}/…") } else { host }
}

fn digest(url: &str) -> String {
    hex::encode(Sha256::digest(url.trim().as_bytes()))
}

fn check_interval(secs: i64) -> Result<()> {
    if !(MIN_SUBSCRIPTION_INTERVAL_SECS..=MAX_SUBSCRIPTION_INTERVAL_SECS).contains(&secs) {
        return Err(StoreError::Invalid(format!(
            "a feed is fetched every {MIN_SUBSCRIPTION_INTERVAL_SECS} to {MAX_SUBSCRIPTION_INTERVAL_SECS} seconds"
        )));
    }
    Ok(())
}

fn own_subscription(conn: &Connection, account_id: i64, id: i64) -> Result<CalendarSubscription> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM calendar_subscriptions WHERE id = ?1 AND account_id = ?2"),
        params![id, account_id],
        from_row,
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("subscription {id}")))
}

impl Store {
    /// Subscribes to a feed: a new calendar and the subscription that fills it, in one step. The
    /// account's own calendar is made first when it has none yet, so the subscribed one never
    /// becomes the default, where invitations would go.
    pub async fn create_calendar_subscription(
        &self,
        account_id: i64,
        new: NewCalendarSubscription,
        default_calendar: NewDavCollection,
    ) -> Result<(DavCollection, CalendarSubscription)> {
        check_interval(new.interval_secs)?;
        let (result, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let count: i64 = tx.query_row(
                    "SELECT count(*) FROM calendar_subscriptions WHERE account_id = ?1",
                    [account_id],
                    |row| row.get(0),
                )?;
                if count as usize >= MAX_CALENDAR_SUBSCRIPTIONS {
                    return Err(StoreError::Rule {
                        code: "subscriptionLimit",
                        message: format!("at most {MAX_CALENDAR_SUBSCRIPTIONS} subscribed calendars"),
                    });
                }
                let digest = digest(&new.url);
                let taken: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM calendar_subscriptions WHERE account_id = ?1 AND url_digest = ?2)",
                    params![account_id, digest],
                    |row| row.get(0),
                )?;
                if taken {
                    return Err(StoreError::Rule {
                        code: "subscriptionExists",
                        message: "this calendar is subscribed already".into(),
                    });
                }
                let has_own: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM dav_collections WHERE account_id = ?1 AND kind = 'calendar')",
                    [account_id],
                    |row| row.get(0),
                )?;
                if !has_own {
                    insert_collection(tx, &mut log, account_id, DavKind::Calendar, &default_calendar)?;
                }
                let collection_id = insert_collection(tx, &mut log, account_id, DavKind::Calendar, &new.collection)?;
                let sealed = seal(tx, new.url.trim())?;
                let at = now();
                tx.execute(
                    "INSERT INTO calendar_subscriptions (account_id, collection_id, url, url_digest, url_shown,
                         interval_secs, keep_alarms, next_run_at, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        account_id,
                        collection_id,
                        sealed,
                        digest,
                        shown_url(&new.url),
                        new.interval_secs,
                        new.keep_alarms,
                        at + new.interval_secs,
                        at
                    ],
                )?;
                let id = tx.last_insert_rowid();
                Ok((
                    (own_collection(tx, account_id, collection_id)?, own_subscription(tx, account_id, id)?),
                    log.modseq(),
                ))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(result)
    }

    pub async fn calendar_subscriptions(&self, account_id: i64) -> Result<Vec<CalendarSubscription>> {
        self.read(move |conn| {
            let mut stmt = conn
                .prepare(&format!("SELECT {COLUMNS} FROM calendar_subscriptions WHERE account_id = ?1 ORDER BY id"))?;
            let rows = stmt.query_map([account_id], from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    pub async fn calendar_subscription(&self, account_id: i64, id: i64) -> Result<CalendarSubscription> {
        self.read(move |conn| own_subscription(conn, account_id, id)).await
    }

    /// The feed's address, to fetch it. It may be a credential, so it only ever goes to the feed.
    pub async fn calendar_subscription_url(&self, account_id: i64, id: i64) -> Result<String> {
        self.read(move |conn| {
            let sealed: Vec<u8> = conn
                .query_row(
                    "SELECT url FROM calendar_subscriptions WHERE id = ?1 AND account_id = ?2",
                    params![id, account_id],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("subscription {id}")))?;
            unseal(conn, &sealed)
        })
        .await
    }

    pub async fn update_calendar_subscription(
        &self,
        account_id: i64,
        id: i64,
        update: CalendarSubscriptionUpdate,
    ) -> Result<CalendarSubscription> {
        if let Some(secs) = update.interval_secs {
            check_interval(secs)?;
        }
        self.write(move |tx| {
            let current = own_subscription(tx, account_id, id)?;
            if let Some(url) = &update.url {
                let digest = digest(url);
                let taken: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM calendar_subscriptions
                         WHERE account_id = ?1 AND url_digest = ?2 AND id <> ?3)",
                    params![account_id, digest, id],
                    |row| row.get(0),
                )?;
                if taken {
                    return Err(StoreError::Rule {
                        code: "subscriptionExists",
                        message: "this calendar is subscribed already".into(),
                    });
                }
                tx.execute(
                    "UPDATE calendar_subscriptions SET url = ?1, url_digest = ?2, url_shown = ?3, validator = NULL,
                         next_run_at = 0, failures = 0 WHERE id = ?4",
                    params![seal(tx, url.trim())?, digest, shown_url(url), id],
                )?;
            }
            if let Some(secs) = update.interval_secs {
                tx.execute(
                    "UPDATE calendar_subscriptions SET interval_secs = ?1,
                         next_run_at = min(next_run_at, coalesce(last_run_at, 0) + ?1) WHERE id = ?2",
                    params![secs, id],
                )?;
            }
            if let Some(keep) = update.keep_alarms
                && keep != current.keep_alarms
            {
                // The entries have to be written again, with or without their reminders.
                tx.execute(
                    "UPDATE calendar_subscriptions SET keep_alarms = ?1, validator = NULL, next_run_at = 0 WHERE id = ?2",
                    params![keep, id],
                )?;
            }
            if let Some(enabled) = update.enabled {
                tx.execute(
                    "UPDATE calendar_subscriptions SET enabled = ?1, failures = CASE WHEN ?1 THEN 0 ELSE failures END,
                         next_run_at = CASE WHEN ?1 AND NOT enabled THEN 0 ELSE next_run_at END WHERE id = ?2",
                    params![enabled, id],
                )?;
            }
            own_subscription(tx, account_id, id)
        })
        .await
    }

    /// Ends a subscription. With `keep_entries` the calendar stays as an ordinary one, with the
    /// entries it had; otherwise it goes too.
    pub async fn delete_calendar_subscription(&self, account_id: i64, id: i64, keep_entries: bool) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let subscription = own_subscription(tx, account_id, id)?;
                tx.execute("DELETE FROM calendar_subscriptions WHERE id = ?1", [id])?;
                let collection = own_collection(tx, account_id, subscription.collection_id)?;
                if keep_entries {
                    // Its rights changed: clients should ask again.
                    crate::dav::next_change(tx, collection.id)?;
                    log.collection(tx, &collection, "updated")?;
                } else {
                    delete_collection(tx, &mut log, &collection)?;
                }
                Ok(log.modseq())
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(())
    }

    /// Subscriptions whose turn it is, of people still here with CalDAV on, the longest waiting
    /// first.
    pub async fn calendar_subscriptions_due(&self, limit: usize) -> Result<Vec<CalendarSubscription>> {
        let at = now();
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM calendar_subscriptions
                 WHERE enabled = 1 AND next_run_at <= ?1
                   AND account_id IN (SELECT id FROM accounts WHERE deleted_at IS NULL AND caldav_enabled = 1)
                 ORDER BY next_run_at LIMIT ?2"
            ))?;
            let rows = stmt.query_map(params![at, limit as i64], from_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Notes how a run went and when the next one is: after the interval when it worked, and
    /// after longer and longer waits while it keeps failing.
    pub async fn note_subscription_run(&self, id: i64, run: SubscriptionRun) -> Result<()> {
        let at = now();
        self.write(move |tx| {
            let Some((interval, failures)): Option<(i64, i64)> = tx
                .query_row("SELECT interval_secs, failures FROM calendar_subscriptions WHERE id = ?1", [id], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()?
            else {
                return Ok(());
            };
            match run {
                Ok((validator, entries)) => {
                    tx.execute(
                        "UPDATE calendar_subscriptions SET last_run_at = ?1, last_ok_at = ?1, last_error = '',
                             failures = 0, validator = ?2, entries = ?3, next_run_at = ?4 WHERE id = ?5",
                        params![at, validator, entries as i64, at + interval, id],
                    )?;
                }
                Err(error) => {
                    let wait = interval.saturating_mul(1 << failures.clamp(0, 10)).clamp(interval, MAX_BACKOFF_SECS);
                    let error: String = error.chars().take(300).collect();
                    tx.execute(
                        "UPDATE calendar_subscriptions SET last_run_at = ?1, last_error = ?2, failures = failures + 1,
                             next_run_at = ?3 WHERE id = ?4",
                        params![at, error, at + wait.max(interval), id],
                    )?;
                }
            }
            Ok(())
        })
        .await
    }

    /// Asks for a run as soon as the worker comes by. `false` when one ran within the last minute.
    pub async fn subscription_due_now(&self, account_id: i64, id: i64) -> Result<bool> {
        let at = now();
        self.write(move |tx| {
            let subscription = own_subscription(tx, account_id, id)?;
            if subscription.last_run_at.is_some_and(|last| at - last < SUBSCRIPTION_REFRESH_PAUSE_SECS) {
                return Ok(false);
            }
            tx.execute("UPDATE calendar_subscriptions SET next_run_at = 0 WHERE id = ?1", [id])?;
            Ok(true)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, Role};

    async fn account(store: &Store, address: &str) -> i64 {
        store.create_domain("example.org").await.ok();
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id
    }

    fn new(slug: &str, url: &str) -> NewCalendarSubscription {
        NewCalendarSubscription {
            collection: NewDavCollection { slug: slug.into(), display_name: slug.into(), ..Default::default() },
            url: url.into(),
            interval_secs: 3600,
            keep_alarms: false,
        }
    }

    #[test]
    fn addresses_show_only_their_host() {
        assert_eq!(
            shown_url("https://calendar.google.com/calendar/ical/x%40gmail.com/private-abc/basic.ics"),
            "calendar.google.com/…"
        );
        assert_eq!(shown_url("https://user:secret@Feeds.Example.NET"), "feeds.example.net");
    }

    #[tokio::test]
    async fn subscriptions_are_sealed_limited_and_scheduled() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.org").await;
        let leni = account(&store, "leni@example.org").await;
        let url = "https://calendar.example.net/private-0123/basic.ics";
        let (calendar, sub) = store
            .create_calendar_subscription(mini, new("ferien", url), NewDavCollection::default_calendar("Kalender"))
            .await
            .unwrap();
        assert!(calendar.subscribed && !calendar.is_default);
        let calendars = store
            .dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("Kalender"))
            .await
            .unwrap();
        assert_eq!(calendars.len(), 2, "the own calendar was made first");
        assert!(calendars.iter().any(|c| c.is_default && !c.subscribed));
        assert_eq!(sub.source, "calendar.example.net/…");
        assert_eq!(store.calendar_subscription_url(mini, sub.id).await.unwrap(), url);
        assert!(store.calendar_subscription_url(leni, sub.id).await.is_err(), "only its owner reads it");
        let raw: Vec<u8> = store
            .read(move |conn| Ok(conn.query_row("SELECT url FROM calendar_subscriptions", [], |row| row.get(0))?))
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("private-0123"), "the address is sealed");
        assert!(matches!(
            store.create_calendar_subscription(mini, new("again", url), NewDavCollection::default_calendar("K")).await,
            Err(StoreError::Rule { code: "subscriptionExists", .. })
        ));
        let mut bad = new("fast", "https://calendar.example.net/other.ics");
        bad.interval_secs = 60;
        assert!(store.create_calendar_subscription(mini, bad, NewDavCollection::default_calendar("K")).await.is_err());

        assert!(store.calendar_subscriptions_due(10).await.unwrap().is_empty(), "the first run is the portal's");
        assert!(store.subscription_due_now(mini, sub.id).await.unwrap());
        assert_eq!(store.calendar_subscriptions_due(10).await.unwrap().len(), 1);
        store.note_subscription_run(sub.id, Err("feedUnreachable".into())).await.unwrap();
        let failed = store.calendar_subscription(mini, sub.id).await.unwrap();
        assert_eq!((failed.failures, failed.last_error.as_str()), (1, "feedUnreachable"));
        assert!(failed.next_run_at >= failed.last_run_at.unwrap() + 3600);
        assert!(!store.subscription_due_now(mini, sub.id).await.unwrap(), "not twice a minute");
        store.note_subscription_run(sub.id, Ok((Some("etag:\"1\"".into()), 12))).await.unwrap();
        let ok = store.calendar_subscription(mini, sub.id).await.unwrap();
        assert_eq!((ok.failures, ok.entries, ok.validator.as_deref()), (0, 12, Some("etag:\"1\"")));

        let changed = store
            .update_calendar_subscription(
                mini,
                sub.id,
                CalendarSubscriptionUpdate {
                    url: Some("https://other.example.net/cal.ics".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!((changed.source.as_str(), changed.validator, changed.next_run_at), ("other.example.net/…", None, 0));
        store.delete_calendar_subscription(mini, sub.id, false).await.unwrap();
        assert!(store.dav_collection(mini, DavKind::Calendar, "ferien").await.unwrap().is_none());
    }
}
