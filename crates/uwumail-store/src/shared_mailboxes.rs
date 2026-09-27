//! Shared mailboxes: a mailbox several people use, such as support@ (docs/groups.md). It is an
//! account of its own that nobody signs in to; its members reach all of its folders from their
//! own accounts, the way a shared folder is reached (docs/sharing.md), and those who may send
//! answer with its address.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::acl::ACTIVE_PERSON;
use crate::address::{base_local_part, normalize_address};
use crate::directory::login_key;
use crate::identity_grants::{Granted, grant_identity, revoke_identities};
use crate::{ALL_RIGHTS, Account, NewAccount, Protocols, Result, Role, Store, StoreError, now};

/// How many people one shared mailbox may have.
const SHARED_MAILBOX_MAX_MEMBERS: usize = 500;

#[derive(Debug, Clone)]
pub struct NewSharedMailbox {
    pub address: String,
    pub name: String,
    /// 0 means unlimited.
    pub quota_bytes: i64,
    /// Logins of the members, and whether each may send with its address.
    pub members: Vec<(String, bool)>,
}

/// Someone who uses a shared mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedMailboxMember {
    pub id: i64,
    pub login: String,
    pub name: String,
    pub may_send: bool,
}

/// A shared mailbox as a member sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedMembership {
    pub id: i64,
    pub address: String,
    pub name: String,
    pub may_send: bool,
}

/// A shared mailbox with its members, for the admin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedMailboxInfo {
    pub login: String,
    pub name: String,
    pub members: Vec<SharedMailboxMember>,
}

fn members_of(conn: &Connection, account_id: i64) -> Result<Vec<SharedMailboxMember>> {
    let mut stmt = conn.prepare_cached(
        "SELECT a.id, a.login, a.display_name, s.may_send FROM shared_mailbox_members s
         JOIN accounts a ON a.id = s.member_id WHERE s.account_id = ?1 ORDER BY a.login",
    )?;
    let rows = stmt.query_map([account_id], |row| {
        Ok(SharedMailboxMember { id: row.get(0)?, login: row.get(1)?, name: row.get(2)?, may_send: row.get(3)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn addresses_of(conn: &Connection, account_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT a.local_part || '@' || d.name FROM addresses a JOIN domains d ON d.id = a.domain_id
         WHERE a.account_id = ?1",
    )?;
    let rows = stmt.query_map([account_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Replaces the members of a shared mailbox, and with them who has an identity for its addresses.
fn replace_members(
    tx: &rusqlite::Transaction<'_>,
    account_id: i64,
    members: &[(String, bool)],
    granted: &mut Granted,
) -> Result<()> {
    if members.len() > SHARED_MAILBOX_MAX_MEMBERS {
        return Err(StoreError::Invalid(format!(
            "a shared mailbox may have at most {SHARED_MAILBOX_MAX_MEMBERS} members"
        )));
    }
    let mut wanted: Vec<(i64, bool)> = Vec::new();
    for (login, may_send) in members {
        let key = login_key(login).map_err(|_| StoreError::NotFound(format!("account {login}")))?;
        // People only: a service or another shared mailbox has nobody to open it.
        let id: i64 = tx
            .query_row(&format!("SELECT id FROM accounts WHERE login = ?1 AND {ACTIVE_PERSON}"), [&key], |row| {
                row.get(0)
            })
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("account {login}")))?;
        match wanted.iter_mut().find(|(known, _)| *known == id) {
            Some(entry) => entry.1 |= may_send,
            None => wanted.push((id, *may_send)),
        }
    }
    let before: Vec<i64> = members_of(tx, account_id)?.into_iter().map(|member| member.id).collect();
    tx.execute("DELETE FROM shared_mailbox_members WHERE account_id = ?1", [account_id])?;
    for (member, may_send) in &wanted {
        tx.execute(
            "INSERT INTO shared_mailbox_members (account_id, member_id, rights, may_send, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![account_id, member, ALL_RIGHTS, may_send, now()],
        )?;
    }
    let name: String =
        tx.query_row("SELECT display_name FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))?;
    let addresses = addresses_of(tx, account_id)?;
    for member in before {
        for address in &addresses {
            revoke_identities(tx, member, address, granted)?;
        }
    }
    for (member, may_send) in &wanted {
        if *may_send {
            for address in &addresses {
                grant_identity(tx, *member, address, &name, granted)?;
            }
        }
    }
    // Members see the mailbox's folders appear or go: its state moves on for whoever watches it.
    let modseq = crate::db::next_modseq(tx, account_id)?;
    granted.push(account_id, modseq);
    Ok(())
}

/// Keeps the identities of a shared mailbox's members in step with its addresses: members who may
/// send for it get one for an address it gains, and everyone loses the one for an address it loses.
/// Does nothing for any other account.
pub(crate) fn address_changed(
    conn: &Connection,
    account_id: i64,
    address: &str,
    added: bool,
    granted: &mut Granted,
) -> Result<()> {
    let name: Option<String> = conn
        .query_row("SELECT display_name FROM accounts WHERE id = ?1 AND shared_mailbox = 1", [account_id], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(name) = name else { return Ok(()) };
    for member in members_of(conn, account_id)? {
        if added && member.may_send {
            grant_identity(conn, member.id, address, &name, granted)?;
        } else if !added {
            revoke_identities(conn, member.id, address, granted)?;
        }
    }
    Ok(())
}

/// The members of a shared mailbox with the addresses they may send as for it, to take the
/// identities away again when the mailbox goes. Empty for any other account.
pub(crate) fn sending_members(conn: &Connection, account_id: i64) -> Result<Vec<(i64, Vec<String>)>> {
    let shared: bool = conn
        .query_row("SELECT shared_mailbox FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
        .optional()?
        .unwrap_or(false);
    if !shared {
        return Ok(Vec::new());
    }
    let addresses = addresses_of(conn, account_id)?;
    Ok(members_of(conn, account_id)?
        .into_iter()
        .filter(|member| member.may_send)
        .map(|member| (member.id, addresses.clone()))
        .collect())
}

fn shared_account(conn: &Connection, login: &str) -> Result<i64> {
    let key = login_key(login)?;
    conn.query_row("SELECT id FROM accounts WHERE login = ?1 AND shared_mailbox = 1", [&key], |row| row.get(0))
        .optional()?
        .ok_or_else(|| StoreError::NotFound(format!("shared mailbox {login}")))
}

impl Store {
    /// Creates a shared mailbox: an account without a password of its own, with its members.
    pub async fn create_shared_mailbox(&self, new: NewSharedMailbox) -> Result<Account> {
        let account = self
            .create_account(NewAccount {
                address: new.address,
                display_name: new.name,
                password: None,
                role: Role::Service,
                quota_bytes: new.quota_bytes,
                protocols: Some(Protocols { smtp: true, imap: true, jmap: true, caldav: false, carddav: false }),
            })
            .await?;
        let id = account.id;
        let members = new.members;
        let result = self
            .write(move |tx| {
                tx.execute("UPDATE accounts SET shared_mailbox = 1 WHERE id = ?1", [id])?;
                let mut granted = Granted::default();
                replace_members(tx, id, &members, &mut granted)?;
                Ok(granted)
            })
            .await;
        match result {
            Ok(granted) => {
                self.notify_granted(granted);
                Ok(Account { shared_mailbox: true, ..account })
            }
            Err(err) => {
                // Nothing may stay of a shared mailbox whose members were refused.
                self.delete_account(&account.login).await?;
                Err(err)
            }
        }
    }

    /// Replaces the members of a shared mailbox.
    pub async fn set_shared_mailbox_members(
        &self,
        login: &str,
        members: Vec<(String, bool)>,
    ) -> Result<Vec<SharedMailboxMember>> {
        let login = login.to_owned();
        let (list, granted) = self
            .write(move |tx| {
                let id = shared_account(tx, &login)?;
                let mut granted = Granted::default();
                replace_members(tx, id, &members, &mut granted)?;
                Ok((members_of(tx, id)?, granted))
            })
            .await?;
        self.notify_granted(granted);
        Ok(list)
    }

    /// The members of a shared mailbox.
    pub async fn shared_mailbox_members(&self, account_id: i64) -> Result<Vec<SharedMailboxMember>> {
        self.read(move |conn| members_of(conn, account_id)).await
    }

    /// Every shared mailbox with its members.
    pub async fn shared_mailboxes(&self) -> Result<Vec<SharedMailboxInfo>> {
        self.read(|conn| {
            let accounts: Vec<(i64, String, String)> = conn
                .prepare(
                    "SELECT id, login, display_name FROM accounts
                     WHERE shared_mailbox = 1 AND deleted_at IS NULL ORDER BY login",
                )?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            accounts
                .into_iter()
                .map(|(id, login, name)| Ok(SharedMailboxInfo { login, name, members: members_of(conn, id)? }))
                .collect()
        })
        .await
    }

    /// The shared mailboxes a person uses.
    pub async fn shared_memberships(&self, member_id: i64) -> Result<Vec<SharedMembership>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT a.id, a.login, a.display_name, s.may_send FROM shared_mailbox_members s
                 JOIN accounts a ON a.id = s.account_id AND a.deleted_at IS NULL
                 WHERE s.member_id = ?1 ORDER BY a.login",
            )?;
            let rows = stmt.query_map([member_id], |row| {
                Ok(SharedMembership { id: row.get(0)?, address: row.get(1)?, name: row.get(2)?, may_send: row.get(3)? })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// The shared mailbox `address` belongs to, `+tag` or not, when `member_id` may send for it.
    pub async fn shared_mailbox_sending_as(&self, member_id: i64, address: &str) -> Result<Option<i64>> {
        let Ok((local, domain)) = normalize_address(address) else {
            return Ok(None);
        };
        self.read(move |conn| {
            let base = base_local_part(&local).to_owned();
            Ok(conn
                .query_row(
                    "SELECT s.account_id FROM shared_mailbox_members s
                     JOIN accounts acc ON acc.id = s.account_id AND acc.deleted_at IS NULL
                     JOIN addresses a ON a.account_id = s.account_id JOIN domains d ON d.id = a.domain_id
                     WHERE s.member_id = ?1 AND s.may_send = 1 AND d.name = ?2 AND a.local_part IN (?3, ?4)
                     LIMIT 1",
                    params![member_id, domain, local, base],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MailboxRole;
    use crate::test_support::store;

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

    #[tokio::test]
    async fn members_reach_every_folder_of_a_shared_mailbox() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let leni = person(&store, "leni@example.org").await;
        let nyu = person(&store, "nyu@example.org").await;
        let support = store
            .create_shared_mailbox(NewSharedMailbox {
                address: "support@example.org".into(),
                name: "Support".into(),
                quota_bytes: 0,
                members: vec![("mini@example.org".into(), true), ("leni@example.org".into(), false)],
            })
            .await
            .unwrap();
        assert!(support.shared_mailbox && support.is_service() && !support.can_log_in());
        assert_eq!(store.resolve_recipient("support@example.org").await.unwrap(), Some(support.id));

        // Every folder, a new one too, with every right.
        let folders = store.imap_mailboxes(support.id).await.unwrap().len();
        let shared = store.mailboxes_shared_with(leni).await.unwrap();
        assert_eq!(shared.len(), folders);
        assert!(shared.iter().all(|m| m.owner_id == support.id && m.rights == ALL_RIGHTS));
        store.create_mailbox(support.id, "Erledigt", None, None, 0, true).await.unwrap();
        assert_eq!(store.mailboxes_shared_with(leni).await.unwrap().len(), folders + 1);
        assert_eq!(store.sharing_owners(mini).await.unwrap(), vec![support.id]);
        assert!(store.mailboxes_shared_with(nyu).await.unwrap().is_empty());
        let inbox = shared.iter().find(|m| m.mailbox.role == Some(MailboxRole::Inbox)).unwrap().mailbox.id;
        assert!(store.shared_mailbox(mini, inbox).await.unwrap().is_some());

        // It is never someone one shares with, and it has no login of its own.
        assert!(!store.share_people().await.unwrap().iter().any(|p| p.id == support.id));
        let mini_inbox = store.imap_mailboxes(mini).await.unwrap()[0].id;
        assert!(store.set_mailbox_acl(mini, mini_inbox, "support@example.org", "lr").await.is_err());
        let app = store
            .create_app_password(
                support.id,
                crate::NewAppPassword { name: "x".into(), scopes: vec![crate::AppScope::Mail], expires_at: None },
            )
            .await;
        assert!(matches!(app, Err(StoreError::Rule { code: "sharedMailbox", .. })));
        let role = store.set_account_role("support@example.org", Role::User).await;
        assert!(matches!(role, Err(StoreError::Rule { code: "sharedMailbox", .. })));

        // Sending: only who may.
        assert!(store.account_owns_address(mini, "support@example.org").await.unwrap());
        assert!(!store.account_owns_address(leni, "support@example.org").await.unwrap());
        assert_eq!(store.shared_mailbox_sending_as(mini, "Support+x@example.org").await.unwrap(), Some(support.id));
        assert_eq!(store.shared_mailbox_sending_as(leni, "support@example.org").await.unwrap(), None);
        assert!(
            store
                .identities(mini)
                .await
                .unwrap()
                .iter()
                .any(|i| i.email == "support@example.org" && i.name == "Support")
        );
        assert_eq!(store.shared_memberships(leni).await.unwrap()[0].address, "support@example.org");

        // Only people can be members; members change, and so do their rights.
        let refused =
            store.set_shared_mailbox_members("support@example.org", vec![("support@example.org".into(), true)]).await;
        assert!(matches!(refused, Err(StoreError::NotFound(_))));
        store.set_shared_mailbox_members("support@example.org", vec![("leni@example.org".into(), true)]).await.unwrap();
        assert!(store.mailboxes_shared_with(mini).await.unwrap().is_empty());
        assert!(!store.account_owns_address(mini, "support@example.org").await.unwrap());
        assert!(!store.identities(mini).await.unwrap().iter().any(|i| i.email == "support@example.org"));
        assert!(store.account_owns_address(leni, "support@example.org").await.unwrap());
        assert_eq!(store.shared_mailboxes().await.unwrap()[0].members.len(), 1);

        // Its identities follow its addresses, and go with it.
        let emails = |list: Vec<crate::Identity>| list.into_iter().map(|i| i.email).collect::<Vec<_>>();
        store.add_alias("hilfe@example.org", "support@example.org").await.unwrap();
        assert!(emails(store.identities(leni).await.unwrap()).contains(&"hilfe@example.org".to_owned()));
        assert!(!emails(store.identities(mini).await.unwrap()).contains(&"hilfe@example.org".to_owned()));
        store.remove_alias("hilfe@example.org").await.unwrap();
        assert!(!emails(store.identities(leni).await.unwrap()).contains(&"hilfe@example.org".to_owned()));
        store.delete_account("support@example.org").await.unwrap();
        assert_eq!(emails(store.identities(leni).await.unwrap()), vec!["leni@example.org".to_owned()]);
        assert!(store.shared_mailboxes().await.unwrap().is_empty());
    }
}
