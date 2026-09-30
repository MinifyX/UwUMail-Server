//! Web Push subscriptions (migration 0046, docs/jmap-push.md): the addresses at push services the
//! server tells about changes, for JMAP `PushSubscription`. Sending is in the JMAP crate; what is
//! kept here is the address and keys, sealed, which login made the subscription, and how its
//! pushes went. The server's own VAPID key (RFC 8292) is kept here too.

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use rusqlite::{Connection, OptionalExtension, Row, params};
use sha2::{Digest, Sha256};

use crate::acl::GRANTS;
use crate::db::{get_setting, set_setting};
use crate::fetch::{seal, unseal};
use crate::{Result, Store, StoreError, now, random_bytes};

/// Subscriptions one account may have, verified or not, across all its devices and logins.
pub const MAX_PUSH_SUBSCRIPTIONS: usize = 50;
/// The furthest a subscription may run without being renewed (RFC 8620: at least 48 hours, and it
/// SHOULD be at least 7 days). It is also what a subscription gets when it asks for nothing.
pub const PUSH_SUBSCRIPTION_MAX_SECS: i64 = 7 * 86_400;
/// Wrong verification codes before a subscription is dropped.
pub const PUSH_MAX_VERIFY_ATTEMPTS: i64 = 5;
/// Failed pushes in a row after which a subscription is dropped.
pub const PUSH_MAX_FAILURES: i64 = 20;
/// A subscription that never got its code back is dropped after a day; it only takes a place.
const UNVERIFIED_LIFETIME_SECS: i64 = 86_400;
/// The server's VAPID key pair (PKCS#8), sealed.
const VAPID_KEY: &str = "push.vapid_key";

/// A subscription as its owner sees it. The address and the keys never leave the server again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSubscription {
    pub id: i64,
    pub device_client_id: String,
    /// The code the client sent back; `None` until it did.
    pub verification_code: Option<String>,
    pub expires: i64,
    /// `None` means every type.
    pub types: Option<Vec<String>>,
    /// The push service's host, for the log.
    pub url_shown: String,
    pub created_at: i64,
    pub last_ok_at: Option<i64>,
    pub failures: i64,
}

/// The encryption keys of a subscription (RFC 8291), base64url as the client sent them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushKeys {
    pub p256dh: String,
    pub auth: String,
}

#[derive(Debug, Clone)]
pub struct NewPushSubscription {
    pub account_id: i64,
    /// The login that makes it: see [`push_credential_for_session`] and the migration.
    pub credential: String,
    pub device_client_id: String,
    pub url: String,
    pub keys: Option<PushKeys>,
    pub expires: i64,
    pub types: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct PushSubscriptionUpdate {
    pub verification_code: Option<String>,
    pub expires: Option<i64>,
    /// `Some(None)` sets every type.
    pub types: Option<Option<Vec<String>>>,
}

/// Where and how to push: everything the sender needs, unsealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTarget {
    pub id: i64,
    pub account_id: i64,
    pub url: String,
    pub url_shown: String,
    pub keys: Option<PushKeys>,
    pub types: Option<Vec<String>>,
    pub verification_code: String,
    /// Whether the login that made it may reach calendars and address books (the `dav` scope): an
    /// app password or OAuth app without it hears nothing of them, as over the EventSource.
    pub may_use_dav: bool,
}

/// How a verification code that came back was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verify {
    Unchanged,
    Right,
    Wrong,
    TooMany,
}

/// The credential of a webmail session for [`NewPushSubscription::credential`]: the same hash of
/// the cookie's token the session table keeps, so the subscription ends with the session.
pub fn push_credential_for_session(token: &str) -> String {
    format!("session:{}", hex::encode(crate::web::token_hash(token)))
}

/// The credential of an app password.
pub fn push_credential_for_app_password(id: i64) -> String {
    format!("app:{id}")
}

/// The credential of an app signed in with OAuth: its grant, which ends when the app is signed out.
pub fn push_credential_for_oauth_grant(id: i64) -> String {
    format!("oauth:{id}")
}

/// The credential of the account password.
pub const PUSH_CREDENTIAL_PASSWORD: &str = "password";

/// Whether the login behind a subscription `p` (joined with its account `a`) still holds: the
/// account may log in, and the session, app password or password is the one it was made with.
pub(crate) const STILL_VALID: &str = "a.disabled = 0 AND a.deleted_at IS NULL AND CASE
    WHEN p.credential = 'password' THEN a.password_changed_at <= p.created_at
    WHEN p.credential LIKE 'app:%' THEN EXISTS (SELECT 1 FROM app_passwords ap
        WHERE ap.id = CAST(substr(p.credential, 5) AS INTEGER) AND ap.account_id = p.account_id
          AND (ap.expires_at IS NULL OR ap.expires_at > ?1))
    WHEN p.credential LIKE 'oauth:%' THEN EXISTS (SELECT 1 FROM oauth_grants g
        WHERE g.id = CAST(substr(p.credential, 7) AS INTEGER) AND g.account_id = p.account_id)
    WHEN p.credential LIKE 'session:%' THEN EXISTS (SELECT 1 FROM web_sessions s
        WHERE lower(hex(s.token_hash)) = substr(p.credential, 9) AND s.account_id = p.account_id
          AND s.expires_at > ?1)
    ELSE 0 END";

const COLUMNS: &str = "p.id, p.device_client_id, p.verification_code, p.verified, p.expires, p.types, p.url_shown, \
     p.created_at, p.last_ok_at, p.failures";

fn from_row(row: &Row<'_>) -> rusqlite::Result<PushSubscription> {
    let verified: bool = row.get(3)?;
    let code: String = row.get(2)?;
    Ok(PushSubscription {
        id: row.get(0)?,
        device_client_id: row.get(1)?,
        verification_code: verified.then_some(code),
        expires: row.get(4)?,
        types: parse_types(row.get(5)?),
        url_shown: row.get(6)?,
        created_at: row.get(7)?,
        last_ok_at: row.get(8)?,
        failures: row.get(9)?,
    })
}

fn parse_types(raw: Option<String>) -> Option<Vec<String>> {
    raw.and_then(|raw| serde_json::from_str(&raw).ok())
}

fn types_json(types: &Option<Vec<String>>) -> Option<String> {
    types.as_ref().map(|types| serde_json::to_string(types).unwrap_or_else(|_| "[]".into()))
}

/// The push service's host, without the path that names the device.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    authority.rsplit_once('@').map_or(authority, |(_, host)| host).to_ascii_lowercase()
}

fn digest(url: &str) -> String {
    hex::encode(Sha256::digest(url.as_bytes()))
}

/// Constant-time comparison, so the time a wrong code takes says nothing about the right one.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn own(conn: &Connection, account_id: i64, credential: &str, id: i64) -> Result<PushSubscription> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM push_subscriptions p
             WHERE p.id = ?1 AND p.account_id = ?2 AND p.credential = ?3 AND p.expires > ?4"
        ),
        params![id, account_id, credential, now()],
        from_row,
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("push subscription {id}")))
}

fn target(conn: &Connection, row: &Row<'_>) -> Result<PushTarget> {
    let url: Vec<u8> = row.get(2)?;
    let p256dh: Option<String> = row.get(4)?;
    let auth: Option<Vec<u8>> = row.get(5)?;
    let keys = match (p256dh, auth) {
        (Some(p256dh), Some(auth)) => Some(PushKeys { p256dh, auth: unseal(conn, &auth)? }),
        _ => None,
    };
    Ok(PushTarget {
        id: row.get(0)?,
        account_id: row.get(1)?,
        url: unseal(conn, &url)?,
        url_shown: row.get(3)?,
        keys,
        types: parse_types(row.get(6)?),
        may_use_dav: row.get(8)?,
        verification_code: row.get(7)?,
    })
}

/// Whether the login behind a subscription `p` may reach calendars and address books: the account
/// password and the webmail may, an app password or OAuth app only with the `dav` scope.
macro_rules! may_use_dav {
    () => {
        "CASE
    WHEN p.credential LIKE 'app:%' THEN EXISTS (SELECT 1 FROM app_passwords ap
        WHERE ap.id = CAST(substr(p.credential, 5) AS INTEGER) AND ' ' || ap.scopes || ' ' LIKE '% dav %')
    WHEN p.credential LIKE 'oauth:%' THEN EXISTS (SELECT 1 FROM oauth_grants g
        WHERE g.id = CAST(substr(p.credential, 7) AS INTEGER) AND ' ' || g.scopes || ' ' LIKE '% dav %')
    ELSE 1 END"
    };
}

const TARGET_COLUMNS: &str = concat!(
    "p.id, p.account_id, p.url, p.url_shown, p.keys_p256dh, p.keys_auth, p.types, p.verification_code, ",
    may_use_dav!()
);

impl Store {
    /// Adds a subscription, not yet verified, and returns it with where to send its code.
    pub async fn create_push_subscription(&self, new: NewPushSubscription) -> Result<(PushSubscription, PushTarget)> {
        self.write(move |tx| {
            let now = now();
            purge(tx, now)?;
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM push_subscriptions WHERE account_id = ?1",
                [new.account_id],
                |row| row.get(0),
            )?;
            if count as usize >= MAX_PUSH_SUBSCRIPTIONS {
                return Err(StoreError::Rule {
                    code: "overQuota",
                    message: format!("an account may have {MAX_PUSH_SUBSCRIPTIONS} push subscriptions"),
                });
            }
            let code = hex::encode(random_bytes::<16>());
            let url = seal(tx, &new.url)?;
            let auth = match &new.keys {
                Some(keys) => Some(seal(tx, &keys.auth)?),
                None => None,
            };
            tx.execute(
                "INSERT INTO push_subscriptions (account_id, credential, device_client_id, url, url_digest, url_shown,
                     keys_p256dh, keys_auth, verification_code, expires, types, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    new.account_id,
                    new.credential,
                    new.device_client_id,
                    url,
                    digest(&new.url),
                    host_of(&new.url),
                    new.keys.as_ref().map(|keys| keys.p256dh.clone()),
                    auth,
                    code,
                    new.expires,
                    types_json(&new.types),
                    now,
                ],
            )?;
            let id = tx.last_insert_rowid();
            let subscription = own(tx, new.account_id, &new.credential, id)?;
            let may_use_dav = tx.query_row(
                concat!("SELECT ", may_use_dav!(), " FROM push_subscriptions p WHERE p.id = ?1"),
                [id],
                |row| row.get(0),
            )?;
            let target = PushTarget {
                id,
                account_id: new.account_id,
                url_shown: subscription.url_shown.clone(),
                url: new.url,
                keys: new.keys,
                types: new.types,
                verification_code: code,
                may_use_dav,
            };
            Ok((subscription, target))
        })
        .await
    }

    /// The subscriptions a login made and that have not expired, oldest first.
    pub async fn push_subscriptions(&self, account_id: i64, credential: &str) -> Result<Vec<PushSubscription>> {
        let credential = credential.to_owned();
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM push_subscriptions p
                 WHERE p.account_id = ?1 AND p.credential = ?2 AND p.expires > ?3 ORDER BY p.id"
            ))?;
            let rows = stmt.query_map(params![account_id, credential, now()], from_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Changes a subscription of this login. A verification code has to be the one that was sent:
    /// a wrong one is the rule `invalidVerificationCode`, and after [`PUSH_MAX_VERIFY_ATTEMPTS`]
    /// wrong ones the subscription is gone. Once verified, older subscriptions of the account to
    /// the same address are dropped: they were the same device asking again.
    pub async fn update_push_subscription(
        &self,
        account_id: i64,
        credential: &str,
        id: i64,
        update: PushSubscriptionUpdate,
    ) -> Result<PushSubscription> {
        let credential = credential.to_owned();
        let (verify, subscription) = self
            .write(move |tx| {
                let current = own(tx, account_id, &credential, id)?;
                let mut verify = Verify::Unchanged;
                if let Some(sent) = &update.verification_code {
                    let (expected, verified, attempts): (String, bool, i64) = tx.query_row(
                        "SELECT verification_code, verified, verify_attempts FROM push_subscriptions WHERE id = ?1",
                        [id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )?;
                    if same(sent, &expected) {
                        if !verified {
                            tx.execute("UPDATE push_subscriptions SET verified = 1 WHERE id = ?1", [id])?;
                            let digest: String =
                                tx.query_row("SELECT url_digest FROM push_subscriptions WHERE id = ?1", [id], |row| {
                                    row.get(0)
                                })?;
                            tx.execute(
                                "DELETE FROM push_subscriptions WHERE account_id = ?1 AND url_digest = ?2 AND id != ?3",
                                params![account_id, digest, id],
                            )?;
                        }
                        verify = Verify::Right;
                    } else if attempts + 1 >= PUSH_MAX_VERIFY_ATTEMPTS {
                        tx.execute("DELETE FROM push_subscriptions WHERE id = ?1", [id])?;
                        return Ok((Verify::TooMany, current));
                    } else {
                        tx.execute(
                            "UPDATE push_subscriptions SET verify_attempts = verify_attempts + 1 WHERE id = ?1",
                            [id],
                        )?;
                        return Ok((Verify::Wrong, current));
                    }
                }
                if let Some(expires) = update.expires {
                    tx.execute("UPDATE push_subscriptions SET expires = ?1 WHERE id = ?2", params![expires, id])?;
                }
                if let Some(types) = &update.types {
                    tx.execute(
                        "UPDATE push_subscriptions SET types = ?1 WHERE id = ?2",
                        params![types_json(types), id],
                    )?;
                }
                Ok((verify, own(tx, account_id, &credential, id)?))
            })
            .await?;
        match verify {
            Verify::Wrong => Err(StoreError::Rule {
                code: "invalidVerificationCode",
                message: "that is not the verification code that was pushed".into(),
            }),
            Verify::TooMany => Err(StoreError::NotFound(format!("push subscription {id}"))),
            Verify::Right | Verify::Unchanged => Ok(subscription),
        }
    }

    /// Removes a subscription of this login.
    pub async fn destroy_push_subscription(&self, account_id: i64, credential: &str, id: i64) -> Result<()> {
        let credential = credential.to_owned();
        self.write(move |tx| {
            let removed = tx.execute(
                "DELETE FROM push_subscriptions WHERE id = ?1 AND account_id = ?2 AND credential = ?3",
                params![id, account_id, credential],
            )?;
            if removed == 0 {
                return Err(StoreError::NotFound(format!("push subscription {id}")));
            }
            Ok(())
        })
        .await
    }

    /// Accounts whose push subscriptions hear of a change in `account_id`: itself, and everyone it
    /// shares mailboxes with, one by one or as the members of a shared mailbox (docs/groups.md).
    pub async fn push_audience(&self, account_id: i64) -> Result<Vec<i64>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT DISTINCT acl.grantee_id FROM {GRANTS} acl
                 JOIN accounts g ON g.id = acl.grantee_id AND g.deleted_at IS NULL
                 WHERE acl.owner_id = ?1 AND acl.grantee_id != ?1 ORDER BY acl.grantee_id"
            ))?;
            let mut accounts = vec![account_id];
            accounts.extend(stmt.query_map([account_id], |row| row.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?);
            Ok(accounts)
        })
        .await
    }

    /// Who hears of new mail in `account_id` since state `since` (JMAP `EmailDelivery`): the account
    /// itself, and those who may read the folder it came into, shared one by one or as a member of
    /// a shared mailbox. New mail is a message that
    /// arrived after `since`, is neither read nor a draft, and is not in the drafts, sent, junk or
    /// trash folder; a copy an app filed in Sent or a draft it saved is no news, and a browser has
    /// to show something for every push it gets.
    pub async fn push_deliveries(&self, account_id: i64, since: i64) -> Result<Vec<i64>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "WITH delivered AS (
                     SELECT DISTINCT em.mailbox_id FROM changes c
                     JOIN emails e ON e.id = c.object_id AND e.account_id = c.account_id
                     JOIN email_mailboxes em ON em.email_id = e.id
                     JOIN mailboxes m ON m.id = em.mailbox_id
                     WHERE c.account_id = ?1 AND c.modseq > ?2 AND c.kind = 'Email' AND c.change = 'created'
                       AND (m.role IS NULL OR m.role NOT IN ('drafts', 'sent', 'junk', 'trash'))
                       AND NOT EXISTS (SELECT 1 FROM email_keywords k
                                       WHERE k.email_id = e.id AND lower(k.keyword) IN ('$seen', '$draft')))
                 SELECT ?1 WHERE EXISTS (SELECT 1 FROM delivered)
                 UNION
                 SELECT acl.grantee_id FROM {GRANTS} acl JOIN delivered d ON d.mailbox_id = acl.mailbox_id
                 JOIN accounts g ON g.id = acl.grantee_id AND g.deleted_at IS NULL
                 WHERE acl.owner_id = ?1 AND instr(acl.rights, 'r') > 0"
            ))?;
            let rows = stmt.query_map(params![account_id, since], |row| row.get::<_, i64>(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// The verified subscriptions of these accounts that may be pushed to now: not expired, not
    /// waiting after a failure, and made by a login that still holds.
    pub async fn push_targets(&self, account_ids: Vec<i64>) -> Result<Vec<PushTarget>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {TARGET_COLUMNS} FROM push_subscriptions p JOIN accounts a ON a.id = p.account_id
                 WHERE p.account_id = ?2 AND p.verified = 1 AND p.expires > ?1 AND p.retry_at <= ?1 AND {STILL_VALID}
                 ORDER BY p.id"
            ))?;
            let now = now();
            let mut targets = Vec::new();
            for account_id in account_ids {
                let mut rows = stmt.query(params![now, account_id])?;
                while let Some(row) = rows.next()? {
                    targets.push(target(conn, row)?);
                }
            }
            Ok(targets)
        })
        .await
    }

    /// Whether any subscription is verified at all; the sender sleeps while none is.
    pub async fn has_push_subscriptions(&self) -> Result<bool> {
        self.read(move |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM push_subscriptions WHERE verified = 1 AND expires > ?1)",
                [now()],
                |row| row.get(0),
            )?)
        })
        .await
    }

    /// A push went through.
    pub async fn push_delivered(&self, id: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "UPDATE push_subscriptions SET last_ok_at = ?1, failures = 0, retry_at = 0 WHERE id = ?2",
                params![now(), id],
            )?;
            Ok(())
        })
        .await
    }

    /// A push failed: the next waits, longer after each failure in a row (half a minute, doubling,
    /// up to an hour), and after [`PUSH_MAX_FAILURES`] in a row the subscription is dropped.
    /// Returns whether it was.
    pub async fn push_failed(&self, id: i64) -> Result<bool> {
        self.write(move |tx| {
            tx.execute(
                "UPDATE push_subscriptions SET failures = failures + 1,
                     retry_at = ?1 + min(3600, 30 * (1 << min(failures, 7))) WHERE id = ?2",
                params![now(), id],
            )?;
            Ok(tx.execute(
                "DELETE FROM push_subscriptions WHERE id = ?1 AND failures >= ?2",
                params![id, PUSH_MAX_FAILURES],
            )? > 0)
        })
        .await
    }

    /// Drops a subscription the push service no longer knows (404, 410).
    pub async fn push_gone(&self, id: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute("DELETE FROM push_subscriptions WHERE id = ?1", [id])?;
            Ok(())
        })
        .await
    }

    /// Drops expired subscriptions, those never verified within a day, and those whose login
    /// ended. Returns how many went.
    pub async fn purge_push_subscriptions(&self) -> Result<usize> {
        self.write(move |tx| purge(tx, now())).await
    }

    /// The server's VAPID key pair as PKCS#8, made on first use and kept sealed.
    pub async fn push_vapid_key(&self) -> Result<Vec<u8>> {
        if let Some(key) = self.read(read_vapid_key).await? {
            return Ok(key);
        }
        self.write(|tx| {
            // Someone else may have made it in the meantime.
            if let Some(key) = read_vapid_key(tx)? {
                return Ok(key);
            }
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
                .map_err(|_| StoreError::Internal("making the VAPID key failed".into()))?;
            let key = pkcs8.as_ref().to_vec();
            let sealed = seal(tx, &hex::encode(&key))?;
            set_setting(tx, VAPID_KEY, &hex::encode(sealed))?;
            Ok(key)
        })
        .await
    }
}

fn read_vapid_key(conn: &Connection) -> Result<Option<Vec<u8>>> {
    let Some(stored) = get_setting(conn, VAPID_KEY)? else {
        return Ok(None);
    };
    let sealed = hex::decode(stored).map_err(|_| StoreError::Internal("the stored VAPID key is garbled".into()))?;
    let key = hex::decode(unseal(conn, &sealed)?)
        .map_err(|_| StoreError::Internal("the stored VAPID key is garbled".into()))?;
    Ok(Some(key))
}

/// Ends the push subscriptions made with a credential that goes away now: `app:<id>` when the app
/// password is revoked, `oauth:<id>` when the app is signed out. They would never be pushed to
/// again anyway, but need not wait for the next clean-up.
pub(crate) fn forget_push_credential(conn: &Connection, account_id: i64, credential: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM push_subscriptions WHERE account_id = ?1 AND credential = ?2",
        params![account_id, credential],
    )
}

fn purge(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute(
        &format!(
            "DELETE FROM push_subscriptions WHERE expires <= ?1 OR (verified = 0 AND created_at <= ?1 - {UNVERIFIED_LIFETIME_SECS})
                 OR id IN (SELECT p.id FROM push_subscriptions p JOIN accounts a ON a.id = p.account_id
                           WHERE NOT ({STILL_VALID}))
                 OR account_id NOT IN (SELECT id FROM accounts)"
        ),
        [now],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{AppScope, IngestRequest, MailboxRole, MailboxTarget, NewAccount, NewAppPassword, Role};

    async fn account(store: &Store, login: &str) -> i64 {
        store.create_domain("example.org").await.ok();
        store
            .create_account(NewAccount {
                address: format!("{login}@example.org"),
                display_name: login.into(),
                password: Some("katzenpfote-123".into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id
    }

    fn new(account_id: i64, credential: &str, url: &str) -> NewPushSubscription {
        NewPushSubscription {
            account_id,
            credential: credential.into(),
            device_client_id: "phone".into(),
            url: url.into(),
            keys: Some(PushKeys { p256dh: "BCVx".into(), auth: "BTBZ".into() }),
            expires: now() + 3600,
            types: Some(vec!["Email".into()]),
        }
    }

    #[tokio::test]
    async fn subscriptions_are_sealed_verified_and_belong_to_their_login() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini").await;
        let (created, target) =
            store.create_push_subscription(new(mini, "password", "https://push.example.net/d/secret")).await.unwrap();
        assert_eq!(created.verification_code, None, "the code is only shown once it came back");
        assert_eq!(created.url_shown, "push.example.net");
        assert_eq!(target.url, "https://push.example.net/d/secret");
        assert_eq!(target.keys.as_ref().unwrap().auth, "BTBZ");

        // Sealed at rest: neither the address nor the auth secret is in the table as text.
        let raw: (Vec<u8>, Vec<u8>) = store
            .read(|conn| {
                Ok(conn.query_row("SELECT url, keys_auth FROM push_subscriptions", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?)
            })
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&raw.0).contains("secret"));
        assert!(!String::from_utf8_lossy(&raw.1).contains("BTBZ"));

        // Another login of the same account sees nothing, and nothing is pushed before verifying.
        assert!(store.push_subscriptions(mini, "app:1").await.unwrap().is_empty());
        assert!(store.push_targets(vec![mini]).await.unwrap().is_empty());

        let wrong = PushSubscriptionUpdate { verification_code: Some("nope".into()), ..Default::default() };
        let err = store.update_push_subscription(mini, "password", created.id, wrong).await.unwrap_err();
        assert!(matches!(err, StoreError::Rule { code: "invalidVerificationCode", .. }), "{err:?}");

        let right =
            PushSubscriptionUpdate { verification_code: Some(target.verification_code.clone()), ..Default::default() };
        let verified = store.update_push_subscription(mini, "password", created.id, right).await.unwrap();
        assert_eq!(verified.verification_code.as_deref(), Some(target.verification_code.as_str()));
        assert_eq!(store.push_targets(vec![mini]).await.unwrap(), vec![target.clone()]);

        // The same address subscribed again replaces the old one once verified.
        let (again, again_target) =
            store.create_push_subscription(new(mini, "password", "https://push.example.net/d/secret")).await.unwrap();
        let code = PushSubscriptionUpdate {
            verification_code: Some(again_target.verification_code),
            types: Some(None),
            ..Default::default()
        };
        let updated = store.update_push_subscription(mini, "password", again.id, code).await.unwrap();
        assert_eq!(updated.types, None);
        let left = store.push_subscriptions(mini, "password").await.unwrap();
        assert_eq!(left.iter().map(|s| s.id).collect::<Vec<_>>(), vec![again.id]);

        store.destroy_push_subscription(mini, "password", again.id).await.unwrap();
        assert!(store.push_subscriptions(mini, "password").await.unwrap().is_empty());
    }

    /// Whether a subscription's login may know of calendars: the account password may, an app
    /// password only with `dav`.
    #[tokio::test]
    async fn targets_know_whether_their_login_may_use_dav() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini").await;
        let app = |name: &str, scopes| NewAppPassword { name: name.into(), scopes, expires_at: None };
        let mail = store.create_app_password(mini, app("Mail", vec![AppScope::Mail])).await.unwrap();
        let dav = store.create_app_password(mini, app("Both", vec![AppScope::Mail, AppScope::Dav])).await.unwrap();
        let mut expected = Vec::new();
        for (credential, may) in [
            ("password".to_owned(), true),
            (push_credential_for_app_password(mail.app_password.id), false),
            (push_credential_for_app_password(dav.app_password.id), true),
        ] {
            let url = format!("https://push.example.net/{}", credential.replace(':', "-"));
            let (created, target) = store.create_push_subscription(new(mini, &credential, &url)).await.unwrap();
            assert_eq!(target.may_use_dav, may, "{credential}");
            let right =
                PushSubscriptionUpdate { verification_code: Some(target.verification_code), ..Default::default() };
            store.update_push_subscription(mini, &credential, created.id, right).await.unwrap();
            expected.push(may);
        }
        let targets = store.push_targets(vec![mini]).await.unwrap();
        assert_eq!(targets.iter().map(|t| t.may_use_dav).collect::<Vec<_>>(), expected);
    }

    #[tokio::test]
    async fn too_many_wrong_codes_end_a_subscription() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini").await;
        let (created, _) =
            store.create_push_subscription(new(mini, "password", "https://push.example.net/a")).await.unwrap();
        for attempt in 1..=PUSH_MAX_VERIFY_ATTEMPTS {
            let wrong = PushSubscriptionUpdate { verification_code: Some("nope".into()), ..Default::default() };
            let err = store.update_push_subscription(mini, "password", created.id, wrong).await.unwrap_err();
            if attempt < PUSH_MAX_VERIFY_ATTEMPTS {
                assert!(matches!(err, StoreError::Rule { .. }), "{err:?}");
            } else {
                assert!(matches!(err, StoreError::NotFound(_)), "{err:?}");
            }
        }
        assert!(store.push_subscriptions(mini, "password").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn subscriptions_are_limited_and_end_with_their_login() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini").await;
        for n in 0..MAX_PUSH_SUBSCRIPTIONS {
            store
                .create_push_subscription(new(mini, "password", &format!("https://push.example.net/{n}")))
                .await
                .unwrap();
        }
        let err =
            store.create_push_subscription(new(mini, "password", "https://push.example.net/x")).await.unwrap_err();
        assert!(matches!(err, StoreError::Rule { code: "overQuota", .. }), "{err:?}");
        store.write(|tx| Ok(tx.execute("DELETE FROM push_subscriptions", [])?)).await.unwrap();

        let verify = |store: Store, credential: &'static str, url: &'static str| async move {
            let (created, target) = store.create_push_subscription(new(mini, credential, url)).await.unwrap();
            let code =
                PushSubscriptionUpdate { verification_code: Some(target.verification_code), ..Default::default() };
            store.update_push_subscription(mini, credential, created.id, code).await.unwrap();
        };
        // A web session's subscription goes with the session.
        let session = store.create_web_session(mini, 3600, "192.0.2.1", "test").await.unwrap();
        let credential: &'static str = Box::leak(push_credential_for_session(&session.token).into_boxed_str());
        verify(store.clone(), credential, "https://push.example.net/web").await;
        // An app password's with the app password.
        let app = store
            .create_app_password(
                mini,
                NewAppPassword { name: "phone".into(), scopes: vec![AppScope::Mail], expires_at: None },
            )
            .await
            .unwrap();
        let app_credential: &'static str =
            Box::leak(push_credential_for_app_password(app.app_password.id).into_boxed_str());
        verify(store.clone(), app_credential, "https://push.example.net/app").await;
        verify(store.clone(), PUSH_CREDENTIAL_PASSWORD, "https://push.example.net/password").await;
        assert_eq!(store.push_targets(vec![mini]).await.unwrap().len(), 3);

        // The password's subscription is older than this second: revoking an app password must not
        // end it all the same (it did while both went by credentials_changed_at).
        store
            .write(move |tx| {
                tx.execute("UPDATE accounts SET password_changed_at = password_changed_at - 10 WHERE id = ?1", [mini])?;
                Ok(tx.execute(
                    "UPDATE push_subscriptions SET created_at = created_at - 5 WHERE credential = 'password'",
                    [],
                )?)
            })
            .await
            .unwrap();
        store.delete_web_session(&session.token).await.unwrap();
        store.revoke_app_password(mini, app.app_password.id).await.unwrap();
        // Revoking the app password ends its subscriptions right away, not at the next clean-up.
        assert!(store.push_subscriptions(mini, app_credential).await.unwrap().is_empty());
        let left = store.push_targets(vec![mini]).await.unwrap();
        assert_eq!(left.iter().map(|t| t.url.as_str()).collect::<Vec<_>>(), vec!["https://push.example.net/password"]);
        // A new password (or second factor) ends what the password made; within the same second
        // it still counts, so the change is dated a moment later here.
        store
            .write(move |tx| {
                Ok(tx.execute(
                    "UPDATE accounts SET credentials_changed_at = ?1, password_changed_at = ?1 WHERE id = ?2",
                    params![now() + 5, mini],
                )?)
            })
            .await
            .unwrap();
        assert!(store.push_targets(vec![mini]).await.unwrap().is_empty());
        assert_eq!(store.purge_push_subscriptions().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn failures_back_off_and_end_a_subscription() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini").await;
        let (created, target) =
            store.create_push_subscription(new(mini, "password", "https://push.example.net/a")).await.unwrap();
        let code = PushSubscriptionUpdate { verification_code: Some(target.verification_code), ..Default::default() };
        store.update_push_subscription(mini, "password", created.id, code).await.unwrap();
        assert!(!store.push_failed(created.id).await.unwrap());
        assert!(store.push_targets(vec![mini]).await.unwrap().is_empty(), "waits after a failure");
        store.push_delivered(created.id).await.unwrap();
        assert_eq!(store.push_targets(vec![mini]).await.unwrap().len(), 1);
        for _ in 1..PUSH_MAX_FAILURES {
            assert!(!store.push_failed(created.id).await.unwrap());
        }
        let retry_at: i64 = store
            .read(move |conn| {
                Ok(conn.query_row("SELECT retry_at FROM push_subscriptions WHERE id = ?1", [created.id], |row| {
                    row.get(0)
                })?)
            })
            .await
            .unwrap();
        assert!(retry_at <= now() + 3600, "an hour at most");
        assert!(store.push_failed(created.id).await.unwrap(), "dropped after too many");
    }

    #[tokio::test]
    async fn the_vapid_key_is_made_once_and_kept_sealed() {
        let (store, _dir) = store().await;
        let key = store.push_vapid_key().await.unwrap();
        assert!(EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &key).is_ok());
        assert_eq!(store.push_vapid_key().await.unwrap(), key);
        let stored = store.setting(VAPID_KEY).await.unwrap().unwrap();
        assert!(!stored.contains(&hex::encode(&key)));
    }

    #[tokio::test]
    async fn shared_mailboxes_widen_the_audience() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini").await;
        let nyu = account(&store, "nyu").await;
        assert_eq!(store.push_audience(mini).await.unwrap(), vec![mini]);
        let inbox = store.mailboxes(mini).await.unwrap().into_iter().next().unwrap();
        store.set_mailbox_acl_for(mini, inbox.id, nyu, "lr").await.unwrap();
        assert_eq!(store.push_audience(mini).await.unwrap(), vec![mini, nyu]);
    }

    async fn ingest(store: &Store, account: i64, role: MailboxRole, keywords: &[&str]) -> i64 {
        let before = store.account_modseq(account).await.unwrap();
        store
            .ingest(IngestRequest {
                account_id: account,
                raw: b"From: nyu@example.net\r\nTo: mini@example.org\r\nSubject: Hi\r\n\r\nHallo\r\n".to_vec(),
                mailboxes: vec![MailboxTarget::Role(role)],
                keywords: keywords.iter().map(|k| (*k).to_owned()).collect(),
                received_at: None,
            })
            .await
            .unwrap();
        before
    }

    #[tokio::test]
    async fn only_new_unread_mail_counts_as_a_delivery() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini").await;
        let nyu = account(&store, "nyu").await;

        // A copy in Sent, a draft, junk and a message that is already read are no news.
        let since = ingest(&store, mini, MailboxRole::Sent, &["$seen"]).await;
        ingest(&store, mini, MailboxRole::Drafts, &["$draft"]).await;
        ingest(&store, mini, MailboxRole::Junk, &[]).await;
        ingest(&store, mini, MailboxRole::Inbox, &["$seen"]).await;
        assert!(store.push_deliveries(mini, since).await.unwrap().is_empty());

        // New mail in the inbox is, for the account and for whoever may read the inbox.
        let since = ingest(&store, mini, MailboxRole::Inbox, &[]).await;
        assert_eq!(store.push_deliveries(mini, since).await.unwrap(), vec![mini]);
        let inbox =
            store.mailboxes(mini).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap();
        store.set_mailbox_acl_for(mini, inbox.id, nyu, "lr").await.unwrap();
        assert_eq!(store.push_deliveries(mini, since).await.unwrap(), vec![mini, nyu]);
        // Nothing new after that.
        assert!(store.push_deliveries(mini, store.account_modseq(mini).await.unwrap()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn members_of_a_shared_mailbox_hear_of_it() {
        let sorted = |mut ids: Vec<i64>| {
            ids.sort_unstable();
            ids
        };
        let (store, _dir) = store().await;
        let mini = account(&store, "mini").await;
        let nyu = account(&store, "nyu").await;
        let support = store
            .create_shared_mailbox(crate::NewSharedMailbox {
                address: "support@example.org".into(),
                name: "Support".into(),
                quota_bytes: 0,
                members: vec![("mini@example.org".into(), true)],
            })
            .await
            .unwrap()
            .id;
        assert_eq!(store.push_audience(support).await.unwrap(), vec![support, mini]);
        let since = ingest(&store, support, MailboxRole::Inbox, &[]).await;
        assert_eq!(sorted(store.push_deliveries(support, since).await.unwrap()), sorted(vec![support, mini]));
        // Junk is no news for the members either.
        let since = ingest(&store, support, MailboxRole::Junk, &[]).await;
        assert_eq!(store.push_deliveries(support, since).await.unwrap(), Vec::<i64>::new());

        // Membership changes, and so does who hears of it.
        store.set_shared_mailbox_members("support@example.org", vec![("nyu@example.org".into(), false)]).await.unwrap();
        assert_eq!(store.push_audience(support).await.unwrap(), vec![support, nyu]);
        let since = ingest(&store, support, MailboxRole::Inbox, &[]).await;
        assert_eq!(sorted(store.push_deliveries(support, since).await.unwrap()), sorted(vec![support, nyu]));
    }
}
