//! Backups into an S3 bucket: Amazon S3 or any server that speaks its API (MinIO, Backblaze B2,
//! Hetzner Object Storage, Wasabi, Garage, …).
//!
//! Only four requests are needed: put, get and delete an object, and list what is under a prefix.
//! Each one is signed with AWS Signature Version 4, written out here rather than pulled in with an
//! SDK: it is a handful of HMACs, and the whole of it fits on a page.
//!
//! An object is written in one PUT, which S3 stores whole or not at all, so there is no temporary
//! name and no rename as on SFTP. What the bucket answers is treated like anything a backup server
//! says: sizes are capped before they are read, and names in listings are checked by the caller.

use std::time::Duration;

use aws_lc_rs::hmac;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use sha2::{Digest, Sha256};

use crate::Error;
use crate::target::S3Target;

/// How long an answer may take to begin, on top of the time the upload itself needs.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);
/// The slowest upload this still waits for, in bytes per second.
const SLOWEST_UPLOAD: u64 = 32 * 1024;
/// How long the answer may pause between two parts of its body.
const BODY_TIMEOUT: Duration = Duration::from_secs(120);
/// What an error answer or a listing page may be at most. A page of 1000 names is about 300 KiB.
const LISTING_MAX: u64 = 8 * 1024 * 1024;
const ERROR_MAX: u64 = 64 * 1024;
/// Tries per request: a busy bucket answers 503 SlowDown and wants to be asked again.
const ATTEMPTS: u32 = 5;
/// Pages of a listing this follows at most, 1000 names each.
const MAX_PAGES: usize = 10_000;
/// Names one listing may bring, and their bytes together. Pages are read one after the other and
/// their names kept until the last: a server that says "truncated" for ever could otherwise fill
/// the memory at every backup (security-audit-0.16.0 PLAT-6). A repository of a few million
/// objects lists a few ten thousand per folder.
const MAX_LISTED_NAMES: usize = 2_000_000;
const MAX_LISTED_BYTES: usize = 128 * 1024 * 1024;
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// A connection to a bucket. It holds no socket of its own: every request takes one from the pool.
pub struct S3 {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    scheme: String,
    /// `host` or `host:port`, as the Host header wants it.
    authority: String,
    /// The host requests connect to (with the bucket in front for virtual-host style), and its port.
    host: String,
    port: u16,
    target: S3Target,
    /// The folder inside the bucket, without slashes at either end.
    root: String,
}

fn tls_config() -> rustls::ClientConfig {
    let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// Percent-encodes everything but the characters S3 leaves alone (RFC 3986 unreserved), and `/`
/// too when `keep_slash`. The same text goes into the request and into the signature.
pub fn uri_encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data).as_ref().to_vec()
}

/// `20130524T000000Z` for a Unix time.
pub fn amz_date(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let seconds = unix.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z", seconds / 3600, seconds % 3600 / 60, seconds % 60)
}

/// What a request is signed with.
pub struct Credentials<'a> {
    pub access_key: &'a str,
    pub secret_key: &'a str,
    pub region: &'a str,
}

/// The `Authorization` header of AWS Signature Version 4 for S3.
///
/// `canonical_uri` and `canonical_query` are already encoded, exactly as they go into the request.
/// `headers` are the ones to sign, with lower-case names; `host`, `x-amz-date` and
/// `x-amz-content-sha256` must be among them.
pub fn authorization(
    credentials: &Credentials<'_>,
    method: &str,
    canonical_uri: &str,
    canonical_query: &str,
    headers: &[(&str, &str)],
    payload_sha256: &str,
    amz_date: &str,
) -> String {
    let mut headers: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.split_whitespace().collect::<Vec<_>>().join(" ")))
        .collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(name, value)| format!("{name}:{value}\n")).collect();
    let signed_headers = headers.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(";");
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_sha256}"
    );
    let day = &amz_date[..8];
    let scope = format!("{day}/{}/s3/aws4_request", credentials.region);
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(canonical_request.as_bytes()));
    let mut key = hmac_sha256(format!("AWS4{}", credentials.secret_key).as_bytes(), day.as_bytes());
    for part in [credentials.region, "s3", "aws4_request"] {
        key = hmac_sha256(&key, part.as_bytes());
    }
    let signature = hex::encode(hmac_sha256(&key, string_to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key
    )
}

/// The text of every `<tag>…</tag>` in a piece of XML, unescaped. S3's answers are plain enough
/// for this: no attributes on the elements read here, no CDATA, no namespaces in the way.
pub(crate) fn xml_values(xml: &str, tag: &str) -> Vec<String> {
    xml_raw_values(xml, tag).into_iter().map(xml_unescape).collect()
}

/// The same, as written: for elements whose own elements are read next, so that nothing is
/// unescaped twice.
fn xml_raw_values<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut values = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else { break };
        values.push(&after[..end]);
        rest = &after[end + close.len()..];
    }
    values
}

fn xml_unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        // An entity is short: the `;` is looked for in the next few bytes only, not in all the rest
        // of the text for every `&`, which made a run of `&&&…` quadratic (security-audit-0.16.0
        // PLAT-6).
        let Some(end) = after.as_bytes().iter().take(11).position(|byte| *byte == b';') else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..end];
        let resolved = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match resolved {
            Some(character) => {
                out.push(character);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// What went wrong, in S3's words: its error code and message.
fn error_of(status: StatusCode, body: &[u8]) -> (String, String) {
    let text = String::from_utf8_lossy(body);
    let code = xml_values(&text, "Code").into_iter().next().unwrap_or_default();
    let message = xml_values(&text, "Message").into_iter().next().unwrap_or_else(|| status.to_string());
    (code, message)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64)
}

impl S3 {
    /// Checks the settings and gets ready to talk to the bucket. Nothing is sent yet.
    pub fn new(target: &S3Target) -> Result<S3, Error> {
        let endpoint = target.endpoint.trim().trim_end_matches('/');
        let url = url::Url::parse(endpoint)
            .map_err(|_| Error::Config(format!("{endpoint} is not an address like https://s3.example.com")))?;
        if !matches!(url.scheme(), "https" | "http") {
            return Err(Error::Config("the S3 address starts with https:// (or http:// in the own network)".into()));
        }
        if url.path() != "/" || url.query().is_some() || !url.username().is_empty() {
            return Err(Error::Config("the S3 address is only the server, without a bucket or a path".into()));
        }
        let host = url.host_str().ok_or_else(|| Error::Config("the S3 address has no host".into()))?;
        let bucket = target.bucket.trim();
        let valid_bucket = (3..=63).contains(&bucket.len())
            && bucket.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-_".contains(&byte));
        if !valid_bucket {
            return Err(Error::Config(format!("'{bucket}' is not a bucket name")));
        }
        if target.access_key.trim().is_empty() || target.secret_key.is_empty() {
            return Err(Error::Config("S3 needs an access key and a secret key".into()));
        }
        let region = target.region.trim();
        if region.is_empty() || region.contains(char::is_whitespace) || region.contains('/') {
            return Err(Error::Config("S3 needs a region, e.g. us-east-1".into()));
        }
        let host = if target.path_style { host.to_owned() } else { format!("{bucket}.{host}") };
        let authority = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.clone(),
        };
        let port = url.port_or_known_default().unwrap_or(443);
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config())
            .https_or_http()
            .enable_http1()
            .build();
        let client = Client::builder(TokioExecutor::new()).build(connector);
        let target = S3Target {
            endpoint: endpoint.to_owned(),
            region: region.to_owned(),
            bucket: bucket.to_owned(),
            prefix: target.prefix.clone(),
            access_key: target.access_key.trim().to_owned(),
            secret_key: target.secret_key.clone(),
            path_style: target.path_style,
        };
        let root = target.prefix.trim_matches('/').to_owned();
        let host = host.trim_matches(['[', ']']).to_owned();
        Ok(S3 { client, scheme: url.scheme().to_owned(), authority, host, port, target, root })
    }

    /// Checks where the requests would go before any is sent. Over `https://` the bucket may be
    /// anywhere. Plain `http://` carries the backups and the signed requests unencrypted, which is
    /// only acceptable inside the own network: then every address the name has must be a private
    /// one (a MinIO next to the server, a NAS), never one on the internet.
    pub async fn check_route(&self) -> Result<(), Error> {
        if self.scheme == "https" {
            return Ok(());
        }
        let addresses: Vec<std::net::IpAddr> = match self.host.parse::<std::net::IpAddr>() {
            Ok(ip) => vec![ip],
            Err(_) => tokio::net::lookup_host((self.host.as_str(), self.port))
                .await
                .map_err(|_| Error::Storage(format!("{} could not be looked up", self.host)))?
                .map(|address| address.ip())
                .collect(),
        };
        if addresses.is_empty() {
            return Err(Error::Storage(format!("{} could not be looked up", self.host)));
        }
        if !addresses.into_iter().all(in_own_network) {
            return Err(Error::Config(format!(
                "{} is on the internet; use https:// for it (http:// only works in the own network)",
                self.host
            )));
        }
        Ok(())
    }

    /// The object key of a path in the repository.
    fn key(&self, path: &str) -> String {
        match (self.root.is_empty(), path.is_empty()) {
            (true, _) => path.to_owned(),
            (false, true) => self.root.clone(),
            (false, false) => format!("{}/{path}", self.root),
        }
    }

    /// Sends one signed request, again when S3 is busy, and hands back the status and up to
    /// `limit` bytes of the answer -- or one byte more, so the caller can tell it was too long.
    async fn send(
        &self,
        method: Method,
        key: Option<&str>,
        query: &[(&str, &str)],
        body: Bytes,
        limit: u64,
    ) -> Result<(StatusCode, Vec<u8>), Error> {
        let mut path = String::from("/");
        if self.target.path_style {
            path.push_str(&uri_encode(&self.target.bucket, false));
            if key.is_some() {
                path.push('/');
            }
        }
        if let Some(key) = key {
            path.push_str(&uri_encode(key, true));
        }
        let mut pairs: Vec<(String, String)> =
            query.iter().map(|(name, value)| (uri_encode(name, false), uri_encode(value, false))).collect();
        pairs.sort();
        let query = pairs.iter().map(|(name, value)| format!("{name}={value}")).collect::<Vec<_>>().join("&");
        let uri = if query.is_empty() {
            format!("{}://{}{path}", self.scheme, self.authority)
        } else {
            format!("{}://{}{path}?{query}", self.scheme, self.authority)
        };
        let payload = if body.is_empty() { EMPTY_SHA256.to_owned() } else { sha256_hex(&body) };
        let upload_time = Duration::from_secs(body.len() as u64 / SLOWEST_UPLOAD);

        let mut last = String::new();
        for attempt in 0..ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(250 << attempt)).await;
            }
            let date = amz_date(now());
            let credentials = Credentials {
                access_key: &self.target.access_key,
                secret_key: &self.target.secret_key,
                region: &self.target.region,
            };
            let signed = [("host", self.authority.as_str()), ("x-amz-content-sha256", &payload), ("x-amz-date", &date)];
            let authorization = authorization(&credentials, method.as_str(), &path, &query, &signed, &payload, &date);
            let request = Request::builder()
                .method(method.clone())
                .uri(&uri)
                .header("host", &self.authority)
                .header("x-amz-content-sha256", &payload)
                .header("x-amz-date", &date)
                .header("authorization", authorization)
                .header("content-length", body.len())
                .body(Full::new(body.clone()))
                .map_err(|err| Error::Config(format!("the S3 request cannot be made: {err}")))?;
            let response = match tokio::time::timeout(ANSWER_TIMEOUT + upload_time, self.client.request(request)).await
            {
                Err(_) => {
                    last = format!("{} did not answer in time", self.authority);
                    continue;
                }
                Ok(Err(err)) => {
                    last = format!("{} cannot be reached: {err}", self.authority);
                    continue;
                }
                Ok(Ok(response)) => response,
            };
            let status = response.status();
            let cap = if status.is_success() { limit } else { ERROR_MAX };
            let mut body = response.into_body();
            let mut bytes = Vec::new();
            // Every part of the body has BODY_TIMEOUT to come, and the whole of it as long as the
            // slowest upload would take for the most it may be: a byte now and then no longer
            // keeps a request going for ever.
            let whole = ANSWER_TIMEOUT.saturating_add(Duration::from_secs(cap / SLOWEST_UPLOAD));
            let read = async {
                while let Some(frame) = tokio::time::timeout(BODY_TIMEOUT, body.frame())
                    .await
                    .map_err(|_| format!("{} stopped sending", self.authority))?
                {
                    let frame = frame.map_err(|err| format!("reading from {}: {err}", self.authority))?;
                    if let Ok(data) = frame.into_data() {
                        bytes.extend_from_slice(&data);
                        if bytes.len() as u64 > cap {
                            bytes.truncate(cap as usize + 1);
                            break;
                        }
                    }
                }
                Ok::<_, String>(())
            };
            let read = tokio::time::timeout(whole, read)
                .await
                .unwrap_or_else(|_| Err(format!("{} took too long to send its answer", self.authority)));
            if let Err(err) = read {
                last = err;
                continue;
            }
            // Busy or broken for a moment: S3 asks to be asked again.
            if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
                last = format!("{} answered {status}: {}", self.authority, error_of(status, &bytes).1);
                continue;
            }
            return Ok((status, bytes));
        }
        Err(Error::Storage(last))
    }

    /// Turns an answer that is not a success into the error it means.
    fn failure(&self, status: StatusCode, body: &[u8]) -> Error {
        let (code, message) = error_of(status, body);
        match code.as_str() {
            "InvalidAccessKeyId" | "SignatureDoesNotMatch" | "InvalidSecurity" => {
                Error::LoginRefused(self.target.access_key.clone())
            }
            "NoSuchBucket" => {
                Error::Storage(format!("there is no bucket {} at {}", self.target.bucket, self.authority))
            }
            "" => Error::Storage(format!("{} answered {status}", self.authority)),
            _ => Error::Storage(format!("{} answered {code}: {message}", self.authority)),
        }
    }

    pub async fn read(&self, path: &str, limit: u64) -> Result<Option<Vec<u8>>, Error> {
        let key = self.key(path);
        let (status, body) = self.send(Method::GET, Some(&key), &[], Bytes::new(), limit).await?;
        match status {
            status if status.is_success() => Ok(Some(body)),
            StatusCode::NOT_FOUND if error_of(status, &body).0 != "NoSuchBucket" => Ok(None),
            status => Err(self.failure(status, &body)),
        }
    }

    pub async fn write(&self, path: &str, bytes: &[u8]) -> Result<(), Error> {
        let key = self.key(path);
        let (status, body) = self.send(Method::PUT, Some(&key), &[], Bytes::copy_from_slice(bytes), ERROR_MAX).await?;
        if !status.is_success() {
            return Err(self.failure(status, &body));
        }
        Ok(())
    }

    pub async fn remove(&self, path: &str) -> Result<(), Error> {
        let key = self.key(path);
        let (status, body) = self.send(Method::DELETE, Some(&key), &[], Bytes::new(), ERROR_MAX).await?;
        match status {
            status if status.is_success() => Ok(()),
            StatusCode::NOT_FOUND if error_of(status, &body).0 != "NoSuchBucket" => Ok(()),
            status => Err(self.failure(status, &body)),
        }
    }

    /// What is directly inside a folder: the names of objects and of the folders below it, one
    /// level deep, like a directory listing.
    pub async fn list(&self, dir: &str) -> Result<Vec<String>, Error> {
        self.list_within(dir, MAX_LISTED_NAMES, MAX_LISTED_BYTES).await
    }

    async fn list_within(&self, dir: &str, max_names: usize, max_bytes: usize) -> Result<Vec<String>, Error> {
        let prefix = match self.key(dir) {
            key if key.is_empty() => String::new(),
            key => format!("{key}/"),
        };
        let mut names = Vec::new();
        let mut listed_bytes = 0;
        let mut token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut query = vec![("list-type", "2"), ("delimiter", "/"), ("prefix", prefix.as_str())];
            if let Some(token) = token.as_deref() {
                query.push(("continuation-token", token));
            }
            let (status, body) = self.send(Method::GET, None, &query, Bytes::new(), LISTING_MAX).await?;
            if !status.is_success() {
                return Err(self.failure(status, &body));
            }
            if body.len() as u64 > LISTING_MAX {
                return Err(Error::Damaged(format!("{} sent a listing bigger than this reads", self.authority)));
            }
            let xml = String::from_utf8_lossy(&body);
            let relative =
                |full: &str| full.strip_prefix(prefix.as_str()).map(|name| name.trim_end_matches('/').to_owned());
            let keys = xml_raw_values(&xml, "Contents")
                .into_iter()
                .filter_map(|contents| xml_values(contents, "Key").into_iter().next());
            let folders = xml_raw_values(&xml, "CommonPrefixes")
                .into_iter()
                .filter_map(|common| xml_values(common, "Prefix").into_iter().next());
            for name in keys.chain(folders).filter_map(|key| relative(&key)) {
                if name.is_empty() || name.contains('/') {
                    continue;
                }
                listed_bytes += name.len();
                names.push(name);
                if names.len() > max_names || listed_bytes > max_bytes {
                    return Err(Error::Damaged(format!("{} lists more than this reads", self.authority)));
                }
            }
            let truncated = xml_values(&xml, "IsTruncated").first().is_some_and(|value| value == "true");
            token = xml_values(&xml, "NextContinuationToken").into_iter().next().filter(|token| !token.is_empty());
            if !truncated || token.is_none() {
                return Ok(names);
            }
        }
        Err(Error::Damaged(format!("{} never finished its listing", self.authority)))
    }
}

/// Whether an address is inside the own network: this machine, a private network or the
/// shared address space of a VPN such as Tailscale. Not merely "not public": an address that only
/// is not public (reserved, documentation, Teredo) is no place backups may go to unencrypted.
fn in_own_network(ip: std::net::IpAddr) -> bool {
    match ip.to_canonical() {
        std::net::IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || (a == 100 && (b & 0xc0) == 64)
        }
        std::net::IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE_CREDENTIALS: Credentials<'static> = Credentials {
        access_key: "AKIAIOSFODNN7EXAMPLE",
        secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        region: "us-east-1",
    };

    fn signature(authorization: &str) -> &str {
        authorization.rsplit_once("Signature=").unwrap().1
    }

    /// The "GET Object" example of Amazon's documentation for Signature Version 4 in S3
    /// (Authenticating Requests: Using the Authorization Header, "Example: GET Object").
    #[test]
    fn signs_amazons_get_object_example() {
        let date = "20130524T000000Z";
        let headers = [
            ("Host", "examplebucket.s3.amazonaws.com"),
            ("Range", "bytes=0-9"),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", date),
        ];
        let header = authorization(&EXAMPLE_CREDENTIALS, "GET", "/test.txt", "", &headers, EMPTY_SHA256, date);
        assert!(
            header.starts_with(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
                 SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, "
            ),
            "{header}"
        );
        assert_eq!(signature(&header), "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41");
    }

    /// The "GET Bucket (List Objects)" example of the same page: a query string, sorted and encoded.
    #[test]
    fn signs_amazons_list_objects_example() {
        let date = "20130524T000000Z";
        let headers =
            [("host", "examplebucket.s3.amazonaws.com"), ("x-amz-content-sha256", EMPTY_SHA256), ("x-amz-date", date)];
        let header =
            authorization(&EXAMPLE_CREDENTIALS, "GET", "/", "max-keys=2&prefix=J", &headers, EMPTY_SHA256, date);
        assert_eq!(signature(&header), "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7");
    }

    #[test]
    fn dates_and_encoding_are_what_s3_expects() {
        assert_eq!(amz_date(1_369_353_600), "20130524T000000Z");
        assert_eq!(amz_date(1_790_000_000), "20260921T141320Z");
        assert_eq!(uri_encode("a b/c~d+é", true), "a%20b/c~d%2B%C3%A9");
        assert_eq!(uri_encode("data/ab/", false), "data%2Fab%2F");
    }

    #[test]
    fn listings_and_errors_are_read() {
        let xml = "<ListBucketResult><IsTruncated>false</IsTruncated>\
            <Contents><Key>b/snapshots/1&amp;2</Key><Size>3</Size></Contents>\
            <CommonPrefixes><Prefix>b/data/ab/</Prefix></CommonPrefixes></ListBucketResult>";
        assert_eq!(xml_values(xml, "Key"), ["b/snapshots/1&2"]);
        assert_eq!(xml_values(xml, "Prefix"), ["b/data/ab/"]);
        let (code, message) = error_of(
            StatusCode::FORBIDDEN,
            b"<Error><Code>SignatureDoesNotMatch</Code><Message>The request signature &#x27;x&#39;</Message></Error>",
        );
        assert_eq!((code.as_str(), message.as_str()), ("SignatureDoesNotMatch", "The request signature 'x'"));
    }

    #[test]
    fn settings_are_checked_before_anything_is_sent() {
        let target = S3Target {
            endpoint: "https://s3.example.com/".into(),
            region: "eu-central-1".into(),
            bucket: "backups".into(),
            prefix: "/uwumail/".into(),
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "geheim".into(),
            path_style: false,
        };
        let s3 = S3::new(&target).unwrap();
        assert_eq!(s3.authority, "backups.s3.example.com");
        assert_eq!(s3.key("snapshots/1"), "uwumail/snapshots/1");
        let path_style =
            S3::new(&S3Target { endpoint: "http://192.0.2.10:9000".into(), path_style: true, ..target.clone() });
        assert_eq!(path_style.unwrap().authority, "192.0.2.10:9000");
        for broken in [
            S3Target { endpoint: "ftp://s3.example.com".into(), ..target.clone() },
            S3Target { endpoint: "https://s3.example.com/backups".into(), ..target.clone() },
            S3Target { bucket: "Not A Bucket".into(), ..target.clone() },
            S3Target { secret_key: String::new(), ..target.clone() },
            S3Target { region: String::new(), ..target.clone() },
        ] {
            assert!(matches!(S3::new(&broken), Err(Error::Config(_))), "{broken:?}");
        }
    }

    /// security-audit-0.16.0 PLAT-6: a run of `&` cost a search through all the rest of the text
    /// each, which for a page of 8 MiB took hours.
    #[test]
    fn unescaping_takes_linear_time() {
        let ampersands = "&".repeat(1024 * 1024);
        assert_eq!(xml_unescape(&ampersands), ampersands);
        let xml = format!("<Key>{ampersands}&amp;lt;</Key>");
        assert_eq!(xml_values(&xml, "Key")[0].len(), ampersands.len() + 4, "unescaped once: &lt; stays");
    }

    /// security-audit-0.16.0 PLAT-6: a server whose listing never ends, with new names on every
    /// page, is stopped once a listing holds what it may.
    #[tokio::test]
    async fn an_endless_listing_ends_at_its_budget() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let pages = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let served = pages.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let pages = served.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |_request| {
                        let page = pages.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let keys: String = (0..1000)
                            .map(|n| format!("<Contents><Key>data/ab/ab{page:06}{n:04}</Key></Contents>"))
                            .collect();
                        let xml = format!(
                            "<ListBucketResult><IsTruncated>true</IsTruncated>{keys}\
                             <NextContinuationToken>t{page}</NextContinuationToken></ListBucketResult>"
                        );
                        async move { Ok::<_, std::convert::Infallible>(hyper::Response::new(Full::new(Bytes::from(xml)))) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        let s3 = S3::new(&S3Target {
            endpoint: format!("http://{address}"),
            region: "us-east-1".into(),
            bucket: "backups".into(),
            prefix: String::new(),
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "geheim".into(),
            path_style: true,
        })
        .unwrap();
        let listed = s3.list_within("data/ab", 5_000, usize::MAX).await;
        assert!(matches!(listed, Err(Error::Damaged(_))), "{listed:?}");
        assert_eq!(pages.load(std::sync::atomic::Ordering::SeqCst), 6, "no page after the budget was spent");
    }

    /// Plain http only reaches into the own network; https goes anywhere.
    #[tokio::test]
    async fn plain_http_stays_in_the_own_network() {
        let target = |endpoint: &str| S3Target {
            endpoint: endpoint.into(),
            region: "us-east-1".into(),
            bucket: "backups".into(),
            prefix: String::new(),
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "geheim".into(),
            path_style: true,
        };
        for local in ["http://127.0.0.1:9000", "http://192.168.1.20:9000", "http://[::1]:9000"] {
            S3::new(&target(local)).unwrap().check_route().await.unwrap();
        }
        // Not in the own network either: not public, but not a place for backups in the clear.
        for elsewhere in ["http://192.0.2.10:9000", "http://[2001:db8::1]:9000", "http://[2001:0:4136:e378::1]:9000"] {
            let s3 = S3::new(&target(elsewhere)).unwrap();
            assert!(matches!(s3.check_route().await, Err(Error::Config(_))), "{elsewhere}");
        }
        S3::new(&target("https://192.0.2.10")).unwrap().check_route().await.unwrap();
    }
}
