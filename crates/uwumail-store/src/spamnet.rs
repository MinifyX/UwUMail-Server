//! Spam-Netz: the list the hub shares, what a stored mail would report, and the reports waiting to
//! be sent. Working out digests and talking to the hub happens in the SMTP crate; this only keeps
//! rows. What a report carries is an opaque JSON text of the four kinds the protocol allows.

use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::blobs::BlobHash;
use crate::{Result, Store, now};

/// A mail is reported only while this fresh; after that nobody marks it in time to matter.
pub const SPAMNET_FEATURES_DAYS: i64 = 30;
/// A report the hub never took is given up after this long.
pub const SPAMNET_QUEUE_DAYS: i64 = 14;

/// One entry of the shared list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpamNetEntry {
    pub kind: String,
    pub value: String,
    pub score: f64,
    /// RFC 3339, as the hub sends it.
    pub expires: String,
}

/// An entry the hub took off the list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpamNetRemoved {
    pub kind: String,
    pub value: String,
}

/// A report waiting to be sent.
#[derive(Clone, Debug, PartialEq)]
pub struct SpamNetReport {
    pub id: i64,
    pub verdict: String,
    pub reason: String,
    /// `{digests, domains, ip, from}` as JSON.
    pub features: String,
}

/// What a stored mail would report and what decides whether it may be reported.
#[derive(Clone, Debug)]
pub struct SpamNetFeatures {
    pub features: String,
    pub from_address: String,
    pub from_host: String,
    pub authenticated: bool,
}

/// A person marked a stored mail as spam (or took it out of Junk): queue a report of it, unless it
/// came from one of the server's own domains, or it is authenticated mail from one of their contacts.
pub(crate) fn queue_marked(tx: &Transaction<'_>, blob: &str, account_id: i64, spam: bool) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO spamnet_queue (blob_hash, verdict, reason, features, created_at)
         SELECT f.blob_hash, ?3, 'user', f.features, ?4 FROM spamnet_features f
         WHERE f.blob_hash = ?1
           AND NOT EXISTS (SELECT 1 FROM domains d WHERE d.name = f.from_host
                           OR d.name = json_extract(f.features, '$.from'))
           AND NOT (f.authenticated AND EXISTS (SELECT 1 FROM contact_emails c
                    WHERE c.account_id = ?2 AND lower(c.email) = f.from_address))",
        params![blob, account_id, if spam { "spam" } else { "ham" }, now()],
    )?;
    // Changing one's mind before the report went out takes back the other one.
    tx.execute(
        "DELETE FROM spamnet_queue WHERE blob_hash = ?1 AND verdict = ?2",
        params![blob, if spam { "ham" } else { "spam" }],
    )?;
    Ok(())
}

fn is_own(conn: &Connection, from_host: &str, features: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM domains WHERE name = ?1 OR name = json_extract(?2, '$.from'))",
        params![from_host, features],
        |row| row.get(0),
    )?)
}

impl Store {
    /// Keeps what a delivered mail would report, should a person mark it later.
    pub async fn keep_spamnet_features(&self, blob: BlobHash, features: SpamNetFeatures) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "INSERT OR REPLACE INTO spamnet_features
                     (blob_hash, features, from_address, from_host, authenticated, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    blob.as_str(),
                    features.features,
                    features.from_address.to_lowercase(),
                    features.from_host.to_lowercase(),
                    features.authenticated,
                    now()
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Queues a report the filter itself decided on, e.g. a mail it refused. Mail from the
    /// server's own domains is left out here too.
    pub async fn queue_spamnet_report(&self, verdict: &str, reason: &str, features: SpamNetFeatures) -> Result<()> {
        let (verdict, reason) = (verdict.to_owned(), reason.to_owned());
        self.write(move |tx| {
            if is_own(tx, &features.from_host.to_lowercase(), &features.features)? {
                return Ok(());
            }
            tx.execute(
                "INSERT INTO spamnet_queue (blob_hash, verdict, reason, features, created_at)
                 VALUES (NULL, ?1, ?2, ?3, ?4)",
                params![verdict, reason, features.features, now()],
            )?;
            Ok(())
        })
        .await
    }

    /// The oldest reports waiting to be sent.
    pub async fn spamnet_reports(&self, limit: usize) -> Result<Vec<SpamNetReport>> {
        self.read(move |conn| {
            let mut stmt =
                conn.prepare("SELECT id, verdict, reason, features FROM spamnet_queue ORDER BY id LIMIT ?1")?;
            let rows = stmt.query_map(params![limit as i64], |row| {
                Ok(SpamNetReport { id: row.get(0)?, verdict: row.get(1)?, reason: row.get(2)?, features: row.get(3)? })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Takes sent reports off the queue.
    pub async fn drop_spamnet_reports(&self, ids: Vec<i64>) -> Result<()> {
        self.write(move |tx| {
            let mut stmt = tx.prepare("DELETE FROM spamnet_queue WHERE id = ?1")?;
            for id in ids {
                stmt.execute([id])?;
            }
            Ok(())
        })
        .await
    }

    /// How many reports wait to be sent.
    pub async fn spamnet_queue_len(&self) -> Result<i64> {
        self.read(|conn| Ok(conn.query_row("SELECT count(*) FROM spamnet_queue", [], |row| row.get(0))?)).await
    }

    /// Reporting was switched off: nothing kept for it is sent later.
    pub async fn clear_spamnet_reports(&self) -> Result<()> {
        self.write(|tx| {
            tx.execute_batch("DELETE FROM spamnet_queue; DELETE FROM spamnet_features;")?;
            Ok(())
        })
        .await
    }

    /// Takes in what the hub sent: a full list replaces the old one, otherwise entries are added or
    /// updated and removed ones dropped. Entries whose expiry does not parse are skipped.
    pub async fn apply_spamnet_list(
        &self,
        full: bool,
        entries: Vec<SpamNetEntry>,
        removed: Vec<SpamNetRemoved>,
    ) -> Result<()> {
        self.write(move |tx| {
            if full {
                tx.execute("DELETE FROM spamnet_list", [])?;
            }
            let mut drop = tx.prepare("DELETE FROM spamnet_list WHERE kind = ?1 AND value = ?2")?;
            for gone in removed {
                drop.execute(params![gone.kind, gone.value])?;
            }
            let mut put = tx.prepare(
                "INSERT INTO spamnet_list (kind, value, score, expires) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (kind, value) DO UPDATE SET score = excluded.score, expires = excluded.expires",
            )?;
            for entry in entries {
                let Ok(expires) = chrono::DateTime::parse_from_rfc3339(&entry.expires) else { continue };
                if !entry.score.is_finite() {
                    continue;
                }
                put.execute(params![entry.kind, entry.value.to_lowercase(), entry.score, expires.timestamp()])?;
            }
            Ok(())
        })
        .await
    }

    /// The entries of the list that match, and have not expired, as (kind, value, score).
    pub async fn spamnet_matches(&self, wanted: Vec<(&'static str, String)>) -> Result<Vec<(String, String, f64)>> {
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        self.read(move |conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT kind, value, score FROM spamnet_list WHERE kind = ?1 AND value = ?2 AND expires > ?3",
            )?;
            let now = now();
            let mut found = Vec::new();
            for (kind, value) in wanted {
                let mut rows = stmt.query(params![kind, value, now])?;
                while let Some(row) = rows.next()? {
                    found.push((row.get(0)?, row.get(1)?, row.get(2)?));
                }
            }
            Ok(found)
        })
        .await
    }

    /// How many entries of the list are in force.
    pub async fn spamnet_list_len(&self) -> Result<i64> {
        self.read(|conn| {
            Ok(conn.query_row("SELECT count(*) FROM spamnet_list WHERE expires > ?1", [now()], |row| row.get(0))?)
        })
        .await
    }

    /// Forgets what is too old to report, reports the hub never took, and expired entries.
    pub async fn prune_spamnet(&self) -> Result<()> {
        self.write(|tx| {
            let now = now();
            tx.execute("DELETE FROM spamnet_features WHERE created_at < ?1", [now - SPAMNET_FEATURES_DAYS * 86400])?;
            tx.execute("DELETE FROM spamnet_queue WHERE created_at < ?1", [now - SPAMNET_QUEUE_DAYS * 86400])?;
            tx.execute("DELETE FROM spamnet_list WHERE expires <= ?1", [now])?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{
        ContactCardWrite, DavKind, EmailUpdate, IngestRequest, KeywordsChange, MailboxRole, MailboxTarget,
        MailboxesChange, NewAccount, NewDavCollection, Role,
    };

    fn features(from: &str, authenticated: bool) -> SpamNetFeatures {
        let host = from.rsplit('@').next().unwrap().to_owned();
        SpamNetFeatures {
            features: serde_json::json!({"digests": ["d1:00"], "domains": [], "ip": "192.0.2.1", "from": host})
                .to_string(),
            from_address: from.into(),
            from_host: host,
            authenticated,
        }
    }

    #[tokio::test]
    async fn marks_queue_reports_but_never_own_or_contact_mail() {
        let (store, _dir) = store().await;
        store.create_domain("uwu.test").await.unwrap();
        let account = NewAccount {
            address: "mini@uwu.test".into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let mini = store.create_account(account).await.unwrap().id;
        let book = store
            .dav_collections(
                mini,
                DavKind::Addressbook,
                NewDavCollection { slug: "contacts".into(), display_name: "Kontakte".into(), ..Default::default() },
            )
            .await
            .unwrap()[0]
            .clone();
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:nyu\r\nFN:Nyu\r\nEMAIL:Nyu@Friend.example\r\nEND:VCARD\r\n";
        let write = ContactCardWrite {
            id: None,
            address_book_id: book.id,
            content: card.into(),
            uid: "nyu".into(),
            if_etag: None,
        };
        store.put_contact_card(mini, write).await.unwrap();

        let mut ids = Vec::new();
        for (subject, from, authenticated) in [
            ("spam", "x@spam.example", false),
            ("own", "leni@uwu.test", true),
            ("contact", "nyu@friend.example", true),
            ("forged contact", "nyu@friend.example", false),
        ] {
            let raw = format!("Subject: {subject}\r\n\r\nx\r\n").into_bytes();
            let email = store
                .ingest(IngestRequest {
                    account_id: mini,
                    raw,
                    mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
                    keywords: Default::default(),
                    received_at: None,
                })
                .await
                .unwrap();
            store.keep_spamnet_features(email.blob.clone(), features(from, authenticated)).await.unwrap();
            ids.push(email.id);
        }
        let junk = store.mailboxes(mini).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Junk));
        let junk = junk.unwrap().id;
        let marks = ids
            .iter()
            .map(|&id| EmailUpdate {
                id,
                keywords: KeywordsChange::Keep,
                mailboxes: MailboxesChange::Replace(vec![junk]),
            })
            .collect();
        store.update_emails(mini, marks).await.unwrap();
        let reports = store.spamnet_reports(10).await.unwrap();
        assert_eq!(reports.len(), 2, "own and authenticated contact mail stay out: {reports:?}");
        assert!(reports.iter().all(|r| r.verdict == "spam" && r.reason == "user"));

        // Taking the first one back out of Junk replaces its spam report with a ham one.
        let inbox = store.mailboxes(mini).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox));
        let back = EmailUpdate {
            id: ids[0],
            keywords: KeywordsChange::Keep,
            mailboxes: MailboxesChange::Replace(vec![inbox.unwrap().id]),
        };
        store.update_emails(mini, vec![back]).await.unwrap();
        let verdicts: Vec<_> = store.spamnet_reports(10).await.unwrap().into_iter().map(|r| r.verdict).collect();
        assert_eq!(verdicts, ["spam", "ham"]);

        // The filter's own reports skip own domains as well.
        store.queue_spamnet_report("spam", "filter", features("a@uwu.test", false)).await.unwrap();
        store.queue_spamnet_report("spam", "filter", features("a@spam.example", false)).await.unwrap();
        assert_eq!(store.spamnet_queue_len().await.unwrap(), 3);
        let sent: Vec<_> = store.spamnet_reports(2).await.unwrap().into_iter().map(|r| r.id).collect();
        store.drop_spamnet_reports(sent).await.unwrap();
        assert_eq!(store.spamnet_queue_len().await.unwrap(), 1);
        store.clear_spamnet_reports().await.unwrap();
        assert_eq!(store.spamnet_queue_len().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn the_list_is_replaced_updated_and_expires() {
        let (store, _dir) = store().await;
        let entry = |kind: &str, value: &str, expires: &str| SpamNetEntry {
            kind: kind.into(),
            value: value.into(),
            score: 2.0,
            expires: expires.into(),
        };
        let later = "2999-01-01T00:00:00Z";
        store
            .apply_spamnet_list(
                true,
                vec![
                    entry("domain", "Spam.Example", later),
                    entry("ip", "192.0.2.1", "2000-01-01T00:00:00Z"),
                    entry("from", "x.example", "not a date"),
                ],
                vec![],
            )
            .await
            .unwrap();
        let wanted = || vec![("domain", "spam.example".to_owned()), ("ip", "192.0.2.1".to_owned())];
        let found = store.spamnet_matches(wanted()).await.unwrap();
        assert_eq!(
            found,
            [("domain".to_owned(), "spam.example".to_owned(), 2.0)],
            "expired and broken entries count not"
        );
        let removed = vec![SpamNetRemoved { kind: "domain".into(), value: "spam.example".into() }];
        store.apply_spamnet_list(false, vec![entry("digest", "d1:ab", later)], removed).await.unwrap();
        assert!(store.spamnet_matches(wanted()).await.unwrap().is_empty());
        store.prune_spamnet().await.unwrap();
        assert_eq!(store.spamnet_list_len().await.unwrap(), 1);
        store.apply_spamnet_list(true, vec![], vec![]).await.unwrap();
        assert_eq!(store.spamnet_list_len().await.unwrap(), 0, "a full list replaces everything");
    }
}
