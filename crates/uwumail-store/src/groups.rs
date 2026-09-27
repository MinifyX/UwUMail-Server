//! Groups: an address of one of our domains that delivers to several people here, such as info@
//! or vorstand@ of a club (docs/groups.md). A group has no mailbox of its own; every member gets
//! the message the way mail for their own address arrives.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::address::{base_local_part, normalize_address};
use crate::directory::{domain_id, login_key};
use crate::identity_grants::{Granted, grant_identity, revoke_identities};
use crate::{Result, Store, StoreError, now};

/// How many people one group may have.
pub const GROUP_MAX_MEMBERS: usize = 500;
const GROUP_NAME_MAX_CHARS: usize = 200;

/// Who may write to a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WhoMaySend {
    /// Everyone, here and elsewhere.
    Anyone,
    /// Only its members, from one of their addresses.
    Members,
    /// Only addresses of the group's own domain.
    Domain,
}

impl WhoMaySend {
    pub fn as_str(self) -> &'static str {
        match self {
            WhoMaySend::Anyone => "anyone",
            WhoMaySend::Members => "members",
            WhoMaySend::Domain => "domain",
        }
    }

    pub fn parse(value: &str) -> Option<WhoMaySend> {
        match value {
            "anyone" => Some(WhoMaySend::Anyone),
            "members" => Some(WhoMaySend::Members),
            "domain" => Some(WhoMaySend::Domain),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupMember {
    pub id: i64,
    pub login: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: i64,
    pub address: String,
    pub domain: String,
    pub name: String,
    pub who_may_send: WhoMaySend,
    pub members_may_send_as: bool,
    pub members: Vec<GroupMember>,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewGroup {
    pub address: String,
    pub name: String,
    pub who_may_send: WhoMaySend,
    pub members_may_send_as: bool,
    /// Logins of the members.
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct GroupUpdate {
    pub name: Option<String>,
    pub who_may_send: Option<WhoMaySend>,
    pub members_may_send_as: Option<bool>,
    /// Replaces the members, by login.
    pub members: Option<Vec<String>>,
}

/// What delivering to a group needs to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDelivery {
    pub id: i64,
    /// The group's own address, without a `+tag`.
    pub address: String,
    pub who_may_send: WhoMaySend,
    /// Accounts of the members that are not in the trash.
    pub members: Vec<i64>,
}

/// The group that takes mail for `local`@`domain`: the address itself, or its base without a
/// `+tag`.
pub(crate) fn find(conn: &Connection, local: &str, domain_id: i64) -> Result<Option<i64>> {
    let lookup = |local: &str| -> Result<Option<i64>> {
        Ok(conn
            .query_row(
                "SELECT id FROM groups WHERE local_part = ?1 AND domain_id = ?2",
                params![local, domain_id],
                |row| row.get(0),
            )
            .optional()?)
    };
    if let Some(id) = lookup(local)? {
        return Ok(Some(id));
    }
    let base = base_local_part(local);
    if base != local { lookup(base) } else { Ok(None) }
}

fn load(conn: &Connection, filter: &str, value: &dyn rusqlite::ToSql) -> Result<Vec<Group>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT g.id, g.local_part || '@' || d.name, d.name, g.name, g.who_may_send, g.members_may_send_as,
                g.created_at
         FROM groups g JOIN domains d ON d.id = g.domain_id WHERE {filter} ORDER BY d.name, g.local_part"
    ))?;
    let mut groups = stmt
        .query_map([value], |row| {
            Ok(Group {
                id: row.get(0)?,
                address: row.get(1)?,
                domain: row.get(2)?,
                name: row.get(3)?,
                who_may_send: WhoMaySend::parse(&row.get::<_, String>(4)?).unwrap_or(WhoMaySend::Anyone),
                members_may_send_as: row.get(5)?,
                members: Vec::new(),
                created_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut stmt = conn.prepare_cached(
        "SELECT a.id, a.login, a.display_name FROM group_members m JOIN accounts a ON a.id = m.account_id
         WHERE m.group_id = ?1 ORDER BY a.login",
    )?;
    for group in &mut groups {
        group.members = stmt
            .query_map([group.id], |row| Ok(GroupMember { id: row.get(0)?, login: row.get(1)?, name: row.get(2)? }))?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(groups)
}

fn load_one(conn: &Connection, id: i64) -> Result<Group> {
    load(conn, "g.id = ?1", &id)?.pop().ok_or_else(|| StoreError::NotFound(format!("group {id}")))
}

fn group_by_address(conn: &Connection, local: &str, domain: &str) -> Result<i64> {
    conn.query_row(
        "SELECT g.id FROM groups g JOIN domains d ON d.id = g.domain_id WHERE g.local_part = ?1 AND d.name = ?2",
        params![local, domain],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("group {local}@{domain}")))
}

fn check_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.chars().count() > GROUP_NAME_MAX_CHARS {
        return Err(StoreError::Invalid(format!("a name may have at most {GROUP_NAME_MAX_CHARS} characters")));
    }
    Ok(name.to_owned())
}

/// Account ids for member logins, each once. Accounts in the trash cannot join.
fn member_ids(conn: &Connection, logins: &[String]) -> Result<Vec<i64>> {
    if logins.len() > GROUP_MAX_MEMBERS {
        return Err(StoreError::Invalid(format!("a group may have at most {GROUP_MAX_MEMBERS} members")));
    }
    let mut ids = Vec::new();
    for login in logins {
        let key = login_key(login).map_err(|_| StoreError::NotFound(format!("account {login}")))?;
        let id: i64 = conn
            .query_row("SELECT id FROM accounts WHERE login = ?1 AND deleted_at IS NULL", [&key], |row| row.get(0))
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("account {login}")))?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Replaces the members of a group and keeps the identities of those who may send as it in step.
fn set_members(
    tx: &rusqlite::Transaction<'_>,
    group: i64,
    address: &str,
    name: &str,
    members: Option<&[i64]>,
    send_as: bool,
    granted: &mut Granted,
) -> Result<()> {
    let before: Vec<i64> = tx
        .prepare("SELECT account_id FROM group_members WHERE group_id = ?1")?
        .query_map([group], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let after = members.map(<[i64]>::to_vec).unwrap_or_else(|| before.clone());
    if members.is_some() {
        tx.execute("DELETE FROM group_members WHERE group_id = ?1", [group])?;
        for account in &after {
            tx.execute("INSERT INTO group_members (group_id, account_id) VALUES (?1, ?2)", params![group, account])?;
        }
    }
    for account in before.iter().filter(|account| !after.contains(account) || !send_as) {
        revoke_identities(tx, *account, address, granted)?;
    }
    if send_as {
        for account in &after {
            grant_identity(tx, *account, address, name, granted)?;
        }
    }
    Ok(())
}

impl Store {
    /// The groups of one domain, or of all of them.
    pub async fn groups(&self, domain: Option<String>) -> Result<Vec<Group>> {
        self.read(move |conn| match domain {
            Some(domain) => load(conn, "d.name = ?1", &domain),
            None => load(conn, "?1", &1),
        })
        .await
    }

    /// The groups an account is a member of.
    pub async fn account_groups(&self, account_id: i64) -> Result<Vec<Group>> {
        self.read(move |conn| {
            load(conn, "g.id IN (SELECT group_id FROM group_members WHERE account_id = ?1)", &account_id)
        })
        .await
    }

    pub async fn create_group(&self, new: NewGroup) -> Result<Group> {
        let (local, domain) = normalize_address(&new.address)?;
        if local.contains('+') {
            return Err(StoreError::Rule { code: "aliasInvalid", message: "an address cannot contain +".into() });
        }
        let name = check_name(&new.name)?;
        let (group, granted) = self
            .write(move |tx| {
                let domain_id = domain_id(tx, &domain)?;
                crate::directory::check_not_released(tx, &local, domain_id, None)?;
                let login_taken: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM accounts WHERE login = ?1)",
                    [format!("{local}@{domain}")],
                    |row| row.get(0),
                )?;
                if login_taken || crate::forward_addresses::address_in_use(tx, &local, domain_id)? {
                    return Err(StoreError::Conflict(format!("address {local}@{domain}")));
                }
                let members = member_ids(tx, &new.members)?;
                tx.execute(
                    "INSERT INTO groups (local_part, domain_id, name, who_may_send, members_may_send_as, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![local, domain_id, name, new.who_may_send.as_str(), new.members_may_send_as, now()],
                )?;
                let id = tx.last_insert_rowid();
                let mut granted = Granted::default();
                let address = format!("{local}@{domain}");
                set_members(tx, id, &address, &name, Some(&members), new.members_may_send_as, &mut granted)?;
                Ok((load_one(tx, id)?, granted))
            })
            .await?;
        self.notify_granted(granted);
        Ok(group)
    }

    pub async fn update_group(&self, address: &str, update: GroupUpdate) -> Result<Group> {
        let (local, domain) = normalize_address(address)?;
        let name = update.name.as_deref().map(check_name).transpose()?;
        let (group, granted) = self
            .write(move |tx| {
                let id = group_by_address(tx, &local, &domain)?;
                if let Some(name) = &name {
                    tx.execute("UPDATE groups SET name = ?1 WHERE id = ?2", params![name, id])?;
                }
                if let Some(who) = update.who_may_send {
                    tx.execute("UPDATE groups SET who_may_send = ?1 WHERE id = ?2", params![who.as_str(), id])?;
                }
                if let Some(send_as) = update.members_may_send_as {
                    tx.execute("UPDATE groups SET members_may_send_as = ?1 WHERE id = ?2", params![send_as, id])?;
                }
                let members = update.members.as_deref().map(|logins| member_ids(tx, logins)).transpose()?;
                let group = load_one(tx, id)?;
                let mut granted = Granted::default();
                set_members(
                    tx,
                    id,
                    &group.address,
                    &group.name,
                    members.as_deref(),
                    group.members_may_send_as,
                    &mut granted,
                )?;
                Ok((load_one(tx, id)?, granted))
            })
            .await?;
        self.notify_granted(granted);
        Ok(group)
    }

    pub async fn delete_group(&self, address: &str) -> Result<()> {
        let (local, domain) = normalize_address(address)?;
        let granted = self
            .write(move |tx| {
                let id = group_by_address(tx, &local, &domain)?;
                let members: Vec<i64> = tx
                    .prepare("SELECT account_id FROM group_members WHERE group_id = ?1")?
                    .query_map([id], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                tx.execute("DELETE FROM groups WHERE id = ?1", [id])?;
                let mut granted = Granted::default();
                let address = format!("{local}@{domain}");
                for member in members {
                    revoke_identities(tx, member, &address, &mut granted)?;
                }
                Ok(granted)
            })
            .await?;
        self.notify_granted(granted);
        Ok(())
    }

    /// The group that takes mail for `address`, `+tag` or not, with the members to deliver to.
    pub async fn group_delivery(&self, address: &str) -> Result<Option<GroupDelivery>> {
        let Ok((local, domain)) = normalize_address(address) else {
            return Ok(None);
        };
        self.read(move |conn| {
            let Some(domain_id) =
                conn.query_row("SELECT id FROM domains WHERE name = ?1", [&domain], |row| row.get(0)).optional()?
            else {
                return Ok(None);
            };
            let Some(id) = find(conn, &local, domain_id)? else {
                return Ok(None);
            };
            let (address, who): (String, String) = conn.query_row(
                "SELECT local_part || '@' || ?2, who_may_send FROM groups WHERE id = ?1",
                params![id, domain],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let members = conn
                .prepare(
                    "SELECT m.account_id FROM group_members m JOIN accounts a ON a.id = m.account_id
                     WHERE m.group_id = ?1 AND a.deleted_at IS NULL ORDER BY m.account_id",
                )?
                .query_map([id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(Some(GroupDelivery {
                id,
                address,
                who_may_send: WhoMaySend::parse(&who).unwrap_or(WhoMaySend::Anyone),
                members,
            }))
        })
        .await
    }

    /// Whether mail from `sender` may go to a group. `account` is the sender's own account when it
    /// is one of ours and logged in; otherwise only the address counts: for `members` it has to
    /// be one of the members' own addresses (a `+tag` on it is fine).
    pub async fn group_accepts(&self, group: &GroupDelivery, sender: &str, account: Option<i64>) -> Result<bool> {
        let group_domain = group.address.rsplit_once('@').map(|(_, domain)| domain.to_owned()).unwrap_or_default();
        let sender = normalize_address(sender).ok();
        match group.who_may_send {
            WhoMaySend::Anyone => Ok(true),
            WhoMaySend::Domain => Ok(sender.is_some_and(|(_, domain)| domain == group_domain)),
            WhoMaySend::Members => {
                if account.is_some_and(|account| group.members.contains(&account)) {
                    return Ok(true);
                }
                let Some((local, domain)) = sender else { return Ok(false) };
                let id = group.id;
                self.read(move |conn| {
                    let base = base_local_part(&local).to_owned();
                    Ok(conn.query_row(
                        "SELECT EXISTS (SELECT 1 FROM group_members m
                                        JOIN addresses a ON a.account_id = m.account_id
                                        JOIN domains d ON d.id = a.domain_id
                                        WHERE m.group_id = ?1 AND d.name = ?2 AND a.local_part IN (?3, ?4))",
                        params![id, domain, local, base],
                        |row| row.get(0),
                    )?)
                })
                .await
            }
        }
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

    fn group(address: &str, members: &[&str]) -> NewGroup {
        NewGroup {
            address: address.into(),
            name: "Vorstand".into(),
            who_may_send: WhoMaySend::Anyone,
            members_may_send_as: false,
            members: members.iter().map(|m| (*m).to_owned()).collect(),
        }
    }

    #[tokio::test]
    async fn groups_take_an_address_of_their_own() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let leni = person(&store, "leni@example.org").await;

        let created = store
            .create_group(group("Vorstand@example.org", &["mini@example.org", "LENI@example.org", "mini@example.org"]))
            .await
            .unwrap();
        assert_eq!(created.address, "vorstand@example.org");
        assert_eq!(created.members.iter().map(|m| m.id).collect::<Vec<_>>(), vec![leni, mini]);

        // Nobody else may take the address, and the group no one else's.
        assert!(matches!(store.create_group(group("vorstand@example.org", &[])).await, Err(StoreError::Conflict(_))));
        assert!(matches!(store.create_group(group("mini@example.org", &[])).await, Err(StoreError::Conflict(_))));
        assert!(matches!(
            store.add_alias("vorstand@example.org", "mini@example.org").await,
            Err(StoreError::Conflict(_))
        ));
        let forward = store.set_forward_address("vorstand@example.org", vec!["a@example.net".into()], "").await;
        assert!(matches!(forward, Err(StoreError::Conflict(_))));
        assert!(matches!(
            store.create_group(group("x@example.org", &["ghost@example.org"])).await,
            Err(StoreError::NotFound(_))
        ));
        assert!(store.delete_domain("example.org").await.is_err());

        // It has no account; its members are delivered to, +tag or not, and no catch-all takes it.
        store.set_catch_all("example.org", Some("mini@example.org")).await.unwrap();
        assert_eq!(store.resolve_recipient("vorstand@example.org").await.unwrap(), None);
        let delivery = store.group_delivery("Vorstand+2026@example.org").await.unwrap().unwrap();
        assert_eq!((delivery.address.as_str(), delivery.members.clone()), ("vorstand@example.org", vec![mini, leni]));
        assert_eq!(store.account_groups(leni).await.unwrap().len(), 1);

        let changed = store
            .update_group(
                "vorstand@example.org",
                GroupUpdate {
                    name: Some(" Der Vorstand ".into()),
                    who_may_send: Some(WhoMaySend::Members),
                    members: Some(vec!["mini@example.org".into()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!((changed.name.as_str(), changed.members.len()), ("Der Vorstand", 1));
        assert!(store.account_groups(leni).await.unwrap().is_empty());

        store.delete_group("vorstand@example.org").await.unwrap();
        assert!(store.groups(None).await.unwrap().is_empty());
        assert_eq!(store.resolve_recipient("vorstand@example.org").await.unwrap(), Some(mini), "catch-all again");
    }

    #[tokio::test]
    async fn who_may_send_to_a_group() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let leni = person(&store, "leni@example.org").await;
        store.create_group(group("info@example.org", &["mini@example.org"])).await.unwrap();
        let delivery = |who| {
            let store = store.clone();
            async move {
                store
                    .update_group("info@example.org", GroupUpdate { who_may_send: Some(who), ..Default::default() })
                    .await
                    .unwrap();
                store.group_delivery("info@example.org").await.unwrap().unwrap()
            }
        };

        let anyone = delivery(WhoMaySend::Anyone).await;
        assert!(store.group_accepts(&anyone, "fremd@example.net", None).await.unwrap());

        let members = delivery(WhoMaySend::Members).await;
        assert!(store.group_accepts(&members, "mini+x@example.org", None).await.unwrap());
        assert!(!store.group_accepts(&members, "leni@example.org", None).await.unwrap());
        assert!(store.group_accepts(&members, "anything@example.net", Some(mini)).await.unwrap(), "logged in");
        assert!(!store.group_accepts(&members, "leni@example.org", Some(leni)).await.unwrap());

        let domain = delivery(WhoMaySend::Domain).await;
        assert!(store.group_accepts(&domain, "leni@example.org", None).await.unwrap());
        assert!(!store.group_accepts(&domain, "fremd@example.net", None).await.unwrap());
        assert!(!store.group_accepts(&domain, "", None).await.unwrap());
    }

    #[tokio::test]
    async fn members_may_send_as_the_group() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let mini = person(&store, "mini@example.org").await;
        let leni = person(&store, "leni@example.org").await;
        // Mini already has identities, Leni gets hers when she first asks.
        assert_eq!(store.identities(mini).await.unwrap().len(), 1);
        let new = NewGroup {
            members_may_send_as: true,
            ..group("info@example.org", &["mini@example.org", "leni@example.org"])
        };
        store.create_group(new).await.unwrap();
        assert!(store.account_owns_address(mini, "info@example.org").await.unwrap());
        let emails = |list: Vec<crate::Identity>| list.into_iter().map(|i| (i.email, i.name)).collect::<Vec<_>>();
        assert_eq!(
            emails(store.identities(mini).await.unwrap()),
            vec![("mini@example.org".into(), "mini".into()), ("info@example.org".into(), "Vorstand".into())]
        );
        assert_eq!(emails(store.identities(leni).await.unwrap()).len(), 2);

        store
            .update_group(
                "info@example.org",
                GroupUpdate { members: Some(vec!["leni@example.org".into()]), ..Default::default() },
            )
            .await
            .unwrap();
        assert!(!store.account_owns_address(mini, "info@example.org").await.unwrap());
        assert_eq!(store.identities(mini).await.unwrap().len(), 1, "the identity went with the membership");
        store
            .update_group("info@example.org", GroupUpdate { members_may_send_as: Some(false), ..Default::default() })
            .await
            .unwrap();
        assert_eq!(store.identities(leni).await.unwrap().len(), 1);
        assert!(!store.account_owns_address(leni, "info@example.org").await.unwrap());
    }
}
