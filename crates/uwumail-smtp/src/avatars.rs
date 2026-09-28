//! Pictures of people who write to us, fetched by the server (docs/jmap-remote.md, docs/profile-pictures.md):
//! a contact photo that is only an `https:` link, and the Libravatar picture of a sender whose domain
//! publishes `_avatars-sec._tcp`. Never Gravatar, never libravatar.org as a fallback: a domain that
//! does not say where its pictures are gets asked nothing.
//!
//! The requests leave through the egress like remote pictures; DNS is asked directly, as it says
//! nothing about who reads what. What was found — and that nothing was — is kept in memory: a
//! Libravatar answer for a week and for everyone, a contact photo for a day and per account.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::RData;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::egress::{Egress, EgressError};
use crate::profile_pictures::contact_photo_type;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// SRV records as (priority, weight, port, target).
pub type SrvRecords = Vec<(u16, u16, u16, String)>;

/// A Libravatar answer, found or not, is good for a week.
const LIBRAVATAR_FRESH: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// A linked contact photo is asked for again after a day.
const PHOTO_FRESH: Duration = Duration::from_secs(24 * 60 * 60);
/// After a server that did not answer, wait before asking it again.
const RETRY_AFTER: Duration = Duration::from_secs(30 * 60);
const DNS_TIMEOUT: Duration = Duration::from_secs(8);
/// The largest person picture taken from elsewhere. Libravatar pictures are at most 512 × 512.
pub const MAX_AVATAR_BYTES: usize = 1024 * 1024;
/// A linked contact photo may be as big as a remote picture in a message.
pub const MAX_LINKED_PHOTO_BYTES: usize = 10 * 1024 * 1024;
const MAX_CACHED_BYTES: usize = 32 * 1024 * 1024;
const MAX_CACHED_ENTRIES: usize = 20_000;
const PARALLEL_LOOKUPS: usize = 8;
/// How long a lookup waits for one of the [`PARALLEL_LOOKUPS`]; slow servers elsewhere must not
/// hold up every sender picture on the server.
const WAIT_FOR_A_TURN: Duration = Duration::from_secs(5);
const ACCEPT: &str = "image/png,image/jpeg,image/webp,image/gif,image/*;q=0.8";

/// A person's picture from elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Avatar {
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
}

/// How pictures of people are reached: web requests through the egress, SRV records from DNS. Its
/// own trait so tests can stand in for the internet.
pub trait AvatarNet: Send + Sync {
    /// GETs a public https address: the media type sent and the body.
    fn get<'a>(&'a self, url: &'a str, max_bytes: usize) -> BoxFuture<'a, Result<(String, Vec<u8>), EgressError>>;
    /// The SRV records of a name; `Err` when DNS did not answer.
    fn srv<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<SrvRecords, ()>>;
}

/// The real way out.
pub struct LiveNet {
    egress: Egress,
    resolver: Option<TokioResolver>,
}

impl LiveNet {
    pub fn new(egress: Egress) -> LiveNet {
        let resolver = mail_auth::MessageAuthenticator::new_system_conf().ok().map(|auth| auth.resolver().clone());
        LiveNet { egress, resolver }
    }
}

impl AvatarNet for LiveNet {
    fn get<'a>(&'a self, url: &'a str, max_bytes: usize) -> BoxFuture<'a, Result<(String, Vec<u8>), EgressError>> {
        Box::pin(async move {
            let fetched = self.egress.get(url, ACCEPT, max_bytes).await?;
            Ok((fetched.media_type, fetched.body.to_vec()))
        })
    }

    fn srv<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Result<SrvRecords, ()>> {
        Box::pin(async move {
            let Some(resolver) = &self.resolver else { return Err(()) };
            match tokio::time::timeout(DNS_TIMEOUT, resolver.srv_lookup(format!("{name}."))).await {
                Ok(Ok(lookup)) => Ok(lookup
                    .answers()
                    .iter()
                    .filter_map(|record| match &record.data {
                        RData::SRV(srv) => Some((
                            srv.priority,
                            srv.weight,
                            srv.port,
                            srv.target.to_ascii().trim_end_matches('.').to_ascii_lowercase(),
                        )),
                        _ => None,
                    })
                    .collect()),
                Ok(Err(err)) if err.is_no_records_found() || err.is_nx_domain() => Ok(Vec::new()),
                _ => Err(()),
            }
        })
    }
}

struct Entry {
    until: Instant,
    avatar: Option<Avatar>,
}

/// Answers kept in memory, the oldest going first past a size.
struct Cache<K> {
    entries: HashMap<K, Entry>,
    bytes: usize,
}

impl<K: Eq + Hash + Clone> Cache<K> {
    fn new() -> Cache<K> {
        Cache { entries: HashMap::new(), bytes: 0 }
    }

    /// The answer while it is fresh; `Some(None)` is "nothing there".
    fn fresh(&self, key: &K) -> Option<Option<Avatar>> {
        self.entries.get(key).filter(|entry| entry.until > Instant::now()).map(|entry| entry.avatar.clone())
    }

    fn insert(&mut self, key: K, avatar: Option<Avatar>, fresh_for: Duration) {
        if let Some(old) = self.entries.remove(&key) {
            self.bytes -= old.avatar.map_or(0, |avatar| avatar.bytes.len());
        }
        self.bytes += avatar.as_ref().map_or(0, |avatar| avatar.bytes.len());
        self.entries.insert(key, Entry { until: Instant::now() + fresh_for, avatar });
        while self.bytes > MAX_CACHED_BYTES || self.entries.len() > MAX_CACHED_ENTRIES {
            let Some(oldest) = self.entries.iter().min_by_key(|(_, entry)| entry.until).map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(old) = self.entries.remove(&oldest) {
                self.bytes -= old.avatar.map_or(0, |avatar| avatar.bytes.len());
            }
        }
    }
}

enum Lookup {
    Found(Avatar),
    Nothing,
    /// Nobody answered; asked again after [`RETRY_AFTER`].
    Unreachable,
}

/// The name a domain's Libravatar server is published under.
pub fn libravatar_srv_name(domain: &str) -> String {
    format!("_avatars-sec._tcp.{domain}")
}

/// The hash Libravatar knows an address by: SHA-256 of it in lower case, hex.
pub fn libravatar_hash(email: &str) -> String {
    hex::encode(Sha256::digest(email.trim().to_lowercase().as_bytes()))
}

/// Both hashes Libravatar clients may ask for an address with: MD5 (as Gravatar) and SHA-256.
pub fn libravatar_hashes(email: &str) -> [String; 2] {
    use md5::Md5;
    let lower = email.trim().to_lowercase();
    [hex::encode(Md5::digest(lower.as_bytes())), hex::encode(Sha256::digest(lower.as_bytes()))]
}

/// The domain of an address when it is a domain name that can be asked about.
fn domain_of(email: &str) -> Option<String> {
    let (_, host) = email.trim().rsplit_once('@')?;
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let valid = !host.is_empty()
        && host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        });
    valid.then_some(host)
}

/// The best target of SRV records: lowest priority, then highest weight. A single `.` says there is
/// no such service.
fn best_target(mut records: SrvRecords) -> Option<(String, u16)> {
    records.sort_by_key(|(priority, weight, _, _)| (*priority, std::cmp::Reverse(*weight)));
    records
        .into_iter()
        .find(|(_, _, port, target)| *port != 0 && !target.is_empty() && target != ".")
        .map(|(_, _, port, target)| (target, port))
        .filter(|(target, _)| domain_of(&format!("x@{target}")).is_some())
}

pub struct Avatars {
    net: Arc<dyn AvatarNet>,
    libravatar: Mutex<Cache<String>>,
    photos: Mutex<Cache<(i64, String)>>,
    permits: Semaphore,
}

impl Avatars {
    pub fn new(net: Arc<dyn AvatarNet>) -> Avatars {
        Avatars {
            net,
            libravatar: Mutex::new(Cache::new()),
            photos: Mutex::new(Cache::new()),
            permits: Semaphore::new(PARALLEL_LOOKUPS),
        }
    }

    /// The Libravatar picture of an address of another server, if its domain has a Libravatar
    /// server. `offline` only looks at what is already known.
    pub async fn libravatar(&self, email: &str, offline: bool) -> Option<Avatar> {
        let domain = domain_of(email)?;
        let hash = libravatar_hash(email);
        if let Some(known) = self.libravatar.lock().unwrap_or_else(|e| e.into_inner()).fresh(&hash) {
            return known;
        }
        if offline {
            return None;
        }
        let lookup = {
            let _permit = tokio::time::timeout(WAIT_FOR_A_TURN, self.permits.acquire()).await.ok()?.ok()?;
            self.ask_libravatar(&domain, &hash).await
        };
        let (avatar, fresh_for) = match lookup {
            Lookup::Found(avatar) => (Some(avatar), LIBRAVATAR_FRESH),
            Lookup::Nothing => (None, LIBRAVATAR_FRESH),
            Lookup::Unreachable => (None, RETRY_AFTER),
        };
        self.libravatar.lock().unwrap_or_else(|e| e.into_inner()).insert(hash, avatar.clone(), fresh_for);
        avatar
    }

    async fn ask_libravatar(&self, domain: &str, hash: &str) -> Lookup {
        let records = match self.net.srv(&libravatar_srv_name(domain)).await {
            Ok(records) => records,
            Err(()) => return Lookup::Unreachable,
        };
        let Some((target, port)) = best_target(records) else { return Lookup::Nothing };
        let url = format!("https://{target}:{port}/avatar/{hash}?s=128&d=404");
        self.fetch(&url, MAX_AVATAR_BYTES).await
    }

    async fn fetch(&self, url: &str, max_bytes: usize) -> Lookup {
        match self.net.get(url, max_bytes).await {
            Ok((_, bytes)) => match contact_photo_type(&bytes) {
                Some(media_type) => Lookup::Found(Avatar { media_type, bytes }),
                None => Lookup::Nothing,
            },
            Err(EgressError::Unreachable | EgressError::Timeout) => Lookup::Unreachable,
            Err(_) => Lookup::Nothing,
        }
    }

    /// A contact photo that is only an `https:` link, fetched for one account and kept for it.
    /// `offline` only looks at what is already known.
    pub async fn linked_photo(&self, account_id: i64, url: &str, offline: bool) -> Option<Avatar> {
        let key = (account_id, url.to_owned());
        if let Some(known) = self.photos.lock().unwrap_or_else(|e| e.into_inner()).fresh(&key) {
            return known;
        }
        if offline {
            return None;
        }
        let lookup = {
            let _permit = tokio::time::timeout(WAIT_FOR_A_TURN, self.permits.acquire()).await.ok()?.ok()?;
            self.fetch(url, MAX_LINKED_PHOTO_BYTES).await
        };
        let (avatar, fresh_for) = match lookup {
            Lookup::Found(avatar) => (Some(avatar), PHOTO_FRESH),
            Lookup::Nothing => (None, PHOTO_FRESH),
            Lookup::Unreachable => (None, RETRY_AFTER),
        };
        self.photos.lock().unwrap_or_else(|e| e.into_inner()).insert(key, avatar.clone(), fresh_for);
        avatar
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srv_targets_and_hashes() {
        assert_eq!(
            best_target(vec![(10, 5, 443, "b.example.org".into()), (0, 1, 8443, "a.example.org".into())]),
            Some(("a.example.org".into(), 8443))
        );
        assert_eq!(best_target(vec![(0, 0, 0, ".".into())]), None);
        assert_eq!(best_target(vec![(0, 0, 443, "bad host".into())]), None);
        assert_eq!(libravatar_hash(" Mini@Example.ORG "), libravatar_hash("mini@example.org"));
        assert_eq!(domain_of("x@[192.0.2.1]"), None);
        assert_eq!(domain_of("x@Example.org."), Some("example.org".into()));
    }

    #[test]
    fn the_cache_forgets_the_oldest_past_its_size() {
        let mut cache: Cache<String> = Cache::new();
        let big = || Some(Avatar { media_type: "image/png", bytes: vec![0; MAX_CACHED_BYTES / 2] });
        cache.insert("a".into(), big(), PHOTO_FRESH);
        cache.insert("b".into(), big(), PHOTO_FRESH * 2);
        cache.insert("c".into(), big(), PHOTO_FRESH * 3);
        assert!(!cache.entries.contains_key("a"));
        assert!(cache.bytes <= MAX_CACHED_BYTES);
        assert_eq!(cache.fresh(&"c".to_owned()).unwrap().unwrap().bytes.len(), MAX_CACHED_BYTES / 2);
    }
}
