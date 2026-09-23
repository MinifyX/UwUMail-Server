//! What JMAP Contacts needs on top of the CardDAV tables: cards by id across an account's address
//! books, moving cards between address books, and the default address book. Everything is written
//! through the same helpers as CardDAV, so ETags, sync tokens and the change log move together.

use rusqlite::{OptionalExtension, Transaction, params};

use crate::dav::{
    ChangeLog, DavCollection, DavCollectionUpdate, DavKind, DavPrecondition, DavWrite, DavWriteOutcome,
    NewDavCollection, apply_collection_update, delete_collection, delete_entry, insert_collection, move_entry,
    new_entry_name, own_collection, put_entry, set_default,
};
use crate::{DAV_RESOURCE_MAX_BYTES, Result, Store, StoreError};

/// A card as JMAP sees it: a vCard entry of one of the account's address books.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactCardRecord {
    pub id: i64,
    pub address_book_id: i64,
    pub name: String,
    pub uid: String,
    pub etag: String,
    pub content: String,
    pub modified_at: i64,
}

/// A new or changed card, already checked the way CardDAV checks what clients store.
#[derive(Debug, Clone)]
pub struct ContactCardWrite {
    /// `None` for a new card.
    pub id: Option<i64>,
    pub address_book_id: i64,
    pub content: String,
    pub uid: String,
    /// The ETag the card had when it was read: a write over a change made meanwhile fails with
    /// [`StoreError::Conflict`], so the caller can read it again.
    pub if_etag: Option<String>,
}

const CARD_COLUMNS: &str = "r.id, r.collection_id, r.name, r.uid, r.etag, r.content, r.modified_at";

fn card_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ContactCardRecord> {
    Ok(ContactCardRecord {
        id: row.get(0)?,
        address_book_id: row.get(1)?,
        name: row.get(2)?,
        uid: row.get(3)?,
        etag: row.get(4)?,
        content: row.get(5)?,
        modified_at: row.get(6)?,
    })
}

fn own_address_book(tx: &Transaction<'_>, account_id: i64, address_book_id: i64) -> Result<DavCollection> {
    let collection = own_collection(tx, account_id, address_book_id)?;
    if collection.kind != DavKind::Addressbook {
        return Err(StoreError::NotFound(format!("address book {address_book_id}")));
    }
    Ok(collection)
}

impl Store {
    /// Cards of the account's address books: the given ones, or all.
    pub async fn contact_cards(&self, account_id: i64, ids: Option<Vec<i64>>) -> Result<Vec<ContactCardRecord>> {
        let ids_json = ids.map(|ids| serde_json::to_string(&ids).unwrap_or_else(|_| "[]".into()));
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {CARD_COLUMNS} FROM dav_resources r JOIN dav_collections c ON c.id = r.collection_id
                 WHERE c.account_id = ?1 AND c.kind = 'addressbook' AND r.component = 'VCARD'
                   AND (?2 IS NULL OR r.id IN (SELECT value FROM json_each(?2)))
                 ORDER BY r.id"
            ))?;
            let rows = stmt.query_map(params![account_id, ids_json], card_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Creates an address book with its properties in one step.
    pub async fn create_address_book(
        &self,
        account_id: i64,
        new: NewDavCollection,
        update: DavCollectionUpdate,
    ) -> Result<DavCollection> {
        let (collection, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let id = insert_collection(tx, &mut log, account_id, DavKind::Addressbook, &new)?;
                apply_collection_update(tx, id, &update)?;
                Ok((own_collection(tx, account_id, id)?, log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(collection)
    }

    /// Makes an address book the account's default one.
    pub async fn set_default_address_book(&self, account_id: i64, address_book_id: i64) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let book = own_address_book(tx, account_id, address_book_id)?;
                set_default(tx, &mut log, &book)?;
                Ok(log.modseq())
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(())
    }

    /// Deletes an address book; one with cards only when `with_contents`. The last one stays.
    pub async fn destroy_address_book(&self, account_id: i64, address_book_id: i64, with_contents: bool) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let book = own_address_book(tx, account_id, address_book_id)?;
                let count: i64 = tx.query_row(
                    "SELECT count(*) FROM dav_collections WHERE account_id = ?1 AND kind = 'addressbook'",
                    [account_id],
                    |row| row.get(0),
                )?;
                if count <= 1 {
                    return Err(StoreError::Rule {
                        code: "forbidden",
                        message: "the only address book cannot be deleted".into(),
                    });
                }
                if book.resources > 0 && !with_contents {
                    return Err(StoreError::Rule {
                        code: "addressBookHasContents",
                        message: "the address book still has cards".into(),
                    });
                }
                delete_collection(tx, &mut log, &book)?;
                Ok(log.modseq())
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(())
    }

    /// Creates or changes a card, possibly moving it into another address book. A UID stays
    /// unique within an address book, as CardDAV wants. Returns the card's id and new ETag.
    pub async fn put_contact_card(&self, account_id: i64, write: ContactCardWrite) -> Result<(i64, String)> {
        if write.content.len() > DAV_RESOURCE_MAX_BYTES {
            return Err(StoreError::QuotaExceeded);
        }
        let (result, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let target = own_address_book(tx, account_id, write.address_book_id)?;
                let other: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM dav_resources WHERE collection_id = ?1 AND uid = ?2 AND id <> ?3",
                        params![target.id, write.uid, write.id.unwrap_or(-1)],
                        |row| row.get(0),
                    )
                    .optional()?;
                if other.is_some() {
                    return Err(StoreError::Rule {
                        code: "alreadyExists",
                        message: "another card of the address book has the same uid".into(),
                    });
                }
                let entry = |name: String| DavWrite {
                    name,
                    content: write.content.clone(),
                    uid: write.uid.clone(),
                    component: "VCARD".into(),
                    starts_at: None,
                    ends_at: None,
                };
                let Some(id) = write.id else {
                    let name = new_entry_name(tx, target.id, &write.uid, "vcf")?;
                    let condition = DavPrecondition { if_none_match_any: true, ..Default::default() };
                    return match put_entry(tx, &mut log, &target, &entry(name), &condition)? {
                        (DavWriteOutcome::Created { etag }, Some(id)) => Ok(((id, etag), log.modseq())),
                        other => Err(StoreError::Internal(format!("a new card could not be stored: {other:?}"))),
                    };
                };
                let (source_id, name, etag): (i64, String, String) = tx
                    .query_row(
                        "SELECT r.collection_id, r.name, r.etag FROM dav_resources r
                         JOIN dav_collections c ON c.id = r.collection_id
                         WHERE r.id = ?1 AND c.account_id = ?2 AND c.kind = 'addressbook' AND r.component = 'VCARD'",
                        params![id, account_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?
                    .ok_or_else(|| StoreError::NotFound(format!("card {id}")))?;
                if write.if_etag.as_ref().is_some_and(|wanted| *wanted != etag) {
                    return Err(StoreError::Conflict(format!("card {id} changed meanwhile")));
                }
                if source_id == target.id {
                    let condition = DavPrecondition { if_match: Some(etag), ..Default::default() };
                    return match put_entry(tx, &mut log, &target, &entry(name), &condition)? {
                        (DavWriteOutcome::Updated { etag }, _) => Ok(((id, etag), log.modseq())),
                        other => Err(StoreError::Internal(format!("a card could not be updated: {other:?}"))),
                    };
                }
                let etag = move_entry(tx, &mut log, id, source_id, &target, &entry(name), "vcf")?;
                Ok(((id, etag), log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(result)
    }

    /// Deletes a card, unless it changed since it had `if_etag`.
    pub async fn destroy_contact_card(&self, account_id: i64, id: i64, if_etag: Option<String>) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let (address_book_id, name): (i64, String) = tx
                    .query_row(
                        "SELECT r.collection_id, r.name FROM dav_resources r
                         JOIN dav_collections c ON c.id = r.collection_id
                         WHERE r.id = ?1 AND c.account_id = ?2 AND c.kind = 'addressbook' AND r.component = 'VCARD'",
                        params![id, account_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?
                    .ok_or_else(|| StoreError::NotFound(format!("card {id}")))?;
                let book = own_address_book(tx, account_id, address_book_id)?;
                if !delete_entry(tx, &mut log, &book, &name, if_etag.as_deref())? {
                    return Err(StoreError::Conflict(format!("card {id} changed meanwhile")));
                }
                Ok(log.modseq())
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{DavCollectionUpdate, NewAccount, Role};

    async fn account(store: &Store, address: &str) -> i64 {
        store.create_domain("example.de").await.ok();
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

    fn contacts() -> NewDavCollection {
        NewDavCollection { slug: "contacts".into(), display_name: "Kontakte".into(), ..Default::default() }
    }

    fn card(uid: &str, address_book_id: i64, id: Option<i64>) -> ContactCardWrite {
        ContactCardWrite {
            id,
            address_book_id,
            content: format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:Nyu\r\nEND:VCARD\r\n"),
            uid: uid.into(),
            if_etag: None,
        }
    }

    #[tokio::test]
    async fn carddav_writes_reach_the_change_log() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.de").await;
        let start = store.account_modseq(mini).await.unwrap();
        let book = store.dav_collections(mini, DavKind::Addressbook, contacts()).await.unwrap()[0].clone();
        assert!(book.is_default, "the first address book is the default");
        let put = DavWrite {
            name: "nyu.vcf".into(),
            content: "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:nyu\r\nFN:Nyu\r\nEND:VCARD\r\n".into(),
            uid: "nyu".into(),
            component: "VCARD".into(),
            starts_at: None,
            ends_at: None,
        };
        let mut changes = store.subscribe_changes();
        store.dav_put(mini, book.id, put, DavPrecondition::default()).await.unwrap();
        assert_eq!(changes.recv().await.unwrap().account_id, mini, "push hears about it");
        let cards = store.contact_cards(mini, None).await.unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(store.changes(mini, "ContactCard", start, 0).await.unwrap().created, vec![cards[0].id]);
        assert_eq!(store.changes(mini, "AddressBook", start, 0).await.unwrap().created, vec![book.id]);
        assert!(store.changes(mini, "CalendarEvent", start, 0).await.unwrap().created.is_empty());

        let after = store.account_modseq(mini).await.unwrap();
        store.dav_update_collection(mini, book.id, DavCollectionUpdate::default()).await.unwrap();
        assert_eq!(store.changes(mini, "AddressBook", after, 0).await.unwrap().updated, vec![book.id]);
        assert!(store.dav_delete(mini, book.id, "nyu.vcf", None).await.unwrap());
        assert_eq!(store.changes(mini, "ContactCard", after, 0).await.unwrap().destroyed, vec![cards[0].id]);
    }

    #[tokio::test]
    async fn cards_move_between_address_books() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.de").await;
        let leni = account(&store, "leni@example.de").await;
        let personal = store.dav_collections(mini, DavKind::Addressbook, contacts()).await.unwrap()[0].clone();
        let start = store.account_modseq(mini).await.unwrap();
        let family = store
            .create_address_book(
                mini,
                NewDavCollection { slug: "family".into(), display_name: "Familie".into(), ..Default::default() },
                DavCollectionUpdate { sort_order: Some(3), ..Default::default() },
            )
            .await
            .unwrap();
        assert!(!family.is_default);
        assert_eq!(family.sort_order, 3);

        let (id, etag) = store.put_contact_card(mini, card("nyu", personal.id, None)).await.unwrap();
        let again = store.put_contact_card(mini, card("nyu", personal.id, None)).await;
        assert!(matches!(again, Err(StoreError::Rule { code: "alreadyExists", .. })));
        // Another address book may have a card with the same uid, as over CardDAV.
        let (copy, _) = store.put_contact_card(mini, card("nyu", family.id, None)).await.unwrap();
        store.destroy_contact_card(mini, copy, None).await.unwrap();
        assert!(matches!(
            store.put_contact_card(leni, card("x", personal.id, None)).await,
            Err(StoreError::NotFound(_))
        ));

        let stale = ContactCardWrite { if_etag: Some("\"old\"".into()), ..card("nyu", family.id, Some(id)) };
        assert!(matches!(store.put_contact_card(mini, stale).await, Err(StoreError::Conflict(_))));
        let moved = ContactCardWrite { if_etag: Some(etag), ..card("nyu", family.id, Some(id)) };
        assert_eq!(store.put_contact_card(mini, moved).await.unwrap().0, id);
        assert_eq!(store.dav_changes(mini, personal.id, 0).await.unwrap().deleted, vec!["nyu.vcf"]);
        assert_eq!(store.contact_cards(mini, Some(vec![id])).await.unwrap()[0].address_book_id, family.id);
        assert!(store.contact_cards(leni, Some(vec![id])).await.unwrap().is_empty());

        store.set_default_address_book(mini, family.id).await.unwrap();
        assert!(matches!(
            store.destroy_address_book(mini, family.id, false).await,
            Err(StoreError::Rule { code: "addressBookHasContents", .. })
        ));
        store.destroy_address_book(mini, family.id, true).await.unwrap();
        let left = store.dav_collections(mini, DavKind::Addressbook, contacts()).await.unwrap();
        assert_eq!(left.len(), 1);
        assert!(left[0].is_default, "the default moves on");
        assert!(matches!(
            store.destroy_address_book(mini, personal.id, true).await,
            Err(StoreError::Rule { code: "forbidden", .. })
        ));
        let changes = store.changes(mini, "ContactCard", start, 0).await.unwrap();
        assert!(changes.created.is_empty() && changes.destroyed.is_empty(), "created and gone again");
        // Calendars keep their own default.
        let calendars =
            store.dav_collections(mini, DavKind::Calendar, NewDavCollection::default_calendar("K")).await.unwrap();
        assert!(calendars[0].is_default);
    }
}
