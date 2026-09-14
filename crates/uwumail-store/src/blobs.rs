use std::fmt;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::{Result, Store, StoreError, now};

/// SHA-256 of a blob's content, hex encoded.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobHash(String);

impl BlobHash {
    pub fn of(bytes: &[u8]) -> BlobHash {
        BlobHash(hex::encode(Sha256::digest(bytes)))
    }

    pub fn parse(value: &str) -> Result<BlobHash> {
        if value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            Ok(BlobHash(value.to_owned()))
        } else {
            Err(StoreError::Invalid(format!("'{value}' is not a blob id")))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub(crate) struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub async fn open(root: PathBuf) -> Result<BlobStore> {
        tokio::fs::create_dir_all(root.join("tmp")).await?;
        Ok(BlobStore { root })
    }

    fn path(&self, hash: &BlobHash) -> PathBuf {
        let h = hash.as_str();
        self.root.join(&h[0..2]).join(&h[2..4]).join(h)
    }

    /// Writes the bytes unless a blob with the same content already exists.
    pub async fn put(&self, bytes: &[u8]) -> Result<BlobHash> {
        let hash = BlobHash::of(bytes);
        let path = self.path(&hash);
        if tokio::fs::try_exists(&path).await? {
            return Ok(hash);
        }
        let tmp = self.root.join("tmp").join(hex::encode(crate::random_bytes::<12>()));
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::create_dir_all(path.parent().expect("blob paths have a parent")).await?;
        if let Err(err) = tokio::fs::rename(&tmp, &path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            // Another writer may have stored the same content in the meantime.
            if !tokio::fs::try_exists(&path).await? {
                return Err(err.into());
            }
        }
        Ok(hash)
    }

    pub async fn get(&self, hash: &BlobHash) -> Result<Vec<u8>> {
        match tokio::fs::read(self.path(hash)).await {
            Ok(bytes) => Ok(bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(StoreError::NotFound(format!("blob {hash}"))),
            Err(err) => Err(err.into()),
        }
    }

    pub async fn delete(&self, hash: &BlobHash) -> Result<()> {
        match tokio::fs::remove_file(self.path(hash)).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

impl Store {
    /// Stores raw bytes and registers the blob so it can be referenced.
    pub(crate) async fn put_blob(&self, bytes: &[u8]) -> Result<BlobHash> {
        // Garbage collection takes the write side, so it never removes a blob between these two steps.
        let _guard = self.inner.blob_lock.read().await;
        let hash = self.inner.blobs.put(bytes).await?;
        let (key, size) = (hash.as_str().to_owned(), bytes.len() as i64);
        self.write(move |tx| {
            tx.execute(
                "INSERT INTO blobs (hash, size, refs, created_at) VALUES (?1, ?2, 0, ?3) ON CONFLICT (hash) DO UPDATE SET created_at = excluded.created_at WHERE refs <= 0",
                rusqlite::params![key, size, now()],
            )?;
            Ok(())
        })
        .await?;
        Ok(hash)
    }

    pub async fn blob(&self, hash: &BlobHash) -> Result<Vec<u8>> {
        self.inner.blobs.get(hash).await
    }

    /// Deletes blobs nothing refers to anymore. Blobs younger than `min_age_secs`
    /// are kept because a write that references them may still be in flight.
    pub async fn collect_garbage(&self, min_age_secs: i64) -> Result<usize> {
        let _guard = self.inner.blob_lock.write().await;
        let cutoff = now() - min_age_secs;
        let hashes: Vec<String> = self
            .write(move |tx| {
                let mut stmt = tx.prepare("DELETE FROM blobs WHERE refs <= 0 AND created_at <= ?1 RETURNING hash")?;
                let rows = stmt.query_map([cutoff], |row| row.get(0))?.collect::<Result<Vec<String>, _>>()?;
                Ok(rows)
            })
            .await?;
        for hash in &hashes {
            self.inner.blobs.delete(&BlobHash(hash.clone())).await?;
        }
        Ok(hashes.len())
    }
}
