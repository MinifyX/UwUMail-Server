//! How a backup repository looks: a small plain config, objects under `data/<2 hex>/<id>` and one
//! manifest per snapshot under `snapshots/`. Every object is compressed and, unless the repository
//! was created without, encrypted with ChaCha20-Poly1305.
//!
//! An object's id comes from the SHA-256 of its content: keyed with HMAC in an encrypted repository,
//! so the names on the backup server give nothing away, and plain otherwise. The same content is
//! stored once, whichever snapshot brought it.

use std::io::{Read, Write};

use aws_lc_rs::aead::{CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use aws_lc_rs::hmac;
use data_encoding::BASE32_NOPAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Error;

pub const CONFIG_PATH: &str = "uwumail-backup.json";
pub const FORMAT: u32 = 1;
const OBJECT_VERSION: u8 = 1;
const FLAG_ENCRYPTED: u8 = 0x80;
const FLAG_COMPRESSED: u8 = 0x01;
const KEY_CHECK: &[u8] = b"uwumail backup key check";

/// The secret of an encrypted repository. It never leaves the server except as the recovery key.
#[derive(Clone)]
pub struct RepoKey {
    secret: [u8; 32],
}

impl RepoKey {
    pub fn generate() -> RepoKey {
        let mut secret = [0u8; 32];
        aws_lc_rs::rand::fill(&mut secret).expect("the system random generator works");
        RepoKey { secret }
    }

    /// The key as people write it down: groups of four letters and digits.
    pub fn recovery_text(&self) -> String {
        let encoded = BASE32_NOPAD.encode(&self.secret);
        encoded.as_bytes().chunks(4).map(|chunk| String::from_utf8_lossy(chunk)).collect::<Vec<_>>().join("-")
    }

    /// Reads a recovery key; dashes, spaces and lower case do not matter.
    pub fn from_recovery_text(text: &str) -> Result<RepoKey, Error> {
        let cleaned: String =
            text.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect();
        let bytes = BASE32_NOPAD.decode(cleaned.as_bytes()).map_err(|_| Error::WrongKey)?;
        let secret: [u8; 32] = bytes.try_into().map_err(|_| Error::WrongKey)?;
        Ok(RepoKey { secret })
    }

    fn derived(&self, purpose: &[u8]) -> [u8; 32] {
        let key = hmac::Key::new(hmac::HMAC_SHA256, &self.secret);
        hmac::sign(&key, purpose).as_ref().try_into().expect("HMAC-SHA256 has 32 bytes")
    }

    fn check(&self) -> String {
        hex::encode(self.derived(KEY_CHECK))
    }
}

/// `uwumail-backup.json`: readable without the key, so a restore knows what to ask for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RepoConfig {
    pub format: u32,
    pub encrypted: bool,
    /// Tells a wrong recovery key apart from damaged data.
    pub key_check: Option<String>,
    pub created_at: i64,
}

/// Turns content into stored objects and back.
#[derive(Clone)]
pub struct Codec {
    key: Option<RepoKey>,
}

impl Codec {
    pub fn new(config: &RepoConfig, key: Option<RepoKey>) -> Result<Codec, Error> {
        match (config.encrypted, key) {
            (false, _) => Ok(Codec { key: None }),
            (true, None) => Err(Error::WrongKey),
            (true, Some(key)) => {
                if config.key_check.as_deref() != Some(key.check().as_str()) {
                    return Err(Error::WrongKey);
                }
                Ok(Codec { key: Some(key) })
            }
        }
    }

    pub fn config_for(key: Option<&RepoKey>, created_at: i64) -> RepoConfig {
        RepoConfig { format: FORMAT, encrypted: key.is_some(), key_check: key.map(RepoKey::check), created_at }
    }

    /// Whether objects lie there unencrypted, so an id is the plain SHA-256 of its content.
    pub fn is_plain(&self) -> bool {
        self.key.is_none()
    }

    /// The id of content whose SHA-256 is `sha256` (hex), e.g. a blob's hash.
    pub fn id_for_hash(&self, sha256: &str) -> String {
        match &self.key {
            Some(key) => {
                let ids = hmac::Key::new(hmac::HMAC_SHA256, &key.derived(b"ids"));
                hex::encode(hmac::sign(&ids, sha256.as_bytes()).as_ref())
            }
            None => sha256.to_owned(),
        }
    }

    pub fn id_for(&self, content: &[u8]) -> String {
        self.id_for_hash(&hex::encode(Sha256::digest(content)))
    }

    pub fn encode(&self, content: &[u8]) -> Result<Vec<u8>, Error> {
        let mut compressor = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        compressor.write_all(content)?;
        let compressed = compressor.finish()?;
        let (flags, body) =
            if compressed.len() < content.len() { (FLAG_COMPRESSED, compressed) } else { (0, content.to_vec()) };
        let Some(key) = &self.key else {
            let mut object = vec![OBJECT_VERSION, flags];
            object.extend_from_slice(&body);
            return Ok(object);
        };
        let mut nonce = [0u8; NONCE_LEN];
        aws_lc_rs::rand::fill(&mut nonce).map_err(|_| Error::Crypto)?;
        let sealing =
            LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, &key.derived(b"encrypt")).map_err(|_| Error::Crypto)?);
        let header = [OBJECT_VERSION, flags | FLAG_ENCRYPTED];
        let mut sealed = body;
        sealing
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                aws_lc_rs::aead::Aad::from(header),
                &mut sealed,
            )
            .map_err(|_| Error::Crypto)?;
        let mut object = header.to_vec();
        object.extend_from_slice(&nonce);
        object.extend_from_slice(&sealed);
        Ok(object)
    }

    pub fn decode(&self, object: &[u8]) -> Result<Vec<u8>, Error> {
        let [version, flags, rest @ ..] = object else { return Err(Error::Damaged("an object is too short".into())) };
        if *version != OBJECT_VERSION {
            return Err(Error::Damaged(format!("unknown object version {version}")));
        }
        let body = if flags & FLAG_ENCRYPTED != 0 {
            let key = self.key.as_ref().ok_or(Error::WrongKey)?;
            if rest.len() < NONCE_LEN {
                return Err(Error::Damaged("an encrypted object is too short".into()));
            }
            let (nonce, sealed) = rest.split_at(NONCE_LEN);
            let opening = LessSafeKey::new(
                UnboundKey::new(&CHACHA20_POLY1305, &key.derived(b"encrypt")).map_err(|_| Error::Crypto)?,
            );
            let mut sealed = sealed.to_vec();
            let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| Error::Crypto)?;
            let header = [*version, *flags];
            let plain = opening
                .open_in_place(nonce, aws_lc_rs::aead::Aad::from(header), &mut sealed)
                .map_err(|_| Error::Damaged("an object does not decrypt; it was changed or the key is wrong".into()))?;
            plain.to_vec()
        } else {
            rest.to_vec()
        };
        if flags & FLAG_COMPRESSED == 0 {
            return Ok(body);
        }
        let mut content = Vec::new();
        flate2::read::DeflateDecoder::new(&body[..]).read_to_end(&mut content)?;
        Ok(content)
    }
}

/// The path of an object in the repository.
pub fn object_path(id: &str) -> String {
    format!("data/{}/{id}", &id[..2])
}

/// A file from the data directory besides the database and the mail blobs, e.g. certificates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// Relative to the data directory, with `/`.
    pub path: String,
    pub id: String,
    pub size: u64,
}

/// What one snapshot holds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: u32,
    pub created_at: i64,
    pub hostname: String,
    pub version: String,
    /// The database, in order.
    pub database: Vec<String>,
    pub database_size: u64,
    /// SHA-256 of every mail blob; their ids follow from the hashes.
    pub blobs: Vec<String>,
    pub blobs_size: u64,
    pub files: Vec<FileEntry>,
    /// Bytes this snapshot had to upload.
    pub uploaded: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_round_trip_and_hide_their_content() {
        let key = RepoKey::generate();
        let config = Codec::config_for(Some(&key), 0);
        let codec = Codec::new(&config, Some(key.clone())).unwrap();
        let content = b"Subject: Hallo\r\n\r\nHallo Hallo Hallo Hallo Hallo Hallo Hallo Hallo".repeat(20);
        let object = codec.encode(&content).unwrap();
        assert!(object.len() < content.len(), "compressed");
        assert!(!object.windows(5).any(|window| window == b"Hallo"), "encrypted");
        assert_eq!(codec.decode(&object).unwrap(), content);
        let id = codec.id_for(&content);
        assert_ne!(id, hex::encode(Sha256::digest(&content)), "ids are keyed");
        assert_eq!(id, codec.id_for_hash(&hex::encode(Sha256::digest(&content))));

        let mut tampered = object.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(matches!(codec.decode(&tampered), Err(Error::Damaged(_))));
        let other = Codec::new(&Codec::config_for(Some(&RepoKey::generate()), 0), Some(RepoKey::generate()));
        assert!(matches!(other, Err(Error::WrongKey)));
        assert!(matches!(Codec::new(&config, Some(RepoKey::generate())), Err(Error::WrongKey)));

        let text = key.recovery_text();
        assert_eq!(text.len(), 52 + 12, "52 characters in groups of four");
        let typed = text.to_lowercase().replace('-', " ");
        assert!(Codec::new(&config, Some(RepoKey::from_recovery_text(&typed).unwrap())).is_ok());
    }

    #[test]
    fn plain_repositories_only_compress() {
        let config = Codec::config_for(None, 0);
        let codec = Codec::new(&config, None).unwrap();
        let object = codec.encode(b"x").unwrap();
        assert_eq!(object, [OBJECT_VERSION, 0, b'x'], "not worth compressing");
        assert_eq!(codec.decode(&object).unwrap(), b"x");
        assert_eq!(codec.id_for(b"x"), hex::encode(Sha256::digest(b"x")));
    }
}
