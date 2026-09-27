//! Shared mailboxes: a mailbox several people use, such as support@ (docs/groups.md). It is a
//! service with members: nobody signs in to the portal or the webmail as it, programs and mail apps
//! may reach it with app passwords, and its members reach all of its folders from their own
//! accounts, the way a shared folder is reached (docs/sharing.md). Those who may send answer with
//! its address. A person or a service can become one, and a shared mailbox can become a plain
//! service again.

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
    /// Creates a shared mailbox: a service without a password of its own, with its members.
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

    /// Turns a person or a service into a shared mailbox with these members. The mail, the folders
    /// and the addresses stay. A person becomes a service first, the way [`Store::update_account`]
    /// does it: their password turns into an app password, so their mail apps keep working, and
    /// everything of the portal goes. A service without a mailbox gets one.
    pub async fn make_shared_mailbox(&self, login: &str, members: Vec<(String, bool)>) -> Result<Account> {
        let login = login_key(login)?;
        let (account, granted) = self
            .write(move |tx| {
                let before = crate::admin::load_account(tx, &login)?;
                if before.deleted_at.is_some() {
                    return Err(StoreError::Invalid(format!("{login} is in the trash; restore it first")));
                }
                if before.shared_mailbox {
                    return Err(StoreError::Rule {
                        code: "sharedMailbox",
                        message: format!("{login} is a shared mailbox already"),
                    });
                }
                if members.iter().any(|(member, _)| login_key(member).is_ok_and(|member| member == login)) {
                    return Err(StoreError::Invalid("a shared mailbox cannot be its own member".into()));
                }
                crate::admin::keep_an_admin(tx, &before, false)?;
                let mut protocols = before.protocols;
                if !protocols.has_mailbox() {
                    protocols.imap = true;
                    protocols.jmap = true;
                }
                tx.execute(
                    "UPDATE accounts SET role = 'user', kind = 'service', shared_mailbox = 1, imap_enabled = ?1,
                            jmap_enabled = ?2 WHERE id = ?3",
                    params![protocols.imap, protocols.jmap, before.id],
                )?;
                let mailboxes: i64 =
                    tx.query_row("SELECT count(*) FROM mailboxes WHERE account_id = ?1", [before.id], |row| {
                        row.get(0)
                    })?;
                if mailboxes == 0 {
                    crate::mail::create_default_mailboxes(tx, before.id)?;
                }
                let after = crate::admin::load_account(tx, &login)?;
                let mut granted = Granted::default();
                if before.is_service() {
                    crate::admin::leave_sharing(tx, after.id, &mut granted)?;
                } else {
                    crate::admin::become_service(tx, &after, &mut granted)?;
                }
                replace_members(tx, after.id, &members, &mut granted)?;
                Ok((after, granted))
            })
            .await?;
        self.notify_granted(granted);
        Ok(account)
    }

    /// Turns a shared mailbox back into a plain service: its members lose its folders and the
    /// identities for its addresses. Its mail, addresses and app passwords stay.
    pub async fn end_shared_mailbox(&self, login: &str) -> Result<Account> {
        let login = login_key(login)?;
        let (account, granted) = self
            .write(move |tx| {
                let id = shared_account(tx, &login)?;
                let mut granted = Granted::default();
                replace_members(tx, id, &[], &mut granted)?;
                tx.execute("UPDATE accounts SET shared_mailbox = 0 WHERE id = ?1", [id])?;
                Ok((crate::admin::load_account(tx, &login)?, granted))
            })
            .await?;
        self.notify_granted(granted);
        Ok(account)
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

    /// A person who leaves and whose mailbox becomes a shared one used to keep what they set up for
    /// themselves: their forwarding went on sending the mailbox's mail to their private address
    /// (security audit 0.16.0 STORE-3).
    #[tokio::test]
    async fn what_a_person_set_up_for_their_own_mail_stops_when_it_becomes_a_shared_mailbox() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        person(&store, "mini@example.org").await;
        let leaver = person(&store, "leaver@example.org").await;
        let (script, _) = store.put_sieve_script(leaver, "rules", b"keep;").await.unwrap();
        store.activate_sieve_script(leaver, Some(script.id)).await.unwrap();
        store
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO forward_targets (account_id, address, created_at, confirmed_at)
                     VALUES (?1, 'private@example.net', 0, 0)",
                    [leaver],
                )?;
                tx.execute("UPDATE accounts SET forward_keep_copy = 0 WHERE id = ?1", [leaver])?;
                tx.execute(
                    "INSERT INTO fetch_accounts (account_id, address, host, username, password, created_at)
                     VALUES (?1, 'private@example.net', 'imap.example.net', 'private', x'00', 0)",
                    [leaver],
                )?;
                tx.execute(
                    "INSERT INTO migration_jobs (account_id, address, host, login, password_sealed, created_at)
                     VALUES (?1, 'old@example.net', 'imap.example.net', 'old', x'00', 0)",
                    [leaver],
                )?;
                tx.execute(
                    "INSERT INTO dav_collections (id, account_id, kind, slug, created_at)
                     VALUES (4242, ?1, 'calendar', 'feed', 0)",
                    [leaver],
                )?;
                tx.execute(
                    "INSERT INTO calendar_subscriptions (account_id, collection_id, url, url_digest, url_shown, created_at)
                     VALUES (?1, 4242, x'00', 'd', 'calendar.example.net', 0)",
                    [leaver],
                )?;
                let domain: i64 = tx.query_row("SELECT id FROM domains WHERE name = 'example.org'", [], |r| r.get(0))?;
                tx.execute(
                    "INSERT INTO masked_addresses (account_id, local_part, domain_id, state, created_at)
                     VALUES (?1, 'shop.x7', ?2, 'enabled', 0), (?1, 'news.k2', ?2, 'deleted', 0)",
                    params![leaver, domain],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        store.make_shared_mailbox("leaver@example.org", vec![("mini@example.org".into(), true)]).await.unwrap();

        let left = store
            .read(move |conn| {
                let count = |sql: &str| conn.query_row(sql, [leaver], |row| row.get::<_, i64>(0));
                Ok((
                    count("SELECT count(*) FROM forward_targets WHERE account_id = ?1")?,
                    count("SELECT forward_keep_copy FROM accounts WHERE id = ?1")?,
                    count("SELECT count(*) FROM fetch_accounts WHERE account_id = ?1")?,
                    count("SELECT count(*) FROM migration_jobs WHERE account_id = ?1")?,
                    count("SELECT count(*) FROM calendar_subscriptions WHERE account_id = ?1 AND enabled")?,
                    count("SELECT count(*) FROM calendar_subscriptions WHERE account_id = ?1")?,
                    count("SELECT count(*) FROM sieve_scripts WHERE account_id = ?1")?,
                ))
            })
            .await
            .unwrap();
        // Forwards, keep a copy, fetched mailboxes, moves, subscriptions on, subscriptions, scripts.
        assert_eq!(left, (0, 1, 0, 0, 0, 1, 1));
        assert!(store.active_sieve_script(leaver).await.unwrap().is_none(), "the script stays, switched off");
        let states: String = store
            .read(move |conn| {
                Ok(conn.query_row(
                    "SELECT group_concat(state, ' ') FROM (SELECT state FROM masked_addresses WHERE account_id = ?1 ORDER BY local_part)",
                    [leaver],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(states, "deleted disabled", "masked addresses stay the mailbox's, switched off");
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
        assert!(support.shared_mailbox && support.is_service() && support.can_log_in() && !support.can_use_portal());
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

        // It is never someone one shares with. Like any service it has no password, but app
        // passwords open it to programs and mail apps.
        assert!(!store.share_people().await.unwrap().iter().any(|p| p.id == support.id));
        let mini_inbox = store.imap_mailboxes(mini).await.unwrap()[0].id;
        assert!(store.set_mailbox_acl(mini, mini_inbox, "support@example.org", "lr").await.is_err());
        let app = store
            .create_app_password(
                support.id,
                crate::NewAppPassword { name: "x".into(), scopes: vec![crate::AppScope::Mail], expires_at: None },
            )
            .await
            .unwrap();
        let login =
            store.authenticate_mail("support@example.org", &app.secret, crate::AppScope::Mail, "imap", "").await;
        assert!(matches!(login.unwrap(), crate::MailAuth::Ok { account, .. } if account.id == support.id));
        let no_mailbox = crate::AccountUpdate {
            protocols: Some(Protocols { smtp: true, imap: false, jmap: false, caldav: false, carddav: false }),
            ..Default::default()
        };
        let refused = store.update_account("support@example.org", no_mailbox).await;
        assert!(matches!(refused, Err(StoreError::Rule { code: "sharedMailboxNeedsMailbox", .. })), "{refused:?}");
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

    #[tokio::test]
    async fn a_person_becomes_a_shared_mailbox_and_a_service_again() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let leni = person(&store, "leni@example.org").await;
        let info = store
            .create_account(NewAccount {
                address: "info@example.org".into(),
                display_name: "Info".into(),
                password: Some("katzenpfote-123".into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
        store.add_alias("hallo@example.org", "info@example.org").await.unwrap();
        let inbox = store.imap_mailboxes(info.id).await.unwrap()[0].id;
        // As a person it had a folder of leni's and a shared mailbox of its own to use.
        let leni_inbox = store.imap_mailboxes(leni).await.unwrap()[0].id;
        store.set_mailbox_acl(leni, leni_inbox, "info@example.org", "lr").await.unwrap();
        store
            .create_shared_mailbox(NewSharedMailbox {
                address: "support@example.org".into(),
                name: "Support".into(),
                quota_bytes: 0,
                members: vec![("info@example.org".into(), true)],
            })
            .await
            .unwrap();
        assert!(store.identities(info.id).await.unwrap().iter().any(|i| i.email == "support@example.org"));

        // Not a member of itself.
        let itself = store.make_shared_mailbox("info@example.org", vec![("INFO@example.org".into(), true)]).await;
        assert!(matches!(itself, Err(StoreError::Invalid(_))), "{itself:?}");
        assert!(!store.account_by_id(info.id).await.unwrap().unwrap().shared_mailbox, "nothing changed");

        let shared = store
            .make_shared_mailbox(
                "info@example.org",
                vec![("mini@example.org".into(), true), ("leni@example.org".into(), false)],
            )
            .await
            .unwrap();
        assert!(shared.shared_mailbox && shared.is_service() && !shared.can_use_portal());
        // Its folders are the members' now, the mail stays where it was.
        assert!(store.shared_mailbox(mini, inbox).await.unwrap().is_some());
        assert!(store.shared_mailbox(leni, inbox).await.unwrap().is_some());
        let emails = |list: Vec<crate::Identity>| list.into_iter().map(|i| i.email).collect::<Vec<_>>();
        let mini_emails = emails(store.identities(mini).await.unwrap());
        assert!(
            mini_emails.contains(&"info@example.org".to_owned())
                && mini_emails.contains(&"hallo@example.org".to_owned())
        );
        assert!(!emails(store.identities(leni).await.unwrap()).contains(&"info@example.org".to_owned()));
        // The password it had opens its mail apps as an app password, never the portal.
        assert!(store.authenticate("info@example.org", "katzenpfote-123").await.unwrap().is_none());
        let login =
            store.authenticate_mail("info@example.org", "katzenpfote-123", crate::AppScope::Mail, "imap", "").await;
        assert!(matches!(login.unwrap(), crate::MailAuth::Ok { app_password: Some(_), .. }));
        // What it used as a person is gone: shares, the other shared mailbox, its identity there.
        assert!(store.mailboxes_shared_with(info.id).await.unwrap().is_empty());
        assert!(store.shared_memberships(info.id).await.unwrap().is_empty());
        assert!(!emails(store.identities(info.id).await.unwrap()).contains(&"support@example.org".to_owned()));
        assert!(store.mailbox_acl(leni, leni_inbox).await.unwrap().is_empty());
        let again = store.make_shared_mailbox("info@example.org", Vec::new()).await;
        assert!(matches!(again, Err(StoreError::Rule { code: "sharedMailbox", .. })));

        // Back to a plain service: the members lose it, the app password stays.
        let service = store.end_shared_mailbox("info@example.org").await.unwrap();
        assert!(!service.shared_mailbox && service.is_service());
        assert!(store.mailboxes_shared_with(mini).await.unwrap().is_empty());
        assert!(!emails(store.identities(mini).await.unwrap()).contains(&"info@example.org".to_owned()));
        assert_eq!(store.app_passwords(service.id).await.unwrap().len(), 1);
        assert!(matches!(store.end_shared_mailbox("info@example.org").await, Err(StoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn a_service_becomes_a_shared_mailbox_with_its_app_passwords() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        // A service that only sends has no mailbox yet; it gets one.
        let scanner = store
            .create_account(NewAccount {
                address: "scanner@example.org".into(),
                display_name: "Scanner".into(),
                password: None,
                role: Role::Service,
                quota_bytes: 0,
                protocols: Some(Protocols { smtp: true, imap: false, jmap: false, caldav: false, carddav: false }),
            })
            .await
            .unwrap();
        let app = store
            .create_app_password(
                scanner.id,
                crate::NewAppPassword { name: "Scanner".into(), scopes: vec![crate::AppScope::Smtp], expires_at: None },
            )
            .await
            .unwrap();
        store
            .create_group(crate::NewGroup {
                address: "alle@example.org".into(),
                name: "Alle".into(),
                who_may_send: crate::WhoMaySend::Anyone,
                members_may_send_as: false,
                members: vec!["scanner@example.org".into()],
            })
            .await
            .unwrap();

        let shared =
            store.make_shared_mailbox("scanner@example.org", vec![("mini@example.org".into(), true)]).await.unwrap();
        assert!(shared.shared_mailbox && shared.has_mailbox() && shared.protocols.smtp);
        assert!(!store.imap_mailboxes(shared.id).await.unwrap().is_empty());
        assert_eq!(
            store.mailboxes_shared_with(mini).await.unwrap().len(),
            store.imap_mailboxes(shared.id).await.unwrap().len()
        );
        let login =
            store.authenticate_mail("scanner@example.org", &app.secret, crate::AppScope::Smtp, "smtp", "").await;
        assert!(matches!(login.unwrap(), crate::MailAuth::Ok { .. }), "its programs keep sending");
        let group = store.group_delivery("alle@example.org").await.unwrap().unwrap();
        assert_eq!(group.members, vec![shared.id], "it stays in its groups");
    }
}
