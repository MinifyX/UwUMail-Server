//! "Moving" in My account: bringing one's mail over from another provider.
//!
//! The person gives the old address and its password; the server finds the provider's IMAP
//! server the way fetched mailboxes do (and logs in once to be sure), then keeps a job the worker
//! in the server crate copies in the background. The page follows it by asking again every few
//! seconds. The password goes in and never comes out: nothing here returns it, and "done" removes
//! it with the job.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_store::NewMigrationJob;

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Session;

/// The usual IMAP port with TLS from the first byte; the only way the mover connects.
const DEFAULT_PORT: u16 = 993;
/// Starting a move or going on with one logs in at the old provider. A few an hour are plenty,
/// and they must not become a way to try passwords there.
const CALLS_PER_HOUR: usize = 10;

fn polite(web: &Web, session: &Session) -> ApiResult<()> {
    if web.allow_remote_call(session.account.id, CALLS_PER_HOUR) { Ok(()) } else { Err(ApiError::TooManyAttempts) }
}

pub async fn list(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    let jobs = web.store().migration_jobs(session.account.id).await?;
    Ok(Json(json!({
        "jobs": jobs,
        "max": uwumail_store::MAX_MIGRATION_JOBS,
        "hasMailbox": session.account.has_mailbox(),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMove {
    address: String,
    password: String,
    /// The IMAP server, for a provider that cannot be found by the address. Found when left out.
    host: Option<String>,
    port: Option<u16>,
    /// Usually the address itself.
    login: Option<String>,
}

/// Starts a move: finds the old provider's server (or takes the one given), then queues the job.
pub async fn create(
    State(web): State<Web>,
    session: Session,
    Json(new): Json<NewMove>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if !session.account.has_mailbox() {
        return Err(ApiError::Rule("moveNoMailbox", "this account has no mailbox to move mail into".into()));
    }
    let address = new.address.trim().to_owned();
    if new.password.is_empty() {
        return Err(ApiError::Invalid("the password of the old mailbox is missing".into()));
    }
    let held = web.store().migration_jobs(session.account.id).await?.len();
    if held >= uwumail_store::MAX_MIGRATION_JOBS {
        return Err(ApiError::Rule(
            "moveLimit",
            format!("at most {} moves at once", uwumail_store::MAX_MIGRATION_JOBS),
        ));
    }
    polite(&web, &session)?;
    let (host, port, login) = match new.host.map(|host| host.trim().to_owned()).filter(|host| !host.is_empty()) {
        Some(host) => {
            let login = new.login.map(|login| login.trim().to_owned()).filter(|login| !login.is_empty());
            (host, new.port.unwrap_or(DEFAULT_PORT), login.unwrap_or_else(|| address.clone()))
        }
        None => {
            // Found and tried for real: what is kept is what a login answered to.
            let found = uwumail_smtp::autoconfig::discover(web.smtp(), web.dns(), &address, &new.password, true).await;
            match found {
                Ok(settings) => (settings.imap.host.clone(), settings.imap.port, settings.imap.login.of(&address)),
                Err(code) => {
                    return Err(match code.as_str() {
                        "wrongPassword" => {
                            ApiError::Rule("moveWrongPassword", "the old provider refused this password".into())
                        }
                        "notAnAddress" => ApiError::Rule("senderInvalid", format!("'{address}' is not an address")),
                        _ => ApiError::Rule("providerNotFound", "no settings of this provider answered".into()),
                    });
                }
            }
        }
    };
    let job = web
        .store()
        .create_migration_job(NewMigrationJob {
            account_id: session.account.id,
            address,
            host,
            port,
            login,
            password: new.password,
        })
        .await?;
    tracing::info!(account = %session.account.login, from = %job.host, "a move from another provider started");
    Ok((StatusCode::CREATED, Json(json!(job))))
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Again {
    /// A new password, after the old provider refused the stored one.
    password: Option<String>,
}

/// Queues a move again: after it was done, for what arrived at the old provider since; after a
/// pause, to go on where it stopped.
pub async fn sync(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
    Json(again): Json<Again>,
) -> ApiResult<Json<Value>> {
    web.store().migration_job(session.account.id, id).await?.ok_or_else(|| ApiError::NotFound("move".into()))?;
    polite(&web, &session)?;
    Ok(Json(json!(web.store().sync_migration_job(session.account.id, id, again.password).await?)))
}

/// Stops a move until the person goes on with it.
pub async fn pause(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    Ok(Json(json!(web.store().pause_migration_job(session.account.id, id).await?)))
}

/// Done: the job and the password for the old mailbox go. The mail that came stays.
pub async fn finish(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    web.store().delete_migration_job(session.account.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
