//! The remote pictures of messages, fetched through the egress for the reader (docs/jmap-remote.md).
//!
//! A newsletter's pictures are asked for by everyone who got it, and each reader asks for all of them
//! at once. So a picture is fetched once for the whole server and kept on disk for a week, shared by
//! everyone: an entry is found by a hash of the picture's address alone and holds the picture, its
//! type and its size, nothing about who asked for it. Readers that ask for a picture already on its way
//! wait for the same request (single flight).
//!
//! Its size is known once the first bytes are there, long before a big picture is complete, so a reader
//! can lay the message out before the pictures arrive ([`RemoteImages::size`]).
//!
//! Each person has a few requests at a time ([`PER_PERSON`]), everyone together a few more
//! ([`ALL_TOGETHER`]): one reader's newsletter with a hundred pictures neither floods the server nor
//! makes everyone else wait. A picture that could not be fetched is not asked for again for a while;
//! dead tracking hosts give up within seconds anyway ([`PictureLimits`]).

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

use crate::egress::{Egress, EgressError, PictureLimits};

/// Bigger pictures than this are not passed on. Newsletters stay far below it.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Longer addresses are not fetched.
pub const MAX_URL_LENGTH: usize = 4096;
const ACCEPT: &str = "image/avif,image/webp,image/apng,image/svg+xml,image/*;q=0.8";
/// A picture is fetched again after a week at the latest.
const FRESH_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// A picture that could not be fetched is not tried again for this long: one that is not there or no
/// picture ...
const FAILURE_REMEMBERED: Duration = Duration::from_secs(10 * 60);
/// ... and one whose host did not answer in time or could not be reached, which may be the proxy's
/// fault for a moment.
const TROUBLE_REMEMBERED: Duration = Duration::from_secs(60);
const MAX_FAILURES: usize = 20_000;
/// Requests at the same time for one person, and for everyone together.
pub const PER_PERSON: usize = 8;
pub const ALL_TOGETHER: usize = 64;
/// Pictures one person may have waiting for their turn; more are turned away.
const MAX_WAITING: usize = 400;
/// The size is looked for in this much of the start of a picture; later it waits for all of it.
const SIZE_WITHIN: usize = 512 * 1024;
/// ... and at most this many times while the picture comes in.
const SIZE_ATTEMPTS: u32 = 24;
/// What a file in the cache starts with, then the version of its layout.
const MAGIC: &[u8; 8] = b"UWUIMG1\n";
/// After this many new entries the cache looks for expired ones.
const SWEEP_EVERY: u32 = 256;

/// A picture's size in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

/// A remote picture as the reader gets it.
#[derive(Debug)]
pub struct Picture {
    /// Checked against the picture's first bytes; always `image/…`.
    pub media_type: String,
    pub bytes: Bytes,
    /// None for the few kinds whose size can't be read here.
    pub size: Option<Dimensions>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PictureError {
    #[error(transparent)]
    Egress(#[from] EgressError),
    #[error("that is not a picture")]
    NotAPicture,
    #[error("too many pictures at once")]
    Busy,
}

#[derive(Clone)]
enum Progress {
    Waiting,
    Sized(Dimensions),
    Done(Result<Arc<Picture>, PictureError>),
}

type Key = [u8; 32];

fn key(url: &str) -> Key {
    Sha256::digest(url.trim().as_bytes()).into()
}

/// The requests of one person.
struct Person {
    running: Arc<Semaphore>,
    waiting: Arc<Semaphore>,
}

struct Inner {
    egress: Egress,
    /// None: nothing is kept on disk.
    dir: Option<PathBuf>,
    limits: PictureLimits,
    flights: Mutex<HashMap<Key, watch::Receiver<Progress>>>,
    /// Until when a picture that failed is not asked for again, and why.
    failures: Mutex<HashMap<Key, (tokio::time::Instant, PictureError)>>,
    everyone: Arc<Semaphore>,
    people: Mutex<HashMap<i64, Arc<Person>>>,
    index: Mutex<Index>,
}

/// What the cache holds on disk, for keeping it within its size.
#[derive(Default)]
struct Index {
    /// Filled from the disk once, in the background.
    loaded: bool,
    loading: bool,
    entries: HashMap<Key, Entry>,
    bytes: u64,
    added: u32,
}

#[derive(Clone, Copy)]
struct Entry {
    bytes: u64,
    /// Unix seconds.
    stored: i64,
    /// Unix seconds; the entry that went longest without being asked for goes first.
    used: i64,
}

/// The remote pictures of messages. Cheap to clone.
#[derive(Clone)]
pub struct RemoteImages {
    inner: Arc<Inner>,
}

impl RemoteImages {
    /// Keeps pictures in `dir` (created when needed), within the size the egress configuration says;
    /// `None` keeps nothing on disk.
    pub fn new(egress: Egress, dir: Option<PathBuf>) -> RemoteImages {
        RemoteImages::with_limits(egress, dir, PictureLimits::default())
    }

    /// The same with other time limits: for tests.
    pub fn with_limits(egress: Egress, dir: Option<PathBuf>, limits: PictureLimits) -> RemoteImages {
        RemoteImages {
            inner: Arc::new(Inner {
                egress,
                dir,
                limits,
                flights: Mutex::default(),
                failures: Mutex::default(),
                everyone: Arc::new(Semaphore::new(ALL_TOGETHER)),
                people: Mutex::default(),
                index: Mutex::default(),
            }),
        }
    }

    /// The picture at `url`, from the cache or fetched for `person`.
    pub async fn get(&self, person: i64, url: &str) -> Result<Arc<Picture>, PictureError> {
        let key = key(url);
        if let Some(picture) = self.inner.read(&key, true).await {
            return Ok(picture);
        }
        let mut progress = self.flight(person, url, key)?;
        let done = progress.wait_for(|progress| matches!(progress, Progress::Done(_))).await;
        match done.as_deref() {
            Ok(Progress::Done(result)) => result.clone(),
            _ => Err(PictureError::Egress(EgressError::Unreachable)),
        }
    }

    /// The size of the picture at `url` as soon as it is known: often after its first few kilobytes.
    /// `Ok(None)` when the picture came but its size can't be read here. Fetching goes on after the
    /// answer, into the cache, so the picture itself is there soon after.
    pub async fn size(&self, person: i64, url: &str) -> Result<Option<Dimensions>, PictureError> {
        let key = key(url);
        if let Some(size) = self.inner.cached_size(&key).await {
            return Ok(size);
        }
        let mut progress = self.flight(person, url, key)?;
        let known = progress.wait_for(|progress| !matches!(progress, Progress::Waiting)).await;
        match known.as_deref() {
            Ok(Progress::Sized(size)) => Ok(Some(*size)),
            Ok(Progress::Done(Ok(picture))) => Ok(picture.size),
            Ok(Progress::Done(Err(err))) => Err(err.clone()),
            _ => Err(PictureError::Egress(EgressError::Unreachable)),
        }
    }

    /// Joins the request for `url` that is on its way, or starts one for `person`.
    fn flight(&self, person: i64, url: &str, key: Key) -> Result<watch::Receiver<Progress>, PictureError> {
        if url.len() > MAX_URL_LENGTH {
            return Err(PictureError::Egress(EgressError::NotAllowed("the picture's address is too long".into())));
        }
        if let Some(err) = self.inner.failed(&key) {
            return Err(err);
        }
        let mut flights = self.inner.flights.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(progress) = flights.get(&key) {
            return Ok(progress.clone());
        }
        let person = self.inner.person(person);
        let waiting = person.waiting.clone().try_acquire_owned().map_err(|_| PictureError::Busy)?;
        let (sender, progress) = watch::channel(Progress::Waiting);
        flights.insert(key, progress.clone());
        drop(flights);
        let inner = self.inner.clone();
        let url = url.trim().to_owned();
        tokio::spawn(async move {
            let running = person.running.clone().acquire_owned().await;
            let everyone = inner.everyone.clone().acquire_owned().await;
            drop(waiting);
            let result = match (running, everyone) {
                (Ok(running), Ok(everyone)) => inner.fetch(&url, &sender, [running, everyone]).await,
                _ => Err(PictureError::Egress(EgressError::Unreachable)),
            };
            match &result {
                Ok(picture) => inner.store(&key, picture).await,
                Err(err) => inner.remember_failure(key, err.clone()),
            }
            inner.flights.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
            sender.send_replace(Progress::Done(result));
        });
        Ok(progress)
    }
}

impl Inner {
    fn person(&self, id: i64) -> Arc<Person> {
        let mut people = self.people.lock().unwrap_or_else(|e| e.into_inner());
        if people.len() > 1024 {
            // Nobody holds on to the ones without requests.
            people.retain(|_, person| Arc::strong_count(person) > 1);
        }
        people
            .entry(id)
            .or_insert_with(|| {
                Arc::new(Person {
                    running: Arc::new(Semaphore::new(PER_PERSON)),
                    waiting: Arc::new(Semaphore::new(MAX_WAITING)),
                })
            })
            .clone()
    }

    fn failed(&self, key: &Key) -> Option<PictureError> {
        let mut failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        match failures.get(key) {
            Some((until, err)) if tokio::time::Instant::now() < *until => Some(err.clone()),
            Some(_) => {
                failures.remove(key);
                None
            }
            None => None,
        }
    }

    fn remember_failure(&self, key: Key, err: PictureError) {
        if err == PictureError::Busy {
            return;
        }
        let now = tokio::time::Instant::now();
        let remembered = match err {
            PictureError::Egress(EgressError::Timeout | EgressError::Unreachable) => TROUBLE_REMEMBERED,
            _ => FAILURE_REMEMBERED,
        };
        let mut failures = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if failures.len() >= MAX_FAILURES {
            failures.retain(|_, (until, _)| now < *until);
            if failures.len() >= MAX_FAILURES {
                failures.clear();
            }
        }
        failures.insert(key, (now + remembered, err));
    }

    /// Fetches the picture, telling its size on `progress` as soon as its first bytes show it.
    async fn fetch(
        &self,
        url: &str,
        progress: &watch::Sender<Progress>,
        _permits: [OwnedSemaphorePermit; 2],
    ) -> Result<Arc<Picture>, PictureError> {
        let mut attempts = 0;
        let mut sized = false;
        let mut look = |so_far: &[u8]| {
            if sized || attempts >= SIZE_ATTEMPTS || so_far.len() > SIZE_WITHIN {
                return;
            }
            attempts += 1;
            if let Some(size) = dimensions(so_far) {
                sized = true;
                progress.send_replace(Progress::Sized(size));
            }
        };
        let fetched = self.egress.get_message_picture(url, ACCEPT, MAX_IMAGE_BYTES, self.limits, &mut look).await?;
        let media_type = picture_type(&fetched.media_type, &fetched.body).ok_or(PictureError::NotAPicture)?;
        let size = dimensions(&fetched.body)
            .or_else(|| (media_type == "image/svg+xml").then(|| svg_size(&fetched.body)).flatten());
        Ok(Arc::new(Picture { media_type, bytes: fetched.body, size }))
    }

    fn path(&self, key: &Key) -> Option<PathBuf> {
        let name = hex::encode(key);
        Some(self.dir.as_ref()?.join(&name[..2]).join(name))
    }

    /// The picture from the disk, when it is there and fresh. `whole`: with its bytes, otherwise only
    /// what the file's head says.
    async fn read(&self, key: &Key, whole: bool) -> Option<Arc<Picture>> {
        let path = self.path(key)?;
        let found = tokio::task::spawn_blocking(move || read_entry(&path, whole)).await.ok()?;
        match found {
            Some(Ok(picture)) => {
                let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(entry) = index.entries.get_mut(key) {
                    entry.used = crate::now();
                }
                Some(Arc::new(picture))
            }
            Some(Err(Stale)) => {
                self.forget(key).await;
                None
            }
            None => None,
        }
    }

    /// The size of a picture the disk holds: `Some(None)` when it is there without one.
    async fn cached_size(&self, key: &Key) -> Option<Option<Dimensions>> {
        if let Some(Progress::Done(Ok(picture))) =
            self.flights.lock().unwrap_or_else(|e| e.into_inner()).get(key).map(|progress| progress.borrow().clone())
        {
            return Some(picture.size);
        }
        self.read(key, false).await.map(|picture| picture.size)
    }

    async fn forget(&self, key: &Key) {
        if let Some(path) = self.path(key) {
            let _ = tokio::fs::remove_file(path).await;
        }
        let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = index.entries.remove(key) {
            index.bytes = index.bytes.saturating_sub(entry.bytes);
        }
    }

    /// Keeps the picture on disk and the cache within its size.
    async fn store(&self, key: &Key, picture: &Picture) {
        let limit = self.egress.image_cache_limit();
        let Some(path) = self.path(key) else { return };
        let bytes = entry_bytes(picture, crate::now());
        self.load_index().await;
        if limit == 0 || bytes.len() as u64 > limit / 4 {
            self.trim(limit).await;
            return;
        }
        let size = bytes.len() as u64;
        let written = tokio::task::spawn_blocking(move || write_entry(&path, &bytes)).await;
        if !matches!(written, Ok(Ok(()))) {
            tracing::debug!("a remote picture could not be kept on disk");
            return;
        }
        let now = crate::now();
        let sweep = {
            let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(old) = index.entries.insert(*key, Entry { bytes: size, stored: now, used: now }) {
                index.bytes = index.bytes.saturating_sub(old.bytes);
            }
            index.bytes += size;
            index.added += 1;
            index.added.is_multiple_of(SWEEP_EVERY)
        };
        if sweep {
            self.sweep().await;
        }
        self.trim(limit).await;
    }

    /// Reads what the disk holds from an earlier run, once, in the background.
    async fn load_index(&self) {
        let Some(dir) = self.dir.clone() else { return };
        {
            let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
            if index.loaded || index.loading {
                return;
            }
            index.loading = true;
        }
        let found = tokio::task::spawn_blocking(move || scan(&dir)).await.unwrap_or_default();
        let mut index = self.index.lock().unwrap_or_else(|e| e.into_inner());
        for (key, entry) in found {
            if let std::collections::hash_map::Entry::Vacant(vacant) = index.entries.entry(key) {
                vacant.insert(entry);
                index.bytes += entry.bytes;
            }
        }
        index.loaded = true;
        index.loading = false;
    }

    /// Removes entries older than a week.
    async fn sweep(&self) {
        let cutoff = crate::now() - FRESH_FOR.as_secs() as i64;
        let expired: Vec<Key> = {
            let index = self.index.lock().unwrap_or_else(|e| e.into_inner());
            index.entries.iter().filter(|(_, entry)| entry.stored < cutoff).map(|(key, _)| *key).collect()
        };
        for key in expired {
            self.forget(&key).await;
        }
    }

    /// Removes the entries asked for least lately until the cache is back within 90 % of `limit`.
    async fn trim(&self, limit: u64) {
        let doomed: Vec<Key> = {
            let index = self.index.lock().unwrap_or_else(|e| e.into_inner());
            if index.bytes <= limit {
                return;
            }
            let target = limit / 10 * 9;
            let mut entries: Vec<(Key, Entry)> = index.entries.iter().map(|(key, entry)| (*key, *entry)).collect();
            entries.sort_by_key(|(_, entry)| entry.used);
            let mut bytes = index.bytes;
            entries
                .into_iter()
                .take_while(|(_, entry)| {
                    let over = bytes > target;
                    bytes = bytes.saturating_sub(entry.bytes);
                    over
                })
                .map(|(key, _)| key)
                .collect()
        };
        for key in doomed {
            self.forget(&key).await;
        }
    }
}

/// An entry older than a week.
struct Stale;

fn entry_bytes(picture: &Picture, now: i64) -> Vec<u8> {
    let media_type = picture.media_type.as_bytes();
    let mut bytes = Vec::with_capacity(MAGIC.len() + 17 + media_type.len() + picture.bytes.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&now.to_le_bytes());
    let size = picture.size.unwrap_or(Dimensions { width: 0, height: 0 });
    bytes.extend_from_slice(&size.width.to_le_bytes());
    bytes.extend_from_slice(&size.height.to_le_bytes());
    bytes.push(media_type.len().min(255) as u8);
    bytes.extend_from_slice(&media_type[..media_type.len().min(255)]);
    bytes.extend_from_slice(&picture.bytes);
    bytes
}

fn write_entry(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no directory"))?;
    std::fs::create_dir_all(dir)?;
    // A name of its own for each write: two writers of one picture never share a half-written file.
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(|_| std::io::Error::other("the system RNG failed"))?;
    let partial = path.with_extension(format!("part-{}", hex::encode(nonce)));
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })
}

/// None when there is no such file or it is not one of ours.
fn read_entry(path: &Path, whole: bool) -> Option<Result<Picture, Stale>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 8 + 8 + 4 + 4 + 1];
    file.read_exact(&mut head).ok()?;
    if &head[..8] != MAGIC {
        return None;
    }
    let number = |at: usize| u32::from_le_bytes(head[at..at + 4].try_into().expect("four bytes"));
    let stored = i64::from_le_bytes(head[8..16].try_into().expect("eight bytes"));
    if crate::now() - stored > FRESH_FOR.as_secs() as i64 {
        return Some(Err(Stale));
    }
    let (width, height) = (number(16), number(20));
    let mut media_type = vec![0u8; head[24] as usize];
    file.read_exact(&mut media_type).ok()?;
    let media_type = String::from_utf8(media_type).ok().filter(|kind| kind.starts_with("image/"))?;
    let mut bytes = Vec::new();
    if whole {
        file.take(MAX_IMAGE_BYTES as u64 + 1).read_to_end(&mut bytes).ok()?;
    }
    let size = (width > 0 && height > 0).then_some(Dimensions { width, height });
    Some(Ok(Picture { media_type, bytes: Bytes::from(bytes), size }))
}

/// What the cache directory holds: sizes, and the time each was stored for want of anything better.
/// A half-written file older than this is a leftover.
const LEFTOVER_AGE: Duration = Duration::from_secs(600);

fn scan(dir: &Path) -> Vec<(Key, Entry)> {
    let mut found = Vec::new();
    let Ok(shards) = std::fs::read_dir(dir) else { return found };
    for shard in shards.flatten() {
        let Ok(files) = std::fs::read_dir(shard.path()) else { continue };
        for file in files.flatten() {
            let name = file.file_name();
            let Some(key) = name.to_str().and_then(|name| hex::decode(name).ok()).and_then(|key| key.try_into().ok())
            else {
                // Half-written leftovers of a crash, not what is being written right now: the process
                // id says nothing in a container, where it is always the same (IMG-3 of the 0.18.0 audit).
                let old = file
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|at| at.elapsed().ok())
                    .is_none_or(|age| age > LEFTOVER_AGE);
                if old {
                    let _ = std::fs::remove_file(file.path());
                }
                continue;
            };
            let Ok(meta) = file.metadata() else { continue };
            let stored = meta
                .modified()
                .ok()
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |at| at.as_secs() as i64);
            found.push((key, Entry { bytes: meta.len(), stored, used: stored }));
        }
    }
    found
}

/// The size of a picture from its first bytes: PNG, JPEG, GIF and WebP.
pub fn dimensions(bytes: &[u8]) -> Option<Dimensions> {
    // A GIF's decoder wants more than its first bytes; they hold the size all the same.
    if let [b'G', b'I', b'F', b'8', _, _, w0, w1, h0, h1, ..] = *bytes {
        let (width, height) = (u16::from_le_bytes([w0, w1]) as u32, u16::from_le_bytes([h0, h1]) as u32);
        return (width > 0 && height > 0).then_some(Dimensions { width, height });
    }
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    reader.format()?;
    let (width, height) = reader.into_dimensions().ok()?;
    (width > 0 && height > 0).then_some(Dimensions { width, height })
}

/// The size an SVG gives itself: `width`/`height` in pixels, or its `viewBox`.
pub fn svg_size(bytes: &[u8]) -> Option<Dimensions> {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(16 * 1024)]);
    let start = text.find("<svg")?;
    let rest = &text[start..];
    let tag = &rest[..rest.find('>')?];
    let attribute = |name: &str| -> Option<&str> {
        let mut from = 0;
        while let Some(found) = tag[from..].find(name) {
            let at = from + found;
            from = at + name.len();
            let before = tag[..at].chars().next_back();
            if !before.is_some_and(char::is_whitespace) {
                continue;
            }
            let after = tag[from..].trim_start();
            let Some(after) = after.strip_prefix('=') else { continue };
            let after = after.trim_start();
            let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let value = &after[1..];
            return value.find(quote).map(|end| &value[..end]);
        }
        None
    };
    let pixels = |value: &str| -> Option<u32> {
        let value = value.trim();
        let number = value.strip_suffix("px").unwrap_or(value).trim();
        let number: f64 = number.parse().ok()?;
        (number.is_finite() && (1.0..=100_000.0).contains(&number)).then_some(number.round() as u32)
    };
    if let (Some(width), Some(height)) = (attribute("width").and_then(pixels), attribute("height").and_then(pixels)) {
        return Some(Dimensions { width, height });
    }
    let view_box: Vec<f64> = attribute("viewBox")?
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect();
    let [_, _, width, height] = view_box[..] else { return None };
    let fits = |value: f64| value.is_finite() && (1.0..=100_000.0).contains(&value);
    (fits(width) && fits(height)).then(|| Dimensions { width: width.round() as u32, height: height.round() as u32 })
}

/// The type to hand a picture on with, checked against its first bytes: what they show for the usual
/// kinds, whatever it was sent as; an SVG only when it looks like one; another `image/…` type as sent,
/// unless the bytes look like markup. Anything else is not passed on.
pub fn picture_type(sent: &str, body: &[u8]) -> Option<String> {
    let sniffed = match body {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xff, 0xd8, 0xff, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        [_, _, _, _, b'f', b't', b'y', b'p', b'a', b'v', b'i', b'f' | b's', ..] => Some("image/avif"),
        [0, 0, 1, 0, ..] => Some("image/x-icon"),
        [b'B', b'M', ..] => Some("image/bmp"),
        _ => None,
    };
    if let Some(sniffed) = sniffed {
        return Some(sniffed.to_owned());
    }
    let start = String::from_utf8_lossy(&body[..body.len().min(1024)]).trim_start().to_ascii_lowercase();
    let markup = start.starts_with('<');
    let svg = start.starts_with("<svg") || (markup && !start.starts_with("<html") && start.contains("<svg"));
    if svg {
        return (sent == "image/svg+xml" || sent.is_empty() || sent == "text/xml" || sent == "application/xml")
            .then(|| "image/svg+xml".to_owned());
    }
    let subtype = sent.strip_prefix("image/")?;
    let fine = !subtype.is_empty() && subtype.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b));
    (fine && !markup && subtype != "svg+xml").then(|| sent.to_owned())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::egress::EgressConfig;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbImage::new(width, height)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("a PNG");
        out
    }

    async fn read_head(stream: &mut TcpStream) -> String {
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap_or(0) == 1 {
            head.push(byte[0]);
        }
        String::from_utf8_lossy(&head).into_owned()
    }

    /// A website with pictures that counts the requests for each. `/slow.png` sends its first half and
    /// waits for `go`; `/dead.gif` never answers.
    struct Site {
        address: SocketAddr,
        asked: Arc<Mutex<HashMap<String, usize>>>,
        go: Arc<tokio::sync::Notify>,
        running: Arc<AtomicUsize>,
    }

    async fn site() -> Site {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let asked = Arc::new(Mutex::new(HashMap::new()));
        let go = Arc::new(tokio::sync::Notify::new());
        let running = Arc::new(AtomicUsize::new(0));
        let (log, gate, busy) = (asked.clone(), go.clone(), running.clone());
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (log, gate, busy) = (log.clone(), gate.clone(), busy.clone());
                tokio::spawn(async move {
                    loop {
                        let head = read_head(&mut stream).await;
                        if head.is_empty() {
                            return;
                        }
                        let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
                        *log.lock().unwrap().entry(path.clone()).or_default() += 1;
                        let (kind, body) = match path.as_str() {
                            "/wide.png" => ("image/png", png(300, 100)),
                            "/tall.png" => ("image/png", png(100, 300)),
                            "/logo.svg" => (
                                "image/svg+xml",
                                br#"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="40"/>"#.to_vec(),
                            ),
                            "/page.png" => ("image/png", b"<html><script>alert(1)</script></html>".to_vec()),
                            "/slow.png" => {
                                let body = png(640, 480);
                                let head = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\r\n",
                                    body.len()
                                );
                                let half = body.len() / 2;
                                let _ = stream.write_all(head.as_bytes()).await;
                                let _ = stream.write_all(&body[..half]).await;
                                gate.notified().await;
                                let _ = stream.write_all(&body[half..]).await;
                                continue;
                            }
                            "/dead.gif" => {
                                busy.fetch_add(1, Ordering::SeqCst);
                                let _ = stream.read(&mut [0u8; 1]).await;
                                busy.fetch_sub(1, Ordering::SeqCst);
                                return;
                            }
                            _ => {
                                let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").await;
                                continue;
                            }
                        };
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                        let _ = stream.write_all(&body).await;
                    }
                });
            }
        });
        Site { address, asked, go, running }
    }

    impl Site {
        fn asked(&self, path: &str) -> usize {
            self.asked.lock().unwrap().get(path).copied().unwrap_or(0)
        }
    }

    fn limits() -> PictureLimits {
        PictureLimits {
            // Long enough for a busy machine to answer the pictures that are there, short enough for the
            // dead ones to end soon.
            answer: Duration::from_secs(1),
            stall: Duration::from_secs(5),
            total: Duration::from_secs(10),
        }
    }

    fn images(site: &Site, dir: Option<&Path>, cache_mb: u64) -> RemoteImages {
        let egress = Egress::pinned_to(site.address);
        egress.reconfigure(&EgressConfig { image_cache_mb: cache_mb, ..EgressConfig::default() }).unwrap();
        RemoteImages::with_limits(egress, dir.map(Path::to_path_buf), limits())
    }

    #[tokio::test]
    async fn a_picture_is_fetched_once_for_everyone_who_asks_at_the_same_time() {
        let site = site().await;
        let dir = tempfile::tempdir().unwrap();
        let images = images(&site, Some(dir.path()), 1024);
        let url = "http://pictures.example/slow.png";
        let asking: Vec<_> = (0..6)
            .map(|person| {
                let images = images.clone();
                tokio::spawn(async move { images.get(person, url).await })
            })
            .collect();
        let size = images.size(99, url).await.unwrap();
        assert_eq!(size, Some(Dimensions { width: 640, height: 480 }), "known from the first half");
        site.go.notify_one();
        for asking in asking {
            let picture = asking.await.unwrap().unwrap();
            assert_eq!((picture.media_type.as_str(), picture.size), ("image/png", size));
        }
        assert_eq!(site.asked("/slow.png"), 1);

        // Kept on disk for everyone, also after a restart.
        drop(images);
        let again = images_with_cache(&site, dir.path(), 1024);
        assert_eq!(again.get(7, url).await.unwrap().size, size);
        assert_eq!(again.size(8, url).await.unwrap(), size);
        assert_eq!(site.asked("/slow.png"), 1);
    }

    #[tokio::test]
    async fn only_pictures_are_passed_on_with_the_type_they_really_have() {
        let site = site().await;
        let images = images(&site, None, 0);
        let svg = images.get(1, "http://pictures.example/logo.svg").await.unwrap();
        assert_eq!((svg.media_type.as_str(), svg.size), ("image/svg+xml", Some(Dimensions { width: 120, height: 40 })));
        assert_eq!(images.get(1, "http://pictures.example/page.png").await.unwrap_err(), PictureError::NotAPicture);
        assert_eq!(
            images.get(1, "http://pictures.example/gone.png").await.unwrap_err(),
            PictureError::Egress(EgressError::Status(404))
        );
        assert!(matches!(
            images.get(1, "http://10.0.0.1/inside.png").await.unwrap_err(),
            PictureError::Egress(EgressError::NotAllowed(_))
        ));
        // Failures are remembered for a while rather than asked again.
        assert!(images.get(2, "http://pictures.example/gone.png").await.is_err());
        assert_eq!(site.asked("/gone.png"), 1);
    }

    #[tokio::test]
    async fn a_dead_host_costs_little_and_one_person_can_not_hold_up_another() {
        let site = site().await;
        let images = images(&site, None, 0);
        // Someone's newsletter full of dead tracking pixels.
        let dead: Vec<_> = (0..2 * PER_PERSON)
            .map(|n| {
                let images = images.clone();
                tokio::spawn(async move { images.get(1, &format!("http://tracker{n}.example/dead.gif")).await })
            })
            .collect();
        while site.running.load(Ordering::SeqCst) < PER_PERSON {
            tokio::task::yield_now().await;
        }
        let picture = images.get(2, "http://pictures.example/wide.png").await.unwrap();
        assert_eq!(picture.size, Some(Dimensions { width: 300, height: 100 }));
        assert!(!dead.iter().any(|dead| dead.is_finished()), "came while the dead ones were still waiting");
        assert!(site.running.load(Ordering::SeqCst) <= PER_PERSON, "one person runs a few at a time");
        for dead in dead {
            assert_eq!(dead.await.unwrap().unwrap_err(), PictureError::Egress(EgressError::Timeout));
        }
    }

    /// IMG-3 of the 0.18.0 audit: half-written files of a crash go at the next start, whatever the
    /// process id; one being written right now stays.
    #[test]
    fn leftovers_of_a_crash_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let shard = dir.path().join("ab");
        std::fs::create_dir_all(&shard).unwrap();
        let old = shard.join("ab12.part1");
        std::fs::write(&old, b"half").unwrap();
        let file = std::fs::File::options().write(true).open(&old).unwrap();
        file.set_modified(std::time::SystemTime::now() - Duration::from_secs(3600)).unwrap();
        drop(file);
        let writing = shard.join("ab34.part-0011223344556677");
        std::fs::write(&writing, b"half").unwrap();
        assert!(scan(dir.path()).is_empty());
        assert!(!old.exists(), "the leftover went");
        assert!(writing.exists(), "the write under way stays");
    }

    #[tokio::test]
    async fn the_cache_keeps_to_its_size_and_forgets_what_was_asked_for_least() {
        let site = site().await;
        let dir = tempfile::tempdir().unwrap();
        let images = images(&site, Some(dir.path()), 1024);
        let key_of = |url: &str| key(url);
        images.get(1, "http://pictures.example/wide.png").await.unwrap();
        images.get(1, "http://pictures.example/tall.png").await.unwrap();
        let (held, tall) = {
            let mut index = images.inner.index.lock().unwrap();
            // Asked for less lately than the other one.
            let tall = index.entries.get_mut(&key_of("http://pictures.example/tall.png")).unwrap();
            tall.used = 0;
            let tall = tall.bytes;
            (index.bytes, tall)
        };
        // Just enough room for one of them once 10 % are left free.
        images.inner.trim(((held - tall) / 9 + 1) * 10).await;
        assert!(images.inner.path(&key_of("http://pictures.example/wide.png")).unwrap().exists());
        assert!(!images.inner.path(&key_of("http://pictures.example/tall.png")).unwrap().exists());

        // An entry older than a week is fetched again.
        let path = images.inner.path(&key_of("http://pictures.example/wide.png")).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[8..16].copy_from_slice(&(crate::now() - 8 * 24 * 3600).to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        images.get(1, "http://pictures.example/wide.png").await.unwrap();
        assert_eq!(site.asked("/wide.png"), 2);

        // Switched off, nothing is kept.
        let off = images_with_cache(&site, dir.path(), 0);
        off.get(1, "http://pictures.example/logo.svg").await.unwrap();
        assert!(!off.inner.path(&key_of("http://pictures.example/logo.svg")).unwrap().exists());
    }

    fn images_with_cache(site: &Site, dir: &Path, cache_mb: u64) -> RemoteImages {
        images(site, Some(dir), cache_mb)
    }

    #[test]
    fn sizes_are_read_from_the_first_bytes() {
        let picture = png(800, 600);
        assert_eq!(dimensions(&picture[..64]), Some(Dimensions { width: 800, height: 600 }));
        assert_eq!(dimensions(b"GIF89a\x10\x00\x20\x00"), Some(Dimensions { width: 16, height: 32 }));
        assert_eq!(dimensions(b"<html>"), None);
        assert_eq!(dimensions(&[0x89, b'P', b'N']), None);
        let svg = |text: &str| svg_size(text.as_bytes());
        assert_eq!(svg(r#"<svg width="100" height="50px">"#), Some(Dimensions { width: 100, height: 50 }));
        assert_eq!(
            svg(r#"<?xml version="1.0"?><svg viewBox="0 0 24 12">"#),
            Some(Dimensions { width: 24, height: 12 })
        );
        assert_eq!(svg(r#"<svg width="100%" height="2em">"#), None);
        assert_eq!(svg(r#"<svg stroke-width="3" height="5">"#), None);
        assert_eq!(svg("<svg ünïcödé width='7' height='9'>"), Some(Dimensions { width: 7, height: 9 }));
    }

    #[test]
    fn only_pictures_are_passed_on() {
        assert_eq!(picture_type("image/png", &png(1, 1)).unwrap(), "image/png");
        assert_eq!(picture_type("text/html", &png(1, 1)).unwrap(), "image/png", "the bytes say what it is");
        assert_eq!(picture_type("image/svg+xml", b"<svg/>").unwrap(), "image/svg+xml");
        assert_eq!(picture_type("", b"GIF89a").unwrap(), "image/gif");
        assert_eq!(picture_type("application/octet-stream", b"\xff\xd8\xff\xe0").unwrap(), "image/jpeg");
        assert_eq!(picture_type("image/tiff", b"II*\0").unwrap(), "image/tiff");
        assert_eq!(picture_type("text/html", b"<html>"), None);
        assert_eq!(picture_type("image/png", b"<html><script>"), None, "markup is no picture");
        assert_eq!(picture_type("text/html", b"<svg onload=alert(1)>"), None);
        assert_eq!(picture_type("image/svg+xml", b"just text"), None);
        assert_eq!(picture_type("image/", b"x"), None);
        assert_eq!(picture_type("image/png\r\nx: y", b"x"), None);
    }
}
