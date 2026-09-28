//! A folder target: a disk or a share mounted into the container.
//!
//! Others may write into a share -- a compromised NAS, another client of the same NFS or SMB export
//! -- and a symlink on it resolves here, on the mail server, as the user that owns the server's own
//! data. So nothing below the folder is ever reached through a symlink: every operation starts at a
//! handle on the folder and opens one name at a time with `O_NOFOLLOW`, a file is created fresh
//! with `O_EXCL`, and a listing leaves out whatever is neither a file nor a directory. A link on
//! the share can make a backup fail, but never lead it to a file outside the folder
//! (security-audit-0.16.0 PLAT-5). The folder itself is the admin's choice and may be a link.

use std::path::Path;

use crate::Error;

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, Error> + Send + 'static) -> Result<T, Error> {
    tokio::task::spawn_blocking(work).await.map_err(|_| Error::Storage("working on the backup folder failed".into()))?
}

pub(crate) async fn read(root: &Path, path: &str, limit: u64) -> Result<Option<Vec<u8>>, Error> {
    let (root, path) = (root.to_owned(), path.to_owned());
    blocking(move || imp::read(&root, &path, limit)).await
}

pub(crate) async fn write(root: &Path, path: &str, bytes: &[u8]) -> Result<(), Error> {
    let (root, path, bytes) = (root.to_owned(), path.to_owned(), bytes.to_vec());
    blocking(move || imp::write(&root, &path, &bytes)).await
}

pub(crate) async fn list(root: &Path, dir: &str) -> Result<Vec<String>, Error> {
    let (root, dir) = (root.to_owned(), dir.to_owned());
    blocking(move || imp::list(&root, &dir)).await
}

pub(crate) async fn remove(root: &Path, path: &str) -> Result<(), Error> {
    let (root, path) = (root.to_owned(), path.to_owned());
    blocking(move || imp::remove(&root, &path)).await
}

/// The names a path inside the folder is made of. Only ever plain names: the repository's own paths
/// are, and anything else is a mistake to stop at.
fn names(path: &str) -> Result<Vec<&str>, Error> {
    let names: Vec<&str> = path.split('/').collect();
    if names.iter().any(|name| name.is_empty() || *name == "." || *name == ".." || name.contains('\0')) {
        return Err(Error::Storage(format!("{} is not a path inside the backup folder", path.escape_debug())));
    }
    Ok(names)
}

#[cfg(unix)]
mod imp {
    use std::fs::File;
    use std::io::{self, Read as _, Write as _};
    use std::os::fd::{AsFd, OwnedFd};
    use std::path::Path;

    use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags};
    use rustix::io::Errno;

    use super::names;
    use crate::Error;

    fn failed(err: Errno, what: &str) -> Error {
        if err == Errno::LOOP {
            Error::Storage(format!(
                "{} in the backup folder is a symbolic link; backups never follow one",
                what.escape_debug()
            ))
        } else if err == Errno::NOTDIR {
            Error::Storage(format!(
                "{} in the backup folder is not a folder (or a symbolic link, which backups never follow)",
                what.escape_debug()
            ))
        } else {
            Error::Io(io::Error::from(err))
        }
    }

    fn open_root(root: &Path) -> Result<OwnedFd, Error> {
        rustix::fs::openat(CWD, root, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())
            .map_err(|err| Error::Io(err.into()))
    }

    /// The directory `names` below the folder, never through a symlink. `None` when one of them
    /// does not exist and `create` is off.
    fn directory(root: &Path, names: &[&str], create: bool) -> Result<Option<OwnedFd>, Error> {
        if create {
            // The folder itself is the admin's path, links and all; Storage::open made sure it is
            // there when it comes from the settings.
            std::fs::create_dir_all(root)?;
        }
        let mut dir = match open_root(root) {
            Ok(dir) => dir,
            Err(Error::Io(err)) if !create && err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        for (depth, name) in names.iter().enumerate() {
            if create {
                match rustix::fs::mkdirat(&dir, *name, Mode::from_raw_mode(0o777)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(err) => return Err(Error::Io(err.into())),
                }
            }
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            dir = match rustix::fs::openat(&dir, *name, flags, Mode::empty()) {
                Ok(next) => next,
                Err(Errno::NOENT) if !create => return Ok(None),
                // A symlink is refused by O_NOFOLLOW with ELOOP (or ENOTDIR where O_DIRECTORY is
                // looked at first).
                Err(err) => return Err(failed(err, &names[..=depth].join("/"))),
            };
        }
        Ok(Some(dir))
    }

    fn kind(dir: impl AsFd, name: &str) -> Option<FileType> {
        rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok().map(|stat| FileType::from_raw_mode(stat.st_mode))
    }

    pub(super) fn read(root: &Path, path: &str, limit: u64) -> Result<Option<Vec<u8>>, Error> {
        let names = names(path)?;
        let (file, parents) = names.split_last().expect("a path has a name");
        let Some(dir) = directory(root, parents, false)? else { return Ok(None) };
        // O_NONBLOCK so a pipe planted under the name cannot hang the open; it is refused below.
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
        let fd = match rustix::fs::openat(&dir, *file, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(err) => return Err(failed(err, path)),
        };
        let stat = rustix::fs::fstat(&fd).map_err(|err| failed(err, path))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(Error::Storage(format!("{} in the backup folder is not a file", path.escape_debug())));
        }
        let mut bytes = Vec::new();
        File::from(fd).take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }

    pub(super) fn write(root: &Path, path: &str, bytes: &[u8]) -> Result<(), Error> {
        let names = names(path)?;
        let (file, parents) = names.split_last().expect("a path has a name");
        let dir = directory(root, parents, true)?.expect("created");
        let temporary = format!("{file}.part");
        // Whatever an earlier run left under the temporary name goes first; unlinking a symlink
        // removes the link, never what it points to. Then the file is made fresh, and a name
        // planted again in between makes the write fail instead of reaching through it.
        match rustix::fs::unlinkat(&dir, temporary.as_str(), AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(err) => return Err(failed(err, &temporary)),
        }
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(&dir, temporary.as_str(), flags, Mode::from_raw_mode(0o666))
            .map_err(|err| failed(err, &temporary))?;
        File::from(fd).write_all(bytes)?;
        // A rename replaces a symlink under the final name; it never follows one.
        rustix::fs::renameat(&dir, temporary.as_str(), &dir, *file).map_err(|err| failed(err, path))?;
        Ok(())
    }

    pub(super) fn list(root: &Path, path: &str) -> Result<Vec<String>, Error> {
        let names = names(path)?;
        let Some(dir) = directory(root, &names, false)? else { return Ok(Vec::new()) };
        let mut listed = Vec::new();
        for entry in Dir::read_from(&dir).map_err(|err| failed(err, path))? {
            let entry = entry.map_err(|err| failed(err, path))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "." || name == ".." {
                continue;
            }
            let kind = match entry.file_type() {
                FileType::Unknown => kind(&dir, &name),
                known => Some(known),
            };
            if matches!(kind, Some(FileType::RegularFile | FileType::Directory)) {
                listed.push(name);
            } else {
                tracing::warn!(
                    name = %format!("{path}/{name}").escape_debug(),
                    "the backup folder holds something that is neither a file nor a folder; leaving it alone"
                );
            }
        }
        Ok(listed)
    }

    pub(super) fn remove(root: &Path, path: &str) -> Result<(), Error> {
        let names = names(path)?;
        let (file, parents) = names.split_last().expect("a path has a name");
        let Some(dir) = directory(root, parents, false)? else { return Ok(()) };
        // unlinkat takes the name itself away, a symlink included, and never what it points to.
        match rustix::fs::unlinkat(&dir, *file, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => Ok(()),
            Err(err) => Err(failed(err, path)),
        }
    }
}

/// Without `openat`, by name: the server itself only runs on Unix; this keeps the crate building
/// elsewhere.
#[cfg(not(unix))]
mod imp {
    use std::io::Read as _;
    use std::path::{Path, PathBuf};

    use super::names;
    use crate::Error;

    fn joined(root: &Path, names: &[&str]) -> PathBuf {
        names.iter().fold(root.to_owned(), |path, name| path.join(name))
    }

    fn not_found(err: &std::io::Error) -> bool {
        err.kind() == std::io::ErrorKind::NotFound
    }

    pub(super) fn read(root: &Path, path: &str, limit: u64) -> Result<Option<Vec<u8>>, Error> {
        match std::fs::File::open(joined(root, &names(path)?)) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
                Ok(Some(bytes))
            }
            Err(err) if not_found(&err) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub(super) fn write(root: &Path, path: &str, bytes: &[u8]) -> Result<(), Error> {
        let names = names(path)?;
        let target = joined(root, &names);
        std::fs::create_dir_all(joined(root, &names[..names.len() - 1]))?;
        let temporary = target.with_file_name(format!("{}.part", names[names.len() - 1]));
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(&temporary, &target)?;
        Ok(())
    }

    pub(super) fn list(root: &Path, path: &str) -> Result<Vec<String>, Error> {
        let mut listed = Vec::new();
        match std::fs::read_dir(joined(root, &names(path)?)) {
            Ok(entries) => {
                for entry in entries {
                    listed.push(entry?.file_name().to_string_lossy().into_owned());
                }
            }
            Err(err) if not_found(&err) => {}
            Err(err) => return Err(err.into()),
        }
        Ok(listed)
    }

    pub(super) fn remove(root: &Path, path: &str) -> Result<(), Error> {
        match std::fs::remove_file(joined(root, &names(path)?)) {
            Err(err) if !not_found(&err) => Err(err.into()),
            _ => Ok(()),
        }
    }
}
