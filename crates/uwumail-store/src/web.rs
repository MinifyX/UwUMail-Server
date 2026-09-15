//! Web portal sessions, personal portal settings and numbers for the admin overview.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::directory::{ACCOUNT_COLUMN_COUNT, ACCOUNT_COLUMNS, account_from_row};
use crate::{Account, Result, Store, StoreError, now, random_bytes};

/// A session is only written back when it was last seen longer ago than this.
const TOUCH_INTERVAL_SECS: i64 = 300;

/// A freshly created session. `token` goes into the cookie and is never stored.
#[derive(Debug, Clone)]
pub struct NewWebSession {
    pub token: String,
    pub csrf_token: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct WebSession {
    pub account: Account,
    pub csrf_token: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// Counts for the admin overview.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCounts {
    pub domains: i64,
    pub accounts: i64,
    pub admins: i64,
    pub disabled_accounts: i64,
    /// People in the trash.
    pub deleted_accounts: i64,
    pub aliases: i64,
    pub used_bytes: i64,
    pub queued_messages: i64,
    pub pending_recipients: i64,
    /// Pending recipients whose last attempt failed and will be retried.
    pub deferred_recipients: i64,
}

pub(crate) fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

impl Store {
    /// Starts a web session that expires after `lifetime_secs` without use.
    pub async fn create_web_session(
        &self,
        account_id: i64,
        lifetime_secs: i64,
        ip: &str,
        user_agent: &str,
    ) -> Result<NewWebSession> {
        let token = hex::encode(random_bytes::<32>());
        let csrf_token = hex::encode(random_bytes::<24>());
        let now = now();
        let session = NewWebSession { token, csrf_token, expires_at: now + lifetime_secs };
        let (hash, csrf, expires_at) = (token_hash(&session.token), session.csrf_token.clone(), session.expires_at);
        let (ip, user_agent) = (ip.to_owned(), user_agent.chars().take(300).collect::<String>());
        self.write(move |tx| {
            // Expired sessions of everyone are cleaned up on the way.
            tx.execute("DELETE FROM web_sessions WHERE expires_at <= ?1", [now])?;
            tx.execute(
                "INSERT INTO web_sessions (token_hash, account_id, csrf_token, created_at, last_seen_at, expires_at, ip, user_agent)
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7)",
                params![hash, account_id, csrf, now, expires_at, ip, user_agent],
            )?;
            Ok(())
        })
        .await?;
        Ok(session)
    }

    /// Looks up a session by its cookie token and extends it. Expired sessions and
    /// sessions of accounts that may not log in count as missing.
    pub async fn web_session(&self, token: &str, lifetime_secs: i64) -> Result<Option<WebSession>> {
        let hash = token_hash(token);
        let lookup_hash = hash.clone();
        let found = self
            .read(move |conn| {
                let columns = ACCOUNT_COLUMNS.split(", ").map(|c| format!("a.{c}")).collect::<Vec<_>>().join(", ");
                Ok(conn
                    .query_row(
                        &format!(
                            "SELECT {columns}, s.csrf_token, s.created_at, s.last_seen_at, s.expires_at
                             FROM web_sessions s JOIN accounts a ON a.id = s.account_id
                             WHERE s.token_hash = ?1"
                        ),
                        [lookup_hash],
                        |row| {
                            Ok((
                                account_from_row(row)?,
                                row.get::<_, String>(ACCOUNT_COLUMN_COUNT)?,
                                row.get::<_, i64>(ACCOUNT_COLUMN_COUNT + 1)?,
                                row.get::<_, i64>(ACCOUNT_COLUMN_COUNT + 2)?,
                                row.get::<_, i64>(ACCOUNT_COLUMN_COUNT + 3)?,
                            ))
                        },
                    )
                    .optional()?)
            })
            .await?;
        let Some((account, csrf_token, created_at, last_seen_at, expires_at)) = found else {
            return Ok(None);
        };
        let now = now();
        if expires_at <= now || !account.can_log_in() {
            return Ok(None);
        }
        let mut session = WebSession { account, csrf_token, created_at, expires_at };
        if now - last_seen_at >= TOUCH_INTERVAL_SECS {
            session.expires_at = now + lifetime_secs;
            let expires_at = session.expires_at;
            self.write(move |tx| {
                tx.execute(
                    "UPDATE web_sessions SET last_seen_at = ?1, expires_at = ?2 WHERE token_hash = ?3",
                    params![now, expires_at, hash],
                )?;
                Ok(())
            })
            .await?;
        }
        Ok(Some(session))
    }

    pub async fn delete_web_session(&self, token: &str) -> Result<()> {
        let hash = token_hash(token);
        self.write(move |tx| {
            tx.execute("DELETE FROM web_sessions WHERE token_hash = ?1", [hash])?;
            Ok(())
        })
        .await
    }

    /// Ends every web session of an account, e.g. after a password change.
    pub async fn delete_web_sessions(&self, account_id: i64) -> Result<usize> {
        self.write(move |tx| Ok(tx.execute("DELETE FROM web_sessions WHERE account_id = ?1", [account_id])?)).await
    }

    pub async fn preferences(&self, account_id: i64) -> Result<Map<String, Value>> {
        let raw = self
            .read(move |conn| {
                conn.query_row("SELECT preferences FROM accounts WHERE id = ?1", [account_id], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("account {account_id}")))
            })
            .await?;
        Ok(serde_json::from_str(&raw).unwrap_or_default())
    }

    /// Merges `changes` into the stored preferences; `null` removes a key.
    pub async fn update_preferences(&self, account_id: i64, changes: Map<String, Value>) -> Result<Map<String, Value>> {
        self.write(move |tx| {
            let raw: String = tx
                .query_row("SELECT preferences FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("account {account_id}")))?;
            let mut preferences: Map<String, Value> = serde_json::from_str(&raw).unwrap_or_default();
            for (key, value) in changes {
                if value.is_null() {
                    preferences.remove(&key);
                } else {
                    preferences.insert(key, value);
                }
            }
            let encoded = Value::Object(preferences.clone()).to_string();
            if encoded.len() > 16 * 1024 {
                return Err(StoreError::Invalid("preferences are too large".into()));
            }
            tx.execute("UPDATE accounts SET preferences = ?1 WHERE id = ?2", params![encoded, account_id])?;
            Ok(preferences)
        })
        .await
    }

    pub async fn server_counts(&self) -> Result<ServerCounts> {
        self.read(|conn| {
            let count = |sql: &str| -> rusqlite::Result<i64> { conn.query_row(sql, [], |row| row.get(0)) };
            Ok(ServerCounts {
                domains: count("SELECT COUNT(*) FROM domains")?,
                accounts: count("SELECT COUNT(*) FROM accounts WHERE deleted_at IS NULL")?,
                admins: count(
                    "SELECT COUNT(*) FROM accounts WHERE role = 'admin' AND disabled = 0 AND deleted_at IS NULL",
                )?,
                disabled_accounts: count("SELECT COUNT(*) FROM accounts WHERE disabled = 1 AND deleted_at IS NULL")?,
                deleted_accounts: count("SELECT COUNT(*) FROM accounts WHERE deleted_at IS NOT NULL")?,
                aliases: count(
                    "SELECT COUNT(*) FROM addresses ad JOIN accounts a ON a.id = ad.account_id
                     JOIN domains d ON d.id = ad.domain_id
                     WHERE ad.local_part || '@' || d.name <> a.login",
                )?,
                used_bytes: count("SELECT COALESCE(SUM(used_bytes), 0) FROM accounts")?,
                queued_messages: count(
                    "SELECT COUNT(DISTINCT message_id) FROM queue_recipients WHERE status = 'pending'",
                )?,
                pending_recipients: count("SELECT COUNT(*) FROM queue_recipients WHERE status = 'pending'")?,
                deferred_recipients: count(
                    "SELECT COUNT(*) FROM queue_recipients WHERE status = 'pending' AND attempts > 0",
                )?,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::test_support::store;
    use crate::{NewAccount, Role};

    async fn account(store: &crate::Store) -> crate::Account {
        store.create_domain("example.de").await.unwrap();
        store
            .create_account(NewAccount {
                address: "nyu@example.de".into(),
                display_name: "Nyu".into(),
                password: Some("katzenpfote-123".into()),
                role: Role::Admin,
                quota_bytes: 0,
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn sessions_live_until_they_expire_or_are_removed() {
        let (store, _dir) = store().await;
        let nyu = account(&store).await;

        let session = store.create_web_session(nyu.id, 3600, "192.0.2.1", "Firefox").await.unwrap();
        let found = store.web_session(&session.token, 3600).await.unwrap().expect("session exists");
        assert_eq!(found.account.login, "nyu@example.de");
        assert_eq!(found.csrf_token, session.csrf_token);
        assert!(store.web_session("not-a-token", 3600).await.unwrap().is_none());

        store.set_account_disabled("nyu@example.de", true).await.unwrap();
        assert!(store.web_session(&session.token, 3600).await.unwrap().is_none(), "disabled accounts are logged out");
        store.set_account_disabled("nyu@example.de", false).await.unwrap();

        store.delete_web_session(&session.token).await.unwrap();
        assert!(store.web_session(&session.token, 3600).await.unwrap().is_none());

        let expired = store.create_web_session(nyu.id, -1, "", "").await.unwrap();
        assert!(store.web_session(&expired.token, 3600).await.unwrap().is_none());

        let other = store.create_web_session(nyu.id, 3600, "", "").await.unwrap();
        assert_eq!(store.delete_web_sessions(nyu.id).await.unwrap(), 1);
        assert!(store.web_session(&other.token, 3600).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn preferences_merge_and_counts_add_up() {
        let (store, _dir) = store().await;
        let nyu = account(&store).await;
        store.add_alias("hallo@example.de", "nyu@example.de").await.unwrap();

        let changes = json!({ "language": "de", "mode": "pro" }).as_object().unwrap().clone();
        store.update_preferences(nyu.id, changes).await.unwrap();
        let changes = json!({ "mode": null, "tone": "neutral" }).as_object().unwrap().clone();
        let merged = store.update_preferences(nyu.id, changes).await.unwrap();
        assert_eq!(serde_json::Value::Object(merged), json!({ "language": "de", "tone": "neutral" }));
        assert_eq!(store.preferences(nyu.id).await.unwrap()["tone"], "neutral");

        let counts = store.server_counts().await.unwrap();
        assert_eq!((counts.domains, counts.accounts, counts.admins, counts.aliases), (1, 1, 1, 1));
        assert_eq!(counts.queued_messages, 0);
    }
}
