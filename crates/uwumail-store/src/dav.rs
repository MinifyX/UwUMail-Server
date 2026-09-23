//! Calendars and address books for CalDAV and CardDAV: collections, the iCalendar and vCard objects
//! in them with their ETags, and the change numbers that sync tokens are made from.
//!
//! JMAP Calendars and JMAP Contacts read the same collections, so every write to a calendar or an
//! address book also goes into the account's change log (`Calendar`, `CalendarEvent`,
//! `AddressBook`, `ContactCard`), whoever made it.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::db::{next_modseq, record_change};
use crate::{Result, Store, StoreError, now};

/// Entries per collection, and bytes per entry. Calendars of real people stay far below both.
pub const DAV_RESOURCES_PER_COLLECTION: i64 = 50_000;
pub const DAV_RESOURCE_MAX_BYTES: usize = 1024 * 1024;
pub const DAV_COLLECTIONS_PER_ACCOUNT: i64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DavKind {
    Calendar,
    Addressbook,
}

impl DavKind {
    /// The JMAP types of a collection of this kind and of the entries JMAP sees in it, with the
    /// component those entries have.
    fn jmap_types(self) -> (&'static str, &'static str, &'static str) {
        match self {
            DavKind::Calendar => ("Calendar", "CalendarEvent", "VEVENT"),
            DavKind::Addressbook => ("AddressBook", "ContactCard", "VCARD"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DavKind::Calendar => "calendar",
            DavKind::Addressbook => "addressbook",
        }
    }

    fn parse(value: &str) -> DavKind {
        if value == "addressbook" { DavKind::Addressbook } else { DavKind::Calendar }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DavCollection {
    pub id: i64,
    pub account_id: i64,
    pub kind: DavKind,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub color: Option<String>,
    pub sort_order: i64,
    pub components: Vec<String>,
    pub timezone: Option<String>,
    pub change: i64,
    pub resources: i64,
    /// Whether the webmail and the apps show its events (JMAP `isVisible`).
    pub is_visible: bool,
    /// The calendar new events go into by default, or the address book for new cards; one of each
    /// per account.
    pub is_default: bool,
}

#[derive(Debug, Clone, Default)]
pub struct NewDavCollection {
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub color: Option<String>,
    pub components: Vec<String>,
}

impl NewDavCollection {
    /// The calendar everyone starts with, made the first time CalDAV or JMAP looks for one.
    pub fn default_calendar(name: &str) -> NewDavCollection {
        NewDavCollection {
            slug: "personal".into(),
            display_name: name.into(),
            color: Some("#FF4D8DFF".into()),
            components: vec!["VEVENT".into(), "VTODO".into()],
            ..Default::default()
        }
    }

    /// The address book everyone starts with, made the first time CardDAV or JMAP looks for one.
    pub fn default_address_book(name: &str) -> NewDavCollection {
        NewDavCollection { slug: "contacts".into(), display_name: name.into(), ..Default::default() }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DavCollectionUpdate {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub color: Option<Option<String>>,
    pub sort_order: Option<i64>,
    pub timezone: Option<Option<String>>,
    pub is_visible: Option<bool>,
}

/// An entry without its content, for listings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DavResourceInfo {
    pub name: String,
    pub uid: String,
    pub etag: String,
    pub component: String,
    pub size: i64,
    pub modified_at: i64,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DavResource {
    pub info: DavResourceInfo,
    pub content: String,
}

/// What a client wants to store.
#[derive(Debug, Clone)]
pub struct DavWrite {
    pub name: String,
    pub content: String,
    pub uid: String,
    pub component: String,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
}

/// The conditions of a PUT or DELETE (`If-Match`, `If-None-Match: *`).
#[derive(Debug, Clone, Default)]
pub struct DavPrecondition {
    pub if_match: Option<String>,
    pub if_none_match_any: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DavWriteOutcome {
    Created {
        etag: String,
    },
    Updated {
        etag: String,
    },
    /// The conditions did not hold.
    PreconditionFailed,
    /// Another entry of the collection has the same UID.
    UidTaken {
        name: String,
    },
}

/// What changed in a collection after a change number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DavChanges {
    pub change: i64,
    pub changed: Vec<DavResourceInfo>,
    pub deleted: Vec<String>,
}

/// `"…"`: the first 32 hex digits of the content's SHA-256.
pub fn dav_etag(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    format!("\"{}\"", hex::encode(&digest[..16]))
}

const COLLECTION_COLUMNS: &str = "c.id, c.account_id, c.kind, c.slug, c.display_name, c.description, c.color, \
     c.sort_order, c.components, c.timezone, c.change, (SELECT count(*) FROM dav_resources r WHERE r.collection_id = c.id), \
     c.is_visible, c.is_default";

pub(crate) fn collection_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DavCollection> {
    Ok(DavCollection {
        id: row.get(0)?,
        account_id: row.get(1)?,
        kind: DavKind::parse(&row.get::<_, String>(2)?),
        slug: row.get(3)?,
        display_name: row.get(4)?,
        description: row.get(5)?,
        color: row.get(6)?,
        sort_order: row.get(7)?,
        components: row.get::<_, String>(8)?.split_whitespace().map(str::to_owned).collect(),
        timezone: row.get(9)?,
        change: row.get(10)?,
        resources: row.get(11)?,
        is_visible: row.get(12)?,
        is_default: row.get(13)?,
    })
}

const INFO_COLUMNS: &str = "name, uid, etag, component, size, modified_at, starts_at, ends_at";

fn info_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DavResourceInfo> {
    Ok(DavResourceInfo {
        name: row.get(0)?,
        uid: row.get(1)?,
        etag: row.get(2)?,
        component: row.get(3)?,
        size: row.get(4)?,
        modified_at: row.get(5)?,
        starts_at: row.get(6)?,
        ends_at: row.get(7)?,
    })
}

/// The JMAP side of a write: which collections and entries it touched, all under one change number.
pub(crate) struct ChangeLog {
    account_id: i64,
    modseq: Option<i64>,
}

impl ChangeLog {
    pub(crate) fn new(account_id: i64) -> ChangeLog {
        ChangeLog { account_id, modseq: None }
    }

    pub(crate) fn record(&mut self, conn: &Connection, kind: &str, object_id: i64, change: &str) -> Result<()> {
        let modseq = match self.modseq {
            Some(modseq) => modseq,
            None => *self.modseq.insert(next_modseq(conn, self.account_id)?),
        };
        record_change(conn, self.account_id, modseq, kind, object_id, change)
    }

    pub(crate) fn collection(&mut self, conn: &Connection, collection: &DavCollection, change: &str) -> Result<()> {
        self.record(conn, collection.kind.jmap_types().0, collection.id, change)
    }

    /// An entry of a collection changed from one component to another (`None`: not there).
    pub(crate) fn entry(
        &mut self,
        conn: &Connection,
        collection: &DavCollection,
        resource_id: i64,
        before: Option<&str>,
        after: Option<&str>,
    ) -> Result<()> {
        // Only events are CalendarEvents; tasks and journal entries stay CalDAV's.
        let (_, entry_type, component) = collection.kind.jmap_types();
        let change = match (before == Some(component), after == Some(component)) {
            (false, true) => "created",
            (true, true) => "updated",
            (true, false) => "destroyed",
            (false, false) => return Ok(()),
        };
        self.record(conn, entry_type, resource_id, change)
    }

    pub(crate) fn modseq(&self) -> Option<i64> {
        self.modseq
    }
}

impl Store {
    /// Tells push listeners about a write's JMAP changes once it is committed.
    pub(crate) fn notify_log(&self, account_id: i64, modseq: Option<i64>) {
        if let Some(modseq) = modseq {
            self.notify_change(account_id, modseq);
        }
    }
}

pub(crate) fn own_collection(conn: &Connection, account_id: i64, collection_id: i64) -> Result<DavCollection> {
    conn.query_row(
        &format!("SELECT {COLLECTION_COLUMNS} FROM dav_collections c WHERE c.id = ?1 AND c.account_id = ?2"),
        params![collection_id, account_id],
        collection_row,
    )
    .optional()?
    .ok_or_else(|| StoreError::NotFound(format!("collection {collection_id}")))
}

/// Path segments stay short and URL-safe, so hrefs never need escaping rules of their own.
pub(crate) fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 200
        && segment != "."
        && segment != ".."
        && segment.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.@+~".contains(&b))
}

pub(crate) fn next_change(tx: &Transaction<'_>, collection_id: i64) -> Result<i64> {
    Ok(tx.query_row(
        "UPDATE dav_collections SET change = change + 1 WHERE id = ?1 RETURNING change",
        [collection_id],
        |row| row.get(0),
    )?)
}

pub(crate) fn insert_collection(
    tx: &Transaction<'_>,
    log: &mut ChangeLog,
    account_id: i64,
    kind: DavKind,
    new: &NewDavCollection,
) -> Result<i64> {
    if !valid_segment(&new.slug) {
        return Err(StoreError::Invalid(format!("'{}' cannot be part of a URL", new.slug)));
    }
    let count: i64 =
        tx.query_row("SELECT count(*) FROM dav_collections WHERE account_id = ?1", [account_id], |row| row.get(0))?;
    if count >= DAV_COLLECTIONS_PER_ACCOUNT {
        return Err(StoreError::Rule {
            code: "davCollectionsFull",
            message: "too many calendars and address books".into(),
        });
    }
    let inserted = tx.execute(
        "INSERT INTO dav_collections (account_id, kind, slug, display_name, description, color, components, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (account_id, kind, slug) DO NOTHING",
        params![
            account_id,
            kind.as_str(),
            new.slug,
            new.display_name.trim(),
            new.description.trim(),
            new.color,
            new.components.join(" "),
            now()
        ],
    )?;
    if inserted == 0 {
        return Err(StoreError::Conflict(format!("collection {}", new.slug)));
    }
    let id = tx.last_insert_rowid();
    // The first collection of its kind, or the first after all others were deleted, is the default.
    tx.execute(
        "UPDATE dav_collections SET is_default = 1 WHERE id = ?1 AND NOT EXISTS
             (SELECT 1 FROM dav_collections WHERE account_id = ?2 AND kind = ?3 AND is_default)",
        params![id, account_id, kind.as_str()],
    )?;
    log.record(tx, kind.jmap_types().0, id, "created")?;
    Ok(id)
}

/// After the default collection of a kind is gone, the first of the rest takes over.
pub(crate) fn ensure_default(tx: &Transaction<'_>, log: &mut ChangeLog, account_id: i64, kind: DavKind) -> Result<()> {
    let has_default: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM dav_collections WHERE account_id = ?1 AND kind = ?2 AND is_default)",
        params![account_id, kind.as_str()],
        |row| row.get(0),
    )?;
    if has_default {
        return Ok(());
    }
    let first: Option<i64> = tx
        .query_row(
            "SELECT id FROM dav_collections WHERE account_id = ?1 AND kind = ?2 ORDER BY sort_order, id LIMIT 1",
            params![account_id, kind.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = first {
        tx.execute("UPDATE dav_collections SET is_default = 1 WHERE id = ?1", [id])?;
        log.record(tx, kind.jmap_types().0, id, "updated")?;
    }
    Ok(())
}

/// Makes a collection the default one of its kind; both collections whose flag changed are logged.
pub(crate) fn set_default(tx: &Transaction<'_>, log: &mut ChangeLog, collection: &DavCollection) -> Result<()> {
    if collection.is_default {
        return Ok(());
    }
    let mut stmt = tx.prepare(
        "UPDATE dav_collections SET is_default = 0 WHERE account_id = ?1 AND kind = ?2 AND is_default RETURNING id",
    )?;
    let previous: Vec<i64> = stmt
        .query_map(params![collection.account_id, collection.kind.as_str()], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    let collection_type = collection.kind.jmap_types().0;
    for id in previous {
        log.record(tx, collection_type, id, "updated")?;
    }
    tx.execute("UPDATE dav_collections SET is_default = 1 WHERE id = ?1", [collection.id])?;
    log.record(tx, collection_type, collection.id, "updated")
}

/// Deletes a collection with everything in it, telling JMAP which collection and entries went away.
pub(crate) fn delete_collection(tx: &Transaction<'_>, log: &mut ChangeLog, collection: &DavCollection) -> Result<()> {
    let (_, entry_type, component) = collection.kind.jmap_types();
    let mut stmt = tx.prepare("SELECT id FROM dav_resources WHERE collection_id = ?1 AND component = ?2")?;
    let entries: Vec<i64> =
        stmt.query_map(params![collection.id, component], |row| row.get(0))?.collect::<Result<_, _>>()?;
    drop(stmt);
    for entry in entries {
        log.record(tx, entry_type, entry, "destroyed")?;
    }
    log.collection(tx, collection, "destroyed")?;
    tx.execute("DELETE FROM dav_collections WHERE id = ?1", [collection.id])?;
    if collection.is_default {
        ensure_default(tx, log, collection.account_id, collection.kind)?;
    }
    Ok(())
}

/// A resource name for a new entry: the UID when it fits into a URL and is free, else a random one.
pub(crate) fn new_entry_name(tx: &Transaction<'_>, collection_id: i64, uid: &str, extension: &str) -> Result<String> {
    let taken = |name: &str| -> Result<bool> {
        Ok(tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM dav_resources WHERE collection_id = ?1 AND name = ?2)
                 OR EXISTS (SELECT 1 FROM dav_tombstones WHERE collection_id = ?1 AND name = ?2)",
            params![collection_id, name],
            |row| row.get(0),
        )?)
    };
    let from_uid = format!("{uid}.{extension}");
    if valid_segment(&from_uid) && from_uid.len() <= 120 && !taken(&from_uid)? {
        return Ok(from_uid);
    }
    loop {
        let name = format!("{}.{extension}", hex::encode(crate::random_bytes::<16>()));
        if !taken(&name)? {
            return Ok(name);
        }
    }
}

/// Moves an entry into another collection of the account with new content. It keeps its row id,
/// so JMAP sees the same object; CardDAV and CalDAV see it gone from one collection and new in the
/// other. `write.name` is the entry's current name, which it keeps when the target has it free.
/// Returns the new ETag.
pub(crate) fn move_entry(
    tx: &Transaction<'_>,
    log: &mut ChangeLog,
    id: i64,
    source_id: i64,
    target: &DavCollection,
    write: &DavWrite,
    extension: &str,
) -> Result<String> {
    let name = write.name.as_str();
    let count: i64 =
        tx.query_row("SELECT count(*) FROM dav_resources WHERE collection_id = ?1", [target.id], |row| row.get(0))?;
    if count >= DAV_RESOURCES_PER_COLLECTION {
        return Err(StoreError::QuotaExceeded);
    }
    let free: bool = tx.query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM dav_resources WHERE collection_id = ?1 AND name = ?2)",
        params![target.id, name],
        |row| row.get(0),
    )?;
    let new_name = if free { name.to_owned() } else { new_entry_name(tx, target.id, &write.uid, extension)? };
    let old_change = next_change(tx, source_id)?;
    tx.execute(
        "INSERT OR REPLACE INTO dav_tombstones (collection_id, name, change) VALUES (?1, ?2, ?3)",
        params![source_id, name, old_change],
    )?;
    let change = next_change(tx, target.id)?;
    let etag = dav_etag(&write.content);
    tx.execute(
        "UPDATE dav_resources SET collection_id = ?1, name = ?2, uid = ?3, etag = ?4, content = ?5,
             starts_at = ?6, ends_at = ?7, size = ?8, modified_at = ?9, change = ?10
         WHERE id = ?11",
        params![
            target.id,
            new_name,
            write.uid,
            etag,
            write.content,
            write.starts_at,
            write.ends_at,
            write.content.len() as i64,
            now(),
            change,
            id
        ],
    )?;
    tx.execute("DELETE FROM dav_tombstones WHERE collection_id = ?1 AND name = ?2", params![target.id, new_name])?;
    log.entry(tx, target, id, Some(&write.component), Some(&write.component))?;
    Ok(etag)
}

/// Changes a collection's properties and counts it as a change, as clients compare CTags to know
/// that something changed, properties included.
pub(crate) fn apply_collection_update(
    tx: &Transaction<'_>,
    collection_id: i64,
    update: &DavCollectionUpdate,
) -> Result<()> {
    if let Some(name) = &update.display_name {
        tx.execute("UPDATE dav_collections SET display_name = ?1 WHERE id = ?2", params![name.trim(), collection_id])?;
    }
    if let Some(description) = &update.description {
        tx.execute(
            "UPDATE dav_collections SET description = ?1 WHERE id = ?2",
            params![description.trim(), collection_id],
        )?;
    }
    if let Some(color) = &update.color {
        tx.execute("UPDATE dav_collections SET color = ?1 WHERE id = ?2", params![color, collection_id])?;
    }
    if let Some(order) = update.sort_order {
        tx.execute("UPDATE dav_collections SET sort_order = ?1 WHERE id = ?2", params![order, collection_id])?;
    }
    if let Some(timezone) = &update.timezone {
        tx.execute("UPDATE dav_collections SET timezone = ?1 WHERE id = ?2", params![timezone, collection_id])?;
    }
    if let Some(visible) = update.is_visible {
        tx.execute("UPDATE dav_collections SET is_visible = ?1 WHERE id = ?2", params![visible, collection_id])?;
    }
    next_change(tx, collection_id)?;
    Ok(())
}

impl Store {
    /// An account's collections of one kind. The first time, a default collection is made.
    pub async fn dav_collections(
        &self,
        account_id: i64,
        kind: DavKind,
        default: NewDavCollection,
    ) -> Result<Vec<DavCollection>> {
        let (list, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let exists: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM dav_collections WHERE account_id = ?1 AND kind = ?2)",
                    params![account_id, kind.as_str()],
                    |row| row.get(0),
                )?;
                if !exists {
                    insert_collection(tx, &mut log, account_id, kind, &default)?;
                }
                let mut stmt = tx.prepare(&format!(
                    "SELECT {COLLECTION_COLUMNS} FROM dav_collections c WHERE c.account_id = ?1 AND c.kind = ?2
                     ORDER BY c.sort_order, c.id"
                ))?;
                let rows = stmt.query_map(params![account_id, kind.as_str()], collection_row)?;
                Ok((rows.collect::<Result<Vec<_>, _>>()?, log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(list)
    }

    pub async fn dav_collection(&self, account_id: i64, kind: DavKind, slug: &str) -> Result<Option<DavCollection>> {
        let slug = slug.to_owned();
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {COLLECTION_COLUMNS} FROM dav_collections c
                         WHERE c.account_id = ?1 AND c.kind = ?2 AND c.slug = ?3"
                    ),
                    params![account_id, kind.as_str(), slug],
                    collection_row,
                )
                .optional()?)
        })
        .await
    }

    pub async fn dav_create_collection(
        &self,
        account_id: i64,
        kind: DavKind,
        new: NewDavCollection,
    ) -> Result<DavCollection> {
        let (collection, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let id = insert_collection(tx, &mut log, account_id, kind, &new)?;
                Ok((own_collection(tx, account_id, id)?, log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(collection)
    }

    pub async fn dav_update_collection(
        &self,
        account_id: i64,
        collection_id: i64,
        update: DavCollectionUpdate,
    ) -> Result<DavCollection> {
        let (collection, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let collection = own_collection(tx, account_id, collection_id)?;
                apply_collection_update(tx, collection_id, &update)?;
                log.collection(tx, &collection, "updated")?;
                Ok((own_collection(tx, account_id, collection_id)?, log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(collection)
    }

    pub async fn dav_delete_collection(&self, account_id: i64, collection_id: i64) -> Result<()> {
        let modseq = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let collection = own_collection(tx, account_id, collection_id)?;
                delete_collection(tx, &mut log, &collection)?;
                Ok(log.modseq())
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(())
    }

    /// The entries of a collection, without their content.
    pub async fn dav_resources(&self, account_id: i64, collection_id: i64) -> Result<Vec<DavResourceInfo>> {
        self.read(move |conn| {
            own_collection(conn, account_id, collection_id)?;
            let mut stmt = conn
                .prepare(&format!("SELECT {INFO_COLUMNS} FROM dav_resources WHERE collection_id = ?1 ORDER BY name"))?;
            let rows = stmt.query_map([collection_id], info_row)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Entries with their content: the named ones, or all of them.
    pub async fn dav_resource_contents(
        &self,
        account_id: i64,
        collection_id: i64,
        names: Option<Vec<String>>,
    ) -> Result<Vec<DavResource>> {
        self.read(move |conn| {
            own_collection(conn, account_id, collection_id)?;
            let read = |row: &rusqlite::Row<'_>| Ok(DavResource { info: info_row(row)?, content: row.get(8)? });
            let sql = format!("SELECT {INFO_COLUMNS}, content FROM dav_resources WHERE collection_id = ?1");
            match names {
                None => {
                    let mut stmt = conn.prepare(&format!("{sql} ORDER BY name"))?;
                    let rows = stmt.query_map([collection_id], read)?;
                    Ok(rows.collect::<Result<_, _>>()?)
                }
                Some(names) => {
                    let mut stmt = conn.prepare_cached(&format!("{sql} AND name = ?2"))?;
                    let mut found = Vec::new();
                    for name in names {
                        if let Some(resource) = stmt.query_row(params![collection_id, name], read).optional()? {
                            found.push(resource);
                        }
                    }
                    Ok(found)
                }
            }
        })
        .await
    }

    /// Stores an entry under a condition, and keeps UIDs unique within the collection.
    pub async fn dav_put(
        &self,
        account_id: i64,
        collection_id: i64,
        write: DavWrite,
        condition: DavPrecondition,
    ) -> Result<DavWriteOutcome> {
        if !valid_segment(&write.name) {
            return Err(StoreError::Invalid(format!("'{}' cannot be part of a URL", write.name)));
        }
        if write.content.len() > DAV_RESOURCE_MAX_BYTES {
            return Err(StoreError::QuotaExceeded);
        }
        let (outcome, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let collection = own_collection(tx, account_id, collection_id)?;
                let (outcome, _) = put_entry(tx, &mut log, &collection, &write, &condition)?;
                Ok((outcome, log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(outcome)
    }

    /// Deletes an entry. `Ok(false)` when it is not there, or the condition does not hold.
    pub async fn dav_delete(
        &self,
        account_id: i64,
        collection_id: i64,
        name: &str,
        if_match: Option<String>,
    ) -> Result<bool> {
        let name = name.to_owned();
        let (deleted, modseq) = self
            .write(move |tx| {
                let mut log = ChangeLog::new(account_id);
                let collection = own_collection(tx, account_id, collection_id)?;
                let deleted = delete_entry(tx, &mut log, &collection, &name, if_match.as_deref())?;
                Ok((deleted, log.modseq()))
            })
            .await?;
        self.notify_log(account_id, modseq);
        Ok(deleted)
    }

    /// What changed after `since`, for sync-collection.
    pub async fn dav_changes(&self, account_id: i64, collection_id: i64, since: i64) -> Result<DavChanges> {
        self.read(move |conn| {
            let collection = own_collection(conn, account_id, collection_id)?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {INFO_COLUMNS} FROM dav_resources WHERE collection_id = ?1 AND change > ?2 ORDER BY name"
            ))?;
            let changed = stmt.query_map(params![collection_id, since], info_row)?.collect::<Result<_, _>>()?;
            let mut stmt =
                conn.prepare("SELECT name FROM dav_tombstones WHERE collection_id = ?1 AND change > ?2 ORDER BY name")?;
            let deleted = stmt.query_map(params![collection_id, since], |row| row.get(0))?.collect::<Result<_, _>>()?;
            Ok(DavChanges { change: collection.change, changed, deleted })
        })
        .await
    }
}

/// Stores an entry of a collection that is known to belong to the account. Returns the outcome and,
/// when it was written, the entry's row id.
pub(crate) fn put_entry(
    tx: &Transaction<'_>,
    log: &mut ChangeLog,
    collection: &DavCollection,
    write: &DavWrite,
    condition: &DavPrecondition,
) -> Result<(DavWriteOutcome, Option<i64>)> {
    let current: Option<(i64, String, String)> = tx
        .query_row(
            "SELECT id, etag, component FROM dav_resources WHERE collection_id = ?1 AND name = ?2",
            params![collection.id, write.name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let matches = match (&condition.if_match, &current) {
        (Some(wanted), Some((_, etag, _))) => wanted == "*" || wanted == etag,
        (Some(_), None) => false,
        (None, _) => true,
    };
    if !matches || (condition.if_none_match_any && current.is_some()) {
        return Ok((DavWriteOutcome::PreconditionFailed, None));
    }
    let other: Option<String> = tx
        .query_row(
            "SELECT name FROM dav_resources WHERE collection_id = ?1 AND uid = ?2 AND name <> ?3",
            params![collection.id, write.uid, write.name],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(name) = other {
        return Ok((DavWriteOutcome::UidTaken { name }, None));
    }
    if current.is_none() {
        let count: i64 =
            tx.query_row("SELECT count(*) FROM dav_resources WHERE collection_id = ?1", [collection.id], |row| {
                row.get(0)
            })?;
        if count >= DAV_RESOURCES_PER_COLLECTION {
            return Err(StoreError::QuotaExceeded);
        }
    }
    let etag = dav_etag(&write.content);
    let change = next_change(tx, collection.id)?;
    let id: i64 = tx.query_row(
        "INSERT INTO dav_resources (collection_id, name, uid, etag, content, component, starts_at, ends_at, size,
             modified_at, change)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (collection_id, name) DO UPDATE SET uid = excluded.uid, etag = excluded.etag,
             content = excluded.content, component = excluded.component, starts_at = excluded.starts_at,
             ends_at = excluded.ends_at, size = excluded.size, modified_at = excluded.modified_at,
             change = excluded.change
         RETURNING id",
        params![
            collection.id,
            write.name,
            write.uid,
            etag,
            write.content,
            write.component,
            write.starts_at,
            write.ends_at,
            write.content.len() as i64,
            now(),
            change
        ],
        |row| row.get(0),
    )?;
    tx.execute(
        "DELETE FROM dav_tombstones WHERE collection_id = ?1 AND name = ?2",
        params![collection.id, write.name],
    )?;
    log.entry(
        tx,
        collection,
        id,
        current.as_ref().map(|(_, _, component)| component.as_str()),
        Some(&write.component),
    )?;
    Ok((
        if current.is_some() { DavWriteOutcome::Updated { etag } } else { DavWriteOutcome::Created { etag } },
        Some(id),
    ))
}

/// Deletes an entry and leaves a tombstone for sync-collection. `false` when it is not there or
/// the condition does not hold.
pub(crate) fn delete_entry(
    tx: &Transaction<'_>,
    log: &mut ChangeLog,
    collection: &DavCollection,
    name: &str,
    if_match: Option<&str>,
) -> Result<bool> {
    let current: Option<(i64, String, String)> = tx
        .query_row(
            "SELECT id, etag, component FROM dav_resources WHERE collection_id = ?1 AND name = ?2",
            params![collection.id, name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((id, etag, component)) = current else { return Ok(false) };
    if if_match.is_some_and(|wanted| wanted != "*" && wanted != etag) {
        return Ok(false);
    }
    let change = next_change(tx, collection.id)?;
    tx.execute("DELETE FROM dav_resources WHERE id = ?1", [id])?;
    tx.execute(
        "INSERT OR REPLACE INTO dav_tombstones (collection_id, name, change) VALUES (?1, ?2, ?3)",
        params![collection.id, name, change],
    )?;
    log.entry(tx, collection, id, Some(&component), None)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{NewAccount, Role};

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

    fn event(name: &str, uid: &str, summary: &str) -> DavWrite {
        DavWrite {
            name: name.into(),
            content: format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            ),
            uid: uid.into(),
            component: "VEVENT".into(),
            starts_at: None,
            ends_at: None,
        }
    }

    fn calendar() -> NewDavCollection {
        NewDavCollection {
            slug: "personal".into(),
            display_name: "Kalender".into(),
            components: vec!["VEVENT".into(), "VTODO".into()],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn entries_sync_with_etags_and_tombstones() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.de").await;
        let calendars = store.dav_collections(mini, DavKind::Calendar, calendar()).await.unwrap();
        assert_eq!(calendars.len(), 1, "a default calendar appears");
        let id = calendars[0].id;
        assert_eq!(store.dav_collections(mini, DavKind::Calendar, calendar()).await.unwrap().len(), 1);

        let created =
            store.dav_put(mini, id, event("a.ics", "a", "Tierarzt"), DavPrecondition::default()).await.unwrap();
        let DavWriteOutcome::Created { etag } = created else { panic!("{created:?}") };
        let start = store.dav_changes(mini, id, 0).await.unwrap();
        assert_eq!((start.change, start.changed.len()), (1, 1));

        let only_new = DavPrecondition { if_none_match_any: true, ..Default::default() };
        let again = store.dav_put(mini, id, event("a.ics", "a", "Tierarzt"), only_new).await.unwrap();
        assert_eq!(again, DavWriteOutcome::PreconditionFailed);
        let stale = DavPrecondition { if_match: Some("\"old\"".into()), ..Default::default() };
        assert_eq!(
            store.dav_put(mini, id, event("a.ics", "a", "x"), stale).await.unwrap(),
            DavWriteOutcome::PreconditionFailed
        );
        let fresh = DavPrecondition { if_match: Some(etag.clone()), ..Default::default() };
        let updated = store.dav_put(mini, id, event("a.ics", "a", "Tierarzt um 10"), fresh).await.unwrap();
        assert!(matches!(updated, DavWriteOutcome::Updated { etag: ref new } if *new != etag));
        let taken = store.dav_put(mini, id, event("b.ics", "a", "Doppelt"), DavPrecondition::default()).await.unwrap();
        assert_eq!(taken, DavWriteOutcome::UidTaken { name: "a.ics".into() });

        store.dav_put(mini, id, event("b.ics", "b", "Friseur"), DavPrecondition::default()).await.unwrap();
        assert!(store.dav_delete(mini, id, "a.ics", None).await.unwrap());
        let later = store.dav_changes(mini, id, start.change).await.unwrap();
        assert_eq!(later.deleted, vec!["a.ics"]);
        assert_eq!(later.changed.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["b.ics"]);
        let contents = store.dav_resource_contents(mini, id, Some(vec!["b.ics".into(), "a.ics".into()])).await.unwrap();
        assert_eq!(contents.len(), 1);
        assert!(contents[0].content.contains("SUMMARY:Friseur"));
    }

    #[tokio::test]
    async fn collections_belong_to_their_account() {
        let (store, _dir) = store().await;
        let mini = account(&store, "mini@example.de").await;
        let leni = account(&store, "leni@example.de").await;
        let id = store.dav_collections(mini, DavKind::Calendar, calendar()).await.unwrap()[0].id;
        assert!(matches!(store.dav_resources(leni, id).await, Err(StoreError::NotFound(_))));
        assert!(matches!(
            store.dav_put(leni, id, event("x.ics", "x", "fremd"), DavPrecondition::default()).await,
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store
                .dav_create_collection(
                    mini,
                    DavKind::Calendar,
                    NewDavCollection { slug: "../x".into(), ..Default::default() }
                )
                .await,
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.dav_create_collection(mini, DavKind::Calendar, calendar()).await,
            Err(StoreError::Conflict(_))
        ));
        store.dav_delete_collection(mini, id).await.unwrap();
        assert!(store.dav_collection(mini, DavKind::Calendar, "personal").await.unwrap().is_none());
    }
}
