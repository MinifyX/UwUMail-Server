//! Pictures of senders, per address (docs/jmap-remote.md), and the Libravatar provider for this
//! server's own addresses (docs/profile-pictures.md).
//!
//! `pictureUrl` looks, in this order, for the reader's own contact photo, the picture of a person
//! here, a Face that came with DMARC-aligned mail, the sender's Libravatar picture, the logo of one of
//! our domains, and a company's logo. Masked addresses never get a picture from anyone but the
//! reader's own address book.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Extension;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uwumail_smtp::avatars::libravatar_hashes;
use uwumail_smtp::profile_pictures::{self, contact_photo_type};
use uwumail_store::{AddressPicture, ContactPhoto, PictureOwner, Store, contact_photo};

use crate::auth::ClientInfo;
use crate::{Inner, Jmap};

/// What was found for an address.
pub(crate) enum Found {
    /// A person's picture (steps a to d): fills the circle, and changes whenever they change it.
    Person { media_type: String, bytes: Vec<u8> },
    /// A logo (steps e and f), with its kind and the domain it stands for.
    Logo { media_type: String, bytes: Vec<u8>, kind: &'static str, domain: Option<String> },
}

/// The picture for `email` as `account_id` sees it. `logo_only` looks only for logos; `offline`
/// asks nobody outside and takes only what is known already.
pub(crate) async fn resolve(
    inner: &Inner,
    account_id: i64,
    email: &str,
    logo_only: bool,
    offline: bool,
) -> Option<Found> {
    let store = &inner.store;
    // a. The reader's own contact photo, even for a masked address: it is their own card.
    if !logo_only && let Some(card) = store.contact_card_with_photo(account_id, email).await.ok().flatten() {
        match contact_photo(&card) {
            Some(ContactPhoto::Inline(bytes)) => {
                if let Some(media_type) = contact_photo_type(&bytes) {
                    return Some(Found::Person { media_type: media_type.to_owned(), bytes });
                }
            }
            Some(ContactPhoto::Remote(url)) => {
                if let Some(avatar) = inner.avatars.linked_photo(account_id, &url, offline).await {
                    return Some(Found::Person { media_type: avatar.media_type.to_owned(), bytes: avatar.bytes });
                }
            }
            None => {}
        }
    }
    // b. Someone here, when they let people here see their picture.
    let local = match store.address_picture(email, logo_only).await {
        Ok(local) => local,
        Err(err) => {
            tracing::warn!(%err, "looking up a picture of this server failed");
            AddressPicture::Nothing
        }
    };
    let local = match local {
        AddressPicture::Masked => return None,
        AddressPicture::Person(picture) => {
            return Some(Found::Person { media_type: picture.media_type, bytes: picture.bytes });
        }
        other => other,
    };
    if !logo_only {
        // c. The newest Face from mail that passed DMARC for its From domain. Not for our own
        //    addresses: whether people here see their picture is step b's to decide, and a Face
        //    kept from their mail must not outlast turning it off.
        if local == AddressPicture::NotLocal
            && let Some((png, _)) = store.received_face(email).await.ok().flatten()
        {
            return Some(Found::Person { media_type: "image/png".into(), bytes: png });
        }
        // d. Libravatar, only where the sender's domain publishes it and never for our own domains.
        if local == AddressPicture::NotLocal
            && let Some(avatar) = inner.avatars.libravatar(email, offline).await
        {
            return Some(Found::Person { media_type: avatar.media_type.to_owned(), bytes: avatar.bytes });
        }
    }
    // e. The logo of one of our domains.
    if let AddressPicture::Logo(logo) = local {
        let domain = email.rsplit_once('@').map(|(_, domain)| domain.trim().to_ascii_lowercase());
        return Some(Found::Logo { media_type: logo.media_type, bytes: logo.bytes, kind: "logo", domain });
    }
    // f. A company's logo or website icon, as before.
    let found = if offline { inner.pictures.cached(email) } else { inner.pictures.get(email).await };
    found.map(|picture| Found::Logo {
        media_type: picture.media_type.to_owned(),
        bytes: picture.bytes.to_vec(),
        kind: picture.kind.as_str(),
        domain: Some(picture.domain),
    })
}

/// The ETag of a person's picture: it changes with the picture.
pub(crate) fn etag(bytes: &[u8]) -> String {
    format!("\"{}\"", &hex::encode(Sha256::digest(bytes))[..32])
}

/// Headers every picture answer carries, so that opening one on its own runs nothing on this origin.
pub(crate) fn sandbox(headers: &mut HeaderMap) {
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
    );
}

/// Requests one network may make to `/avatar/` per minute.
const LIBRAVATAR_PER_MINUTE: u32 = 120;
/// Networks remembered at once; past it the ones whose minute is over are forgotten.
const LIBRAVATAR_NETWORKS: usize = 10_000;
/// Sizes Libravatar clients may ask for (`s=`), and the one they get without asking.
const MAX_AVATAR_SIZE: u32 = 512;
const DEFAULT_AVATAR_SIZE: u32 = 80;

/// The network a request counts for: a /24 for IPv4, a /48 for IPv6.
fn network(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            IpAddr::V4([a, b, c, 0].into())
        }
        IpAddr::V6(v6) => {
            let mut segments = v6.segments();
            segments[3..].fill(0);
            IpAddr::V6(segments.into())
        }
    }
}

struct Table {
    version: i64,
    owners: HashMap<String, PictureOwner>,
}

/// The Libravatar side of the server: which hash stands for which public picture, made anew only when
/// the addresses or who may see what changed, and how often each network asked lately.
pub(crate) struct Provider {
    store: Store,
    table: tokio::sync::Mutex<Table>,
    asked: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl Provider {
    pub fn new(store: Store) -> Provider {
        Provider {
            store,
            table: tokio::sync::Mutex::new(Table { version: -1, owners: HashMap::new() }),
            asked: Mutex::new(HashMap::new()),
        }
    }

    /// Whose public picture a hash stands for.
    async fn owner(&self, hash: &str) -> Option<PictureOwner> {
        let version = self.store.avatar_version().await.ok()?;
        let mut table = self.table.lock().await;
        if table.version != version {
            let mut owners = HashMap::new();
            match self.store.public_avatars().await {
                Ok(addresses) => {
                    for (address, owner) in addresses {
                        for hash in libravatar_hashes(&address) {
                            owners.insert(hash, owner);
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, "reading the public pictures failed");
                    return None;
                }
            }
            *table = Table { version, owners };
        }
        table.owners.get(hash).copied()
    }

    /// Whether a network may ask once more this minute.
    fn allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut asked = self.asked.lock().unwrap_or_else(|e| e.into_inner());
        if asked.len() >= LIBRAVATAR_NETWORKS {
            asked.retain(|_, (since, _)| now.duration_since(*since) < Duration::from_secs(60));
        }
        // Still full: networks not yet known share one count, so the map stays bounded however
        // many networks ask at once.
        let mut key = network(ip);
        if asked.len() >= LIBRAVATAR_NETWORKS && !asked.contains_key(&key) {
            key = IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED);
        }
        let entry = asked.entry(key).or_insert((now, 0));
        if now.duration_since(entry.0) >= Duration::from_secs(60) {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= LIBRAVATAR_PER_MINUTE
    }
}

#[derive(Deserialize, Default)]
pub struct AvatarQuery {
    s: Option<String>,
    size: Option<String>,
    d: Option<String>,
    default: Option<String>,
}

fn image_response(bytes: Vec<u8>, media_type: &str, cache: &'static str) -> Response {
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(media_type).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; sandbox"));
    response
}

/// What to answer when there is no public picture for the hash, as `d=` asks: `404`, the silhouette
/// (`mm`, `mp`, and anything this server does not draw), a transparent picture (`blank`), or a
/// redirect to an `https:` address.
async fn fallback(default: Option<&str>, size: u32) -> Response {
    let default = default.unwrap_or("mm").trim();
    if default == "404" {
        return (StatusCode::NOT_FOUND, [(header::CACHE_CONTROL, "public, max-age=3600")]).into_response();
    }
    if default.len() <= 2048
        && default.get(..8).is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
        && let Ok(location) = HeaderValue::from_str(default)
    {
        let mut response = StatusCode::FOUND.into_response();
        response.headers_mut().insert(header::LOCATION, location);
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600"));
        return response;
    }
    let blank = default == "blank";
    let picture = tokio::task::spawn_blocking(move || {
        if blank { profile_pictures::blank(size) } else { profile_pictures::silhouette(size) }
    })
    .await
    .unwrap_or_default();
    image_response(picture, "image/png", "public, max-age=3600")
}

/// `GET /avatar/<md5 or sha256>`: the public picture of one of this server's addresses, for
/// Libravatar clients that found the server through `_avatars-sec._tcp`. No login; asked often from
/// one network, it answers `429`.
pub async fn libravatar(
    State(jmap): State<Jmap>,
    Path(hash): Path<String>,
    Query(query): Query<AvatarQuery>,
    client: Option<Extension<ClientInfo>>,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let provider = &jmap.inner.libravatar;
    if !provider.allow(client.ip) {
        return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "60")]).into_response();
    }
    let hash = hash.trim().to_ascii_lowercase();
    if !matches!(hash.len(), 32 | 64) || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let size = query
        .s
        .as_deref()
        .or(query.size.as_deref())
        .and_then(|size| size.trim().parse::<u32>().ok())
        .map_or(DEFAULT_AVATAR_SIZE, |size| size.clamp(1, MAX_AVATAR_SIZE));
    let default = query.d.as_deref().or(query.default.as_deref());
    let Some(owner) = provider.owner(&hash).await else {
        return fallback(default, size).await;
    };
    let Some(picture) = jmap.inner.store.picture(owner).await.ok().flatten() else {
        return fallback(default, size).await;
    };
    let scaled = tokio::task::spawn_blocking(move || profile_pictures::scaled(&picture.bytes, size)).await;
    match scaled {
        Ok(Some((bytes, media_type))) => image_response(bytes, media_type, "public, max-age=3600"),
        _ => fallback(default, size).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks_are_counted_whole() {
        assert_eq!(network("192.0.2.77".parse().unwrap()), "192.0.2.0".parse::<IpAddr>().unwrap());
        assert_eq!(network("2001:db8:1:2:3::4".parse().unwrap()), "2001:db8:1::".parse::<IpAddr>().unwrap());
        assert_eq!(network("::ffff:192.0.2.9".parse().unwrap()), "192.0.2.0".parse::<IpAddr>().unwrap());
    }
}
