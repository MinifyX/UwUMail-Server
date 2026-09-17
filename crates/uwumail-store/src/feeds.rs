//! What the built-in lists brought when the server last fetched them. The SMTP side knows the lists and
//! their meaning; this only keeps their values and how fetching went.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{Result, Store, now};

const FEEDS: &str = "feeds";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedState {
    pub key: String,
    pub fetched_at: Option<i64>,
    pub changed_at: Option<i64>,
    #[serde(skip)]
    pub validator: Option<String>,
    pub error: Option<String>,
    pub entries: i64,
}

impl Store {
    /// How fetching went for every list fetched at least once.
    pub async fn feed_states(&self) -> Result<Vec<FeedState>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT s.key, s.fetched_at, s.changed_at, s.validator, s.error,
                        (SELECT COUNT(*) FROM feed_entries e WHERE e.key = s.key)
                 FROM feed_state s ORDER BY s.key",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok(FeedState {
                    key: row.get(0)?,
                    fetched_at: row.get(1)?,
                    changed_at: row.get(2)?,
                    validator: row.get(3)?,
                    error: row.get(4)?,
                    entries: row.get(5)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// Replaces a list's values with a fresh fetch.
    pub async fn replace_feed(&self, key: &str, values: Vec<String>, validator: Option<String>) -> Result<()> {
        let key = key.to_owned();
        self.write(move |tx| {
            tx.execute("DELETE FROM feed_entries WHERE key = ?1", [&key])?;
            let mut insert = tx.prepare("INSERT OR IGNORE INTO feed_entries (key, value) VALUES (?1, ?2)")?;
            for value in &values {
                insert.execute(params![key, value])?;
            }
            drop(insert);
            let at = now();
            tx.execute(
                "INSERT INTO feed_state (key, fetched_at, changed_at, validator, error) VALUES (?1, ?2, ?2, ?3, NULL)
                 ON CONFLICT (key) DO UPDATE SET fetched_at = ?2, changed_at = ?2, validator = ?3, error = NULL",
                params![key, at, validator],
            )?;
            tx.execute(
                "INSERT INTO list_versions (name, version) VALUES (?1, 1)
                 ON CONFLICT (name) DO UPDATE SET version = version + 1",
                [FEEDS],
            )?;
            Ok(())
        })
        .await
    }

    /// Notes a fetch that brought nothing new (`error` is `None`) or failed; the values stay.
    pub async fn feed_fetched(&self, key: &str, error: Option<String>) -> Result<()> {
        let key = key.to_owned();
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO feed_state (key, fetched_at, error) VALUES (?1, ?2, ?3)
                 ON CONFLICT (key) DO UPDATE SET fetched_at = ?2, error = ?3",
                params![key, now(), error],
            )?;
            Ok(())
        })
        .await
    }

    /// The ETag or Last-Modified of a list's last good fetch.
    pub async fn feed_validator(&self, key: &str) -> Result<Option<String>> {
        let key = key.to_owned();
        self.read(move |conn| {
            Ok(conn
                .query_row("SELECT validator FROM feed_state WHERE key = ?1", [key], |row| row.get(0))
                .optional()?
                .flatten())
        })
        .await
    }

    /// Changes whenever a list brought new values.
    pub async fn feeds_version(&self) -> Result<i64> {
        self.read(|conn| {
            Ok(conn
                .query_row("SELECT version FROM list_versions WHERE name = ?1", [FEEDS], |row| row.get(0))
                .optional()?
                .unwrap_or(0))
        })
        .await
    }

    /// Every list's values.
    pub async fn feed_values(&self) -> Result<HashMap<String, Vec<String>>> {
        self.read(|conn| {
            let mut stmt = conn.prepare("SELECT key, value FROM feed_entries")?;
            let mut rows = stmt.query([])?;
            let mut values: HashMap<String, Vec<String>> = HashMap::new();
            while let Some(row) = rows.next()? {
                values.entry(row.get(0)?).or_default().push(row.get(1)?);
            }
            Ok(values)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_fresh_fetch_replaces_values_and_a_failed_one_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        assert_eq!(store.feeds_version().await.unwrap(), 0);
        store
            .replace_feed("disposable", vec!["trash.example".into(), "trash.example".into()], Some("etag:\"1\"".into()))
            .await
            .unwrap();
        assert_eq!(store.feeds_version().await.unwrap(), 1);
        store.feed_fetched("disposable", Some("the server answered 503".into())).await.unwrap();
        let states = store.feed_states().await.unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!((states[0].entries, states[0].error.as_deref()), (1, Some("the server answered 503")));
        assert_eq!(store.feed_validator("disposable").await.unwrap().as_deref(), Some("etag:\"1\""));
        assert_eq!(store.feed_values().await.unwrap()["disposable"], ["trash.example"]);

        store.replace_feed("disposable", vec!["other.example".into()], None).await.unwrap();
        assert_eq!(store.feed_values().await.unwrap()["disposable"], ["other.example"]);
        assert_eq!(store.feed_states().await.unwrap()[0].error, None);
        assert_eq!(store.feed_validator("unknown").await.unwrap(), None);
    }
}
