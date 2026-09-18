//! Backups in the admin panel: where they go, how they log in, when they run, and what is there.
//! Secrets stay on the server: the panel sees the public SSH key, whether a password is set, and the
//! recovery key only right after it was made or after confirming with one's password.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_backup::{Backups, Login, RepoKey, Retention, Target};

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::routes::security::confirm_identity;
use crate::session::Admin;

fn backups(web: &Web) -> ApiResult<&Backups> {
    web.backups().ok_or_else(|| ApiError::NotFound("backups on this server".into()))
}

pub(crate) fn api_error(err: uwumail_backup::Error) -> ApiError {
    use uwumail_backup::Error;
    match err {
        Error::WrongKey => ApiError::Rule("backupWrongKey", err.to_string()),
        Error::HostKeyChanged { .. } => ApiError::Rule("backupHostKeyChanged", err.to_string()),
        Error::LoginRefused(_) => ApiError::Rule("backupLoginRefused", err.to_string()),
        Error::Storage(_) | Error::Damaged(_) => ApiError::Rule("backupFailed", err.to_string()),
        Error::Config(detail) => ApiError::Invalid(detail),
        Error::Store(err) => err.into(),
        Error::Io(_) | Error::Crypto => {
            tracing::error!(%err, "backup request failed");
            ApiError::Internal
        }
    }
}

async fn view(web: &Web) -> ApiResult<Value> {
    let backups = backups(web)?;
    let settings = backups.settings().await.map_err(api_error)?;
    let target = settings.target.as_ref().map(|target| {
        let (method, public_key, password_set) = match &target.login {
            Login::Key { private_key } => ("key", uwumail_backup::sftp::public_key_line(private_key).ok(), false),
            Login::Password { password } => ("password", None, !password.is_empty()),
        };
        json!({
            "host": target.host,
            "port": target.port,
            "user": target.user,
            "path": target.path,
            "method": method,
            "publicKey": public_key,
            "passwordSet": password_set,
            "hostKey": target.host_key,
        })
    });
    Ok(json!({
        "enabled": settings.enabled,
        "hour": settings.hour,
        "minute": settings.minute,
        "retention": settings.retention,
        "encrypted": settings.key.is_some(),
        "target": target,
        "status": backups.status().await,
        "running": backups.is_running(),
        "restore": restore_view(web, backups).await,
    }))
}

/// Everything about putting a backup back: whether this server can, what is going on right now, and
/// how the last one went.
async fn restore_view(web: &Web, backups: &Backups) -> Value {
    // Written by the server before it opened this database, in the start after the restore. It is
    // the only place the answer can come from: the process that asked for it is long gone.
    let last: Option<Value> =
        web.store().setting(RESTORE_STATUS_KEY).await.ok().flatten().and_then(|raw| serde_json::from_str(&raw).ok());
    json!({
        "available": backups.can_restore(),
        "fetching": backups.fetching(),
        "staged": backups.staged(),
        "last": last,
    })
}

/// Where the server writes down how the restore it carried out at start-up went.
const RESTORE_STATUS_KEY: &str = "restore.status";

pub async fn show(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(view(&web).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetBody {
    host: String,
    port: u16,
    user: String,
    path: String,
    /// "key" or "password".
    method: String,
    /// A new password; left out keeps the stored one.
    password: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsBody {
    enabled: bool,
    hour: u8,
    /// Left out by an older app, which only knew full hours.
    #[serde(default)]
    minute: u8,
    retention: Retention,
    encrypted: bool,
    target: Option<TargetBody>,
}

pub async fn save(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(body): Json<SettingsBody>,
) -> ApiResult<Json<Value>> {
    let backups = backups(&web)?;
    let mut settings = backups.settings().await.map_err(api_error)?;
    let backed_up = backups.status().await.last_success_at.is_some();

    let mut recovery_key = None;
    if body.encrypted != settings.key.is_some() {
        if backed_up {
            return Err(ApiError::Rule(
                "backupEncryptionFixed",
                "encryption cannot change once there are backups; use a new directory for that".into(),
            ));
        }
        settings.key = body.encrypted.then(|| RepoKey::generate().recovery_text());
        recovery_key = settings.key.clone();
    }

    settings.target = match body.target {
        None => None,
        Some(new) => {
            let (host, user, path) =
                (new.host.trim().to_owned(), new.user.trim().to_owned(), new.path.trim().to_owned());
            if host.is_empty() || user.is_empty() || new.port == 0 {
                return Err(ApiError::Invalid("the backup server needs a host, a port and a user".into()));
            }
            let old = settings.target.take();
            let same_server = old.as_ref().is_some_and(|old| old.host == host && old.port == new.port);
            let login = match (new.method.as_str(), old.as_ref().map(|old| &old.login)) {
                ("key", Some(Login::Key { private_key })) => Login::Key { private_key: private_key.clone() },
                ("key", _) => {
                    let comment = format!("uwumail-backup@{}", web.settings().hostname);
                    let (private_key, _) = uwumail_backup::sftp::generate_key(&comment).map_err(api_error)?;
                    Login::Key { private_key }
                }
                ("password", old_login) => match (new.password.filter(|password| !password.is_empty()), old_login) {
                    (Some(password), _) => Login::Password { password },
                    (None, Some(Login::Password { password })) => Login::Password { password: password.clone() },
                    (None, _) => return Err(ApiError::Invalid("a password is needed".into())),
                },
                _ => return Err(ApiError::Invalid("the login method is key or password".into())),
            };
            Some(Target {
                host,
                port: new.port,
                user,
                path,
                login,
                host_key: old.filter(|_| same_server).and_then(|old| old.host_key),
            })
        }
    };
    settings.enabled = body.enabled && settings.target.is_some();
    settings.hour = body.hour;
    settings.minute = body.minute;
    settings.retention = body.retention;
    backups.save_settings(&settings).await.map_err(api_error)?;

    let details = json!({
        "enabled": settings.enabled,
        "host": settings.target.as_ref().map(|target| &target.host),
        "encrypted": settings.key.is_some(),
    });
    audit(&web, &session, "backup.settings", "server", details).await;
    let mut value = view(&web).await?;
    if let Some(key) = recovery_key {
        value["recoveryKey"] = json!(key);
    }
    Ok(Json(value))
}

/// Connects once: shows the host key the first time and whether logging in works.
pub async fn test(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let backups = backups(&web)?;
    let settings = backups.settings().await.map_err(api_error)?;
    let target = settings.target.ok_or_else(|| ApiError::Invalid("no backup server is set up".into()))?;
    let connection = uwumail_backup::sftp::Sftp::connect(&target).await.map_err(api_error)?;
    let host_key = connection.host_key.clone();
    connection.close().await;
    // Like ssh on the first connection: the admin sees the key and it is remembered from now on.
    let known = target.host_key.is_some();
    if !known {
        let mut settings = backups.settings().await.map_err(api_error)?;
        if let Some(saved) = settings.target.as_mut() {
            saved.host_key = Some(host_key.clone());
        }
        backups.save_settings(&settings).await.map_err(api_error)?;
    }
    Ok(Json(json!({ "hostKey": host_key, "known": known })))
}

/// Trusts the host key the backup server shows now, after it changed on purpose.
pub async fn forget_host_key(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<Value>> {
    let backups = backups(&web)?;
    let mut settings = backups.settings().await.map_err(api_error)?;
    if let Some(target) = settings.target.as_mut() {
        target.host_key = None;
    }
    backups.save_settings(&settings).await.map_err(api_error)?;
    audit(&web, &session, "backup.forgetHostKey", "server", json!({})).await;
    Ok(Json(view(&web).await?))
}

pub async fn run(State(web): State<Web>, Admin(session): Admin) -> ApiResult<StatusCode> {
    let backups = backups(&web)?;
    if backups.settings().await.map_err(api_error)?.target.is_none() {
        return Err(ApiError::Invalid("no backup server is set up".into()));
    }
    backups.run_soon();
    audit(&web, &session, "backup.run", "server", json!({})).await;
    Ok(StatusCode::ACCEPTED)
}

pub async fn snapshots(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let list = backups(&web)?.snapshots().await.map_err(api_error)?;
    Ok(Json(Value::Array(
        list.into_iter()
            .map(|(name, manifest)| {
                json!({
                    "name": name,
                    "createdAt": manifest.created_at,
                    "mails": manifest.blobs.len(),
                    "size": manifest.database_size + manifest.blobs_size,
                    "uploaded": manifest.uploaded,
                    "version": manifest.version,
                })
            })
            .collect(),
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreBody {
    /// The snapshot to put back, or `latest`.
    snapshot: String,
    /// Keep the gateway this machine is paired with instead of the snapshot's. On by default,
    /// because the usual reason to restore is that this machine stands in for one that died.
    #[serde(default = "yes")]
    keep_gateway: bool,
    #[serde(default)]
    password: Option<String>,
}

fn yes() -> bool {
    true
}

/// Puts a backup back over everything this server has.
///
/// This fetches the snapshot and then stops the server; the next start puts the files in place,
/// because that is the only moment the database is nobody's. Docker brings the container back by
/// itself. The answer comes as soon as the fetching has started, and the portal follows it — until
/// the server goes away under it, which is the point.
pub async fn restore(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(body): Json<RestoreBody>,
) -> ApiResult<Json<Value>> {
    let backups = backups(&web)?;
    // Everything on this server is about to be replaced by what was on another one. Of all the
    // things the portal can do, this is the one that most deserves the password again.
    confirm_identity(&web, &session, body.password.as_deref()).await?;
    audit(&web, &session, "backups.restore", &body.snapshot, json!({ "keepGateway": body.keep_gateway })).await;
    backups.start_restore(&body.snapshot, body.keep_gateway, &session.account.login).await.map_err(api_error)?;
    Ok(Json(view(&web).await?))
}

/// Puts the note about the last restore away, once it has been read.
pub async fn forget_restore(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let _ = web.store().delete_setting(RESTORE_STATUS_KEY).await;
    Ok(Json(view(&web).await?))
}

#[derive(Deserialize)]
pub struct Confirmation {
    password: Option<String>,
}

/// The recovery key again, after confirming with one's password.
pub async fn recovery_key(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(body): Json<Confirmation>,
) -> ApiResult<Json<Value>> {
    confirm_identity(&web, &session, body.password.as_deref()).await?;
    let key = backups(&web)?.settings().await.map_err(api_error)?.key;
    let key = key.ok_or_else(|| ApiError::NotFound("a recovery key; backups are not encrypted".into()))?;
    audit(&web, &session, "backup.showRecoveryKey", "server", json!({})).await;
    Ok(Json(json!({ "recoveryKey": key })))
}
