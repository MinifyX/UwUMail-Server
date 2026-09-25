//! Calendars and address books shared between people of this server (migration 0038).
//!
//! The owner decides who sees a collection and what they may do with it. Shared collections stay
//! where they are: whoever they are shared with reads and writes the owner's entries, and every
//! change is logged for everyone who sees the collection (see `ChangeLog` in `dav.rs`), so JMAP
//! push and sync reach them all.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::dav::{COLLECTION_COLUMNS, ChangeLog, DavCollection, DavKind, collection_by_id, collection_row};
use crate::{Result, Store, StoreError, now};

/// People one collection may be shared with. More would be a mailing list, not a calendar.
pub const DAV_SHARES_PER_COLLECTION: i64 = 100;

/// What someone may do with a collection shared with them. Ordered: more rights compare greater.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareRights {
    /// See the entries.
    Read,
    /// See, add, change and delete entries.
    Write,
    /// Everything but deleting the collection: its name and colour, and sharing it further.
    All,
}

impl ShareRights {
    pub fn as_str(self) -> &'static str {
        match self {
            ShareRights::Read => "read",
            ShareRights::Write => "write",
            ShareRights::All => "all",
        }
    }

    pub fn parse(value: &str) -> Option<ShareRights> {
        match value {
            "read" => Some(ShareRights::Read),
            "write" => Some(ShareRights::Write),
            "all" => Some(ShareRights::All),
            _ => None,
        }
    }
}

/// How an account gets at a collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DavAccess {
    Owner,
    Shared(ShareRights),
}

impl DavAccess {
    /// Adding, changing and deleting entries.
    pub fn may_write(self) -> bool {
        matches!(self, DavAccess::Owner | DavAccess::Shared(ShareRights::Write | ShareRights::All))
    }

    /// Changing the collection's properties and who it is shared with.
    pub fn may_admin(self) -> bool {
        matches!(self, DavAccess::Owner | DavAccess::Shared(ShareRights::All))
    }

    pub fn is_owner(self) -> bool {
        self == DavAccess::Owner
    }
}

/// One person a collection is shared with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DavShare {
    pub collection_id: i64,
    pub account_id: i64,
    pub login: String,
    pub display_name: String,
    pub rights: ShareRights,
}

/// A collection someone else shares with an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedDavCollection {
    pub collection: DavCollection,
    pub owner_login: String,
    pub owner_name: String,
    pub rights: ShareRights,
}

fn share_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DavShare> {
    Ok(DavShare {
        collection_id: row.get(0)?,
        account_id: row.get(1)?,
        login: row.get(2)?,
        display_name: row.get(3)?,
        rights: ShareRights::parse(&row.get::<_, String>(4)?).unwrap_or(ShareRights::Read),
    })
}

const SHARE_COLUMNS: &str = "s.collection_id, s.account_id, a.login, a.display_name, s.rights";

/// How `account_id` gets at a collection, if at all.
pub(crate) fn access(
    conn: &Connection,
    account_id: i64,
    collection_id: i64,
) -> Result<Option<(DavCollection, DavAccess)>> {
    let Some(collection) = collection_by_id(conn, collection_id).ok() else { return Ok(None) };
    if collection.account_id == account_id {
        return Ok(Some((collection, DavAccess::Owner)));
    }
    let rights: Option<String> = conn
        .query_row(
            "SELECT rights FROM dav_shares WHERE collection_id = ?1 AND account_id = ?2",
            params![collection_id, account_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(rights.and_then(|rights| ShareRights::parse(&rights)).map(|rights| (collection, DavAccess::Shared(rights))))
}

/// A collection `account_id` may write entries into, or why not.
pub(crate) fn writable(conn: &Connection, account_id: i64, collection_id: i64) -> Result<DavCollection> {
    match access(conn, account_id, collection_id)? {
        Some((collection, access)) if access.may_write() => Ok(collection),
        Some(_) => Err(StoreError::Rule { code: "forbidden", message: "this is shared with you to read only".into() }),
        None => Err(StoreError::NotFound(format!("collection {collection_id}"))),
    }
}

/// The SQL condition for collections `?1` sees: its own and those shared with it. `c` is the
/// collection.
pub(crate) const VISIBLE: &str =
    "(c.account_id = ?1 OR c.id IN (SELECT collection_id FROM dav_shares WHERE account_id = ?1))";

impl Store {
    /// How an account gets at a collection: as its owner, through a share, or not at all.
    pub async fn dav_access(&self, account_id: i64, collection_id: i64) -> Result<Option<(DavCollection, DavAccess)>> {
        self.read(move |conn| access(conn, account_id, collection_id)).await
    }

    /// The collections of one kind others share with an account, oldest share first.
    pub async fn dav_shared_with(&self, account_id: i64, kind: DavKind) -> Result<Vec<SharedDavCollection>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLLECTION_COLUMNS}, o.login, o.display_name, s.rights
                 FROM dav_shares s JOIN dav_collections c ON c.id = s.collection_id
                 JOIN accounts o ON o.id = c.account_id
                 WHERE s.account_id = ?1 AND c.kind = ?2 AND o.deleted_at IS NULL
                 ORDER BY s.created_at, c.id"
            ))?;
            let rows = stmt.query_map(params![account_id, kind.as_str()], |row| {
                Ok(SharedDavCollection {
                    collection: collection_row(row)?,
                    owner_login: row.get(14)?,
                    owner_name: row.get(15)?,
                    rights: ShareRights::parse(&row.get::<_, String>(16)?).unwrap_or(ShareRights::Read),
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Who the collections of an account are shared with: one collection's, or all of them.
    pub async fn dav_shares(&self, owner_id: i64, collection_id: Option<i64>) -> Result<Vec<DavShare>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {SHARE_COLUMNS} FROM dav_shares s JOIN accounts a ON a.id = s.account_id
                 JOIN dav_collections c ON c.id = s.collection_id
                 WHERE c.account_id = ?1 AND (?2 IS NULL OR c.id = ?2)
                 ORDER BY c.id, a.login"
            ))?;
            let rows = stmt.query_map(params![owner_id, collection_id], share_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Shares a collection with the person behind `address`, or changes what they may do with it.
    /// `actor_id` is who asks: the owner, or someone it is shared with with all rights, who may
    /// share it on but not give more than they have or change the owner's own access.
    pub async fn dav_share(
        &self,
        actor_id: i64,
        collection_id: i64,
        address: &str,
        rights: ShareRights,
    ) -> Result<DavShare> {
        let grantee = self
            .resolve_recipient(address)
            .await?
            .ok_or_else(|| StoreError::Rule { code: "unknownPerson", message: format!("nobody here has {address}") })?;
        self.dav_share_with(actor_id, collection_id, grantee, rights).await
    }

    /// [`Store::dav_share`] with the grantee's account id.
    pub async fn dav_share_with(
        &self,
        actor_id: i64,
        collection_id: i64,
        grantee: i64,
        rights: ShareRights,
    ) -> Result<DavShare> {
        let (share, logged) = self
            .write(move |tx| {
                let Some((collection, access)) = access(tx, actor_id, collection_id)? else {
                    return Err(StoreError::NotFound(format!("collection {collection_id}")));
                };
                if !access.may_admin() {
                    return Err(StoreError::Rule {
                        code: "forbidden",
                        message: "only its owner may share this".into(),
                    });
                }
                if grantee == collection.account_id {
                    return Err(StoreError::Rule {
                        code: "ownShare",
                        message: "a calendar cannot be shared with its owner".into(),
                    });
                }
                let active: bool =
                    tx.query_row("SELECT deleted_at IS NULL FROM accounts WHERE id = ?1", [grantee], |row| row.get(0))?;
                if !active {
                    return Err(StoreError::Rule { code: "unknownPerson", message: "that account is gone".into() });
                }
                let before: Option<String> = tx
                    .query_row(
                        "SELECT rights FROM dav_shares WHERE collection_id = ?1 AND account_id = ?2",
                        params![collection_id, grantee],
                        |row| row.get(0),
                    )
                    .optional()?;
                if before.is_none() {
                    let count: i64 = tx.query_row(
                        "SELECT count(*) FROM dav_shares WHERE collection_id = ?1",
                        [collection_id],
                        |row| row.get(0),
                    )?;
                    if count >= DAV_SHARES_PER_COLLECTION {
                        return Err(StoreError::Rule {
                            code: "tooManyShares",
                            message: "this is shared with too many people already".into(),
                        });
                    }
                }
                tx.execute(
                    "INSERT INTO dav_shares (collection_id, account_id, rights, created_at) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (collection_id, account_id) DO UPDATE SET rights = excluded.rights",
                    params![collection_id, grantee, rights.as_str(), now()],
                )?;
                let mut log = ChangeLog::new(actor_id);
                if before.is_none() {
                    log.whole_collection(tx, grantee, &collection, "created")?;
                } else if before.as_deref() != Some(rights.as_str()) {
                    log.record_for(tx, grantee, collection.kind.jmap_types().0, collection.id, "updated")?;
                }
                // The owner sees who it is shared with.
                log.record_for(tx, collection.account_id, collection.kind.jmap_types().0, collection.id, "updated")?;
                let share = tx.query_row(
                    &format!(
                        "SELECT {SHARE_COLUMNS} FROM dav_shares s JOIN accounts a ON a.id = s.account_id
                         WHERE s.collection_id = ?1 AND s.account_id = ?2"
                    ),
                    params![collection_id, grantee],
                    share_row,
                )?;
                Ok((share, log.modseq()))
            })
            .await?;
        self.notify_log(actor_id, logged);
        Ok(share)
    }

    /// Stops sharing a collection with someone. The owner and those with all rights may take anyone
    /// off; anyone may take themselves off, which is how a shared calendar is left.
    pub async fn dav_unshare(&self, actor_id: i64, collection_id: i64, grantee: i64) -> Result<()> {
        let logged = self
            .write(move |tx| {
                let Some((collection, access)) = access(tx, actor_id, collection_id)? else {
                    return Err(StoreError::NotFound(format!("collection {collection_id}")));
                };
                if actor_id != grantee && !access.may_admin() {
                    return Err(StoreError::Rule { code: "forbidden", message: "only its owner may do that".into() });
                }
                let removed = tx.execute(
                    "DELETE FROM dav_shares WHERE collection_id = ?1 AND account_id = ?2",
                    params![collection_id, grantee],
                )?;
                if removed == 0 {
                    return Err(StoreError::NotFound(format!("share of collection {collection_id}")));
                }
                let mut log = ChangeLog::new(actor_id);
                log.whole_collection(tx, grantee, &collection, "destroyed")?;
                log.record_for(tx, collection.account_id, collection.kind.jmap_types().0, collection.id, "updated")?;
                Ok(log.modseq())
            })
            .await?;
        self.notify_log(actor_id, logged);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{DavPrecondition, DavWrite, NewAccount, NewDavCollection, Role};

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

    fn event(uid: &str) -> DavWrite {
        DavWrite {
            name: format!("{uid}.ics"),
            content: format!("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"),
            uid: uid.into(),
            component: "VEVENT".into(),
            starts_at: None,
            ends_at: None,
        }
    }

    #[tokio::test]
    async fn shares_reach_the_grantee_and_go_again() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.org").await;
        let leni = account(&store, "leni@example.org").await;
        let nyu = account(&store, "nyu@example.org").await;
        let calendar =
            store.dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("K")).await.unwrap()[0]
                .clone();
        store.dav_put(mini, calendar.id, event("a"), DavPrecondition::default()).await.unwrap();
        let start = store.account_modseq(leni).await.unwrap();

        assert!(matches!(
            store.dav_share(leni, calendar.id, "leni@example.org", ShareRights::Read).await,
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.dav_share(mini, calendar.id, "mini@example.org", ShareRights::Read).await,
            Err(StoreError::Rule { code: "ownShare", .. })
        ));
        assert!(matches!(
            store.dav_share(mini, calendar.id, "nobody@example.org", ShareRights::Read).await,
            Err(StoreError::Rule { code: "unknownPerson", .. })
        ));
        let share = store.dav_share(mini, calendar.id, "leni@example.org", ShareRights::Read).await.unwrap();
        assert_eq!((share.account_id, share.rights), (leni, ShareRights::Read));
        let shared = store.dav_shared_with(leni, DavKind::Calendar).await.unwrap();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].owner_login, "mini@example.org");
        assert_eq!(store.changes(leni, "Calendar", start, 0).await.unwrap().created, vec![calendar.id]);
        assert_eq!(store.changes(leni, "CalendarEvent", start, 0).await.unwrap().created.len(), 1);

        // Reading only: no writing, no sharing on.
        assert_eq!(store.dav_access(leni, calendar.id).await.unwrap().unwrap().1, DavAccess::Shared(ShareRights::Read));
        assert!(store.dav_share(leni, calendar.id, "nyu@example.org", ShareRights::Read).await.is_err());
        assert!(store.dav_access(nyu, calendar.id).await.unwrap().is_none());

        // The owner's writes reach the grantee's change log.
        let before = store.account_modseq(leni).await.unwrap();
        store.dav_put(mini, calendar.id, event("b"), DavPrecondition::default()).await.unwrap();
        assert_eq!(store.changes(leni, "CalendarEvent", before, 0).await.unwrap().created.len(), 1);

        store.dav_share(mini, calendar.id, "leni@example.org", ShareRights::All).await.unwrap();
        store.dav_share(leni, calendar.id, "nyu@example.org", ShareRights::Write).await.unwrap();
        assert_eq!(store.dav_shares(mini, Some(calendar.id)).await.unwrap().len(), 2);

        let before = store.account_modseq(leni).await.unwrap();
        store.dav_unshare(leni, calendar.id, leni).await.unwrap();
        assert!(store.dav_shared_with(leni, DavKind::Calendar).await.unwrap().is_empty());
        let gone = store.changes(leni, "Calendar", before, 0).await.unwrap();
        assert_eq!(gone.destroyed, vec![calendar.id]);
        assert!(store.dav_unshare(nyu, calendar.id, mini).await.is_err(), "a writer cannot take others off");
    }
}
