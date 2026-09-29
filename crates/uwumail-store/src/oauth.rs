//! OAuth 2.0 and OpenID Connect for mail apps (docs/oauth.md): apps that registered themselves,
//! what a person allowed them, and the tokens they sign in with. Also the logins at other OpenID
//! Connect providers that belong to an account here, and where an account's password is checked.
//!
//! Tokens and codes are long random strings kept only as SHA-256 hashes, like app passwords.

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use data_encoding::BASE64URL_NOPAD;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::db::{get_setting, set_setting};
use crate::directory::{ACCOUNT_COLUMNS, account_from_row, login_key};
use crate::fetch::{seal, unseal};
use crate::security::{MailAuth, MailAuthDenied};
use crate::{Account, AppScope, Result, Store, StoreError, now, random_bytes};

/// An access token works for an hour; apps trade their refresh token in for the next one.
pub const OAUTH_ACCESS_TOKEN_SECS: i64 = 3600;
/// A refresh token lasts 90 days after it was handed out, and each use hands out a new one.
pub const OAUTH_REFRESH_TOKEN_SECS: i64 = 90 * 24 * 3600;
/// An authorization code is traded in right away by the app that asked for it.
pub const OAUTH_CODE_SECS: i64 = 120;
/// Apps that registered themselves and nobody uses any more are forgotten after this long.
const UNUSED_CLIENT_SECS: i64 = 7 * 24 * 3600;
/// Apps that nobody ever allowed in are forgotten after a day: a mail app registers while an
/// account is set up in it and asks for the person's consent right after.
const NEVER_USED_CLIENT_SECS: i64 = 24 * 3600;
const MAX_REDIRECT_URIS: usize = 10;
const MAX_REDIRECT_URI_LEN: usize = 500;
const MAX_CLIENTS: i64 = 10_000;
/// The ES256 key ID tokens are signed with, sealed like provider passwords.
const SIGNING_KEY: &str = "oauth.signing_key";

const ACCESS_PREFIX: &str = "uwu_at_";
const REFRESH_PREFIX: &str = "uwu_rt_";
const CODE_PREFIX: &str = "uwu_ac_";

/// The scopes this server knows, in the order they are shown. Everything else an app asks for is
/// left out of what it gets.
pub const OAUTH_SCOPES: &[&str] =
    &["openid", "email", "profile", "offline_access", "mail", "smtp", "dav", MASKED_EMAIL_SCOPE];

/// Masked addresses and nothing else of the mailbox: JMAP's session, `Core/echo` and the
/// `MaskedEmail` methods, with push for their changes (docs/jmap-masked-email.md). For a password
/// manager that makes addresses for its people, such as UwULock Server. `mail` includes it.
pub const MASKED_EMAIL_SCOPE: &str = "maskedemail";

/// What an app asks for, cut down to the scopes this server knows, without repeats, in the order of
/// [`OAUTH_SCOPES`]. A few other names mail apps use for the same things count as well.
pub fn oauth_scopes(requested: &str) -> Vec<&'static str> {
    let mut wanted: Vec<&'static str> = Vec::new();
    for scope in requested.split_whitespace() {
        let known = match scope {
            "imap" | "jmap" | "sieve" | "managesieve" => "mail",
            "submission" => "smtp",
            "caldav" | "carddav" => "dav",
            other => match OAUTH_SCOPES.iter().find(|known| **known == other) {
                Some(known) => known,
                None => continue,
            },
        };
        if !wanted.contains(&known) {
            wanted.push(known);
        }
    }
    OAUTH_SCOPES.iter().copied().filter(|scope| wanted.contains(scope)).collect()
}

/// Whether a set of scopes lets an app do anything at all: a protocol, or signing in (`openid`).
pub fn oauth_scopes_usable(scopes: &[&str]) -> bool {
    scopes.iter().any(|scope| matches!(*scope, "mail" | "smtp" | "dav" | "openid" | MASKED_EMAIL_SCOPE))
}

fn scope_list(scopes: &[&str]) -> String {
    scopes.join(" ")
}

fn token_hash(kind: &str, token: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update(b":");
    hasher.update(token.as_bytes());
    hasher.finalize().to_vec()
}

fn new_token(prefix: &str) -> String {
    format!("{prefix}{}", BASE64URL_NOPAD.encode(&random_bytes::<32>()))
}

/// Whether a bearer token looks like one of ours at all, before any lookup.
pub fn is_oauth_access_token(token: &str) -> bool {
    token.starts_with(ACCESS_PREFIX) && token.len() < 200
}

/// Compares two strings without letting the time taken say how much of them matched.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// PKCE (RFC 7636) with S256: the verifier the app kept must hash to the challenge it sent first.
pub fn pkce_matches(challenge: &str, verifier: &str) -> bool {
    let valid = (43..=128).contains(&verifier.len())
        && verifier.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
    valid && same(BASE64URL_NOPAD.encode(&Sha256::digest(verifier.as_bytes())).as_bytes(), challenge.as_bytes())
}

/// A PKCE challenge as S256 makes them: 43 characters of unpadded base64url.
pub fn valid_pkce_challenge(challenge: &str) -> bool {
    challenge.len() == 43 && challenge.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// The loopback host of an `http://` redirect, if it is one (RFC 8252 section 7.3).
fn loopback(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = if let Some(v6) = authority.strip_prefix('[') {
        let end = v6.find(']')?;
        &authority[..end + 2]
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    let port = &authority[host.len()..];
    if !(port.is_empty() || (port.starts_with(':') && port[1..].bytes().all(|b| b.is_ascii_digit()))) {
        return None;
    }
    matches!(host, "127.0.0.1" | "[::1]" | "localhost").then_some((host, path))
}

/// Whether an app may be sent back to this address: https, a loopback address of the device the
/// app runs on (RFC 8252 section 7.3), or a private-use scheme in reverse-DNS form such as
/// `com.example.mail:/oauth` (section 7.1). Never a fragment, never a login in it.
pub fn valid_redirect_uri(uri: &str) -> bool {
    if uri.is_empty() || uri.len() > MAX_REDIRECT_URI_LEN || uri.contains('#') {
        return false;
    }
    if uri.bytes().any(|b| b.is_ascii_control() || b == b' ' || b == b'\\') {
        return false;
    }
    let Some((scheme, rest)) = uri.split_once(':') else { return false };
    let scheme = scheme.to_ascii_lowercase();
    if scheme == "https" {
        let Some(authority) = rest.strip_prefix("//") else { return false };
        let authority = authority.split(['/', '?']).next().unwrap_or_default();
        return !authority.is_empty() && !authority.contains('@');
    }
    if scheme == "http" {
        return loopback(uri).is_some();
    }
    // A private-use scheme: at least two labels, as a reversed domain name the app owns.
    let valid_scheme = scheme.contains('.')
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        && !matches!(scheme.as_str(), "javascript" | "data" | "file" | "vbscript");
    valid_scheme && !rest.is_empty()
}

/// Whether the address an app sends in the authorization request is one it registered. Loopback
/// addresses may name any port: the app picks a free one each time.
pub fn redirect_uri_registered(registered: &[String], asked: &str) -> bool {
    registered.iter().any(|known| {
        if known == asked {
            return true;
        }
        match (loopback(known), loopback(asked)) {
            (Some((host, path)), Some((asked_host, asked_path))) => host == asked_host && path == asked_path,
            _ => false,
        }
    })
}

/// An app that registered itself. Every app is a public client (RFC 6749 section 2.1): it runs on
/// the person's own device and could not keep a secret, so PKCE is what ties a code to it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClient {
    #[serde(skip)]
    pub id: i64,
    pub client_id: String,
    pub name: String,
    pub redirect_uris: Vec<String>,
    pub created_at: i64,
}

fn client_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OAuthClient> {
    Ok(OAuthClient {
        id: row.get(0)?,
        client_id: row.get(1)?,
        name: row.get(2)?,
        redirect_uris: row.get::<_, String>(3)?.lines().map(str::to_owned).collect(),
        created_at: row.get(4)?,
    })
}

const CLIENT_COLUMNS: &str = "id, client_id, name, redirect_uris, created_at";

/// One app signed in with OAuth, as the person and their admin see it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthGrant {
    pub id: i64,
    pub client_name: String,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub last_used_protocol: Option<String>,
    pub last_used_ip: Option<String>,
}

const GRANT_COLUMNS: &str =
    "g.id, c.name, g.scopes, g.created_at, g.last_used_at, g.last_used_protocol, g.last_used_ip";

fn grant_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OAuthGrant> {
    Ok(OAuthGrant {
        id: row.get(0)?,
        client_name: row.get(1)?,
        scopes: row.get::<_, String>(2)?.split_whitespace().map(str::to_owned).collect(),
        created_at: row.get(3)?,
        last_used_at: row.get(4)?,
        last_used_protocol: row.get(5)?,
        last_used_ip: row.get(6)?,
    })
}

/// What an authorization code stands for, until the app trades it in.
#[derive(Debug, Clone)]
pub struct NewOAuthCode {
    pub client_id: i64,
    pub account_id: i64,
    pub redirect_uri: String,
    pub scopes: Vec<&'static str>,
    pub code_challenge: String,
    pub nonce: Option<String>,
    /// When the person last proved who they are: the portal session's start.
    pub auth_time: i64,
}

/// Tokens handed to an app, with what an ID token needs to say.
#[derive(Debug, Clone)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
    pub scopes: Vec<String>,
    pub account: Account,
    pub grant_id: i64,
    /// Set for a new grant: whether it is the app's first sign-in for this account, for the notice.
    pub new_grant: bool,
    pub nonce: Option<String>,
    pub auth_time: i64,
    pub client_name: String,
}

/// Why a code or refresh token was not traded in. The names are OAuth's error codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthRefusal {
    /// Unknown, expired, used, for another app or another redirect address, or PKCE failed.
    InvalidGrant,
    /// A refresh token came back after it had been traded in: someone copied it. The grant is gone.
    Reused { account_id: i64, client_name: String },
}

impl Store {
    /// Registers an app (RFC 7591), always as a public client.
    pub async fn register_oauth_client(&self, name: &str, redirect_uris: Vec<String>) -> Result<OAuthClient> {
        let name: String = name.trim().chars().filter(|c| !c.is_control()).take(80).collect();
        let name = if name.is_empty() { "Mail app".to_owned() } else { name };
        let mut uris: Vec<String> = Vec::new();
        for uri in redirect_uris {
            let uri = uri.trim().to_owned();
            if !valid_redirect_uri(&uri) {
                return Err(StoreError::Rule {
                    code: "invalid_redirect_uri",
                    message: format!(
                        "{uri} cannot be a redirect address: use https, a loopback address or an app scheme like com.example.app:/oauth"
                    ),
                });
            }
            if !uris.contains(&uri) {
                uris.push(uri);
            }
        }
        if uris.is_empty() || uris.len() > MAX_REDIRECT_URIS {
            return Err(StoreError::Rule {
                code: "invalid_redirect_uri",
                message: format!("an app needs between 1 and {MAX_REDIRECT_URIS} redirect addresses"),
            });
        }
        let client_id = format!("uwu-{}", BASE64URL_NOPAD.encode(&random_bytes::<18>()));
        let created_at = now();
        let client = OAuthClient {
            id: 0,
            client_id: client_id.clone(),
            name: name.clone(),
            redirect_uris: uris.clone(),
            created_at,
        };
        let id = self
            .write(move |tx| {
                purge(tx)?;
                let count: i64 = tx.query_row("SELECT COUNT(*) FROM oauth_clients", [], |row| row.get(0))?;
                // A full table makes room by forgetting the oldest app nobody ever allowed in, so
                // registrations alone cannot keep new apps out (WEB-2). Apps in use are never
                // pushed out.
                if count >= MAX_CLIENTS
                    && tx.execute(
                        "DELETE FROM oauth_clients WHERE id IN
                             (SELECT id FROM oauth_clients WHERE last_used_at IS NULL
                                AND NOT EXISTS (SELECT 1 FROM oauth_grants g WHERE g.client_id = oauth_clients.id)
                              ORDER BY created_at, id LIMIT ?1)",
                        [count - MAX_CLIENTS + 1],
                    )? == 0
                {
                    return Err(StoreError::Rule {
                        code: "temporarily_unavailable",
                        message: "too many apps are registered here right now".into(),
                    });
                }
                tx.execute(
                    "INSERT INTO oauth_clients (client_id, name, redirect_uris, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![client_id, name, uris.join("\n"), created_at],
                )?;
                Ok(tx.last_insert_rowid())
            })
            .await?;
        Ok(OAuthClient { id, ..client })
    }

    pub async fn oauth_client(&self, client_id: &str) -> Result<Option<OAuthClient>> {
        let client_id = client_id.to_owned();
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {CLIENT_COLUMNS} FROM oauth_clients WHERE client_id = ?1"),
                    [client_id],
                    client_from_row,
                )
                .optional()?)
        })
        .await
    }

    /// Whether the person agreed to all of these scopes for this app before.
    pub async fn oauth_consented(&self, account_id: i64, client_id: i64, scopes: &[&str]) -> Result<bool> {
        let wanted: Vec<String> = scopes.iter().map(|scope| (*scope).to_owned()).collect();
        self.read(move |conn| {
            let given: Option<String> = conn
                .query_row(
                    "SELECT scopes FROM oauth_consents WHERE account_id = ?1 AND client_id = ?2",
                    params![account_id, client_id],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(given.is_some_and(|given| {
                let given: Vec<&str> = given.split_whitespace().collect();
                wanted.iter().all(|scope| given.contains(&scope.as_str()))
            }))
        })
        .await
    }

    /// Hands out an authorization code for what the person just allowed, and remembers that they
    /// allowed it.
    pub async fn create_oauth_code(&self, new: NewOAuthCode) -> Result<String> {
        if !valid_pkce_challenge(&new.code_challenge) {
            return Err(StoreError::Invalid("the PKCE challenge is not an S256 one".into()));
        }
        let code = new_token(CODE_PREFIX);
        let hash = token_hash("oauth-code", &code);
        let scopes = scope_list(&new.scopes);
        self.write(move |tx| {
            purge(tx)?;
            let created_at = now();
            tx.execute(
                "INSERT INTO oauth_codes (code_hash, client_id, account_id, redirect_uri, scopes, code_challenge,
                                          nonce, auth_time, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    hash,
                    new.client_id,
                    new.account_id,
                    new.redirect_uri,
                    scopes,
                    new.code_challenge,
                    new.nonce,
                    new.auth_time,
                    created_at + OAUTH_CODE_SECS
                ],
            )?;
            // What was allowed before stays allowed: the scopes only ever grow here.
            let before: Option<String> = tx
                .query_row(
                    "SELECT scopes FROM oauth_consents WHERE account_id = ?1 AND client_id = ?2",
                    params![new.account_id, new.client_id],
                    |row| row.get(0),
                )
                .optional()?;
            let mut all: Vec<&str> = before.as_deref().map(|s| s.split_whitespace().collect()).unwrap_or_default();
            for scope in scopes.split_whitespace() {
                if !all.contains(&scope) {
                    all.push(scope);
                }
            }
            tx.execute(
                "INSERT INTO oauth_consents (account_id, client_id, scopes, created_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (account_id, client_id) DO UPDATE SET scopes = excluded.scopes",
                params![new.account_id, new.client_id, all.join(" "), created_at],
            )?;
            tx.execute("UPDATE oauth_clients SET last_used_at = ?1 WHERE id = ?2", params![created_at, new.client_id])?;
            Ok(())
        })
        .await?;
        Ok(code)
    }

    /// Trades an authorization code in for tokens (RFC 6749 section 4.1.3, RFC 7636). The code
    /// works once, only for the app and the redirect address it was made for, and only with the
    /// verifier behind its PKCE challenge.
    pub async fn redeem_oauth_code(
        &self,
        code: &str,
        client_id: i64,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<std::result::Result<OAuthTokens, OAuthRefusal>> {
        if !code.starts_with(CODE_PREFIX) {
            return Ok(Err(OAuthRefusal::InvalidGrant));
        }
        let hash = token_hash("oauth-code", code);
        let (redirect_uri, verifier) = (redirect_uri.to_owned(), verifier.to_owned());
        let access = new_token(ACCESS_PREFIX);
        let refresh = new_token(REFRESH_PREFIX);
        let (access_hash, refresh_hash) = (token_hash("oauth-access", &access), token_hash("oauth-refresh", &refresh));
        let result = self
            .write(move |tx| {
                // Gone at the first try, right or wrong: a code is never guessed twice.
                let found = tx
                    .query_row(
                        "DELETE FROM oauth_codes WHERE code_hash = ?1
                         RETURNING client_id, account_id, redirect_uri, scopes, code_challenge, nonce, auth_time, expires_at",
                        [&hash],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, String>(4)?,
                                row.get::<_, Option<String>>(5)?,
                                row.get::<_, i64>(6)?,
                                row.get::<_, i64>(7)?,
                            ))
                        },
                    )
                    .optional()?;
                let Some((code_client, account_id, code_redirect, scopes, challenge, nonce, auth_time, expires_at)) =
                    found
                else {
                    return Ok(Err(OAuthRefusal::InvalidGrant));
                };
                let now = now();
                if code_client != client_id
                    || code_redirect != redirect_uri
                    || expires_at <= now
                    || !pkce_matches(&challenge, &verifier)
                {
                    return Ok(Err(OAuthRefusal::InvalidGrant));
                }
                let account = tx
                    .query_row(&format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = ?1"), [account_id], account_from_row)
                    .optional()?;
                let Some(account) = account.filter(|account| account.can_use_portal()) else {
                    return Ok(Err(OAuthRefusal::InvalidGrant));
                };
                let earlier: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM oauth_grants WHERE account_id = ?1 AND client_id = ?2)",
                    params![account_id, client_id],
                    |row| row.get(0),
                )?;
                let grant_id = crate::db::next_id(tx, "oauth_grants")?;
                tx.execute(
                    "INSERT INTO oauth_grants (id, account_id, client_id, scopes, created_at) VALUES (?5, ?1, ?2, ?3, ?4)",
                    params![account_id, client_id, scopes, now, grant_id],
                )?;
                insert_tokens(tx, grant_id, &access_hash, &refresh_hash, now)?;
                let client_name: String =
                    tx.query_row("SELECT name FROM oauth_clients WHERE id = ?1", [client_id], |row| row.get(0))?;
                Ok(Ok(OAuthTokens {
                    access_token: String::new(),
                    refresh_token: String::new(),
                    expires_in: OAUTH_ACCESS_TOKEN_SECS,
                    scopes: scopes.split_whitespace().map(str::to_owned).collect(),
                    account,
                    grant_id,
                    new_grant: !earlier,
                    nonce,
                    auth_time,
                    client_name,
                }))
            })
            .await?;
        Ok(result.map(|tokens| OAuthTokens { access_token: access, refresh_token: refresh, ..tokens }))
    }

    /// Trades a refresh token in for new tokens (RFC 6749 section 6). The old refresh token is
    /// used up; if it ever comes back, the grant ends for everyone holding it.
    pub async fn refresh_oauth(
        &self,
        refresh_token: &str,
        client_id: i64,
    ) -> Result<std::result::Result<OAuthTokens, OAuthRefusal>> {
        if !refresh_token.starts_with(REFRESH_PREFIX) {
            return Ok(Err(OAuthRefusal::InvalidGrant));
        }
        let hash = token_hash("oauth-refresh", refresh_token);
        let access = new_token(ACCESS_PREFIX);
        let refresh = new_token(REFRESH_PREFIX);
        let (access_hash, refresh_hash) = (token_hash("oauth-access", &access), token_hash("oauth-refresh", &refresh));
        let result = self
            .write(move |tx| {
                let found = tx
                    .query_row(
                        "SELECT t.grant_id, t.expires_at, t.used_at, g.client_id, g.account_id, g.scopes, g.created_at, c.name
                         FROM oauth_tokens t JOIN oauth_grants g ON g.id = t.grant_id
                         JOIN oauth_clients c ON c.id = g.client_id
                         WHERE t.token_hash = ?1 AND t.kind = 'refresh'",
                        [&hash],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, Option<i64>>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, String>(5)?,
                                row.get::<_, i64>(6)?,
                                row.get::<_, String>(7)?,
                            ))
                        },
                    )
                    .optional()?;
                let Some((grant_id, expires_at, used_at, grant_client, account_id, scopes, granted_at, client_name)) =
                    found
                else {
                    return Ok(Err(OAuthRefusal::InvalidGrant));
                };
                if grant_client != client_id {
                    return Ok(Err(OAuthRefusal::InvalidGrant));
                }
                if used_at.is_some() {
                    forget_grant(tx, grant_id)?;
                    return Ok(Err(OAuthRefusal::Reused { account_id, client_name }));
                }
                let now = now();
                if expires_at <= now {
                    return Ok(Err(OAuthRefusal::InvalidGrant));
                }
                let account = tx
                    .query_row(&format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE id = ?1"), [account_id], account_from_row)
                    .optional()?;
                let Some(account) = account.filter(|account| account.can_use_portal()) else {
                    return Ok(Err(OAuthRefusal::InvalidGrant));
                };
                tx.execute("UPDATE oauth_tokens SET used_at = ?1 WHERE token_hash = ?2", params![now, hash])?;
                // What of this grant ran out goes now; an app that refreshes all day keeps no pile.
                tx.execute("DELETE FROM oauth_tokens WHERE grant_id = ?1 AND expires_at <= ?2", params![grant_id, now])?;
                insert_tokens(tx, grant_id, &access_hash, &refresh_hash, now)?;
                tx.execute("UPDATE oauth_clients SET last_used_at = ?1 WHERE id = ?2", params![now, client_id])?;
                Ok(Ok(OAuthTokens {
                    access_token: String::new(),
                    refresh_token: String::new(),
                    expires_in: OAUTH_ACCESS_TOKEN_SECS,
                    scopes: scopes.split_whitespace().map(str::to_owned).collect(),
                    account,
                    grant_id,
                    new_grant: false,
                    nonce: None,
                    auth_time: granted_at,
                    client_name,
                }))
            })
            .await?;
        Ok(result.map(|tokens| OAuthTokens { access_token: access, refresh_token: refresh, ..tokens }))
    }

    /// Revokes the grant behind an access or refresh token (RFC 7009), if it belongs to this app.
    /// Unknown tokens are no error: the answer is the same either way.
    pub async fn revoke_oauth_token(&self, token: &str, client_id: i64) -> Result<Option<(i64, String)>> {
        let hashes = [token_hash("oauth-access", token), token_hash("oauth-refresh", token)];
        self.write(move |tx| {
            for hash in hashes {
                let found = tx
                    .query_row(
                        "SELECT g.id, g.account_id, c.name FROM oauth_tokens t
                         JOIN oauth_grants g ON g.id = t.grant_id JOIN oauth_clients c ON c.id = g.client_id
                         WHERE t.token_hash = ?1 AND g.client_id = ?2",
                        params![hash, client_id],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?)),
                    )
                    .optional()?;
                if let Some((grant_id, account_id, name)) = found {
                    forget_grant(tx, grant_id)?;
                    return Ok(Some((account_id, name)));
                }
            }
            Ok(None)
        })
        .await
    }

    /// The account and scopes behind a valid access token, for the userinfo endpoint.
    pub async fn oauth_token_info(&self, token: &str) -> Result<Option<(Account, Vec<String>)>> {
        if !is_oauth_access_token(token) {
            return Ok(None);
        }
        let hash = token_hash("oauth-access", token);
        self.read(move |conn| {
            let found = conn
                .query_row(
                    &format!(
                        "SELECT {ACCOUNT_COLUMNS}, x.scopes FROM accounts JOIN (
                             SELECT g.account_id AS account_id, g.scopes AS scopes FROM oauth_tokens t
                             JOIN oauth_grants g ON g.id = t.grant_id
                             WHERE t.token_hash = ?1 AND t.kind = 'access' AND t.expires_at > ?2
                         ) x ON x.account_id = accounts.id"
                    ),
                    params![hash, now()],
                    |row| Ok((account_from_row(row)?, row.get::<_, String>(crate::directory::ACCOUNT_COLUMN_COUNT)?)),
                )
                .optional()?;
            Ok(found
                .filter(|(account, _)| account.can_use_portal())
                .map(|(account, scopes)| (account, scopes.split_whitespace().map(str::to_owned).collect())))
        })
        .await
    }

    /// Checks an OAuth access token from a mail app (IMAP, SMTP, JMAP, DAV, ManageSieve), the same
    /// way [`Store::authenticate_bearer`] checks an app password: the account may log in and use the
    /// protocol, and the grant covers it.
    pub async fn authenticate_oauth(&self, token: &str, scope: AppScope, protocol: &str, ip: &str) -> Result<MailAuth> {
        Ok(self.authenticate_oauth_grant(token, scope, protocol, ip).await?.0)
    }

    /// [`Store::authenticate_oauth`], also naming the grant a good token belongs to, which is the
    /// credential push subscriptions made with the token live and end with.
    pub async fn authenticate_oauth_grant(
        &self,
        token: &str,
        scope: AppScope,
        protocol: &str,
        ip: &str,
    ) -> Result<(MailAuth, Option<i64>)> {
        self.authenticate_oauth_within(token, scope, false, protocol, ip).await
    }

    /// [`Store::authenticate_oauth_grant`] for JMAP, which also lets in apps allowed nothing but
    /// masked addresses (the `maskedemail` scope). Their [`MailAuth::Ok`] carries none of the
    /// [`AppScope`]s, not `mail` either: the caller has to keep them to `MaskedEmail`.
    pub async fn authenticate_oauth_grant_or_masked(
        &self,
        token: &str,
        scope: AppScope,
        protocol: &str,
        ip: &str,
    ) -> Result<(MailAuth, Option<i64>)> {
        self.authenticate_oauth_within(token, scope, true, protocol, ip).await
    }

    async fn authenticate_oauth_within(
        &self,
        token: &str,
        scope: AppScope,
        or_masked: bool,
        protocol: &str,
        ip: &str,
    ) -> Result<(MailAuth, Option<i64>)> {
        let denied =
            |reason: MailAuthDenied| -> Result<(MailAuth, Option<i64>)> { Ok((MailAuth::Denied(reason), None)) };
        if !is_oauth_access_token(token) {
            return denied(MailAuthDenied::Invalid);
        }
        let hash = token_hash("oauth-access", token);
        let found = self
            .read(move |conn| {
                Ok(conn
                    .query_row(
                        &format!(
                            "SELECT {ACCOUNT_COLUMNS}, x.grant_id, x.scopes, x.expires_at FROM accounts JOIN (
                                 SELECT g.account_id AS account_id, g.id AS grant_id, g.scopes AS scopes,
                                        t.expires_at AS expires_at
                                 FROM oauth_tokens t JOIN oauth_grants g ON g.id = t.grant_id
                                 WHERE t.token_hash = ?1 AND t.kind = 'access'
                             ) x ON x.account_id = accounts.id"
                        ),
                        params![hash],
                        |row| {
                            let n = crate::directory::ACCOUNT_COLUMN_COUNT;
                            Ok((
                                account_from_row(row)?,
                                row.get::<_, i64>(n)?,
                                row.get::<_, String>(n + 1)?,
                                row.get::<_, i64>(n + 2)?,
                            ))
                        },
                    )
                    .optional()?)
            })
            .await?;
        let Some((account, grant_id, scopes, expires_at)) = found else {
            return denied(MailAuthDenied::Invalid);
        };
        let now = now();
        // A service never signs in to the portal, so it can never have agreed to anything.
        if !account.can_use_portal() {
            return denied(MailAuthDenied::Invalid);
        }
        if expires_at <= now {
            return denied(MailAuthDenied::Expired);
        }
        if !account.may_use(protocol) {
            return denied(MailAuthDenied::ProtocolOff);
        }
        if !scopes.split_whitespace().any(|given| given == scope.as_str() || (or_masked && given == MASKED_EMAIL_SCOPE))
        {
            return denied(MailAuthDenied::WrongScope);
        }
        let (protocol, ip) = (protocol.to_owned(), ip.to_owned());
        self.write(move |tx| {
            tx.execute(
                "UPDATE oauth_grants SET last_used_at = ?1, last_used_protocol = ?2, last_used_ip = ?3
                 WHERE id = ?4 AND (last_used_at IS NULL OR last_used_at < ?1 - 60
                                    OR last_used_protocol IS NOT ?2 OR last_used_ip IS NOT ?3)",
                params![now, protocol, ip, grant_id],
            )?;
            Ok(())
        })
        .await?;
        let credential = crate::push_credential_for_oauth_grant(grant_id);
        let scopes = AppScope::parse_list(&scopes);
        Ok((MailAuth::Ok { account, app_password: None, credential, scopes }, Some(grant_id)))
    }

    /// The name of the app a grant belongs to, as it registered itself.
    pub async fn oauth_grant_client_name(&self, grant_id: i64) -> Result<Option<String>> {
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT c.name FROM oauth_grants g JOIN oauth_clients c ON c.id = g.client_id WHERE g.id = ?1",
                    [grant_id],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }

    /// The apps signed in to an account with OAuth, newest first.
    pub async fn oauth_grants(&self, account_id: i64) -> Result<Vec<OAuthGrant>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {GRANT_COLUMNS} FROM oauth_grants g JOIN oauth_clients c ON c.id = g.client_id
                 WHERE g.account_id = ?1 ORDER BY g.created_at DESC, g.id DESC"
            ))?;
            let rows = stmt.query_map([account_id], grant_from_row)?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// Ends an app's sign-in: every token of the grant stops working at once, and the app has to
    /// ask again next time.
    pub async fn revoke_oauth_grant(&self, account_id: i64, grant_id: i64) -> Result<OAuthGrant> {
        self.write(move |tx| {
            let grant = tx
                .query_row(
                    &format!(
                        "SELECT {GRANT_COLUMNS} FROM oauth_grants g JOIN oauth_clients c ON c.id = g.client_id
                         WHERE g.id = ?1 AND g.account_id = ?2"
                    ),
                    params![grant_id, account_id],
                    grant_from_row,
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("OAuth grant {grant_id}")))?;
            forget_grant(tx, grant_id)?;
            Ok(grant)
        })
        .await
    }

    /// The key ID tokens are signed with (ECDSA P-256, PKCS#8), made the first time it is needed.
    pub async fn oauth_signing_key(&self) -> Result<Vec<u8>> {
        self.write(|tx| {
            if let Some(stored) = get_setting(tx, SIGNING_KEY)? {
                let sealed = hex::decode(stored)
                    .map_err(|_| StoreError::Internal("the stored OAuth signing key is damaged".into()))?;
                return hex::decode(unseal(tx, &sealed)?)
                    .map_err(|_| StoreError::Internal("the stored OAuth signing key is damaged".into()));
            }
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
                .map_err(|_| StoreError::Internal("making an OAuth signing key failed".into()))?;
            let pkcs8 = pkcs8.as_ref().to_vec();
            let sealed = seal(tx, &hex::encode(&pkcs8))?;
            set_setting(tx, SIGNING_KEY, &hex::encode(sealed))?;
            Ok(pkcs8)
        })
        .await
    }

    // Logins elsewhere

    /// The account a login at another OpenID Connect provider belongs to, noting the login.
    pub async fn external_identity(&self, issuer: &str, subject: &str) -> Result<Option<i64>> {
        let (issuer, subject) = (issuer.to_owned(), subject.to_owned());
        self.write(move |tx| {
            let found: Option<i64> = tx
                .query_row(
                    "SELECT account_id FROM external_identities WHERE issuer = ?1 AND subject = ?2",
                    params![issuer, subject],
                    |row| row.get(0),
                )
                .optional()?;
            if found.is_some() {
                tx.execute(
                    "UPDATE external_identities SET last_login_at = ?1 WHERE issuer = ?2 AND subject = ?3",
                    params![now(), issuer, subject],
                )?;
            }
            Ok(found)
        })
        .await
    }

    /// Remembers that a login at another provider belongs to this account. Refused (`false`) when
    /// the account already belongs to another login at the same provider: someone who changed
    /// their address there must not take over an account that is somebody else's login.
    pub async fn link_external_identity(
        &self,
        account_id: i64,
        issuer: &str,
        subject: &str,
        email: &str,
    ) -> Result<bool> {
        let (issuer, subject, email) = (issuer.to_owned(), subject.to_owned(), email.to_owned());
        self.write(move |tx| {
            let other: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM external_identities WHERE account_id = ?1 AND issuer = ?2 AND subject <> ?3)",
                params![account_id, issuer, subject],
                |row| row.get(0),
            )?;
            if other {
                return Ok(false);
            }
            let at = now();
            tx.execute(
                "INSERT INTO external_identities (account_id, issuer, subject, email, created_at, last_login_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT (issuer, subject) DO NOTHING",
                params![account_id, issuer, subject, email, at],
            )?;
            Ok(true)
        })
        .await
    }

    /// Where the account's password is checked: `local`, `ldap` or `oidc`.
    pub async fn auth_source(&self, account_id: i64) -> Result<String> {
        self.read(move |conn| {
            Ok(conn
                .query_row("SELECT auth_source FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
                .optional()?
                .unwrap_or_else(|| "local".to_owned()))
        })
        .await
    }

    /// Whether the account has a password of its own here.
    pub async fn has_password(&self, account_id: i64) -> Result<bool> {
        self.read(move |conn| {
            Ok(conn
                .query_row("SELECT password_hash IS NOT NULL FROM accounts WHERE id = ?1", [account_id], |row| {
                    row.get(0)
                })
                .optional()?
                .unwrap_or(false))
        })
        .await
    }

    /// Changes where an account's password is checked. Moving an account to the directory removes
    /// its password here, so the old one cannot be used next to the directory's.
    pub async fn set_auth_source(&self, login: &str, source: &str) -> Result<()> {
        if !matches!(source, "local" | "ldap" | "oidc") {
            return Err(StoreError::Invalid(format!("{source} is not a way to check passwords")));
        }
        let login = login_key(login)?;
        let source = source.to_owned();
        self.write(move |tx| {
            let clear_password = source == "ldap";
            let changed = tx.execute(
                "UPDATE accounts SET auth_source = ?1, credentials_changed_at = ?2,
                                     password_hash = CASE WHEN ?3 THEN NULL ELSE password_hash END
                 WHERE login = ?4 AND kind <> 'service'",
                params![source, now(), clear_password, login],
            )?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("account {login}")));
            }
            Ok(())
        })
        .await
    }
}

fn insert_tokens(conn: &Connection, grant_id: i64, access: &[u8], refresh: &[u8], now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO oauth_tokens (token_hash, grant_id, kind, expires_at) VALUES (?1, ?2, 'access', ?3)",
        params![access, grant_id, now + OAUTH_ACCESS_TOKEN_SECS],
    )?;
    conn.execute(
        "INSERT INTO oauth_tokens (token_hash, grant_id, kind, expires_at) VALUES (?1, ?2, 'refresh', ?3)",
        params![refresh, grant_id, now + OAUTH_REFRESH_TOKEN_SECS],
    )?;
    Ok(())
}

/// Removes a grant with its tokens, and the consent for its app when no other grant of the same app
/// remains: revoking an app means it has to ask again.
fn forget_grant(conn: &Connection, grant_id: i64) -> Result<()> {
    let owner: Option<(i64, i64)> = conn
        .query_row("SELECT account_id, client_id FROM oauth_grants WHERE id = ?1", [grant_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    conn.execute("DELETE FROM oauth_grants WHERE id = ?1", [grant_id])?;
    if let Some((account_id, client_id)) = owner {
        let credential = crate::push_credential_for_oauth_grant(grant_id);
        crate::push::forget_push_credential(conn, account_id, &credential)?;
        crate::held::cancel_held(
            conn,
            account_id,
            crate::held::HeldBy::Credential(&credential),
            crate::held::NOT_SENT_REVOKED,
        )?;
        conn.execute(
            "DELETE FROM oauth_consents WHERE account_id = ?1 AND client_id = ?2
               AND NOT EXISTS (SELECT 1 FROM oauth_grants WHERE account_id = ?1 AND client_id = ?2)",
            params![account_id, client_id],
        )?;
    }
    Ok(())
}

/// Clears what ran out: codes, tokens, grants without a token left, and apps nobody uses.
fn purge(conn: &Connection) -> rusqlite::Result<()> {
    let now = now();
    conn.execute("DELETE FROM oauth_codes WHERE expires_at <= ?1", [now])?;
    // Used refresh tokens stay while they would still be valid, to notice them coming back.
    conn.execute("DELETE FROM oauth_tokens WHERE expires_at <= ?1", [now])?;
    let unused = "NOT EXISTS (SELECT 1 FROM oauth_tokens t WHERE t.grant_id = oauth_grants.id)";
    conn.execute(
        &format!(
            "DELETE FROM push_subscriptions WHERE credential IN
                 (SELECT 'oauth:' || id FROM oauth_grants WHERE {unused})"
        ),
        [],
    )?;
    conn.execute(&format!("DELETE FROM oauth_grants WHERE {unused}"), [])?;
    conn.execute(
        "DELETE FROM oauth_clients WHERE coalesce(last_used_at, created_at) < ?1
           AND NOT EXISTS (SELECT 1 FROM oauth_grants g WHERE g.client_id = oauth_clients.id)",
        [now - UNUSED_CLIENT_SECS],
    )?;
    conn.execute(
        "DELETE FROM oauth_clients WHERE last_used_at IS NULL AND created_at < ?1
           AND NOT EXISTS (SELECT 1 FROM oauth_grants g WHERE g.client_id = oauth_clients.id)",
        [now - NEVER_USED_CLIENT_SECS],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewAccount, Role};

    fn challenge(verifier: &str) -> String {
        BASE64URL_NOPAD.encode(&Sha256::digest(verifier.as_bytes()))
    }

    #[test]
    fn redirect_addresses_apps_may_use() {
        assert!(valid_redirect_uri("https://app.example.com/callback"));
        assert!(valid_redirect_uri("http://127.0.0.1/cb"));
        assert!(valid_redirect_uri("http://127.0.0.1:53682/"));
        assert!(valid_redirect_uri("http://[::1]:8080/oauth"));
        assert!(valid_redirect_uri("http://localhost"));
        assert!(valid_redirect_uri("com.example.mail:/oauth2redirect"));
        assert!(!valid_redirect_uri("http://app.example.com/callback"), "plain http only on the device itself");
        assert!(!valid_redirect_uri("https://app.example.com/cb#fragment"));
        assert!(!valid_redirect_uri("https://user@app.example.com/cb"));
        assert!(!valid_redirect_uri("javascript:alert(1)"));
        assert!(!valid_redirect_uri("mailapp:/cb"), "a private scheme is a reversed domain name");
        assert!(!valid_redirect_uri("http://127.0.0.1.example.net/"));

        let registered = vec!["http://127.0.0.1/cb".to_owned(), "https://app.example.com/cb".to_owned()];
        assert!(redirect_uri_registered(&registered, "http://127.0.0.1:61234/cb"), "any port on loopback");
        assert!(!redirect_uri_registered(&registered, "http://127.0.0.1:61234/other"));
        assert!(redirect_uri_registered(&registered, "https://app.example.com/cb"));
        assert!(!redirect_uri_registered(&registered, "https://app.example.com:444/cb"));
    }

    #[test]
    fn scopes_are_cut_to_what_is_known() {
        assert_eq!(oauth_scopes("mail smtp openid"), vec!["openid", "mail", "smtp"]);
        assert_eq!(oauth_scopes("imap submission caldav bogus mail"), vec!["mail", "smtp", "dav"]);
        assert!(oauth_scopes("bogus").is_empty());
        assert!(!oauth_scopes_usable(&oauth_scopes("email profile")));
        assert_eq!(oauth_scopes("maskedemail openid"), vec!["openid", "maskedemail"]);
        assert!(oauth_scopes_usable(&oauth_scopes("maskedemail")));
        // RFC 7636 appendix B.
        assert!(pkce_matches(
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        ));
        assert!(!pkce_matches(
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "wrong-verifier-wrong-verifier-wrong-verifier"
        ));
    }

    #[tokio::test]
    async fn codes_tokens_rotation_and_reuse() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.org").await.unwrap();
        let leni = store
            .create_account(NewAccount {
                address: "leni@example.org".into(),
                display_name: "Leni".into(),
                password: Some("Seifenblase-Wanderweg-17".into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
        let client = store.register_oauth_client("Thunderbird", vec!["http://127.0.0.1/".into()]).await.unwrap();
        assert!(store.oauth_client(&client.client_id).await.unwrap().is_some());
        let refused = store.register_oauth_client("Evil", vec!["http://app.example.com/cb".into()]).await;
        assert!(matches!(refused, Err(StoreError::Rule { code: "invalid_redirect_uri", .. })));
        let verifier = "a".repeat(43);
        let new_code = |scopes: Vec<&'static str>| NewOAuthCode {
            client_id: client.id,
            account_id: leni.id,
            redirect_uri: "http://127.0.0.1:4000/".into(),
            scopes,
            code_challenge: challenge(&verifier),
            nonce: Some("n-0S6_WzA2Mj".into()),
            auth_time: now(),
        };
        assert!(!store.oauth_consented(leni.id, client.id, &["mail"]).await.unwrap());
        let code = store.create_oauth_code(new_code(vec!["openid", "mail"])).await.unwrap();
        assert!(store.oauth_consented(leni.id, client.id, &["mail"]).await.unwrap());
        assert!(!store.oauth_consented(leni.id, client.id, &["mail", "smtp"]).await.unwrap());

        // The wrong verifier spends the code as well.
        let wrong = store.redeem_oauth_code(&code, client.id, "http://127.0.0.1:4000/", &"b".repeat(43)).await;
        assert_eq!(wrong.unwrap().unwrap_err(), OAuthRefusal::InvalidGrant);
        let again = store.redeem_oauth_code(&code, client.id, "http://127.0.0.1:4000/", &verifier).await;
        assert_eq!(again.unwrap().unwrap_err(), OAuthRefusal::InvalidGrant);

        let code = store.create_oauth_code(new_code(vec!["openid", "mail"])).await.unwrap();
        let tokens = store.redeem_oauth_code(&code, client.id, "http://127.0.0.1:4000/", &verifier).await;
        let tokens = tokens.unwrap().unwrap();
        assert!(tokens.new_grant);
        assert_eq!(tokens.nonce.as_deref(), Some("n-0S6_WzA2Mj"));
        let auth = store.authenticate_oauth(&tokens.access_token, AppScope::Mail, "imap", "192.0.2.4").await.unwrap();
        assert!(matches!(auth, MailAuth::Ok { ref account, .. } if account.id == leni.id), "{auth:?}");
        let auth = store.authenticate_oauth(&tokens.access_token, AppScope::Smtp, "smtp", "").await.unwrap();
        assert!(matches!(auth, MailAuth::Denied(MailAuthDenied::WrongScope)));
        let grants = store.oauth_grants(leni.id).await.unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].last_used_protocol.as_deref(), Some("imap"));

        // Rotation: the new refresh token works, the old one ends the grant.
        let next = store.refresh_oauth(&tokens.refresh_token, client.id).await.unwrap().unwrap();
        let auth = store.authenticate_oauth(&next.access_token, AppScope::Mail, "jmap", "").await.unwrap();
        assert!(matches!(auth, MailAuth::Ok { .. }));
        let reused = store.refresh_oauth(&tokens.refresh_token, client.id).await.unwrap();
        assert!(matches!(reused, Err(OAuthRefusal::Reused { account_id, .. }) if account_id == leni.id));
        let auth = store.authenticate_oauth(&next.access_token, AppScope::Mail, "jmap", "").await.unwrap();
        assert!(matches!(auth, MailAuth::Denied(MailAuthDenied::Invalid)), "the whole grant is gone");
        assert!(store.oauth_grants(leni.id).await.unwrap().is_empty());
        assert!(!store.oauth_consented(leni.id, client.id, &["mail"]).await.unwrap(), "and the consent with it");

        // Revoking by token, and by the person.
        let code = store.create_oauth_code(new_code(vec!["mail"])).await.unwrap();
        let tokens =
            store.redeem_oauth_code(&code, client.id, "http://127.0.0.1:4000/", &verifier).await.unwrap().unwrap();
        assert_eq!(store.revoke_oauth_token(&tokens.refresh_token, client.id + 1).await.unwrap(), None);
        assert!(store.revoke_oauth_token(&tokens.refresh_token, client.id).await.unwrap().is_some());
        assert!(store.refresh_oauth(&tokens.refresh_token, client.id).await.unwrap().is_err());
        let code = store.create_oauth_code(new_code(vec!["mail"])).await.unwrap();
        let tokens =
            store.redeem_oauth_code(&code, client.id, "http://127.0.0.1:4000/", &verifier).await.unwrap().unwrap();
        store.revoke_oauth_grant(leni.id, tokens.grant_id).await.unwrap();
        let auth = store.authenticate_oauth(&tokens.access_token, AppScope::Mail, "imap", "").await.unwrap();
        assert!(matches!(auth, MailAuth::Denied(_)));

        // The signing key stays the same once made.
        let key = store.oauth_signing_key().await.unwrap();
        assert_eq!(store.oauth_signing_key().await.unwrap(), key);
    }

    #[tokio::test]
    async fn registrations_cannot_keep_new_apps_out() {
        let (store, _dir) = crate::test_support::store().await;
        let used = store.register_oauth_client("In use", vec!["http://127.0.0.1/".into()]).await.unwrap();
        let stale = now() - NEVER_USED_CLIENT_SECS - 60;
        store
            .write(move |tx| {
                tx.execute("UPDATE oauth_clients SET created_at = ?1, last_used_at = ?2", params![stale - 10, now()])?;
                let mut insert = tx.prepare(
                    "INSERT INTO oauth_clients (client_id, name, redirect_uris, created_at) VALUES (?1, 'Filler', 'http://127.0.0.1/', ?2)",
                )?;
                // One that ran out, the rest registered just now and never allowed in.
                insert.execute(params!["filler-old", stale])?;
                for n in 1..MAX_CLIENTS {
                    insert.execute(params![format!("filler-{n}"), now()])?;
                }
                Ok(())
            })
            .await
            .unwrap();

        // The table is full of unused apps: a new one still gets in, and the app in use stays.
        let fresh = store.register_oauth_client("Fresh", vec!["http://127.0.0.1/".into()]).await.unwrap();
        assert!(store.oauth_client(&fresh.client_id).await.unwrap().is_some());
        assert!(store.oauth_client(&used.client_id).await.unwrap().is_some());
        assert!(store.oauth_client("filler-old").await.unwrap().is_none(), "never used for a day: forgotten");
        let count: i64 = store
            .read(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM oauth_clients", [], |row| row.get(0))?))
            .await
            .unwrap();
        assert!(count <= MAX_CLIENTS);
        store.register_oauth_client("Next", vec!["http://127.0.0.1/".into()]).await.unwrap();
        assert!(store.oauth_client("filler-1").await.unwrap().is_none(), "the oldest unused app made room");
        assert!(store.oauth_client(&used.client_id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn services_and_switched_off_protocols_get_nothing() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.org").await.unwrap();
        let leni = store
            .create_account(NewAccount {
                address: "leni@example.org".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
        let client = store.register_oauth_client("aerc", vec!["https://app.example.com/cb".into()]).await.unwrap();
        let verifier = "v".repeat(64);
        let code = store
            .create_oauth_code(NewOAuthCode {
                client_id: client.id,
                account_id: leni.id,
                redirect_uri: "https://app.example.com/cb".into(),
                scopes: vec!["mail", "smtp"],
                code_challenge: challenge(&verifier),
                nonce: None,
                auth_time: now(),
            })
            .await
            .unwrap();
        let tokens =
            store.redeem_oauth_code(&code, client.id, "https://app.example.com/cb", &verifier).await.unwrap().unwrap();
        store
            .update_account(
                "leni@example.org",
                crate::AccountUpdate {
                    protocols: Some(crate::Protocols { smtp: false, ..Default::default() }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let auth = store.authenticate_oauth(&tokens.access_token, AppScope::Smtp, "smtp", "").await.unwrap();
        assert!(matches!(auth, MailAuth::Denied(MailAuthDenied::ProtocolOff)));
        store.set_account_disabled("leni@example.org", true).await.unwrap();
        let auth = store.authenticate_oauth(&tokens.access_token, AppScope::Mail, "imap", "").await.unwrap();
        assert!(matches!(auth, MailAuth::Denied(MailAuthDenied::Invalid)));
        assert!(store.refresh_oauth(&tokens.refresh_token, client.id).await.unwrap().is_err());

        assert!(
            store
                .link_external_identity(leni.id, "https://idp.example.net", "sub-1", "leni@example.org")
                .await
                .unwrap()
        );
        assert!(
            !store
                .link_external_identity(leni.id, "https://idp.example.net", "sub-2", "leni@example.org")
                .await
                .unwrap(),
            "a second login at the same provider does not get the account"
        );
        assert_eq!(store.external_identity("https://idp.example.net", "sub-1").await.unwrap(), Some(leni.id));
        assert_eq!(store.external_identity("https://idp.example.net", "sub-2").await.unwrap(), None);
        assert_eq!(store.auth_source(leni.id).await.unwrap(), "local");
        store.set_auth_source("leni@example.org", "ldap").await.unwrap();
        assert_eq!(store.auth_source(leni.id).await.unwrap(), "ldap");
        assert!(store.set_auth_source("leni@example.org", "kerberos").await.is_err());
    }

    #[tokio::test]
    async fn push_subscriptions_made_with_a_token_end_with_its_app() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.org").await.unwrap();
        let leni = store
            .create_account(NewAccount {
                address: "leni@example.org".into(),
                display_name: "Leni".into(),
                password: Some("Seifenblase-Wanderweg-17".into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
        let client = store.register_oauth_client("Phone", vec!["http://127.0.0.1/".into()]).await.unwrap();
        let verifier = "a".repeat(43);
        let code = store
            .create_oauth_code(NewOAuthCode {
                client_id: client.id,
                account_id: leni.id,
                redirect_uri: "http://127.0.0.1/".into(),
                scopes: vec!["mail"],
                code_challenge: challenge(&verifier),
                nonce: None,
                auth_time: now(),
            })
            .await
            .unwrap();
        let tokens = store.redeem_oauth_code(&code, client.id, "http://127.0.0.1/", &verifier).await.unwrap().unwrap();
        let (auth, grant) =
            store.authenticate_oauth_grant(&tokens.access_token, AppScope::Mail, "jmap", "192.0.2.4").await.unwrap();
        assert!(matches!(auth, MailAuth::Ok { .. }), "{auth:?}");
        let grant = grant.expect("a good token names its grant");
        let (_, refused) =
            store.authenticate_oauth_grant("uwu_at_nope", AppScope::Mail, "jmap", "192.0.2.4").await.unwrap();
        assert_eq!(refused, None);

        let credential = crate::push_credential_for_oauth_grant(grant);
        let (created, target) = store
            .create_push_subscription(crate::NewPushSubscription {
                account_id: leni.id,
                credential: credential.clone(),
                device_client_id: "phone".into(),
                url: "https://push.example.net/d/1".into(),
                keys: None,
                expires: now() + 3600,
                types: None,
            })
            .await
            .unwrap();
        let code =
            crate::PushSubscriptionUpdate { verification_code: Some(target.verification_code), ..Default::default() };
        store.update_push_subscription(leni.id, &credential, created.id, code).await.unwrap();
        assert_eq!(store.push_targets(vec![leni.id]).await.unwrap().len(), 1);

        // Signing the app out ends what it subscribed to.
        store.revoke_oauth_grant(leni.id, grant).await.unwrap();
        assert!(store.push_targets(vec![leni.id]).await.unwrap().is_empty());
    }

    /// A grant for masked addresses only opens nothing but JMAP, and there only when the caller
    /// asks for it (docs/jmap-masked-email.md); `mail` stays a scope of its own.
    #[tokio::test]
    async fn a_masked_only_grant_opens_nothing_else() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.org").await.unwrap();
        let leni = store
            .create_account(NewAccount {
                address: "leni@example.org".into(),
                display_name: "Leni".into(),
                password: Some("Seifenblase-Wanderweg-17".into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
        let client = store
            .register_oauth_client("UwULock (lock.example.com)", vec!["https://lock.example.com/cb".into()])
            .await
            .unwrap();
        let verifier = "a".repeat(43);
        let code = store
            .create_oauth_code(NewOAuthCode {
                client_id: client.id,
                account_id: leni.id,
                redirect_uri: "https://lock.example.com/cb".into(),
                scopes: vec![MASKED_EMAIL_SCOPE],
                code_challenge: challenge(&verifier),
                nonce: None,
                auth_time: now(),
            })
            .await
            .unwrap();
        let tokens =
            store.redeem_oauth_code(&code, client.id, "https://lock.example.com/cb", &verifier).await.unwrap().unwrap();
        assert_eq!(tokens.scopes, vec!["maskedemail"]);
        for (scope, protocol) in [(AppScope::Mail, "imap"), (AppScope::Mail, "jmap"), (AppScope::Smtp, "smtp")] {
            let auth = store.authenticate_oauth(&tokens.access_token, scope, protocol, "").await.unwrap();
            assert!(matches!(auth, MailAuth::Denied(MailAuthDenied::WrongScope)), "{protocol}: {auth:?}");
        }
        let (auth, grant) =
            store.authenticate_oauth_grant_or_masked(&tokens.access_token, AppScope::Mail, "jmap", "").await.unwrap();
        let MailAuth::Ok { scopes, .. } = auth else { panic!("{auth:?}") };
        assert!(scopes.is_empty(), "no mail, no sending, no calendars: {scopes:?}");
        let grant = grant.unwrap();
        assert_eq!(store.oauth_grant_client_name(grant).await.unwrap().as_deref(), Some("UwULock (lock.example.com)"));
        assert_eq!(store.oauth_grant_client_name(grant + 1).await.unwrap(), None);

        // Refreshing keeps the scope, and the old refresh token coming back ends the grant.
        let next = store.refresh_oauth(&tokens.refresh_token, client.id).await.unwrap().unwrap();
        assert_eq!(next.scopes, vec!["maskedemail"]);
        let reused = store.refresh_oauth(&tokens.refresh_token, client.id).await.unwrap();
        assert!(matches!(reused, Err(OAuthRefusal::Reused { .. })), "{reused:?}");
        let (auth, _) =
            store.authenticate_oauth_grant_or_masked(&next.access_token, AppScope::Mail, "jmap", "").await.unwrap();
        assert!(matches!(auth, MailAuth::Denied(MailAuthDenied::Invalid)), "{auth:?}");
    }
}
