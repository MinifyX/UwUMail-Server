//! Putting one person's mail back from a snapshot, while the server keeps running.
//!
//! A full restore replaces everything and needs a restart. This takes a snapshot apart instead:
//! only its database is fetched and put together in a folder of its own, and read like a book --
//! never opened as this server's store, which would migrate it. From it come the person's folders
//! and messages; each message is fetched by its hash (from this server's own blobs when it still
//! has them, from the backup otherwise), checked against that hash, and stored again the ordinary
//! way, in a new folder "Restored <date>" with the old folders below it. Nothing the person has now
//! is changed or replaced, and a message they still have is not brought twice.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;
use uwumail_store::{BlobHash, IngestRequest, MailboxTarget, Store, StoreError};

use crate::{BLOB_MAX, Error, Manifest, Repository};

/// A folder of a person in a snapshot.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotFolder {
    pub id: i64,
    pub parent_id: Option<i64>,
    /// The names from the top down, e.g. `["Projects", "2025"]`.
    pub path: Vec<String>,
    /// `inbox`, `sent`, … for the folders that have a role.
    pub role: Option<String>,
    pub emails: i64,
}

/// A person in a snapshot, with their folders.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotPerson {
    pub login: String,
    pub name: String,
    pub emails: i64,
    pub folders: Vec<SnapshotFolder>,
}

/// What putting a mailbox back did.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MailboxRestoreReport {
    /// The folder everything went into.
    pub folder: String,
    pub restored: u64,
    /// Messages the person still has, left out.
    pub skipped: u64,
    pub bytes: u64,
}

/// Where a restore of a mailbox stands, as it goes.
#[derive(Debug, Clone, Copy, Default)]
pub struct MailboxProgress {
    pub total: u64,
    pub done: u64,
    pub restored: u64,
    pub skipped: u64,
}

/// One message of the snapshot, and the folders it was in.
struct SnapshotMail {
    blob: String,
    received_at: i64,
    message_id: Option<String>,
    keywords: Vec<String>,
    mailboxes: Vec<i64>,
}

/// Fetches the database of a snapshot into `path`, telling `progress` how many bytes are there.
/// Returns the snapshot's manifest.
pub async fn fetch_database(
    repo: &Repository,
    snapshot: &str,
    path: &Path,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<Manifest, Error> {
    let manifest = repo.manifest(snapshot).await?;
    crate::fits_this_server(&manifest)?;
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = tokio::fs::remove_file(format!("{}{suffix}", path.display())).await;
    }
    crate::write_database(repo, &manifest, path, progress).await?;
    Ok(manifest)
}

/// Opens a snapshot's database for reading only. The file came from the backup server; SQLite is
/// told not to trust anything in it that could run code.
fn open(path: &Path) -> Result<Connection, Error> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).map_err(|err| damaged(&err))?;
    conn.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF;").map_err(|err| damaged(&err))?;
    let quick: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0)).map_err(|err| damaged(&err))?;
    if quick != "ok" {
        return Err(Error::Damaged(format!("the snapshot's database is broken: {quick}")));
    }
    Ok(conn)
}

fn damaged(err: &rusqlite::Error) -> Error {
    Error::Damaged(format!("the snapshot's database cannot be read: {err}"))
}

/// The folders of one account in the snapshot, each with its path.
fn folders_of(conn: &Connection, account_id: i64) -> rusqlite::Result<Vec<SnapshotFolder>> {
    let mut stmt = conn.prepare(
        "SELECT m.id, m.parent_id, m.name, m.role,
                (SELECT count(*) FROM email_mailboxes em WHERE em.mailbox_id = m.id)
         FROM mailboxes m WHERE m.account_id = ?1",
    )?;
    type Row = (i64, Option<i64>, String, Option<String>, i64);
    let rows: Vec<Row> = stmt
        .query_map([account_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let by_id: HashMap<i64, (Option<i64>, String)> =
        rows.iter().map(|(id, parent, name, _, _)| (*id, (*parent, name.clone()))).collect();
    let mut folders: Vec<SnapshotFolder> = rows
        .into_iter()
        .map(|(id, parent_id, name, role, emails)| {
            let mut path = vec![name];
            let mut current = parent_id;
            // A parent chain that loops is a damaged database; it ends after as many steps as
            // there are folders.
            for _ in 0..by_id.len() {
                let Some((parent, name)) = current.and_then(|id| by_id.get(&id)) else { break };
                path.insert(0, name.clone());
                current = *parent;
            }
            SnapshotFolder { id, parent_id, path, role, emails }
        })
        .collect();
    // The inbox first, then as the portal lists them.
    folders.sort_by(|a, b| {
        (a.role.as_deref() != Some("inbox"), a.path.iter().map(|part| part.to_lowercase()).collect::<Vec<_>>())
            .cmp(&(b.role.as_deref() != Some("inbox"), b.path.iter().map(|part| part.to_lowercase()).collect()))
    });
    Ok(folders)
}

/// Everybody in the snapshot who had a mailbox, with their folders.
pub async fn people(path: &Path) -> Result<Vec<SnapshotPerson>, Error> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let conn = open(&path)?;
        let read = || -> rusqlite::Result<Vec<SnapshotPerson>> {
            let mut stmt = conn.prepare(
                "SELECT id, login, display_name FROM accounts
                 WHERE EXISTS (SELECT 1 FROM mailboxes WHERE mailboxes.account_id = accounts.id)
                 ORDER BY login",
            )?;
            let accounts: Vec<(i64, String, String)> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut people = Vec::new();
            for (id, login, name) in accounts {
                let folders = folders_of(&conn, id)?;
                let emails =
                    conn.query_row("SELECT count(*) FROM emails WHERE account_id = ?1", [id], |row| row.get(0))?;
                people.push(SnapshotPerson { login, name, emails, folders });
            }
            Ok(people)
        };
        read().map_err(|err| damaged(&err))
    })
    .await
    .map_err(|err| Error::Storage(err.to_string()))?
}

/// The messages of one person in the chosen folders (all when `None`), oldest first.
fn mails_of(
    path: &Path,
    login: &str,
    chosen: Option<&[i64]>,
) -> Result<(Vec<SnapshotFolder>, Vec<SnapshotMail>), Error> {
    let conn = open(path)?;
    let read = || -> rusqlite::Result<Option<(Vec<SnapshotFolder>, Vec<SnapshotMail>)>> {
        let Some(account_id) = conn
            .query_row("SELECT id FROM accounts WHERE login = ?1", [login], |row| row.get::<_, i64>(0))
            .optional()?
        else {
            return Ok(None);
        };
        let mut folders = folders_of(&conn, account_id)?;
        if let Some(chosen) = chosen {
            folders.retain(|folder| chosen.contains(&folder.id));
        }
        let wanted: HashSet<i64> = folders.iter().map(|folder| folder.id).collect();
        let mut stmt = conn.prepare(
            "SELECT e.id, e.blob_hash, e.received_at, e.message_id, em.mailbox_id
             FROM emails e JOIN email_mailboxes em ON em.email_id = e.id
             WHERE e.account_id = ?1 ORDER BY e.received_at, e.id",
        )?;
        let mut mails: Vec<(i64, SnapshotMail)> = Vec::new();
        let mut index: HashMap<i64, usize> = HashMap::new();
        let rows = stmt.query_map([account_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        for row in rows {
            let (email, blob, received_at, message_id, mailbox) = row?;
            if !wanted.contains(&mailbox) {
                continue;
            }
            match index.get(&email) {
                Some(&at) => mails[at].1.mailboxes.push(mailbox),
                None => {
                    index.insert(email, mails.len());
                    let mail =
                        SnapshotMail { blob, received_at, message_id, keywords: Vec::new(), mailboxes: vec![mailbox] };
                    mails.push((email, mail));
                }
            }
        }
        let mut keywords = conn.prepare("SELECT keyword FROM email_keywords WHERE email_id = ?1")?;
        for (email, mail) in &mut mails {
            mail.keywords = keywords
                .query_map(params![*email], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                // Marked for deletion is what brought many a message here; it comes back without.
                .filter(|keyword| keyword != uwumail_store::DELETED_KEYWORD)
                .collect();
        }
        Ok(Some((folders, mails.into_iter().map(|(_, mail)| mail).collect())))
    };
    read().map_err(|err| damaged(&err))?.ok_or_else(|| Error::Config(format!("{login} is not in this snapshot")))
}

/// `2026-09-27` for a Unix time.
fn day_of(unix: i64) -> String {
    crate::s3::amz_date(unix)
        .get(..8)
        .map(|day| format!("{}-{}-{}", &day[..4], &day[4..6], &day[6..]))
        .unwrap_or_default()
}

/// The name of the folder a snapshot's mail comes back into.
pub fn restored_folder_name(snapshot_created_at: i64) -> String {
    format!("Restored {}", day_of(snapshot_created_at))
}

/// Finds or makes a folder by name below `parent`.
async fn folder_below(store: &Store, account_id: i64, parent: Option<i64>, name: &str) -> Result<i64, Error> {
    let existing = store
        .mailboxes(account_id)
        .await?
        .into_iter()
        .find(|mailbox| mailbox.parent_id == parent && mailbox.name.eq_ignore_ascii_case(name));
    Ok(match existing {
        Some(mailbox) => mailbox.id,
        None => store.create_mailbox(account_id, name, parent, None, 0, true).await?,
    })
}

/// A message's content: from this server's own blobs while it still has them, else from the
/// backup. Either way it must hash to what the snapshot says.
async fn content_of(store: &Store, repo: &Repository, hash: &str) -> Result<Vec<u8>, Error> {
    let parsed = BlobHash::parse(hash).map_err(|_| Error::Damaged(format!("{hash} is not a mail's name")))?;
    if let Ok(content) = store.blob(&parsed).await
        && BlobHash::of(&content).as_str() == hash
    {
        return Ok(content);
    }
    let content = repo.get(&repo.codec.id_for_hash(hash), BLOB_MAX).await?;
    if BlobHash::of(&content).as_str() != hash {
        return Err(Error::Damaged(format!("the blob {hash} does not match its content")));
    }
    Ok(content)
}

/// Puts the mail of `login` in the snapshot whose database is at `database` back into the live
/// account `account_id`: into a folder "Restored <date>", the old folders below it. Only the
/// chosen folders of the snapshot when `folders` says which.
///
/// A full mailbox stops it with [`StoreError::QuotaExceeded`]; what was restored until then stays.
#[allow(clippy::too_many_arguments)]
pub async fn restore_mailbox(
    store: &Store,
    repo: &Repository,
    database: &Path,
    snapshot_created_at: i64,
    login: &str,
    account_id: i64,
    folders: Option<Vec<i64>>,
    progress: &mut (dyn FnMut(MailboxProgress) + Send),
) -> Result<MailboxRestoreReport, Error> {
    let (path, login_owned) = (database.to_owned(), login.to_owned());
    let (snapshot_folders, mails) =
        tokio::task::spawn_blocking(move || mails_of(&path, &login_owned, folders.as_deref()))
            .await
            .map_err(|err| Error::Storage(err.to_string()))??;
    let paths: HashMap<i64, Vec<String>> =
        snapshot_folders.into_iter().map(|folder| (folder.id, folder.path)).collect();

    let mut report = MailboxRestoreReport { folder: restored_folder_name(snapshot_created_at), ..Default::default() };
    let mut state = MailboxProgress { total: mails.len() as u64, ..Default::default() };
    progress(state);
    if mails.is_empty() {
        return Ok(report);
    }
    let top = folder_below(store, account_id, None, &report.folder).await?;
    // Folders are made when the first message for them comes, so an empty one stays away.
    let mut made: HashMap<i64, i64> = HashMap::new();
    for mail in mails {
        let message_id = mail.message_id.clone().filter(|id| !id.is_empty());
        let hash =
            BlobHash::parse(&mail.blob).map_err(|_| Error::Damaged(format!("{} is not a mail's name", mail.blob)))?;
        if store.holds_message(account_id, message_id, hash).await? {
            report.skipped += 1;
            state.skipped += 1;
        } else {
            let content = content_of(store, repo, &mail.blob).await?;
            let mut targets = Vec::new();
            for snapshot_folder in &mail.mailboxes {
                let id = match made.get(snapshot_folder) {
                    Some(id) => *id,
                    None => {
                        let mut parent = top;
                        for name in paths.get(snapshot_folder).into_iter().flatten() {
                            parent = folder_below(store, account_id, Some(parent), name).await?;
                        }
                        made.insert(*snapshot_folder, parent);
                        parent
                    }
                };
                targets.push(MailboxTarget::Id(id));
            }
            let size = content.len() as u64;
            let request = IngestRequest {
                account_id,
                raw: content,
                mailboxes: targets,
                keywords: mail.keywords,
                received_at: Some(mail.received_at),
            };
            match store.ingest(request).await {
                Ok(_) => {}
                Err(StoreError::QuotaExceeded) => return Err(Error::Store(StoreError::QuotaExceeded)),
                Err(err) => return Err(err.into()),
            }
            report.restored += 1;
            report.bytes += size;
            state.restored += 1;
        }
        state.done += 1;
        progress(state);
    }
    Ok(report)
}

/// The ids of the folders a list of paths names, each with the folders inside it. `Inbox` also
/// finds the folder with the inbox role, whatever it was called.
pub fn folders_named(person: &SnapshotPerson, names: &[String]) -> Result<Vec<i64>, Error> {
    let mut ids = Vec::new();
    for name in names {
        let wanted: Vec<String> = name.split('/').filter(|part| !part.is_empty()).map(str::to_lowercase).collect();
        let matches = |folder: &SnapshotFolder| {
            let path: Vec<String> = folder.path.iter().map(|part| part.to_lowercase()).collect();
            let by_role = wanted.len() == 1 && folder.role.as_deref() == Some(wanted[0].as_str());
            path == wanted || by_role
        };
        let found: Vec<&SnapshotFolder> = person.folders.iter().filter(|folder| matches(folder)).collect();
        if found.is_empty() {
            return Err(Error::Config(format!("{} has no folder {name} in this snapshot", person.login)));
        }
        for root in found {
            for folder in &person.folders {
                if folder.path.starts_with(&root.path) && !ids.contains(&folder.id) {
                    ids.push(folder.id);
                }
            }
        }
    }
    Ok(ids)
}
