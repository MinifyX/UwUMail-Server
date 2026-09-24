//! Where a repository lives: a directory (for tests and local disks) or an SFTP server.

use std::path::PathBuf;

use tokio::io::AsyncReadExt;

use crate::Error;
use crate::sftp::Sftp;

pub enum Storage {
    Local(PathBuf),
    Sftp(Sftp),
}

impl Storage {
    /// The content of a file, `None` when it does not exist. A file longer than `limit` is refused
    /// after reading no more than one byte past it: the backup server decides how big its files are.
    pub async fn read(&self, path: &str, limit: u64) -> Result<Option<Vec<u8>>, Error> {
        let bytes = match self {
            Storage::Local(root) => match tokio::fs::File::open(root.join(path)).await {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take(limit.saturating_add(1)).read_to_end(&mut bytes).await?;
                    bytes
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err.into()),
            },
            Storage::Sftp(sftp) => match sftp.read(path, limit).await? {
                Some(bytes) => bytes,
                None => return Ok(None),
            },
        };
        if bytes.len() as u64 > limit {
            return Err(Error::Damaged(format!("{path} is bigger than the {limit} bytes it may have")));
        }
        Ok(Some(bytes))
    }

    /// Writes a file completely or not at all: first under a temporary name, then renamed.
    pub async fn write(&self, path: &str, bytes: &[u8]) -> Result<(), Error> {
        let (dir, _) = path.rsplit_once('/').unwrap_or(("", path));
        let temporary = format!("{path}.part");
        match self {
            Storage::Local(root) => {
                tokio::fs::create_dir_all(root.join(dir)).await?;
                tokio::fs::write(root.join(&temporary), bytes).await?;
                tokio::fs::rename(root.join(&temporary), root.join(path)).await?;
                Ok(())
            }
            Storage::Sftp(sftp) => {
                sftp.create_dirs(dir).await?;
                sftp.write(&temporary, bytes).await?;
                sftp.rename(&temporary, path).await
            }
        }
    }

    /// File names in a directory; empty when it does not exist. Unfinished uploads are left out.
    pub async fn list(&self, dir: &str) -> Result<Vec<String>, Error> {
        let names = match self {
            Storage::Local(root) => {
                let mut names = Vec::new();
                match tokio::fs::read_dir(root.join(dir)).await {
                    Ok(mut entries) => {
                        while let Some(entry) = entries.next_entry().await? {
                            names.push(entry.file_name().to_string_lossy().into_owned());
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err.into()),
                }
                names
            }
            Storage::Sftp(sftp) => sftp.list(dir).await?,
        };
        Ok(names.into_iter().filter(|name| !name.ends_with(".part") && name != "." && name != "..").collect())
    }

    pub async fn remove(&self, path: &str) -> Result<(), Error> {
        match self {
            Storage::Local(root) => match tokio::fs::remove_file(root.join(path)).await {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
                _ => Ok(()),
            },
            Storage::Sftp(sftp) => sftp.remove(path).await,
        }
    }

    pub async fn close(self) {
        if let Storage::Sftp(sftp) = self {
            sftp.close().await;
        }
    }
}
