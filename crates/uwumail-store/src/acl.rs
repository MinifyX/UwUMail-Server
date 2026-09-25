//! Folders shared between people on this server: who may use someone else's mailbox, and how
//! (docs/sharing.md).
//!
//! Rights are the letters of RFC 4314, kept in their usual order:
//!
//! | Letter | Right |
//! | --- | --- |
//! | `l` | see the mailbox in lists |
//! | `r` | open it and read its messages |
//! | `s` | keep the seen flag (`\Seen`, `$seen`) |
//! | `w` | change the other flags and keywords |
//! | `i` | add messages (APPEND, COPY into it) |
//! | `p` | send to it (kept for completeness, nothing on this server needs it) |
//! | `k` | create mailboxes inside it |
//! | `x` | delete or rename it |
//! | `t` | flag messages as deleted |
//! | `e` | expunge them |
//! | `a` | administer: share it on with others |
//!
//! The owner is never listed: they have every right on their own mailboxes. The mail of a shared
//! mailbox stays the owner's, in the owner's quota, whoever puts it there. Flags, `\Seen` among
//! them, are the email's and so the same for everyone who sees it.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::db::{next_modseq, record_change};
use crate::directory::login_key;
use crate::imap::ImapMailbox;
use crate::mail::MailboxRole;
use crate::{Result, Store, StoreError};

/// Every right, in the order RFC 4314 lists them.
pub const ALL_RIGHTS: &str = "lrswipkxtea";

/// How much someone may do with a shared mailbox, as the portal and the webmail offer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareLevel {
    /// See the messages.
    Read,
    /// Also mark them, file new ones in, and delete them.
    Write,
    /// Everything the owner can do, sharing it on included.
    All,
}

impl ShareLevel {
    pub fn rights(self) -> &'static str {
        match self {
            ShareLevel::Read => "lr",
            ShareLevel::Write => "lrswipte",
            ShareLevel::All => ALL_RIGHTS,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ShareLevel::Read => "read",
            ShareLevel::Write => "write",
            ShareLevel::All => "all",
        }
    }

    pub fn parse(value: &str) -> Option<ShareLevel> {
        match value {
            "read" => Some(ShareLevel::Read),
            "write" => Some(ShareLevel::Write),
            "all" => Some(ShareLevel::All),
            _ => None,
        }
    }

    /// The level closest to a set of rights set some other way (IMAP SETACL, JMAP).
    pub fn of(rights: &str) -> ShareLevel {
        if rights.contains('a') {
            ShareLevel::All
        } else if rights.chars().any(|c| "switexk".contains(c)) {
            ShareLevel::Write
        } else {
            ShareLevel::Read
        }
    }
}

/// Rights letters in their usual order without repeats. The obsolete `c` of RFC 2086 stands for
/// `k` and `d` for `xte`, as RFC 4314 asks. Anything else is refused.
pub fn normalize_rights(input: &str) -> Result<String> {
    let mut wanted = String::new();
    for c in input.chars() {
        match c {
            'c' => wanted.push('k'),
            'd' => wanted.push_str("xte"),
            c if ALL_RIGHTS.contains(c) => wanted.push(c),
            other => return Err(StoreError::Invalid(format!("unknown right {other:?}"))),
        }
    }
    Ok(ALL_RIGHTS.chars().filter(|c| wanted.contains(*c)).collect())
}

/// Whether a set of rights holds every letter of `needed`.
pub fn has_rights(rights: &str, needed: &str) -> bool {
    needed.chars().all(|c| rights.contains(c))
}

/// One person a mailbox is shared with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AclEntry {
    pub mailbox_id: i64,
    pub grantee_id: i64,
    pub grantee_login: String,
    pub grantee_name: String,
    pub rights: String,
}

/// Someone else's mailbox the account may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedMailbox {
    pub owner_id: i64,
    pub owner_login: String,
    pub owner_name: String,
    pub mailbox: ImapMailbox,
    pub rights: String,
}

/// A person on this server, for choosing whom to share with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharePerson {
    pub id: i64,
    pub login: String,
    pub display_name: String,
}

/// Accounts that take part in sharing: people, not services, and not in the trash.
const ACTIVE_PERSON: &str = "kind <> 'service' AND deleted_at IS NULL";

fn owned_mailbox(conn: &Connection, owner_id: i64, mailbox_id: i64) -> Result<()> {
    conn.query_row("SELECT 1 FROM mailboxes WHERE id = ?1 AND account_id = ?2", params![mailbox_id, owner_id], |_| {
        Ok(())
    })
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("mailbox {mailbox_id}")))
}

fn entry_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AclEntry> {
    Ok(AclEntry {
        mailbox_id: row.get(0)?,
        grantee_id: row.get(1)?,
        grantee_login: row.get(2)?,
        grantee_name: row.get(3)?,
        rights: row.get(4)?,
    })
}

const ENTRY_QUERY: &str = "SELECT acl.mailbox_id, acl.grantee_id, g.login, g.display_name, acl.rights
     FROM mailbox_acl acl JOIN accounts g ON g.id = acl.grantee_id";

const SHARED_QUERY: &str = "SELECT o.id, o.login, o.display_name, m.id, m.parent_id, m.name, m.role, m.subscribed,
            m.uid_validity, m.uid_next, acl.rights
     FROM mailbox_acl acl
     JOIN mailboxes m ON m.id = acl.mailbox_id
     JOIN accounts o ON o.id = acl.owner_id AND o.deleted_at IS NULL
     JOIN accounts g ON g.id = acl.grantee_id AND g.deleted_at IS NULL";

fn shared_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SharedMailbox> {
    Ok(SharedMailbox {
        owner_id: row.get(0)?,
        owner_login: row.get(1)?,
        owner_name: row.get(2)?,
        mailbox: ImapMailbox {
            id: row.get(3)?,
            parent_id: row.get(4)?,
            name: row.get(5)?,
            role: row.get::<_, Option<String>>(6)?.as_deref().and_then(MailboxRole::parse),
            subscribed: row.get(7)?,
            uid_validity: row.get::<_, i64>(8)?.clamp(1, u32::MAX as i64) as u32,
            uid_next: row.get::<_, i64>(9)?.clamp(1, u32::MAX as i64) as u32,
        },
        rights: row.get(10)?,
    })
}

impl Store {
    /// Who a mailbox is shared with, by login.
    pub async fn mailbox_acl(&self, owner_id: i64, mailbox_id: i64) -> Result<Vec<AclEntry>> {
        self.read(move |conn| {
            owned_mailbox(conn, owner_id, mailbox_id)?;
            let mut stmt = conn.prepare(&format!("{ENTRY_QUERY} WHERE acl.mailbox_id = ?1 ORDER BY g.login"))?;
            let rows = stmt.query_map([mailbox_id], entry_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Everything an account shares with others, all mailboxes together.
    pub async fn shares_by_owner(&self, owner_id: i64) -> Result<Vec<AclEntry>> {
        self.read(move |conn| {
            let mut stmt =
                conn.prepare(&format!("{ENTRY_QUERY} WHERE acl.owner_id = ?1 ORDER BY acl.mailbox_id, g.login"))?;
            let rows = stmt.query_map([owner_id], entry_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Shares a mailbox with someone on this server, by their login, or takes it back when
    /// `rights` is empty. Returns the rights as stored.
    pub async fn set_mailbox_acl(
        &self,
        owner_id: i64,
        mailbox_id: i64,
        grantee_login: &str,
        rights: &str,
    ) -> Result<String> {
        let login = login_key(grantee_login).map_err(|_| StoreError::NotFound(format!("account {grantee_login}")))?;
        let grantee = self
            .read(move |conn| {
                Ok(conn
                    .query_row(
                        &format!("SELECT id FROM accounts WHERE login = ?1 AND {ACTIVE_PERSON}"),
                        [&login],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?)
            })
            .await?
            .ok_or_else(|| StoreError::NotFound(format!("account {grantee_login}")))?;
        self.set_mailbox_acl_for(owner_id, mailbox_id, grantee, rights).await
    }

    /// The same by account id.
    pub async fn set_mailbox_acl_for(
        &self,
        owner_id: i64,
        mailbox_id: i64,
        grantee_id: i64,
        rights: &str,
    ) -> Result<String> {
        let rights = normalize_rights(rights)?;
        if grantee_id == owner_id {
            return Err(StoreError::Rule {
                code: "shareWithOwner",
                message: "The owner always has every right on their own mailboxes".into(),
            });
        }
        let stored = rights.clone();
        let modseq = self
            .write(move |tx| {
                owned_mailbox(tx, owner_id, mailbox_id)?;
                let person: bool = tx
                    .query_row(
                        &format!("SELECT EXISTS (SELECT 1 FROM accounts WHERE id = ?1 AND {ACTIVE_PERSON})"),
                        [grantee_id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .unwrap_or(false);
                if !person {
                    return Err(StoreError::NotFound(format!("account {grantee_id}")));
                }
                let changed = if rights.is_empty() {
                    tx.execute(
                        "DELETE FROM mailbox_acl WHERE mailbox_id = ?1 AND grantee_id = ?2",
                        params![mailbox_id, grantee_id],
                    )? > 0
                } else {
                    let now = crate::now();
                    tx.execute(
                        "INSERT INTO mailbox_acl (mailbox_id, owner_id, grantee_id, rights, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                         ON CONFLICT (mailbox_id, grantee_id) DO UPDATE SET rights = excluded.rights,
                             updated_at = excluded.updated_at WHERE rights <> excluded.rights",
                        params![mailbox_id, owner_id, grantee_id, rights, now],
                    )? > 0
                };
                if !changed {
                    return Ok(None);
                }
                // The mailbox's rights and sharing changed: its JMAP state moves on, and whoever
                // watches the owner's account (the grantees too) hears of it.
                let modseq = next_modseq(tx, owner_id)?;
                record_change(tx, owner_id, modseq, "Mailbox", mailbox_id, "updated")?;
                Ok(Some(modseq))
            })
            .await?;
        if let Some(modseq) = modseq {
            self.notify_change(owner_id, modseq);
        }
        Ok(stored)
    }

    /// Everyone else's mailboxes an account may use, owners by login, then the mailbox order.
    pub async fn mailboxes_shared_with(&self, grantee_id: i64) -> Result<Vec<SharedMailbox>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "{SHARED_QUERY} WHERE acl.grantee_id = ?1 ORDER BY o.login, m.sort_order, m.name, m.id"
            ))?;
            let rows = stmt.query_map([grantee_id], shared_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// One mailbox of someone else, if it is shared with the account.
    pub async fn shared_mailbox(&self, grantee_id: i64, mailbox_id: i64) -> Result<Option<SharedMailbox>> {
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("{SHARED_QUERY} WHERE acl.grantee_id = ?1 AND acl.mailbox_id = ?2"),
                    params![grantee_id, mailbox_id],
                    shared_row,
                )
                .optional()?)
        })
        .await
    }

    /// Accounts that share at least one mailbox with this one.
    pub async fn sharing_owners(&self, grantee_id: i64) -> Result<Vec<i64>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT DISTINCT o.id FROM mailbox_acl acl
                 JOIN accounts o ON o.id = acl.owner_id AND o.deleted_at IS NULL
                 WHERE acl.grantee_id = ?1 ORDER BY o.id"
            ))?;
            let rows = stmt.query_map([grantee_id], |row| row.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// People on this server one may share with: everyone but services and accounts in the trash,
    /// by login.
    pub async fn share_people(&self) -> Result<Vec<SharePerson>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT id, login, display_name FROM accounts WHERE {ACTIVE_PERSON} ORDER BY login"
            ))?;
            let rows = stmt.query_map([], |row| {
                Ok(SharePerson { id: row.get(0)?, login: row.get(1)?, display_name: row.get(2)? })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, Role};

    async fn person(store: &Store, address: &str) -> i64 {
        store
            .create_account(NewAccount {
                address: address.into(),
                display_name: address.split('@').next().unwrap_or_default().into(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id
    }

    async fn inbox(store: &Store, account: i64) -> i64 {
        store
            .imap_mailboxes(account)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.role == Some(MailboxRole::Inbox))
            .unwrap()
            .id
    }

    #[test]
    fn rights_are_normalized() {
        assert_eq!(normalize_rights("rl").unwrap(), "lr");
        assert_eq!(normalize_rights("lrcd").unwrap(), "lrkxte");
        assert_eq!(normalize_rights("").unwrap(), "");
        assert!(normalize_rights("lrz").is_err());
        assert_eq!(ShareLevel::of("lr"), ShareLevel::Read);
        assert_eq!(ShareLevel::of(ShareLevel::Write.rights()), ShareLevel::Write);
        assert_eq!(ShareLevel::of(ALL_RIGHTS), ShareLevel::All);
        assert!(has_rights("lrswi", "ri") && !has_rights("lr", "i"));
    }

    #[tokio::test]
    async fn sharing_a_mailbox_and_taking_it_back() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let leni = person(&store, "leni@example.org").await;
        let inbox = inbox(&store, mini).await;
        let before = store.account_modseq(mini).await.unwrap();

        let stored = store.set_mailbox_acl(mini, inbox, "Leni@Example.org", "rl").await.unwrap();
        assert_eq!(stored, "lr");
        assert!(store.account_modseq(mini).await.unwrap() > before, "the owner's state moves on");
        let acl = store.mailbox_acl(mini, inbox).await.unwrap();
        assert_eq!(acl.len(), 1);
        assert_eq!((acl[0].grantee_login.as_str(), acl[0].rights.as_str()), ("leni@example.org", "lr"));

        let shared = store.mailboxes_shared_with(leni).await.unwrap();
        assert_eq!(shared.len(), 1);
        assert_eq!((shared[0].owner_id, shared[0].mailbox.id), (mini, inbox));
        assert_eq!(store.sharing_owners(leni).await.unwrap(), vec![mini]);
        assert!(store.shared_mailbox(leni, inbox).await.unwrap().is_some());
        assert!(store.mailboxes_shared_with(mini).await.unwrap().is_empty());

        // Someone else's mailbox cannot be shared, nor with oneself or a stranger.
        assert!(matches!(
            store.set_mailbox_acl(leni, inbox, "mini@example.org", "lr").await,
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.set_mailbox_acl(mini, inbox, "mini@example.org", "lr").await,
            Err(StoreError::Rule { .. })
        ));
        assert!(matches!(
            store.set_mailbox_acl(mini, inbox, "nobody@example.org", "lr").await,
            Err(StoreError::NotFound(_))
        ));

        store.set_mailbox_acl(mini, inbox, "leni@example.org", "").await.unwrap();
        assert!(store.mailboxes_shared_with(leni).await.unwrap().is_empty());
        assert!(store.shares_by_owner(mini).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn deleting_an_account_or_mailbox_removes_its_shares() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let leni = person(&store, "leni@example.org").await;
        let nyu = person(&store, "nyu@example.org").await;
        let folder = store.create_mailbox(mini, "Projekte", None, None, 0, true).await.unwrap();
        let inbox = inbox(&store, mini).await;
        store.set_mailbox_acl(mini, folder, "leni@example.org", ShareLevel::Write.rights()).await.unwrap();
        store.set_mailbox_acl(mini, inbox, "nyu@example.org", ShareLevel::Read.rights()).await.unwrap();
        store.set_mailbox_acl(nyu, inbox(&store, nyu).await, "mini@example.org", "lr").await.unwrap();

        store.destroy_mailbox(mini, folder, true).await.unwrap();
        assert!(store.mailboxes_shared_with(leni).await.unwrap().is_empty());

        store.delete_account("nyu@example.org").await.unwrap();
        assert!(store.shares_by_owner(mini).await.unwrap().is_empty(), "shares with the deleted account are gone");
        assert!(store.mailboxes_shared_with(mini).await.unwrap().is_empty(), "and its own shares too");
    }
}
