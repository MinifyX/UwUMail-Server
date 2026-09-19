//! Everything that protects a login: app passwords for mail apps, second factors (authenticator
//! apps, passkeys, recovery codes), browser sessions and the activity list each person can read.

use aws_lc_rs::hmac;
use data_encoding::BASE32_NOPAD;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::directory::{ACCOUNT_COLUMN_COUNT, ACCOUNT_COLUMNS, account_from_row, login_key};
use crate::{Account, Result, Store, StoreError, now, password, random_bytes};

/// Letters and digits nobody mixes up: no i, l, o, 0 or 1.
const ALPHABET: &[u8; 31] = b"abcdefghjkmnpqrstuvwxyz23456789";
/// 16 characters of 31 are about 79 bits: too many to guess, so a plain SHA-256 is enough.
const APP_PASSWORD_CHARS: usize = 16;
const RECOVERY_CODE_CHARS: usize = 10;
const RECOVERY_CODES: usize = 10;
pub(crate) const MAX_APP_PASSWORDS: i64 = 50;
const TOTP_PERIOD: i64 = 30;
const EVENT_RETENTION_SECS: i64 = 180 * 24 * 3600;

fn random_code(chars: usize) -> String {
    let mut code = String::with_capacity(chars);
    while code.len() < chars {
        for byte in random_bytes::<32>() {
            // 248 = 8 × 31, so every character is equally likely.
            if byte < 248 && code.len() < chars {
                code.push(ALPHABET[usize::from(byte % 31)] as char);
            }
        }
    }
    code
}

/// "abcdefgh" → "abcd-efgh".
fn grouped(code: &str, size: usize) -> String {
    code.as_bytes().chunks(size).map(|chunk| String::from_utf8_lossy(chunk)).collect::<Vec<_>>().join("-")
}

/// What people type: dashes, spaces and capitals do not matter.
fn normalized(input: &str) -> String {
    input.chars().filter(|c| !c.is_whitespace() && *c != '-').flat_map(char::to_lowercase).collect()
}

fn code_hash(kind: &str, normalized: &str) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update(b":");
    hasher.update(normalized.as_bytes());
    hasher.finalize().to_vec()
}

/// The hash to look up, if the input looks like a code of this length at all.
fn candidate(kind: &str, input: &str, chars: usize) -> Option<Vec<u8>> {
    let code = normalized(input);
    (code.len() == chars && code.bytes().all(|b| ALPHABET.contains(&b))).then(|| code_hash(kind, &code))
}

/// What an app password may be used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppScope {
    /// Reading and managing mail with JMAP or IMAP. JMAP can also send.
    Mail,
    /// Sending through SMTP submission.
    Smtp,
    /// Calendars and contacts with CalDAV and CardDAV.
    Dav,
}

/// The uses an app password of this account can sensibly have: only protocols the account may
/// actually use. A service with nothing but SMTP gets a password that can only send.
/// Whether an account may use the protocol at all. A person may use everything; a service is
/// switched on one protocol at a time, and a switch that is off holds whatever password is typed.
///
/// `dav` here means calendars or address books; which of the two a request may touch is decided
/// where the collections are served, because one password covers both.
fn protocol_allowed(account: &crate::Account, protocol: &str) -> bool {
    let protocols = account.protocols;
    match protocol {
        "imap" => protocols.imap,
        "jmap" => protocols.jmap,
        "smtp" => protocols.smtp,
        "dav" => protocols.caldav || protocols.carddav,
        // A protocol nobody taught this function about is not quietly allowed.
        _ => false,
    }
}

pub(crate) fn scopes_for(protocols: crate::Protocols) -> Vec<AppScope> {
    let mut scopes = Vec::new();
    if protocols.imap || protocols.jmap {
        scopes.push(AppScope::Mail);
    }
    if protocols.smtp {
        scopes.push(AppScope::Smtp);
    }
    if protocols.caldav || protocols.carddav {
        scopes.push(AppScope::Dav);
    }
    scopes
}

impl AppScope {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            AppScope::Mail => "mail",
            AppScope::Smtp => "smtp",
            AppScope::Dav => "dav",
        }
    }

    fn parse_list(value: &str) -> Vec<AppScope> {
        value
            .split_whitespace()
            .filter_map(|scope| match scope {
                "mail" => Some(AppScope::Mail),
                "smtp" => Some(AppScope::Smtp),
                "dav" => Some(AppScope::Dav),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPassword {
    pub id: i64,
    pub name: String,
    pub scopes: Vec<AppScope>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
    /// "jmap" or "smtp".
    pub last_used_protocol: Option<String>,
    pub last_used_ip: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewAppPassword {
    pub name: String,
    pub scopes: Vec<AppScope>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct CreatedAppPassword {
    pub app_password: AppPassword,
    /// Shown once, in groups of four: "abcd-efgh-jkmn-pqrs".
    pub secret: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailAuthDenied {
    /// Wrong password, or the person may not log in.
    Invalid,
    /// There is no such login here at all.
    ///
    /// Told apart from [`MailAuthDenied::Invalid`] only so that guessing at names can be stopped
    /// sooner than guessing at passwords: whoever works through `info@`, `sales@` and `admin@` is
    /// not close to a password, they are reading the address book. What the other side is told
    /// stays word for word the same, and so does how long it takes — see the check itself.
    UnknownLogin,
    /// The main password was right, but this person uses app passwords for mail apps.
    AppPasswordRequired,
    Expired,
    /// The app password is not allowed for this protocol.
    WrongScope,
    /// The account itself may not use this protocol, whatever password was typed.
    ProtocolOff,
}

impl std::fmt::Display for MailAuthDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            // Word for word the same as UnknownLogin, and it has to stay that way: this text
            // reaches the other side, and a difference here would say which names exist.
            MailAuthDenied::Invalid => "wrong login or password",
            MailAuthDenied::UnknownLogin => "wrong login or password",
            MailAuthDenied::AppPasswordRequired => "main password used, but an app password is required",
            MailAuthDenied::Expired => "the app password has expired",
            MailAuthDenied::WrongScope => "the app password is not allowed for this",
            // Not „wrong password“: the password may be perfectly right, this account simply does
            // not do this protocol. Saying so saves whoever set it up an evening.
            MailAuthDenied::ProtocolOff => "this account may not use this protocol",
        })
    }
}

#[derive(Debug, Clone)]
pub enum MailAuth {
    Ok { account: Account, app_password: Option<i64> },
    Denied(MailAuthDenied),
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityOverview {
    pub totp: bool,
    pub passkeys: i64,
    pub recovery_codes_left: i64,
    /// The person's own switch.
    pub apps_need_app_password: bool,
    /// An authenticator app or a passkey is set up.
    pub second_factor: bool,
}

impl SecurityOverview {
    /// Mail apps must use app passwords: always with a second factor, or when switched on.
    pub fn app_passwords_required(&self) -> bool {
        self.second_factor || self.apps_need_app_password
    }
}

#[derive(Debug, Clone)]
pub struct TotpSetup {
    /// Base32, for typing into the app.
    pub secret: String,
    /// `otpauth://` link for the QR code.
    pub uri: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeCheck {
    Totp,
    RecoveryCode { left: i64 },
    Invalid,
}

#[derive(Debug, Clone)]
pub struct SecurityEvent {
    pub kind: String,
    /// Empty when the person did it, otherwise the admin's login or "cli".
    pub actor: String,
    pub ip: String,
    pub details: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityEventRecord {
    pub id: i64,
    pub at: i64,
    pub kind: String,
    pub actor: String,
    pub ip: String,
    pub details: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSessionInfo {
    /// Derived from the token's hash; safe to show.
    pub id: String,
    pub created_at: i64,
    pub last_seen_at: i64,
    pub ip: String,
    pub user_agent: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Passkey {
    pub id: i64,
    #[serde(skip)]
    pub account_id: i64,
    #[serde(skip)]
    pub credential_id: Vec<u8>,
    #[serde(skip)]
    pub public_key: Vec<u8>,
    #[serde(skip)]
    pub sign_count: i64,
    pub name: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

const PASSKEY_COLUMNS: &str = "id, account_id, credential_id, public_key, sign_count, name, created_at, last_used_at";

fn passkey_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Passkey> {
    Ok(Passkey {
        id: row.get(0)?,
        account_id: row.get(1)?,
        credential_id: row.get(2)?,
        public_key: row.get(3)?,
        sign_count: row.get(4)?,
        name: row.get(5)?,
        created_at: row.get(6)?,
        last_used_at: row.get(7)?,
    })
}

const APP_PASSWORD_COLUMNS: &str =
    "id, name, scopes, created_at, expires_at, last_used_at, last_used_protocol, last_used_ip";

fn app_password_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppPassword> {
    Ok(AppPassword {
        id: row.get(0)?,
        name: row.get(1)?,
        scopes: AppScope::parse_list(&row.get::<_, String>(2)?),
        created_at: row.get(3)?,
        expires_at: row.get(4)?,
        last_used_at: row.get(5)?,
        last_used_protocol: row.get(6)?,
        last_used_ip: row.get(7)?,
    })
}

fn has_second_factor(conn: &Connection, account_id: i64) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM totp_secrets WHERE account_id = ?1 AND confirmed_at IS NOT NULL)
             OR EXISTS (SELECT 1 FROM passkeys WHERE account_id = ?1)",
        [account_id],
        |row| row.get(0),
    )
}

fn credentials_changed(conn: &Connection, account_id: i64) -> rusqlite::Result<()> {
    conn.execute("UPDATE accounts SET credentials_changed_at = ?1 WHERE id = ?2", params![now(), account_id])?;
    Ok(())
}

/// Replaces all recovery codes and returns the new ones, grouped for reading.
fn new_recovery_codes(conn: &Connection, account_id: i64) -> rusqlite::Result<Vec<String>> {
    conn.execute("DELETE FROM recovery_codes WHERE account_id = ?1", [account_id])?;
    let created_at = now();
    (0..RECOVERY_CODES)
        .map(|_| {
            let code = random_code(RECOVERY_CODE_CHARS);
            conn.execute(
                "INSERT INTO recovery_codes (account_id, code_hash, created_at) VALUES (?1, ?2, ?3)",
                params![account_id, code_hash("recovery", &code), created_at],
            )?;
            Ok(grouped(&code, 5))
        })
        .collect()
}

/// Recovery codes only make sense while there is a second factor to recover from.
fn drop_recovery_codes_if_unused(conn: &Connection, account_id: i64) -> rusqlite::Result<()> {
    if !has_second_factor(conn, account_id)? {
        conn.execute("DELETE FROM recovery_codes WHERE account_id = ?1", [account_id])?;
    }
    Ok(())
}

/// RFC 6238 with the defaults every authenticator app uses: SHA-1, 6 digits, 30 seconds.
fn totp_code(secret: &[u8], step: i64) -> u32 {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = hmac::sign(&key, &step.to_be_bytes());
    let digest = tag.as_ref();
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    let binary =
        u32::from_be_bytes([digest[offset] & 0x7f, digest[offset + 1], digest[offset + 2], digest[offset + 3]]);
    binary % 1_000_000
}

/// The time step a code belongs to, allowing one step of clock drift either way. Steps up to
/// `last_step` were used already.
fn totp_step(secret: &[u8], code: &str, at: i64, last_step: i64) -> Option<i64> {
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let code: u32 = code.parse().ok()?;
    let current = at / TOTP_PERIOD;
    (current - 1..=current + 1).find(|&step| step > last_step && totp_code(secret, step) == code)
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Store {
    pub async fn security_overview(&self, account_id: i64) -> Result<SecurityOverview> {
        self.read(move |conn| {
            Ok(conn.query_row(
                "SELECT
                    EXISTS (SELECT 1 FROM totp_secrets WHERE account_id = ?1 AND confirmed_at IS NOT NULL),
                    (SELECT COUNT(*) FROM passkeys WHERE account_id = ?1),
                    (SELECT COUNT(*) FROM recovery_codes WHERE account_id = ?1 AND used_at IS NULL),
                    (SELECT apps_need_app_password FROM accounts WHERE id = ?1)",
                [account_id],
                |row| {
                    let (totp, passkeys): (bool, i64) = (row.get(0)?, row.get(1)?);
                    Ok(SecurityOverview {
                        totp,
                        passkeys,
                        recovery_codes_left: row.get(2)?,
                        apps_need_app_password: row.get::<_, Option<bool>>(3)?.unwrap_or_default(),
                        second_factor: totp || passkeys > 0,
                    })
                },
            )?)
        })
        .await
    }

    /// Logins of admins who have no second factor, for the health overview.
    pub async fn admins_without_second_factor(&self) -> Result<Vec<String>> {
        self.read(|conn| {
            let mut stmt = conn.prepare(
                "SELECT login FROM accounts a
                 WHERE role = 'admin' AND disabled = 0 AND deleted_at IS NULL
                   AND NOT EXISTS (SELECT 1 FROM totp_secrets t WHERE t.account_id = a.id AND t.confirmed_at IS NOT NULL)
                   AND NOT EXISTS (SELECT 1 FROM passkeys p WHERE p.account_id = a.id)
                 ORDER BY login",
            )?;
            let logins = stmt.query_map([], |row| row.get(0))?.collect::<Result<Vec<String>, _>>()?;
            Ok(logins)
        })
        .await
    }

    pub async fn set_apps_need_app_password(&self, account_id: i64, on: bool) -> Result<()> {
        self.write(move |tx| {
            tx.execute("UPDATE accounts SET apps_need_app_password = ?1 WHERE id = ?2", params![on, account_id])?;
            credentials_changed(tx, account_id)?;
            Ok(())
        })
        .await
    }

    // App passwords

    pub async fn app_passwords(&self, account_id: i64) -> Result<Vec<AppPassword>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {APP_PASSWORD_COLUMNS} FROM app_passwords WHERE account_id = ?1 ORDER BY created_at DESC, id DESC"
            ))?;
            let rows = stmt.query_map([account_id], app_password_from_row)?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn create_app_password(&self, account_id: i64, new: NewAppPassword) -> Result<CreatedAppPassword> {
        let name = new.name.trim().to_owned();
        if name.is_empty() || name.chars().count() > 60 {
            return Err(StoreError::Invalid("an app password needs a name of up to 60 characters".into()));
        }
        let mut scopes = new.scopes.clone();
        scopes.sort_by_key(|scope| scope.as_str());
        scopes.dedup();
        if scopes.is_empty() {
            return Err(StoreError::Invalid("an app password needs at least one use".into()));
        }
        let created_at = now();
        if new.expires_at.is_some_and(|at| at <= created_at) {
            return Err(StoreError::Invalid("the expiry date must be in the future".into()));
        }
        let secret = random_code(APP_PASSWORD_CHARS);
        let hash = code_hash("app", &secret);
        let scope_list = scopes.iter().map(|scope| scope.as_str()).collect::<Vec<_>>().join(" ");
        let expires_at = new.expires_at;
        let id = self
            .write(move |tx| {
                let count: i64 =
                    tx.query_row("SELECT COUNT(*) FROM app_passwords WHERE account_id = ?1", [account_id], |r| {
                        r.get(0)
                    })?;
                if count >= MAX_APP_PASSWORDS {
                    return Err(StoreError::Rule {
                        code: "tooManyAppPasswords",
                        message: format!("at most {MAX_APP_PASSWORDS} app passwords"),
                    });
                }
                tx.execute(
                    "INSERT INTO app_passwords (account_id, name, secret_hash, scopes, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![account_id, name, hash, scope_list, created_at, expires_at],
                )?;
                Ok(tx.last_insert_rowid())
            })
            .await?;
        Ok(CreatedAppPassword {
            app_password: AppPassword {
                id,
                name: new.name.trim().to_owned(),
                scopes,
                created_at,
                expires_at,
                last_used_at: None,
                last_used_protocol: None,
                last_used_ip: None,
            },
            secret: grouped(&secret, 4),
        })
    }

    /// Takes over an app password from another server by its hash, e.g. mailcow's bcrypt. It keeps
    /// working with the secret the person already typed into their apps. Taking over the same name
    /// again changes nothing.
    pub async fn import_app_password(
        &self,
        account_id: i64,
        name: &str,
        stored: &str,
        scopes: Vec<AppScope>,
    ) -> Result<AppPassword> {
        let name = name.trim().chars().take(60).collect::<String>();
        if name.is_empty() {
            return Err(StoreError::Invalid("an app password needs a name".into()));
        }
        let stored = password::import_hash(stored)?;
        let mut scopes = scopes;
        scopes.sort_by_key(|scope| scope.as_str());
        scopes.dedup();
        if scopes.is_empty() {
            return Err(StoreError::Invalid("an app password needs at least one use".into()));
        }
        let scope_list = scopes.iter().map(|scope| scope.as_str()).collect::<Vec<_>>().join(" ");
        self.write(move |tx| {
            let existing = tx
                .query_row(
                    &format!(
                        "SELECT {APP_PASSWORD_COLUMNS} FROM app_passwords
                         WHERE account_id = ?1 AND name = ?2 AND imported_hash IS NOT NULL"
                    ),
                    params![account_id, name],
                    app_password_from_row,
                )
                .optional()?;
            if let Some(existing) = existing {
                return Ok(existing);
            }
            let count: i64 =
                tx.query_row("SELECT COUNT(*) FROM app_passwords WHERE account_id = ?1", [account_id], |r| r.get(0))?;
            if count >= MAX_APP_PASSWORDS {
                return Err(StoreError::Rule {
                    code: "tooManyAppPasswords",
                    message: format!("at most {MAX_APP_PASSWORDS} app passwords"),
                });
            }
            // Never matches a typed code: only the imported hash is checked for this one.
            let unmatchable = random_bytes::<32>().to_vec();
            tx.execute(
                "INSERT INTO app_passwords (account_id, name, secret_hash, scopes, created_at, imported_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![account_id, name, unmatchable, scope_list, now(), stored],
            )?;
            let id = tx.last_insert_rowid();
            Ok(tx.query_row(
                &format!("SELECT {APP_PASSWORD_COLUMNS} FROM app_passwords WHERE id = ?1"),
                [id],
                app_password_from_row,
            )?)
        })
        .await
    }

    /// Takes over a password hash from another server, e.g. mailcow's BLF-CRYPT. At the next login
    /// with the right password it is replaced by our own.
    pub async fn import_password_hash(&self, account_id: i64, stored: &str) -> Result<()> {
        let stored = password::import_hash(stored)?;
        self.write(move |tx| {
            let changed =
                tx.execute("UPDATE accounts SET password_hash = ?1 WHERE id = ?2", params![stored, account_id])?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("account {account_id}")));
            }
            Ok(())
        })
        .await
    }

    /// Replaces a hash taken over from another server with our own, now that the password proved right.
    /// A password too short for our own rules keeps the old hash.
    pub(crate) async fn upgrade_imported_hash(&self, account_id: i64, old: String, password: String) {
        let Ok(Ok(new)) = tokio::task::spawn_blocking(move || password::hash(&password)).await else {
            return;
        };
        let result = self
            .write(move |tx| {
                tx.execute(
                    "UPDATE accounts SET password_hash = ?1 WHERE id = ?2 AND password_hash = ?3",
                    params![new, account_id, old],
                )?;
                Ok(())
            })
            .await;
        if let Err(err) = result {
            tracing::warn!(%err, account_id, "replacing a taken-over password hash failed");
        }
    }

    pub async fn revoke_app_password(&self, account_id: i64, id: i64) -> Result<AppPassword> {
        self.write(move |tx| {
            let found = tx
                .query_row(
                    &format!("SELECT {APP_PASSWORD_COLUMNS} FROM app_passwords WHERE id = ?1 AND account_id = ?2"),
                    params![id, account_id],
                    app_password_from_row,
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("app password {id}")))?;
            tx.execute("DELETE FROM app_passwords WHERE id = ?1", [id])?;
            credentials_changed(tx, account_id)?;
            Ok(found)
        })
        .await
    }

    /// Checks a login from a mail app (JMAP, SMTP, later IMAP). App passwords always work within
    /// their scope; the main password only while the person does not require app passwords.
    /// Unknown logins and wrong passwords take as long as right ones.
    pub async fn authenticate_mail(
        &self,
        login: &str,
        password: &str,
        scope: AppScope,
        protocol: &str,
        ip: &str,
    ) -> Result<MailAuth> {
        let login = login_key(login).unwrap_or_default();
        let app_hash = candidate("app", password, APP_PASSWORD_CHARS);
        let found = self
            .read(move |conn| {
                let row = conn
                    .query_row(
                        &format!(
                            "SELECT {ACCOUNT_COLUMNS}, password_hash, apps_need_app_password FROM accounts WHERE login = ?1"
                        ),
                        [login],
                        |row| {
                            Ok((
                                account_from_row(row)?,
                                row.get::<_, Option<String>>(ACCOUNT_COLUMN_COUNT)?,
                                row.get::<_, bool>(ACCOUNT_COLUMN_COUNT + 1)?,
                            ))
                        },
                    )
                    .optional()?;
                let Some((account, hash, flag)) = row else {
                    return Ok(None);
                };
                let required = flag || has_second_factor(conn, account.id)?;
                let app = match &app_hash {
                    Some(app_hash) => conn
                        .query_row(
                            "SELECT id, scopes, expires_at FROM app_passwords WHERE account_id = ?1 AND secret_hash = ?2",
                            params![account.id, app_hash],
                            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<i64>>(2)?)),
                        )
                        .optional()?,
                    None => None,
                };
                // App passwords taken over from another server can only be told apart by checking each hash.
                let mut imported = Vec::new();
                if app.is_none() {
                    let mut stmt = conn.prepare(
                        "SELECT id, scopes, expires_at, imported_hash FROM app_passwords
                         WHERE account_id = ?1 AND imported_hash IS NOT NULL",
                    )?;
                    let rows = stmt.query_map([account.id], |row| {
                        Ok(((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<i64>>(2)?), row.get::<_, String>(3)?))
                    })?;
                    imported = rows.collect::<rusqlite::Result<Vec<_>>>()?;
                }
                Ok(Some((account, hash, required, app, imported)))
            })
            .await?;

        let Some((account, hash, required, mut app, imported)) = found else {
            // The hashing happens anyway, against nothing: without it this answer would come back
            // faster than a wrong password does, and the clock alone would say which names exist.
            let password = password.to_owned();
            let _ = tokio::task::spawn_blocking(move || password::verify(&password, None)).await;
            return Ok(MailAuth::Denied(MailAuthDenied::UnknownLogin));
        };
        if app.is_none() && !imported.is_empty() {
            let password = password.to_owned();
            app = tokio::task::spawn_blocking(move || {
                imported.into_iter().find(|(_, stored)| password::verify(&password, Some(stored))).map(|(app, _)| app)
            })
            .await
            .map_err(|err| StoreError::Internal(err.to_string()))?;
        }

        if let Some((id, scopes, expires_at)) = app {
            let now = now();
            if !account.can_log_in() {
                return Ok(MailAuth::Denied(MailAuthDenied::Invalid));
            }
            if !protocol_allowed(&account, protocol) {
                return Ok(MailAuth::Denied(MailAuthDenied::ProtocolOff));
            }
            if expires_at.is_some_and(|at| at <= now) {
                return Ok(MailAuth::Denied(MailAuthDenied::Expired));
            }
            if !AppScope::parse_list(&scopes).contains(&scope) {
                return Ok(MailAuth::Denied(MailAuthDenied::WrongScope));
            }
            let (protocol, ip) = (protocol.to_owned(), ip.to_owned());
            self.write(move |tx| {
                tx.execute(
                    "UPDATE app_passwords SET last_used_at = ?1, last_used_protocol = ?2, last_used_ip = ?3
                     WHERE id = ?4 AND (last_used_at IS NULL OR last_used_at < ?1 - 60
                                        OR last_used_protocol IS NOT ?2 OR last_used_ip IS NOT ?3)",
                    params![now, protocol, ip, id],
                )?;
                Ok(())
            })
            .await?;
            return Ok(MailAuth::Ok { account, app_password: Some(id) });
        }

        let (typed, stored) = (password.to_owned(), hash.clone());
        let valid = tokio::task::spawn_blocking(move || password::verify(&typed, stored.as_deref()))
            .await
            .map_err(|err| StoreError::Internal(err.to_string()))?;
        if !valid || !account.can_log_in() {
            return Ok(MailAuth::Denied(MailAuthDenied::Invalid));
        }
        if !protocol_allowed(&account, protocol) {
            return Ok(MailAuth::Denied(MailAuthDenied::ProtocolOff));
        }
        if let Some(old) = hash.filter(|hash| password::is_imported(hash)) {
            self.upgrade_imported_hash(account.id, old, password.to_owned()).await;
        }
        if required {
            // The person should learn that their main password was typed into a mail app,
            // but a phone that keeps retrying must not flood the list.
            let (account_id, ip, protocol) = (account.id, ip.to_owned(), protocol.to_owned());
            self.write(move |tx| {
                let recent: bool = tx.query_row(
                    "SELECT EXISTS (SELECT 1 FROM security_events WHERE account_id = ?1 AND kind = 'mainPasswordRefused' AND at > ?2)",
                    params![account_id, now() - 3600],
                    |row| row.get(0),
                )?;
                if !recent {
                    insert_event(
                        tx,
                        account_id,
                        &SecurityEvent {
                            kind: "mainPasswordRefused".into(),
                            actor: String::new(),
                            ip,
                            details: serde_json::json!({ "protocol": protocol }),
                        },
                    )?;
                }
                Ok(())
            })
            .await?;
            return Ok(MailAuth::Denied(MailAuthDenied::AppPasswordRequired));
        }
        Ok(MailAuth::Ok { account, app_password: None })
    }

    // Password

    /// Changes the password after checking the current one, and logs out every other browser.
    pub async fn change_password(&self, account_id: i64, current: &str, new: &str, keep_token: &str) -> Result<()> {
        let stored: Option<String> = self
            .read(move |conn| {
                Ok(conn
                    .query_row("SELECT password_hash FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
                    .optional()?
                    .flatten())
            })
            .await?;
        let (current, new) = (current.to_owned(), new.to_owned());
        let hash = tokio::task::spawn_blocking(move || {
            if !password::verify(&current, stored.as_deref()) {
                return Err(StoreError::Rule {
                    code: "wrongPassword",
                    message: "the current password is wrong".into(),
                });
            }
            password::hash(&new)
        })
        .await
        .map_err(|err| StoreError::Internal(err.to_string()))??;
        let keep = crate::web::token_hash(keep_token);
        self.write(move |tx| {
            tx.execute("UPDATE accounts SET password_hash = ?1 WHERE id = ?2", params![hash, account_id])?;
            credentials_changed(tx, account_id)?;
            tx.execute(
                "DELETE FROM web_sessions WHERE account_id = ?1 AND token_hash != ?2",
                params![account_id, keep],
            )?;
            tx.execute("DELETE FROM password_links WHERE account_id = ?1", [account_id])?;
            Ok(())
        })
        .await
    }

    // Authenticator app

    /// Starts setting up an authenticator app with a fresh secret. Nothing changes for the login
    /// until [`Store::confirm_totp`] sees a correct code.
    pub async fn begin_totp(&self, account_id: i64, issuer: &str, login: &str) -> Result<TotpSetup> {
        let secret = random_bytes::<20>();
        let encoded = BASE32_NOPAD.encode(&secret);
        let uri = format!(
            "otpauth://totp/{}:{}?secret={encoded}&issuer={}&algorithm=SHA1&digits=6&period={TOTP_PERIOD}",
            percent_encode(issuer),
            percent_encode(login),
            percent_encode(issuer),
        );
        self.write(move |tx| {
            let active: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM totp_secrets WHERE account_id = ?1 AND confirmed_at IS NOT NULL)",
                [account_id],
                |row| row.get(0),
            )?;
            if active {
                return Err(StoreError::Rule {
                    code: "totpActive",
                    message: "an authenticator app is already set up".into(),
                });
            }
            tx.execute(
                "INSERT OR REPLACE INTO totp_secrets (account_id, secret, created_at) VALUES (?1, ?2, ?3)",
                params![account_id, secret.to_vec(), now()],
            )?;
            Ok(())
        })
        .await?;
        Ok(TotpSetup { secret: encoded, uri })
    }

    /// Switches the authenticator app on when the code matches. Returns recovery codes if this
    /// is the person's first second factor.
    pub async fn confirm_totp(&self, account_id: i64, code: &str) -> Result<Option<Vec<String>>> {
        let code = code.to_owned();
        self.write(move |tx| {
            let pending = tx
                .query_row(
                    "SELECT secret, created_at FROM totp_secrets WHERE account_id = ?1 AND confirmed_at IS NULL",
                    [account_id],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            let Some((secret, created_at)) = pending else {
                return Err(StoreError::Rule { code: "totpNotStarted", message: "start the setup again".into() });
            };
            if now() - created_at > 3600 {
                return Err(StoreError::Rule { code: "totpNotStarted", message: "start the setup again".into() });
            }
            let Some(step) = totp_step(&secret, &code, now(), 0) else {
                return Err(StoreError::Rule { code: "codeInvalid", message: "the code does not match".into() });
            };
            let first = !has_second_factor(tx, account_id)?;
            tx.execute(
                "UPDATE totp_secrets SET confirmed_at = ?1, last_step = ?2 WHERE account_id = ?3",
                params![now(), step, account_id],
            )?;
            credentials_changed(tx, account_id)?;
            Ok(if first { Some(new_recovery_codes(tx, account_id)?) } else { None })
        })
        .await
    }

    pub async fn disable_totp(&self, account_id: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute("DELETE FROM totp_secrets WHERE account_id = ?1", [account_id])?;
            drop_recovery_codes_if_unused(tx, account_id)?;
            credentials_changed(tx, account_id)?;
            Ok(())
        })
        .await
    }

    /// A code typed at login: from the authenticator app (6 digits) or a recovery code.
    /// Each code works once.
    pub async fn check_second_factor_code(&self, account_id: i64, code: &str) -> Result<CodeCheck> {
        let code = code.to_owned();
        self.write(move |tx| {
            let totp = tx
                .query_row(
                    "SELECT secret, last_step FROM totp_secrets WHERE account_id = ?1 AND confirmed_at IS NOT NULL",
                    [account_id],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            if let Some((secret, last_step)) = totp
                && let Some(step) = totp_step(&secret, &code, now(), last_step)
            {
                tx.execute("UPDATE totp_secrets SET last_step = ?1 WHERE account_id = ?2", params![step, account_id])?;
                return Ok(CodeCheck::Totp);
            }
            if let Some(hash) = candidate("recovery", &code, RECOVERY_CODE_CHARS) {
                let used = tx.execute(
                    "UPDATE recovery_codes SET used_at = ?1 WHERE account_id = ?2 AND code_hash = ?3 AND used_at IS NULL",
                    params![now(), account_id, hash],
                )?;
                if used > 0 {
                    let left: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM recovery_codes WHERE account_id = ?1 AND used_at IS NULL",
                        [account_id],
                        |row| row.get(0),
                    )?;
                    return Ok(CodeCheck::RecoveryCode { left });
                }
            }
            Ok(CodeCheck::Invalid)
        })
        .await
    }

    pub async fn regenerate_recovery_codes(&self, account_id: i64) -> Result<Vec<String>> {
        self.write(move |tx| {
            if !has_second_factor(tx, account_id)? {
                return Err(StoreError::Rule {
                    code: "noSecondFactor",
                    message: "recovery codes need an authenticator app or a passkey first".into(),
                });
            }
            Ok(new_recovery_codes(tx, account_id)?)
        })
        .await
    }

    /// Removes every second factor, e.g. when an admin helps someone who lost their phone.
    pub async fn reset_second_factors(&self, account_id: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute("DELETE FROM totp_secrets WHERE account_id = ?1", [account_id])?;
            tx.execute("DELETE FROM passkeys WHERE account_id = ?1", [account_id])?;
            tx.execute("DELETE FROM recovery_codes WHERE account_id = ?1", [account_id])?;
            credentials_changed(tx, account_id)?;
            Ok(())
        })
        .await
    }

    // Passkeys

    pub async fn passkeys(&self, account_id: i64) -> Result<Vec<Passkey>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {PASSKEY_COLUMNS} FROM passkeys WHERE account_id = ?1 ORDER BY created_at, id"
            ))?;
            let rows = stmt.query_map([account_id], passkey_from_row)?.collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// Stores a verified passkey. Returns recovery codes if this is the first second factor.
    pub async fn add_passkey(
        &self,
        account_id: i64,
        credential_id: Vec<u8>,
        public_key: Vec<u8>,
        sign_count: i64,
        name: &str,
    ) -> Result<(Passkey, Option<Vec<String>>)> {
        let name = name.trim().chars().take(60).collect::<String>();
        let name = if name.is_empty() { "Passkey".to_owned() } else { name };
        self.write(move |tx| {
            let first = !has_second_factor(tx, account_id)?;
            let created_at = now();
            tx.execute(
                "INSERT INTO passkeys (account_id, credential_id, public_key, sign_count, name, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![account_id, credential_id, public_key, sign_count, name, created_at],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                    StoreError::Rule { code: "passkeyKnown", message: "this passkey is already registered".into() }
                }
                other => other.into(),
            })?;
            let id = tx.last_insert_rowid();
            credentials_changed(tx, account_id)?;
            let codes = if first { Some(new_recovery_codes(tx, account_id)?) } else { None };
            let passkey =
                Passkey { id, account_id, credential_id, public_key, sign_count, name, created_at, last_used_at: None };
            Ok((passkey, codes))
        })
        .await
    }

    pub async fn passkey_by_credential(&self, credential_id: &[u8]) -> Result<Option<Passkey>> {
        let credential_id = credential_id.to_vec();
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {PASSKEY_COLUMNS} FROM passkeys WHERE credential_id = ?1"),
                    [credential_id],
                    passkey_from_row,
                )
                .optional()?)
        })
        .await
    }

    pub async fn touch_passkey(&self, id: i64, sign_count: i64) -> Result<()> {
        self.write(move |tx| {
            tx.execute(
                "UPDATE passkeys SET sign_count = ?1, last_used_at = ?2 WHERE id = ?3",
                params![sign_count, now(), id],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn remove_passkey(&self, account_id: i64, id: i64) -> Result<Passkey> {
        self.write(move |tx| {
            let passkey = tx
                .query_row(
                    &format!("SELECT {PASSKEY_COLUMNS} FROM passkeys WHERE id = ?1 AND account_id = ?2"),
                    params![id, account_id],
                    passkey_from_row,
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound(format!("passkey {id}")))?;
            tx.execute("DELETE FROM passkeys WHERE id = ?1", [id])?;
            drop_recovery_codes_if_unused(tx, account_id)?;
            credentials_changed(tx, account_id)?;
            Ok(passkey)
        })
        .await
    }

    // Activity

    pub async fn record_security_event(&self, account_id: i64, event: SecurityEvent) -> Result<()> {
        self.write(move |tx| {
            insert_event(tx, account_id, &event)?;
            Ok(())
        })
        .await
    }

    /// How often something happened to a person since a point in time, optionally only for one
    /// address in the details. For throttling.
    pub async fn count_security_events(
        &self,
        account_id: i64,
        kind: &str,
        since: i64,
        address: Option<&str>,
    ) -> Result<i64> {
        let (kind, address) = (kind.to_owned(), address.map(str::to_owned));
        self.read(move |conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM security_events
                 WHERE account_id = ?1 AND kind = ?2 AND at >= ?3
                   AND (?4 IS NULL OR json_extract(details, '$.address') = ?4)",
                params![account_id, kind, since, address],
                |row| row.get(0),
            )?)
        })
        .await
    }

    pub async fn security_events(&self, account_id: i64, limit: usize) -> Result<Vec<SecurityEventRecord>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, at, kind, actor, ip, details FROM security_events
                 WHERE account_id = ?1 ORDER BY at DESC, id DESC LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![account_id, limit as i64], |row| {
                    Ok(SecurityEventRecord {
                        id: row.get(0)?,
                        at: row.get(1)?,
                        kind: row.get(2)?,
                        actor: row.get(3)?,
                        ip: row.get(4)?,
                        details: serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or(Value::Null),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    // Browser sessions

    /// The id of the session behind a cookie token, as [`Store::web_sessions`] shows it.
    pub fn web_session_id(token: &str) -> String {
        hex::encode(&crate::web::token_hash(token)[..8])
    }

    pub async fn web_sessions(&self, account_id: i64) -> Result<Vec<WebSessionInfo>> {
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT token_hash, created_at, last_seen_at, ip, user_agent FROM web_sessions
                 WHERE account_id = ?1 AND expires_at > ?2 ORDER BY last_seen_at DESC",
            )?;
            let rows = stmt
                .query_map(params![account_id, now()], |row| {
                    let hash: Vec<u8> = row.get(0)?;
                    Ok(WebSessionInfo {
                        id: hex::encode(&hash[..8.min(hash.len())]),
                        created_at: row.get(1)?,
                        last_seen_at: row.get(2)?,
                        ip: row.get(3)?,
                        user_agent: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn end_web_session(&self, account_id: i64, id: &str) -> Result<()> {
        let id = id.to_ascii_lowercase();
        if id.len() != 16 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(StoreError::NotFound("session".into()));
        }
        self.write(move |tx| {
            let removed = tx.execute(
                "DELETE FROM web_sessions WHERE account_id = ?1 AND lower(hex(substr(token_hash, 1, 8))) = ?2",
                params![account_id, id],
            )?;
            if removed == 0 {
                return Err(StoreError::NotFound("session".into()));
            }
            Ok(())
        })
        .await
    }

    /// Logs out every browser except the one with `keep_token`. Returns how many were ended.
    pub async fn end_other_web_sessions(&self, account_id: i64, keep_token: &str) -> Result<usize> {
        let keep = crate::web::token_hash(keep_token);
        self.write(move |tx| {
            Ok(tx.execute(
                "DELETE FROM web_sessions WHERE account_id = ?1 AND token_hash != ?2",
                params![account_id, keep],
            )?)
        })
        .await
    }
}

fn insert_event(conn: &Connection, account_id: i64, event: &SecurityEvent) -> rusqlite::Result<()> {
    let at = now();
    conn.execute(
        "INSERT INTO security_events (account_id, at, kind, actor, ip, details) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![account_id, at, event.kind, event.actor, event.ip, event.details.to_string()],
    )?;
    conn.execute(
        "DELETE FROM security_events WHERE account_id = ?1 AND at < ?2",
        params![account_id, at - EVENT_RETENTION_SECS],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewAccount, Role};

    #[test]
    fn totp_matches_the_rfc_test_vectors() {
        let secret = b"12345678901234567890";
        // RFC 6238 appendix B, SHA-1, last six digits.
        assert_eq!(totp_code(secret, 59 / 30), 287_082);
        assert_eq!(totp_code(secret, 1_111_111_109 / 30), 81_804);
        assert_eq!(totp_code(secret, 2_000_000_000 / 30), 279_037);
        assert_eq!(totp_step(secret, "081 804", 1_111_111_109, 0), Some(1_111_111_109 / 30));
        assert_eq!(totp_step(secret, "081804", 1_111_111_109 + 30, 0), Some(1_111_111_109 / 30), "one step late");
        assert_eq!(totp_step(secret, "081804", 1_111_111_109, 1_111_111_109 / 30), None, "used before");
        assert_eq!(totp_step(secret, "81804", 1_111_111_109, 0), None);
    }

    #[tokio::test]
    async fn a_switched_off_protocol_holds_every_password() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.de").await.unwrap();
        let service = store
            .create_account(NewAccount {
                address: "monitoring@example.de".into(),
                display_name: "Monitoring".into(),
                password: None,
                role: Role::Service,
                quota_bytes: 0,
                protocols: Some(crate::Protocols {
                    smtp: true,
                    imap: false,
                    jmap: false,
                    caldav: false,
                    carddav: false,
                }),
            })
            .await
            .unwrap();
        let created = store
            .create_app_password(
                service.id,
                NewAppPassword {
                    name: "Sender".into(),
                    scopes: vec![AppScope::Smtp, AppScope::Mail],
                    expires_at: None,
                },
            )
            .await
            .unwrap();
        let secret = created.secret.replace(' ', "");

        // Sending is what this one is for.
        let sending = store.authenticate_mail("monitoring@example.de", &secret, AppScope::Smtp, "smtp", "").await;
        assert!(matches!(sending, Ok(MailAuth::Ok { .. })), "{sending:?}");

        // IMAP is off for the account, so the password does not open it, right or not.
        let reading = store.authenticate_mail("monitoring@example.de", &secret, AppScope::Mail, "imap", "").await;
        assert!(matches!(reading, Ok(MailAuth::Denied(MailAuthDenied::ProtocolOff))), "{reading:?}");
        let jmap = store.authenticate_mail("monitoring@example.de", &secret, AppScope::Mail, "jmap", "").await;
        assert!(matches!(jmap, Ok(MailAuth::Denied(MailAuthDenied::ProtocolOff))), "{jmap:?}");
        let dav = store.authenticate_mail("monitoring@example.de", &secret, AppScope::Dav, "dav", "").await;
        assert!(matches!(dav, Ok(MailAuth::Denied(MailAuthDenied::ProtocolOff))), "{dav:?}");

        // And a protocol nobody taught the gate about is not quietly allowed either.
        let unknown = store.authenticate_mail("monitoring@example.de", &secret, AppScope::Mail, "pop3", "").await;
        assert!(matches!(unknown, Ok(MailAuth::Denied(MailAuthDenied::ProtocolOff))), "{unknown:?}");

        // Switching IMAP back on lets the same password in.
        store
            .update_account(
                "monitoring@example.de",
                crate::AccountUpdate {
                    protocols: Some(crate::Protocols { imap: true, ..service.protocols }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let again = store.authenticate_mail("monitoring@example.de", &secret, AppScope::Mail, "imap", "").await;
        assert!(matches!(again, Ok(MailAuth::Ok { .. })), "{again:?}");
    }

    #[tokio::test]
    async fn passwords_and_app_passwords_from_mailcow_keep_working() {
        let (store, _dir) = crate::test_support::store().await;
        store.create_domain("example.de").await.unwrap();
        let new = NewAccount {
            address: "mini@example.de".into(),
            display_name: String::new(),
            password: None,
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        };
        let mini = store.create_account(new).await.unwrap();
        let bcrypt = |secret: &str| {
            let parts = bcrypt::hash_with_result(secret, 4).unwrap();
            format!("{{BLF-CRYPT}}{}", parts.format_for_version(bcrypt::Version::TwoY))
        };
        store.import_password_hash(mini.id, &bcrypt("katzenpfote-123")).await.unwrap();
        assert!(store.import_password_hash(mini.id, "{SHA512-CRYPT}$6$x$y").await.is_err());

        let stored = |store: Store| async move {
            store
                .read(move |conn| {
                    Ok(conn.query_row("SELECT password_hash FROM accounts WHERE id = ?1", [mini.id], |row| {
                        row.get::<_, String>(0)
                    })?)
                })
                .await
                .unwrap()
        };
        assert!(store.authenticate("mini@example.de", "falsch-falsch").await.unwrap().is_none());
        assert!(store.authenticate("mini@example.de", "katzenpfote-123").await.unwrap().is_some());
        assert!(stored(store.clone()).await.starts_with("$argon2"), "replaced by our own hash at the first login");
        let auth = store.authenticate_mail("mini@example.de", "katzenpfote-123", AppScope::Mail, "imap", "").await;
        assert!(matches!(auth.unwrap(), MailAuth::Ok { app_password: None, .. }));

        let phone =
            store.import_app_password(mini.id, "iPhone", &bcrypt("mein-altes-app-pw"), vec![AppScope::Mail]).await;
        let phone = phone.unwrap();
        let again = store.import_app_password(mini.id, "iPhone", &bcrypt("anderes"), vec![AppScope::Smtp]).await;
        assert_eq!(again.unwrap().id, phone.id, "importing twice keeps the first");
        let auth = store.authenticate_mail("mini@example.de", "mein-altes-app-pw", AppScope::Mail, "imap", "").await;
        assert!(matches!(auth.unwrap(), MailAuth::Ok { app_password: Some(id), .. } if id == phone.id));
        let auth = store.authenticate_mail("mini@example.de", "mein-altes-app-pw", AppScope::Smtp, "smtp", "").await;
        assert!(matches!(auth.unwrap(), MailAuth::Denied(MailAuthDenied::WrongScope)));
    }

    #[test]
    fn codes_are_readable_and_forgiving() {
        let code = random_code(16);
        assert_eq!(code.len(), 16);
        assert!(code.bytes().all(|b| ALPHABET.contains(&b)));
        let shown = grouped(&code, 4);
        assert_eq!(shown.len(), 19);
        assert_eq!(candidate("app", &shown.to_uppercase(), 16), Some(code_hash("app", &code)));
        assert_eq!(candidate("app", "not-an-app-password", 16), None);
        assert_eq!(percent_encode("UwUMail (mail.example.de)"), "UwUMail%20%28mail.example.de%29");
    }

    async fn person(store: &Store) -> Account {
        store.create_domain("example.de").await.unwrap();
        store
            .create_account(NewAccount {
                address: "leni@example.de".into(),
                display_name: "Leni".into(),
                password: Some("Seifenblase-Wanderweg-17".into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn app_passwords_and_second_factors_decide_how_mail_apps_log_in() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let leni = person(&store).await;
        let ok = |auth: &MailAuth| matches!(auth, MailAuth::Ok { .. });
        let denied = |auth: MailAuth| match auth {
            MailAuth::Denied(reason) => Some(reason),
            MailAuth::Ok { .. } => None,
        };

        let main =
            store.authenticate_mail("leni@example.de", "Seifenblase-Wanderweg-17", AppScope::Smtp, "smtp", "").await;
        assert!(ok(&main.unwrap()), "without anything set up the main password works");

        let phone = store
            .create_app_password(
                leni.id,
                NewAppPassword { name: "Handy".into(), scopes: vec![AppScope::Mail, AppScope::Smtp], expires_at: None },
            )
            .await
            .unwrap();
        let printer = store
            .create_app_password(
                leni.id,
                NewAppPassword { name: "Drucker".into(), scopes: vec![AppScope::Smtp], expires_at: Some(now() + 60) },
            )
            .await
            .unwrap();
        let auth = store.authenticate_mail("leni@example.de", &phone.secret, AppScope::Mail, "jmap", "192.0.2.7").await;
        assert!(ok(&auth.unwrap()));
        let listed = store.app_passwords(leni.id).await.unwrap();
        let phone_row = listed.iter().find(|p| p.id == phone.app_password.id).unwrap();
        assert_eq!(phone_row.last_used_protocol.as_deref(), Some("jmap"));
        assert_eq!(phone_row.last_used_ip.as_deref(), Some("192.0.2.7"));
        let auth = store.authenticate_mail("leni@example.de", &printer.secret, AppScope::Mail, "jmap", "").await;
        assert_eq!(denied(auth.unwrap()), Some(MailAuthDenied::WrongScope));

        // An authenticator app makes the main password useless for mail apps.
        let setup = store.begin_totp(leni.id, "UwUMail", "leni@example.de").await.unwrap();
        assert!(setup.uri.starts_with("otpauth://totp/UwUMail:leni%40example.de?secret="));
        assert!(store.confirm_totp(leni.id, "abcdef").await.is_err());
        let secret = BASE32_NOPAD.decode(setup.secret.as_bytes()).unwrap();
        let code = format!("{:06}", totp_code(&secret, now() / TOTP_PERIOD));
        let codes = store.confirm_totp(leni.id, &code).await.unwrap().expect("first factor brings recovery codes");
        assert_eq!(codes.len(), 10);
        let auth = store.authenticate_mail("leni@example.de", "Seifenblase-Wanderweg-17", AppScope::Smtp, "smtp", "");
        assert_eq!(denied(auth.await.unwrap()), Some(MailAuthDenied::AppPasswordRequired));
        let events = store.security_events(leni.id, 10).await.unwrap();
        assert_eq!(events[0].kind, "mainPasswordRefused");
        let auth = store.authenticate_mail("leni@example.de", &phone.secret, AppScope::Smtp, "smtp", "").await;
        assert!(ok(&auth.unwrap()), "app passwords keep working");

        // Codes at login: the same TOTP code is spent, a recovery code works once.
        assert_eq!(store.check_second_factor_code(leni.id, &code).await.unwrap(), CodeCheck::Invalid);
        assert_eq!(
            store.check_second_factor_code(leni.id, &codes[3]).await.unwrap(),
            CodeCheck::RecoveryCode { left: 9 }
        );
        assert_eq!(store.check_second_factor_code(leni.id, &codes[3]).await.unwrap(), CodeCheck::Invalid);

        let overview = store.security_overview(leni.id).await.unwrap();
        assert!(overview.totp && overview.second_factor && overview.app_passwords_required());
        assert_eq!(overview.recovery_codes_left, 9);

        store.revoke_app_password(leni.id, phone.app_password.id).await.unwrap();
        let auth = store.authenticate_mail("leni@example.de", &phone.secret, AppScope::Smtp, "smtp", "").await;
        assert_eq!(denied(auth.unwrap()), Some(MailAuthDenied::Invalid));

        store.disable_totp(leni.id).await.unwrap();
        let overview = store.security_overview(leni.id).await.unwrap();
        assert!(!overview.second_factor && overview.recovery_codes_left == 0);
        store.set_apps_need_app_password(leni.id, true).await.unwrap();
        let auth = store.authenticate_mail("leni@example.de", "Seifenblase-Wanderweg-17", AppScope::Mail, "jmap", "");
        assert_eq!(denied(auth.await.unwrap()), Some(MailAuthDenied::AppPasswordRequired));
        let changed = store.account_by_id(leni.id).await.unwrap().unwrap().credentials_changed_at;
        assert!(changed > 0);
    }

    #[tokio::test]
    async fn changing_the_password_logs_out_other_browsers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let leni = person(&store).await;
        let here = store.create_web_session(leni.id, 3600, "192.0.2.1", "Firefox").await.unwrap();
        let there = store.create_web_session(leni.id, 3600, "192.0.2.2", "Safari").await.unwrap();
        assert_eq!(store.web_sessions(leni.id).await.unwrap().len(), 2);

        let wrong = store.change_password(leni.id, "falsch-falsch", "Kirschbluete-Tastatur-42", &here.token).await;
        assert!(matches!(wrong, Err(StoreError::Rule { code: "wrongPassword", .. })));
        store
            .change_password(leni.id, "Seifenblase-Wanderweg-17", "Kirschbluete-Tastatur-42", &here.token)
            .await
            .unwrap();
        let sessions = store.web_sessions(leni.id).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, Store::web_session_id(&here.token));
        assert!(store.web_session(&there.token, 3600).await.unwrap().is_none());
        assert!(store.authenticate("leni@example.de", "Kirschbluete-Tastatur-42").await.unwrap().is_some());

        store.end_web_session(leni.id, &Store::web_session_id(&here.token)).await.unwrap();
        assert!(store.web_sessions(leni.id).await.unwrap().is_empty());
    }
}
