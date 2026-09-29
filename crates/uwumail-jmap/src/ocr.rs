//! Reading the text in pictures (OCR) with Tesseract, run as a program of its own for each picture
//! (docs/jmap-image-text.md).
//!
//! A picture is shrunk to at most [`MAX_SIDE`] pixels a side and turned grey before Tesseract sees it,
//! which bounds the memory and time it takes; it may run [`TIMEOUT`] and is killed after that. At most
//! [`AT_ONCE`] run at the same time on the whole server. What came out is kept by the picture's hash in
//! `cache/ocr` in the data directory, so a newsletter sent to many is read once.

use std::io::Cursor;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{OnceCell, Semaphore};

/// Pictures smaller than this on either side hold no text worth reading (icons, spacers, pixels).
pub const MIN_SIDE: u32 = 64;
/// Pictures with more pixels than this are not read at all.
pub const MAX_PIXELS: u64 = 40_000_000;
/// Bigger pictures are shrunk to this many pixels on their longer side.
pub const MAX_SIDE: u32 = 2400;
/// Bigger files are not read.
pub const MAX_BYTES: usize = 10 * 1024 * 1024;
/// Tesseract runs at the same time, on the whole server.
const AT_ONCE: usize = 2;
const TIMEOUT: Duration = Duration::from_secs(20);
/// What Tesseract writes is cut off here.
const MAX_OUTPUT: usize = 256 * 1024;
/// ... and the text passed on here, in characters.
const MAX_TEXT_CHARS: usize = 20_000;
/// Results kept on disk; the oldest go first.
const MAX_CACHED: usize = 20_000;

/// `[ocr]` in the configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OcrConfig {
    /// Reading text in pictures at all. On by default; it does nothing while Tesseract is missing.
    pub enabled: bool,
    /// The Tesseract program: a name looked up in `PATH`, or a path.
    pub command: String,
    /// Tesseract's languages, joined with `+`.
    pub languages: String,
}

impl Default for OcrConfig {
    fn default() -> Self {
        OcrConfig { enabled: true, command: "tesseract".into(), languages: "deu+eng".into() }
    }
}

/// The text in one picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Read {
    pub text: String,
    /// The picture's own size, before it was shrunk for reading.
    pub width: u32,
    pub height: u32,
}

/// Why a picture was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Skip {
    /// Too small, too big, or not a kind of picture that can be read here.
    Unsuitable,
    /// Tesseract failed or took too long.
    Failed,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Cached {
    Read(Read),
    Skipped,
}

pub struct Ocr {
    config: OcrConfig,
    available: OnceCell<bool>,
    running: Semaphore,
    cache: Option<PathBuf>,
    /// How long Tesseract may take for one picture: [`TIMEOUT`], shorter in tests.
    timeout: Duration,
}

impl Ocr {
    /// Keeps results in `cache` (created when needed); `None` keeps nothing.
    pub fn new(config: OcrConfig, cache: Option<PathBuf>) -> Ocr {
        Ocr { config, available: OnceCell::new(), running: Semaphore::new(AT_ONCE), cache, timeout: TIMEOUT }
    }

    /// Whether Tesseract is there and switched on: asked once, the first time it matters.
    pub async fn available(&self) -> bool {
        if !self.config.enabled {
            return false;
        }
        *self
            .available
            .get_or_init(|| async {
                let run = tokio::process::Command::new(&self.config.command)
                    .arg("--version")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .status();
                let found = matches!(tokio::time::timeout(Duration::from_secs(10), run).await, Ok(Ok(status)) if status.success());
                if found {
                    tracing::info!(command = %self.config.command, "text in pictures is read with Tesseract");
                } else {
                    tracing::info!(
                        command = %self.config.command,
                        "Tesseract was not found; text in pictures is not read (docs/jmap-image-text.md)"
                    );
                }
                found
            })
            .await
    }

    /// The text in a picture, or why it was not read.
    pub async fn read(&self, bytes: &[u8]) -> Result<Read, Skip> {
        if bytes.len() > MAX_BYTES {
            return Err(Skip::Unsuitable);
        }
        let hash = {
            let mut hasher = Sha256::new();
            hasher.update(self.config.languages.as_bytes());
            hasher.update([0]);
            hasher.update(bytes);
            hex::encode(hasher.finalize())
        };
        if let Some(cached) = self.cached(&hash).await {
            return match cached {
                Cached::Read(read) => Ok(read),
                Cached::Skipped => Err(Skip::Unsuitable),
            };
        }
        // Decoding a big picture takes memory as well: it waits for its turn like Tesseract.
        let _turn = self.running.acquire().await.map_err(|_| Skip::Failed)?;
        let owned = bytes.to_vec();
        let prepared = tokio::task::spawn_blocking(move || prepare(&owned)).await.map_err(|_| Skip::Failed)?;
        let Some((png, width, height)) = prepared else {
            self.keep(&hash, &Cached::Skipped).await;
            return Err(Skip::Unsuitable);
        };
        let text = self.tesseract(png).await.ok_or(Skip::Failed)?;
        let read = Read { text, width, height };
        self.keep(&hash, &Cached::Read(read.clone())).await;
        Ok(read)
    }

    async fn tesseract(&self, png: Vec<u8>) -> Option<String> {
        let mut child = tokio::process::Command::new(&self.config.command)
            // The picture comes on stdin, the text leaves on stdout; no files.
            .args(["stdin", "stdout", "-l", &self.config.languages])
            // One thread each; AT_ONCE bounds how many run.
            .env("OMP_THREAD_LIMIT", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .inspect_err(|err| tracing::warn!(%err, "Tesseract could not be started"))
            .ok()?;
        let mut stdin = child.stdin.take()?;
        let mut stdout = child.stdout.take()?;
        let work = async move {
            let feed = async move {
                let _ = stdin.write_all(&png).await;
                drop(stdin);
            };
            let mut output = Vec::new();
            let collect = async {
                let _ = (&mut stdout).take(MAX_OUTPUT as u64).read_to_end(&mut output).await;
            };
            tokio::join!(feed, collect);
            let status = child.wait().await.ok()?;
            status.success().then_some(output)
        };
        let output = match tokio::time::timeout(self.timeout, work).await {
            Ok(output) => output?,
            Err(_) => {
                tracing::debug!("Tesseract took too long on a picture and was stopped");
                return None;
            }
        };
        let text = String::from_utf8_lossy(&output);
        let text = text.trim();
        Some(match text.char_indices().nth(MAX_TEXT_CHARS) {
            Some((cut, _)) => text[..cut].to_owned(),
            None => text.to_owned(),
        })
    }

    fn path(&self, hash: &str) -> Option<PathBuf> {
        Some(self.cache.as_ref()?.join(&hash[..2]).join(format!("{hash}.json")))
    }

    async fn cached(&self, hash: &str) -> Option<Cached> {
        let bytes = tokio::fs::read(self.path(hash)?).await.ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    async fn keep(&self, hash: &str, result: &Cached) {
        let (Some(path), Ok(bytes)) = (self.path(hash), serde_json::to_vec(result)) else { return };
        let Some(dir) = path.parent().map(PathBuf::from) else { return };
        let _ = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir)?;
            let mut nonce = [0u8; 8];
            getrandom::fill(&mut nonce).map_err(|_| std::io::Error::other("the system RNG failed"))?;
            let partial = path.with_extension(format!("part-{}", hex::encode(nonce)));
            std::fs::write(&partial, bytes)?;
            std::fs::rename(&partial, &path)?;
            trim(&dir);
            std::io::Result::Ok(())
        })
        .await;
    }
}

/// Keeps the cache below [`MAX_CACHED`] results: each of its 256 shards to its share, the oldest going
/// first, whenever a new result lands in it.
fn trim(shard: &std::path::Path) {
    let share = MAX_CACHED / 256;
    let Ok(files) = std::fs::read_dir(shard) else { return };
    let mut files: Vec<_> =
        files.flatten().filter_map(|file| Some((file.metadata().ok()?.modified().ok()?, file.path()))).collect();
    if files.len() <= share {
        return;
    }
    files.sort();
    for (_, path) in &files[..files.len() - share] {
        let _ = std::fs::remove_file(path);
    }
}

/// The picture as a grey PNG no bigger than [`MAX_SIDE`], with its own size; None when it is too
/// small, too big or can't be read.
fn prepare(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let reader = || {
        let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(256 * 1024 * 1024);
        limits.max_image_width = Some(20_000);
        limits.max_image_height = Some(20_000);
        reader.limits(limits);
        Some(reader)
    };
    let (width, height) = reader()?.into_dimensions().ok()?;
    if width < MIN_SIDE || height < MIN_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return None;
    }
    // Grey first, then shrunk by whole pixels: a filter's floating-point copy of a large picture
    // took more memory than the decoded picture itself (OCR-3 of the 0.18.0 audit).
    let grey = reader()?.decode().ok()?.into_luma8();
    let grey = if width.max(height) > MAX_SIDE {
        let scale = f64::from(MAX_SIDE) / f64::from(width.max(height));
        let side = |length: u32| ((f64::from(length) * scale).round() as u32).clamp(1, MAX_SIDE);
        image::imageops::thumbnail(&grey, side(width), side(height))
    } else {
        grey
    };
    let mut png = Vec::new();
    grey.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some((png, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbImage::new(width, height).write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
        out
    }

    /// A stand-in for Tesseract: a shell script that answers `--version`, counts its runs in `runs`
    /// and prints `text` for any picture, after `pause` seconds.
    fn fake(dir: &std::path::Path, text: &str, pause: u32) -> OcrConfig {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("tesseract");
        let runs = dir.join("runs");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n[ \"$1\" = --version ] && {{ echo 'tesseract 5 (fake)'; exit 0; }}\n\
                 echo run >> '{}'\ncat > /dev/null\n[ {pause} -gt 0 ] && sleep {pause}\n\
                 printf '%s\\n' \"{text}\" \"$*\"\n",
                runs.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        OcrConfig { command: script.display().to_string(), ..OcrConfig::default() }
    }

    fn runs(dir: &std::path::Path) -> usize {
        std::fs::read_to_string(dir.join("runs")).map_or(0, |runs| runs.lines().count())
    }

    #[tokio::test]
    async fn text_is_read_once_per_picture_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let ocr = Ocr::new(fake(dir.path(), "Premiere: Freitag, 9. Oktober", 0), Some(dir.path().join("cache")));
        assert!(ocr.available().await);
        let picture = png(300, 120);
        let read = ocr.read(&picture).await.unwrap();
        assert!(read.text.starts_with("Premiere: Freitag, 9. Oktober"), "{}", read.text);
        assert!(read.text.contains("stdin stdout -l deu+eng"), "{}", read.text);
        assert_eq!((read.width, read.height), (300, 120));
        assert_eq!(ocr.read(&picture).await.unwrap(), read);
        // Another server process with the same cache reads nothing again either.
        let again = Ocr::new(fake(dir.path(), "other", 0), Some(dir.path().join("cache")));
        assert_eq!(again.read(&picture).await.unwrap(), read);
        assert_eq!(runs(dir.path()), 1);
        // Too small to hold text: not handed to Tesseract at all.
        assert_eq!(ocr.read(&png(40, 40)).await, Err(Skip::Unsuitable));
        assert_eq!(runs(dir.path()), 1);
    }

    #[tokio::test]
    async fn tesseract_that_takes_too_long_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let mut ocr = Ocr::new(fake(dir.path(), "late", 30), None);
        ocr.timeout = Duration::from_millis(300);
        let started = std::time::Instant::now();
        assert_eq!(ocr.read(&png(100, 100)).await, Err(Skip::Failed));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn without_tesseract_nothing_is_read() {
        let missing = OcrConfig { command: "/nonexistent/tesseract".into(), ..OcrConfig::default() };
        assert!(!Ocr::new(missing, None).available().await);
        let off = OcrConfig { enabled: false, ..OcrConfig::default() };
        assert!(!Ocr::new(off, None).available().await);
    }

    /// With the real Tesseract (and its German data) in `PATH`, or named in `UWUMAIL_TEST_TESSERACT`.
    #[tokio::test]
    #[ignore = "needs Tesseract with German and English installed"]
    async fn the_real_tesseract_reads_a_poster() {
        let command = std::env::var("UWUMAIL_TEST_TESSERACT").unwrap_or_else(|_| "tesseract".into());
        let ocr = Ocr::new(OcrConfig { command, ..OcrConfig::default() }, None);
        assert!(ocr.available().await, "Tesseract is not installed");
        let poster = include_bytes!("../tests/fixtures/ocr-poster.png");
        let read = ocr.read(poster).await.unwrap();
        assert!(read.text.contains("Freitag, 9. Oktober"), "{}", read.text);
        assert!(read.text.contains("20:00 Uhr"), "{}", read.text);
    }

    #[test]
    fn only_pictures_worth_reading_are_prepared() {
        assert!(prepare(&png(32, 200)).is_none(), "too narrow");
        assert!(prepare(b"not a picture").is_none());
        let (small, width, height) = prepare(&png(200, 100)).unwrap();
        assert_eq!((width, height), (200, 100));
        assert_eq!(image::load_from_memory(&small).unwrap().color(), image::ColorType::L8);
        let (shrunk, width, height) = prepare(&png(4800, 600)).unwrap();
        assert_eq!((width, height), (4800, 600), "its own size");
        let shrunk = image::load_from_memory(&shrunk).unwrap();
        assert_eq!((shrunk.width(), shrunk.height()), (2400, 300));
    }
}
