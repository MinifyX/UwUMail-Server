//! "Fetched mailboxes" in My account: mailboxes at other providers that this server empties into
//! the person's own mailbox.
//!
//! The password belongs to the provider, not to us. It goes in and never comes out again: no
//! endpoint here returns it, and an update that leaves it out keeps the one that is stored.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::{AfterFetch, FetchAccountUpdate, FetchSecurity, NewFetchAccount, SendSecurity};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Session;

/// The usual IMAP port with TLS from the first byte.
const DEFAULT_PORT: u16 = 993;

fn security_of(value: Option<&str>) -> ApiResult<FetchSecurity> {
    match value {
        None => Ok(FetchSecurity::Tls),
        Some(value) => FetchSecurity::parse(value)
            .ok_or_else(|| ApiError::Rule("badSecurity", format!("'{value}' is not a way to connect"))),
    }
}

/// What to do at the provider, as the page spells it -- the same words the API sends back.
fn after_of(value: Option<&str>) -> ApiResult<AfterFetch> {
    match value {
        None | Some("markRead") => Ok(AfterFetch::MarkRead),
        Some("delete") => Ok(AfterFetch::Delete),
        Some(value) => Err(ApiError::Rule("badAfterFetch", format!("'{value}' is not something to do"))),
    }
}

/// Everything the page shows, plus what it needs to keep its own limits.
pub async fn list(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    let accounts = web.store().fetch_accounts(Some(session.account.id)).await?;
    Ok(Json(json!({
        "accounts": accounts,
        "max": uwumail_store::MAX_FETCH_ACCOUNTS,
        "defaultPort": DEFAULT_PORT,
        "defaultIntervalSecs": uwumail_store::DEFAULT_FETCH_INTERVAL_SECS,
        "minIntervalSecs": uwumail_store::MIN_FETCH_INTERVAL_SECS,
        "maxIntervalSecs": uwumail_store::MAX_FETCH_INTERVAL_SECS,
    })))
}

#[derive(Deserialize)]
pub struct Unknown {
    address: String,
    password: String,
}

/// Works out how a provider's mailbox is reached, from the address and the password alone, so
/// nobody has to know what their provider calls its servers.
///
/// Nothing here is guessed at the person: the settings come back only once a login has really
/// worked with them, so the page can fill its fields with something that has been tried rather
/// than with something that sounded likely. What did not work comes back as a code the page turns
/// into a sentence -- which of the four sources answered is not the person's business, and saying
/// "your password is wrong" only when the server said so keeps the two apart.
pub async fn discover(
    State(web): State<Web>,
    session: Session,
    Json(unknown): Json<Unknown>,
) -> ApiResult<Json<Value>> {
    // Every mailbox this person has already set up counts, so the discovery cannot be used to knock
    // on providers' doors past the limit that holds for keeping one.
    let held = web.store().fetch_accounts(Some(session.account.id)).await?.len();
    if held >= uwumail_store::MAX_FETCH_ACCOUNTS {
        return Err(ApiError::Rule(
            "fetchLimit",
            format!("at most {} fetched mailboxes", uwumail_store::MAX_FETCH_ACCOUNTS),
        ));
    }
    let found =
        uwumail_smtp::autoconfig::discover(web.smtp(), web.dns(), &unknown.address, &unknown.password, true).await;
    match found {
        Ok(settings) => Ok(Json(json!(settings))),
        Err(code) => Err(match code.as_str() {
            "wrongPassword" => ApiError::Rule("wrongPassword", "the provider refused this password".into()),
            "notAnAddress" => ApiError::Rule("senderInvalid", format!("'{}' is not an address", unknown.address)),
            _ => ApiError::Rule("providerNotFound", "no settings of this provider answered".into()),
        }),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMailbox {
    address: String,
    host: String,
    port: Option<u16>,
    security: Option<String>,
    /// Usually the address itself; some providers want something else.
    username: Option<String>,
    password: String,
    after_fetch: Option<String>,
    fetch_junk: Option<bool>,
    interval_secs: Option<i64>,
    auth_serv_id: Option<String>,
    /// Whether the mail that is already in the mailbox comes too, not only what arrives from now on.
    take_existing: Option<bool>,
}

pub async fn create(
    State(web): State<Web>,
    session: Session,
    Json(new): Json<NewMailbox>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let username = new.username.unwrap_or_else(|| new.address.clone());
    let created = web
        .store()
        .create_fetch_account(NewFetchAccount {
            account_id: session.account.id,
            address: new.address,
            host: new.host,
            port: new.port.unwrap_or(DEFAULT_PORT),
            security: security_of(new.security.as_deref())?,
            username,
            password: new.password,
            after_fetch: after_of(new.after_fetch.as_deref())?,
            fetch_junk: new.fetch_junk.unwrap_or(true),
            interval_secs: new.interval_secs.unwrap_or(uwumail_store::DEFAULT_FETCH_INTERVAL_SECS),
            auth_serv_id: new.auth_serv_id.unwrap_or_default(),
        })
        .await?;
    if new.take_existing == Some(true) {
        web.store().request_fetch_backlog(session.account.id, created.id).await?;
        let asked = web.store().fetch_account(session.account.id, created.id).await?.unwrap_or(created);
        return Ok((StatusCode::CREATED, Json(json!(asked))));
    }
    Ok((StatusCode::CREATED, Json(json!(created))))
}

/// Brings over the mail that was already in the mailbox, for one that was set up without it. The
/// next runs work through it next to the new mail; what is already here is not brought twice.
pub async fn take_existing(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    web.store().request_fetch_backlog(session.account.id, id).await?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    host: Option<String>,
    port: Option<u16>,
    security: Option<String>,
    username: Option<String>,
    /// Left out to keep the password that is stored.
    password: Option<String>,
    after_fetch: Option<String>,
    fetch_junk: Option<bool>,
    interval_secs: Option<i64>,
    enabled: Option<bool>,
    auth_serv_id: Option<String>,
    /// Where the provider takes outgoing mail, for answering from this address.
    smtp_host: Option<String>,
    smtp_port: Option<u16>,
    smtp_security: Option<String>,
    send_enabled: Option<bool>,
}

/// How the provider's outgoing server is reached. Never unencrypted.
fn sending_of(value: Option<&str>) -> ApiResult<SendSecurity> {
    match value {
        None | Some("starttls") => Ok(SendSecurity::Starttls),
        Some("tls") => Ok(SendSecurity::Tls),
        Some(value) => Err(ApiError::Rule("badSecurity", format!("'{value}' is not a way to connect"))),
    }
}

// Whose mailbox it is goes into every call below, so the id from the URL can only ever reach one
// of the caller's own: the store asks for both and finds nothing otherwise.
pub async fn update(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
    Json(changes): Json<Changes>,
) -> ApiResult<Json<Value>> {
    let update = FetchAccountUpdate {
        host: changes.host,
        port: changes.port,
        security: changes.security.as_deref().map(|value| security_of(Some(value))).transpose()?,
        username: changes.username,
        password: changes.password,
        after_fetch: changes.after_fetch.as_deref().map(|value| after_of(Some(value))).transpose()?,
        fetch_junk: changes.fetch_junk,
        interval_secs: changes.interval_secs,
        enabled: changes.enabled,
        auth_serv_id: changes.auth_serv_id,
        smtp_host: changes.smtp_host,
        smtp_port: changes.smtp_port,
        smtp_security: changes.smtp_security.as_deref().map(|value| sending_of(Some(value))).transpose()?,
        send_enabled: changes.send_enabled,
    };
    Ok(Json(json!(web.store().update_fetch_account(session.account.id, id, update).await?)))
}

pub async fn delete(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    web.store().delete_fetch_account(session.account.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Fetches now instead of waiting for the interval. The run itself happens in the background, so
/// this only moves it to the front of the queue; the page shows what came of it afterwards.
pub async fn fetch_now(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    web.store().fetch_account_due_now(session.account.id, id).await?;
    Ok(StatusCode::ACCEPTED)
}
