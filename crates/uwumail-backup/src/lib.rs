//! Backups of a UwUMail server: the database, every mail blob and the other files of the data
//! directory, deduplicated and (by default) encrypted, on an SFTP server.
//!
//! The first backup uploads everything; later ones only what is new. Mail blobs are already stored
//! by content, so each one is uploaded once. The database copy is cut into content-defined chunks,
//! so a day of new mail changes only a few of them. Old snapshots go by the retention rules, and
//! objects no snapshot needs anymore go with them.

pub mod format;
pub mod retention;
pub mod service;
pub mod sftp;
pub mod storage;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use uwumail_store::{BlobHash, Store};

pub use format::{Codec, Manifest, RepoConfig, RepoKey};
pub use retention::Retention;
pub use service::{BackupSettings, BackupStatus, Backups};
pub use sftp::{Login, Target};
pub use storage::Storage;

use crate::format::{CONFIG_PATH, FileEntry, object_path};

const CHUNK_MIN: usize = 16 * 1024;
const CHUNK_AVG: usize = 64 * 1024;
const CHUNK_MAX: usize = 256 * 1024;
/// Files bigger than this in the data directory are not part of a backup; they are not ours.
const FILE_MAX: u64 = 64 * 1024 * 1024;
const TEMP_DIR: &str = "backup-tmp";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the recovery key does not fit this backup")]
    WrongKey,
    #[error("the backup is damaged: {0}")]
    Damaged(String),
    #[error("encryption failed")]
    Crypto,
    #[error("{0}")]
    Storage(String),
    #[error("the backup server refused the login for {0}")]
    LoginRefused(String),
    #[error("the backup server's host key changed from {expected} to {seen}; if that is expected, confirm the new key")]
    HostKeyChanged { expected: String, seen: String },
    #[error("{0}")]
    Config(String),
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] uwumail_store::StoreError),
}

/// An opened repository.
pub struct Repository {
    pub storage: Storage,
    pub codec: Codec,
    pub config: RepoConfig,
}

impl Repository {
    /// Opens the repository at the storage, creating it when there is none yet. `key` is needed for
    /// encrypted ones; a new repository is encrypted exactly when a key is given.
    pub async fn open(storage: Storage, key: Option<RepoKey>, now: i64) -> Result<Repository, Error> {
        Self::open_with(storage, key, Some(now)).await
    }

    /// Opens a repository that must exist already, e.g. for a restore.
    pub async fn open_existing(storage: Storage, key: Option<RepoKey>) -> Result<Repository, Error> {
        Self::open_with(storage, key, None).await
    }

    /// Whether the repository at the storage is encrypted, without needing the key.
    pub async fn is_encrypted(storage: &Storage) -> Result<bool, Error> {
        let bytes =
            storage.read(CONFIG_PATH).await?.ok_or_else(|| Error::Config("there is no UwUMail backup there".into()))?;
        let config: RepoConfig = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Damaged(format!("{CONFIG_PATH} is not a UwUMail backup")))?;
        Ok(config.encrypted)
    }

    async fn open_with(storage: Storage, key: Option<RepoKey>, create_at: Option<i64>) -> Result<Repository, Error> {
        let config = match storage.read(CONFIG_PATH).await? {
            Some(bytes) => serde_json::from_slice::<RepoConfig>(&bytes)
                .map_err(|_| Error::Damaged(format!("{CONFIG_PATH} is not a UwUMail backup")))?,
            None if create_at.is_none() => return Err(Error::Config("there is no UwUMail backup there".into())),
            None => {
                let now = create_at.unwrap_or_default();
                let config = Codec::config_for(key.as_ref(), now);
                storage.write(CONFIG_PATH, &serde_json::to_vec_pretty(&config).expect("config serializes")).await?;
                config
            }
        };
        if config.format > format::FORMAT {
            return Err(Error::Config("this backup was written by a newer UwUMail server".into()));
        }
        let codec = Codec::new(&config, key)?;
        Ok(Repository { storage, codec, config })
    }

    /// Ids of every object in the repository.
    async fn object_ids(&self) -> Result<HashSet<String>, Error> {
        let mut ids = HashSet::new();
        for prefix in self.storage.list("data").await? {
            for id in self.storage.list(&format!("data/{prefix}")).await? {
                ids.insert(id);
            }
        }
        Ok(ids)
    }

    /// Stores content unless the repository has it already; returns its id and the bytes uploaded.
    async fn put(&self, content: &[u8], known: &mut HashSet<String>) -> Result<(String, u64), Error> {
        let id = self.codec.id_for(content);
        if known.contains(&id) {
            return Ok((id, 0));
        }
        let object = self.codec.encode(content)?;
        self.storage.write(&object_path(&id), &object).await?;
        known.insert(id.clone());
        Ok((id, object.len() as u64))
    }

    async fn get(&self, id: &str) -> Result<Vec<u8>, Error> {
        let object = self
            .storage
            .read(&object_path(id))
            .await?
            .ok_or_else(|| Error::Damaged(format!("the object {id} is missing")))?;
        self.codec.decode(&object)
    }

    /// Snapshot names, oldest first.
    pub async fn snapshots(&self) -> Result<Vec<String>, Error> {
        let mut names = self.storage.list("snapshots").await?;
        names.sort();
        Ok(names)
    }

    pub async fn manifest(&self, name: &str) -> Result<Manifest, Error> {
        let object = self
            .storage
            .read(&format!("snapshots/{name}"))
            .await?
            .ok_or_else(|| Error::Config(format!("there is no snapshot {name}")))?;
        serde_json::from_slice(&self.codec.decode(&object)?)
            .map_err(|_| Error::Damaged(format!("the snapshot {name} cannot be read")))
    }
}

/// What a backup did.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BackupReport {
    pub snapshot: String,
    pub uploaded: u64,
    pub total: u64,
    pub removed_snapshots: usize,
    pub removed_objects: usize,
}

fn random_suffix() -> String {
    let mut bytes = [0u8; 3];
    aws_lc_rs::rand::fill(&mut bytes).expect("the system random generator works");
    hex::encode(bytes)
}

/// The data directory's files besides the database, the blobs and our own scratch space.
fn other_files(data_dir: &Path) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    let mut pending = vec![(String::new(), data_dir.to_path_buf())];
    while let Some((prefix, dir)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if prefix.is_empty() && (name == "blobs" || name == TEMP_DIR || name.starts_with("uwumail.db")) {
                continue;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                pending.push((relative, entry.path()));
            } else if kind.is_file() && entry.metadata().is_ok_and(|meta| meta.len() <= FILE_MAX) {
                found.push((relative, entry.path()));
            }
        }
    }
    found.sort();
    found
}

/// Backs up the server into the repository and applies the retention rules.
pub async fn backup(
    store: &Store,
    repo: &Repository,
    hostname: &str,
    version: &str,
    retention: Retention,
    now: i64,
) -> Result<BackupReport, Error> {
    let _pause = store.pause_blob_cleanup();
    let mut known = repo.object_ids().await?;
    let mut uploaded = 0;

    // The database: a consistent copy, cut into chunks.
    let temp = store.data_dir().join(TEMP_DIR);
    tokio::fs::create_dir_all(&temp).await?;
    let copy = temp.join("uwumail.db");
    store.snapshot_database(copy.clone()).await?;
    let (sender, mut chunks) = tokio::sync::mpsc::channel::<Result<Vec<u8>, Error>>(2);
    let reading = copy.clone();
    let chunker = tokio::task::spawn_blocking(move || {
        let file = match std::fs::File::open(&reading) {
            Ok(file) => file,
            Err(err) => {
                let _ = sender.blocking_send(Err(err.into()));
                return;
            }
        };
        for chunk in fastcdc::v2020::StreamCDC::new(file, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX) {
            let item = chunk
                .map(|chunk| chunk.data)
                .map_err(|err| Error::Storage(format!("reading the database copy: {err}")));
            if sender.blocking_send(item).is_err() {
                return;
            }
        }
    });
    let mut database = Vec::new();
    let mut database_size = 0;
    while let Some(chunk) = chunks.recv().await {
        let chunk = chunk?;
        database_size += chunk.len() as u64;
        let (id, bytes) = repo.put(&chunk, &mut known).await?;
        uploaded += bytes;
        database.push(id);
    }
    chunker.await.map_err(|err| Error::Storage(err.to_string()))?;
    let _ = tokio::fs::remove_file(&copy).await;

    // Mail blobs, each on its own.
    let mut blobs = Vec::new();
    let mut blobs_size = 0;
    for (hash, size) in store.blob_hashes().await? {
        blobs_size += size;
        let id = repo.codec.id_for_hash(hash.as_str());
        if !known.contains(&id) {
            let content = store.blob(&hash).await?;
            let (stored, bytes) = repo.put(&content, &mut known).await?;
            if stored != id {
                return Err(Error::Damaged(format!("the blob {hash} on disk does not match its name")));
            }
            uploaded += bytes;
        }
        blobs.push(hash.as_str().to_owned());
    }

    // Everything else: certificates, keys and settings files.
    let mut files = Vec::new();
    for (path, full) in other_files(store.data_dir()) {
        let Ok(content) = tokio::fs::read(&full).await else { continue };
        let (id, bytes) = repo.put(&content, &mut known).await?;
        uploaded += bytes;
        files.push(FileEntry { path, id, size: content.len() as u64 });
    }

    let manifest = Manifest {
        format: format::FORMAT,
        created_at: now,
        hostname: hostname.to_owned(),
        version: version.to_owned(),
        database,
        database_size,
        blobs,
        blobs_size,
        files,
        uploaded,
    };
    let name = format!("{now:012}-{}", random_suffix());
    let encoded = repo.codec.encode(&serde_json::to_vec(&manifest).expect("manifests serialize"))?;
    repo.storage.write(&format!("snapshots/{name}"), &encoded).await?;

    let (removed_snapshots, removed_objects) = prune(repo, retention).await?;
    Ok(BackupReport {
        snapshot: name,
        uploaded,
        total: database_size + blobs_size + manifest.files.iter().map(|file| file.size).sum::<u64>(),
        removed_snapshots,
        removed_objects,
    })
}

/// Removes snapshots the retention rules let go, then objects no snapshot needs.
pub async fn prune(repo: &Repository, retention: Retention) -> Result<(usize, usize), Error> {
    let names = repo.snapshots().await?;
    let mut manifests = Vec::new();
    for name in &names {
        manifests.push((name.clone(), repo.manifest(name).await?));
    }
    let times: Vec<i64> = manifests.iter().map(|(_, manifest)| manifest.created_at).collect();
    let kept = retention::keep(&times, retention);
    let mut removed_snapshots = 0;
    let mut needed = HashSet::new();
    for (index, (name, manifest)) in manifests.iter().enumerate() {
        if kept.contains(&index) {
            needed.extend(manifest.database.iter().cloned());
            needed.extend(manifest.blobs.iter().map(|hash| repo.codec.id_for_hash(hash)));
            needed.extend(manifest.files.iter().map(|file| file.id.clone()));
        } else {
            repo.storage.remove(&format!("snapshots/{name}")).await?;
            removed_snapshots += 1;
        }
    }
    let mut removed_objects = 0;
    if !manifests.is_empty() {
        for id in repo.object_ids().await? {
            if !needed.contains(&id) {
                repo.storage.remove(&object_path(&id)).await?;
                removed_objects += 1;
            }
        }
    }
    Ok((removed_snapshots, removed_objects))
}

/// Writes a snapshot into an empty data directory: the database, the blobs and the other files.
pub async fn restore(repo: &Repository, snapshot: &str, data_dir: &Path) -> Result<Manifest, Error> {
    if tokio::fs::try_exists(data_dir.join("uwumail.db")).await? {
        return Err(Error::Config(format!(
            "{} already holds a server; restore into an empty directory",
            data_dir.display()
        )));
    }
    let manifest = repo.manifest(snapshot).await?;
    tokio::fs::create_dir_all(data_dir).await?;

    let partial = data_dir.join("uwumail.db.restoring");
    let mut database = tokio::fs::File::create(&partial).await?;
    for id in &manifest.database {
        let chunk = repo.get(id).await?;
        tokio::io::AsyncWriteExt::write_all(&mut database, &chunk).await?;
    }
    tokio::io::AsyncWriteExt::flush(&mut database).await?;
    drop(database);

    for hash in &manifest.blobs {
        let content = repo.get(&repo.codec.id_for_hash(hash)).await?;
        if BlobHash::of(&content).as_str() != hash {
            return Err(Error::Damaged(format!("the blob {hash} does not match its content")));
        }
        let path = data_dir.join("blobs").join(&hash[0..2]).join(&hash[2..4]).join(hash);
        tokio::fs::create_dir_all(path.parent().expect("blob paths have a parent")).await?;
        tokio::fs::write(path, content).await?;
    }
    for file in &manifest.files {
        let path = data_dir.join(&file.path);
        if file.path.split('/').any(|part| part == "..") {
            return Err(Error::Damaged(format!("the file name {} leaves the data directory", file.path)));
        }
        tokio::fs::create_dir_all(path.parent().unwrap_or(data_dir)).await?;
        tokio::fs::write(path, repo.get(&file.id).await?).await?;
    }
    tokio::fs::rename(partial, data_dir.join("uwumail.db")).await?;
    Ok(manifest)
}

/// Checks that every object a snapshot needs is there. Returns the missing ids.
pub async fn check(repo: &Repository, snapshot: &str) -> Result<Vec<String>, Error> {
    let manifest = repo.manifest(snapshot).await?;
    let present = repo.object_ids().await?;
    let wanted = manifest
        .database
        .iter()
        .cloned()
        .chain(manifest.blobs.iter().map(|hash| repo.codec.id_for_hash(hash)))
        .chain(manifest.files.iter().map(|file| file.id.clone()));
    Ok(wanted.filter(|id| !present.contains(id)).collect())
}
