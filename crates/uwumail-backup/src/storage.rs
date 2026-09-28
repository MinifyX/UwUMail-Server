//! Where a repository lives: a directory (a mounted disk or share, and the tests), an SFTP server or
//! an S3 bucket.

use std::path::PathBuf;

use crate::folder;
use crate::s3::S3;
use crate::sftp::Sftp;
use crate::{Error, Target};

pub enum Storage {
    Local(PathBuf),
    Sftp(Sftp),
    /// Boxed: the client with its connection pool is much bigger than the other two.
    S3(Box<S3>),
}

impl Storage {
    /// Connects to where a target points. A folder must exist already: a mounted disk that is not
    /// there would otherwise quietly become a folder on the disk the server runs from.
    pub async fn open(target: &Target) -> Result<Storage, Error> {
        match target {
            Target::Sftp(sftp) => Ok(Storage::Sftp(Sftp::connect(sftp).await?)),
            Target::S3(s3) => {
                let s3 = S3::new(s3)?;
                s3.check_route().await?;
                Ok(Storage::S3(Box::new(s3)))
            }
            Target::Folder(folder) => {
                let dir = folder.dir();
                if !dir.is_absolute() {
                    return Err(Error::Config(format!("{} is not a full path like /backup", folder.path)));
                }
                match tokio::fs::metadata(&dir).await {
                    Ok(meta) if meta.is_dir() => Ok(Storage::Local(dir)),
                    Ok(_) => Err(Error::Config(format!("{} is not a folder", folder.path))),
                    Err(_) => Err(Error::Config(format!(
                        "there is no folder {}; is the disk or share mounted into the container?",
                        folder.path
                    ))),
                }
            }
        }
    }

    /// The SFTP server's host key, when this is one.
    pub fn host_key(&self) -> Option<&str> {
        match self {
            Storage::Sftp(sftp) => Some(&sftp.host_key),
            _ => None,
        }
    }

    /// Writes a small file and takes it away again: whether backups can be written here at all.
    pub async fn check_writable(&self) -> Result<(), Error> {
        const PROBE: &str = "uwumail-write-test";
        self.write(PROBE, b"UwUMail was here and cleaned up after itself").await?;
        match self.read(PROBE, 1024).await? {
            Some(_) => {}
            None => return Err(Error::Storage("a file written there could not be read back".into())),
        }
        self.remove(PROBE).await
    }

    /// The content of a file, `None` when it does not exist. A file longer than `limit` is refused
    /// after reading no more than one byte past it: the backup server decides how big its files are.
    pub async fn read(&self, path: &str, limit: u64) -> Result<Option<Vec<u8>>, Error> {
        let bytes = match self {
            Storage::Local(root) => match folder::read(root, path, limit).await? {
                Some(bytes) => bytes,
                None => return Ok(None),
            },
            Storage::Sftp(sftp) => match sftp.read(path, limit).await? {
                Some(bytes) => bytes,
                None => return Ok(None),
            },
            Storage::S3(s3) => match s3.read(path, limit).await? {
                Some(bytes) => bytes,
                None => return Ok(None),
            },
        };
        if bytes.len() as u64 > limit {
            return Err(Error::Damaged(format!("{path} is bigger than the {limit} bytes it may have")));
        }
        Ok(Some(bytes))
    }

    /// Writes a file completely or not at all: first under a temporary name, then renamed. S3 needs
    /// no such detour, since it keeps an object whole or not at all by itself.
    pub async fn write(&self, path: &str, bytes: &[u8]) -> Result<(), Error> {
        let (dir, _) = path.rsplit_once('/').unwrap_or(("", path));
        let temporary = format!("{path}.part");
        match self {
            // Its own way to the same end, without ever following a symlink on the share.
            Storage::Local(root) => folder::write(root, path, bytes).await,
            Storage::Sftp(sftp) => {
                sftp.create_dirs(dir).await?;
                sftp.write(&temporary, bytes).await?;
                sftp.rename(&temporary, path).await
            }
            Storage::S3(s3) => s3.write(path, bytes).await,
        }
    }

    /// File names in a directory; empty when it does not exist. Unfinished uploads are left out.
    pub async fn list(&self, dir: &str) -> Result<Vec<String>, Error> {
        let names = match self {
            Storage::Local(root) => folder::list(root, dir).await?,
            Storage::Sftp(sftp) => sftp.list(dir).await?,
            Storage::S3(s3) => s3.list(dir).await?,
        };
        Ok(names.into_iter().filter(|name| !name.ends_with(".part") && name != "." && name != "..").collect())
    }

    pub async fn remove(&self, path: &str) -> Result<(), Error> {
        match self {
            Storage::Local(root) => folder::remove(root, path).await,
            Storage::Sftp(sftp) => sftp.remove(path).await,
            Storage::S3(s3) => s3.remove(path).await,
        }
    }

    pub async fn close(self) {
        if let Storage::Sftp(sftp) = self {
            sftp.close().await;
        }
    }
}
