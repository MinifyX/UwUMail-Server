//! Putting a backup back, in two halves.
//!
//! The half that fetches runs in the portal while the server is up: it downloads a snapshot into
//! `<data_dir>/restore/` and writes `<data_dir>/restore.ready` beside it. It cannot put the files
//! where they belong, because the database it would overwrite is the one it is running on — so it
//! asks the server to stop, and the container comes back (docker restarts it) into the other half.
//!
//! [`take_over`] is that other half. It runs before the store is opened, which is the one moment in
//! the server's life when those files belong to nobody.
//!
//! Everything it does is a rename inside the same directory, so there is no moment where half a
//! database is in place: the old one is moved aside first, and only then does the new one take its
//! name. If the machine dies in between, `restore.ready` is still there and the next start finishes
//! the job.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use uwumail_backup::{READY_FILE as READY, STAGING_DIR as STAGING, Staged};

/// What the database that was here before is called afterwards. Kept, not deleted: it is the only
/// copy of whatever was on this machine, and a restore is exactly the moment someone might realise
/// they took the wrong snapshot.
const REPLACED: &str = "uwumail.db.replaced";

/// How a restore that was waiting turned out. Written into the restored database, because that is
/// where whoever asked will go looking.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Done {
    pub snapshot: String,
    pub hostname: String,
    pub created_at: i64,
    pub finished_at: i64,
    /// Empty when it worked.
    pub error: String,
    /// Whether the gateway pairing of this machine was kept.
    pub kept_gateway: bool,
}

pub fn ready_path(data_dir: &Path) -> PathBuf {
    data_dir.join(READY)
}

pub fn staging_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(STAGING)
}

/// Whether a restore is waiting to be put in place.
pub fn waiting(data_dir: &Path) -> Option<Staged> {
    let raw = std::fs::read_to_string(ready_path(data_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Puts a waiting restore in place, if there is one. Called before the store is opened.
///
/// Answers what happened, for the server to write into the database it has just been handed. A
/// restore that cannot be finished is not fatal: the staging directory goes, the server starts on
/// what it had, and the portal says why.
pub async fn take_over(data_dir: &Path) -> Option<Done> {
    let ready = waiting(data_dir)?;
    let mut done = Done {
        snapshot: ready.snapshot.clone(),
        hostname: ready.hostname.clone(),
        created_at: ready.created_at,
        finished_at: now(),
        error: String::new(),
        kept_gateway: false,
    };
    tracing::warn!(
        snapshot = %ready.snapshot,
        from = %ready.hostname,
        "putting a backup back before starting; the data that is here now is moved aside"
    );
    match swap(data_dir, &ready).await {
        Ok(()) => {
            done.kept_gateway = ready.keep_gateway;
            tracing::warn!(snapshot = %ready.snapshot, "the backup is in place");
        }
        Err(err) => {
            done.error = format!("{err:#}");
            tracing::error!(error = %done.error, "putting the backup back did not work");
        }
    }
    // Whatever came of it, this must not happen again on the next start.
    let _ = tokio::fs::remove_dir_all(staging_dir(data_dir)).await;
    if let Err(err) = tokio::fs::remove_file(ready_path(data_dir)).await {
        // The one failure that would repeat the restore on every start, so it is loud.
        tracing::error!(%err, "could not take away {READY}; remove it by hand before restarting");
    }
    done.finished_at = now();
    Some(done)
}

async fn swap(data_dir: &Path, ready: &Staged) -> anyhow::Result<()> {
    let staging = staging_dir(data_dir);
    let fresh = staging.join("uwumail.db");
    if !tokio::fs::try_exists(&fresh).await.unwrap_or(false) {
        anyhow::bail!("the snapshot was never fully fetched ({} is missing), so nothing was touched", fresh.display());
    }

    // The database this server has been running on, out of the way but not gone.
    let current = data_dir.join("uwumail.db");
    if tokio::fs::try_exists(&current).await.unwrap_or(false) {
        let aside = data_dir.join(format!("{REPLACED}.{}", ready.asked_at));
        tokio::fs::rename(&current, &aside)
            .await
            .with_context(|| format!("moving {} aside to {}", current.display(), aside.display()))?;
        // SQLite's journal belongs to the file it was written for; left behind it would be read
        // against the restored database, which is a different one entirely.
        for extra in ["uwumail.db-wal", "uwumail.db-shm"] {
            let _ = tokio::fs::remove_file(data_dir.join(extra)).await;
        }
    }

    // Everything the snapshot brought, into place. Blobs are named after their content, so a name
    // that is already here holds the same bytes and is left alone.
    move_into(&staging, data_dir).await.context("moving the restored files into place")?;
    tokio::fs::rename(&fresh, &current).await.with_context(|| format!("putting {} in place", current.display()))?;
    Ok(())
}

/// Moves everything in `from` into `to`, merging directories and leaving `uwumail.db` where it is:
/// that one goes last, so the data directory never looks complete before it is.
async fn move_into(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut entries = tokio::fs::read_dir(from).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name();
        if name == "uwumail.db" {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type().await?.is_dir() {
            tokio::fs::create_dir_all(&target).await?;
            Box::pin(move_into(&entry.path(), &target)).await?;
            let _ = tokio::fs::remove_dir(entry.path()).await;
        } else {
            // A blob already here is the same content under the same name; anything else the
            // snapshot brought wins, because putting the snapshot back is the whole point.
            let _ = tokio::fs::remove_file(&target).await;
            tokio::fs::rename(entry.path(), &target).await?;
        }
    }
    Ok(())
}

/// Where the portal reads how the last restore went.
pub const STATUS_KEY: &str = "restore.status";

/// The gateway pairing of the machine as it is now, read before the snapshot takes its place.
///
/// Opening the store for one setting is a little heavy, but it is the only way to read it: this
/// runs before the server has a store, precisely so that it can look at the old one.
pub async fn pairing_here(data_dir: &Path) -> Option<String> {
    let store = uwumail_store::Store::open(data_dir).await.ok()?;
    let pairing = store.setting(crate::gateway::PAIRING_KEY).await.ok().flatten();
    // Dropped here, before the file it belongs to is moved aside. Its journal goes with the old
    // database in [`swap`], which is what makes that safe.
    drop(store);
    pairing
}

/// Puts right, in the database that has just arrived, what belongs to this machine and not to the
/// one the snapshot came from.
pub async fn after(store: &uwumail_store::Store, done: &Done, pairing: Option<String>, hostname: &str) {
    if done.error.is_empty() {
        // The snapshot carries the backup settings of the server it was made on, pointing at its
        // backup server with its recovery key. Left on, the first thing this machine would do is
        // write its own backups over that server's history — possibly while the old machine is
        // still running. So they go off, and the portal says so instead of leaving it a surprise.
        let backups = uwumail_backup::Backups::new(store.clone(), hostname, env!("CARGO_PKG_VERSION"));
        if let Ok(settings) = backups.settings().await
            && settings.enabled
        {
            let off = uwumail_backup::BackupSettings { enabled: false, ..settings };
            if let Err(err) = backups.save_settings(&off).await {
                tracing::warn!(%err, "could not switch the restored backup schedule off");
            } else {
                tracing::warn!("backups are switched off after the restore; turn them on when the target is right");
            }
        }
        // An update the old machine was in the middle of is not this one's to finish.
        let _ = store.delete_setting("updates.status").await;

        // The gateway in the snapshot belongs to the machine that made it. When this one is paired,
        // that pairing is the one that works — this is, after all, very likely its replacement.
        if let Some(pairing) = pairing {
            match store.set_setting(crate::gateway::PAIRING_KEY, &pairing).await {
                Ok(()) => tracing::info!("kept this machine's gateway pairing instead of the snapshot's"),
                Err(err) => tracing::warn!(%err, "could not keep this machine's gateway pairing"),
            }
        }
    }
    let raw = serde_json::to_string(done).expect("the restore note serializes");
    if let Err(err) = store.set_setting(STATUS_KEY, &raw).await {
        tracing::warn!(%err, "could not write down how the restore went");
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(dir: &Path) {
        let ready = Staged { snapshot: "s1".into(), hostname: "old.example".into(), asked_at: 42, ..Staged::default() };
        std::fs::write(ready_path(dir), serde_json::to_string(&ready).unwrap()).unwrap();
    }

    #[tokio::test]
    async fn a_waiting_restore_takes_the_place_of_what_is_here() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        // What this machine has been running on.
        std::fs::write(data.join("uwumail.db"), b"old database").unwrap();
        std::fs::write(data.join("uwumail.db-wal"), b"old journal").unwrap();
        std::fs::create_dir_all(data.join("blobs/aa/bb")).unwrap();
        std::fs::write(data.join("blobs/aa/bb/aabbmine"), b"a blob only this machine has").unwrap();

        // What was fetched.
        let staging = staging_dir(data);
        std::fs::create_dir_all(staging.join("blobs/cc/dd")).unwrap();
        std::fs::write(staging.join("uwumail.db"), b"restored database").unwrap();
        std::fs::write(staging.join("blobs/cc/dd/ccddtheirs"), b"a blob from the snapshot").unwrap();
        std::fs::create_dir_all(staging.join("certs")).unwrap();
        std::fs::write(staging.join("certs/cert.pem"), b"their certificate").unwrap();
        ready(data);

        let done = take_over(data).await.expect("a restore was waiting");
        assert_eq!(done.error, "", "it worked");
        assert_eq!(done.snapshot, "s1");

        assert_eq!(std::fs::read(data.join("uwumail.db")).unwrap(), b"restored database");
        assert_eq!(std::fs::read(data.join("certs/cert.pem")).unwrap(), b"their certificate");
        assert_eq!(std::fs::read(data.join("blobs/cc/dd/ccddtheirs")).unwrap(), b"a blob from the snapshot");
        // Blobs are named after their content, so what was here is still good and stays.
        assert!(data.join("blobs/aa/bb/aabbmine").exists(), "blobs already here are left alone");
        // The journal of the database that went away must not be read against the new one.
        assert!(!data.join("uwumail.db-wal").exists());
        // The old database is kept: this is the moment someone notices they took the wrong one.
        assert_eq!(std::fs::read(data.join("uwumail.db.replaced.42")).unwrap(), b"old database");

        assert!(!ready_path(data).exists(), "and it does not happen again on the next start");
        assert!(!staging_dir(data).exists());
        assert!(take_over(data).await.is_none());
    }

    #[tokio::test]
    async fn a_half_fetched_snapshot_leaves_the_server_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        std::fs::write(data.join("uwumail.db"), b"old database").unwrap();
        // Blobs arrived, the database never did: the fetch was cut short.
        std::fs::create_dir_all(staging_dir(data).join("blobs")).unwrap();
        ready(data);

        let done = take_over(data).await.expect("a restore was waiting");
        assert!(done.error.contains("never fully fetched"), "{}", done.error);
        assert_eq!(std::fs::read(data.join("uwumail.db")).unwrap(), b"old database", "nothing was touched");
        assert!(!ready_path(data).exists(), "and it is not tried again every start");
    }
}
