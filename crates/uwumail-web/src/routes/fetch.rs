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
use uwumail_store::{AfterFetch, FetchAccountUpdate, FetchSecurity, NewFetchAccount};

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
    Ok((StatusCode::CREATED, Json(json!(created))))
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
