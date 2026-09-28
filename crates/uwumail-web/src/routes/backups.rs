//! Backups in the admin panel: where they go (an SFTP server, an S3 bucket or a mounted folder), how
//! they log in, when they run, and what is there. Secrets stay on the server: the panel sees the
//! public SSH key, whether a password or an S3 secret key is set, and the recovery key only right
//! after it was made or after confirming with one's password.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_backup::{Backups, FolderTarget, Login, RepoKey, Retention, S3Target, SftpTarget, Storage, Target};

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
        Error::Busy(detail) => ApiError::Rule("backupBusy", detail),
        Error::Store(err) => err.into(),
        // A folder that is mounted read-only, or belongs to someone else: the admin can fix that.
        Error::Io(_) => {
            tracing::warn!(%err, "backup request failed");
            ApiError::Rule("backupFailed", err.to_string())
        }
        Error::Crypto => {
            tracing::error!(%err, "backup request failed");
            ApiError::Internal
        }
    }
}

/// What the panel may see of a target: everything but the secrets, and whether they are there.
fn target_view(target: &Target) -> Value {
    match target {
        Target::Sftp(target) => {
            let (method, public_key, password_set) = match &target.login {
                Login::Key { private_key } => ("key", uwumail_backup::sftp::public_key_line(private_key).ok(), false),
                Login::Password { password } => ("password", None, !password.is_empty()),
            };
            json!({
                "kind": "sftp",
                "host": target.host,
                "port": target.port,
                "user": target.user,
                "path": target.path,
                "method": method,
                "publicKey": public_key,
                "passwordSet": password_set,
                "hostKey": target.host_key,
            })
        }
        Target::S3(target) => json!({
            "kind": "s3",
            "endpoint": target.endpoint,
            "region": target.region,
            "bucket": target.bucket,
            "prefix": target.prefix,
            "accessKey": target.access_key,
            "secretKeySet": !target.secret_key.is_empty(),
            "pathStyle": target.path_style,
        }),
        Target::Folder(target) => json!({ "kind": "folder", "path": target.path }),
    }
}

async fn view(web: &Web) -> ApiResult<Value> {
    let backups = backups(web)?;
    let settings = backups.settings().await.map_err(api_error)?;
    let target = settings.target.as_ref().map(target_view);
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
        "mailboxRestore": backups.mailbox_restore(),
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

/// Where backups go, as the panel sends it. Which fields count depends on `kind`; an older app
/// sends no kind and means SFTP.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TargetBody {
    /// `sftp`, `s3` or `folder`.
    kind: String,
    host: String,
    port: u16,
    user: String,
    /// The directory on the SFTP server, or the folder on this machine.
    path: String,
    /// "key" or "password".
    method: String,
    /// A new password; left out keeps the stored one.
    password: Option<String>,
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
    access_key: String,
    /// A new secret key; left out keeps the stored one for the same access key.
    secret_key: Option<String>,
    path_style: bool,
}

impl Default for TargetBody {
    fn default() -> Self {
        TargetBody {
            kind: "sftp".into(),
            host: String::new(),
            port: 22,
            user: String::new(),
            path: String::new(),
            method: String::new(),
            password: None,
            endpoint: String::new(),
            region: String::new(),
            bucket: String::new(),
            prefix: String::new(),
            access_key: String::new(),
            secret_key: None,
            path_style: false,
        }
    }
}

/// The target a body describes, taking the secrets it leaves out from the one saved before.
fn target_of(web: &Web, new: TargetBody, old: Option<Target>) -> ApiResult<Target> {
    match new.kind.as_str() {
        "sftp" => {
            let (host, user, path) =
                (new.host.trim().to_owned(), new.user.trim().to_owned(), new.path.trim().to_owned());
            if host.is_empty() || user.is_empty() || new.port == 0 {
                return Err(ApiError::Invalid("the backup server needs a host, a port and a user".into()));
            }
            let old = match old {
                Some(Target::Sftp(old)) => Some(old),
                _ => None,
            };
            let same_server = old.as_ref().is_some_and(|old| old.host == host && old.port == new.port);
            // The stored password is only ever sent to the server and user it was given for:
            // another host would be handed it in the SSH login (security-audit-0.16.0 PLAT-8).
            let same_login = same_server && old.as_ref().is_some_and(|old| old.user == user);
            let login = match (new.method.as_str(), old.as_ref().map(|old| &old.login)) {
                ("key", Some(Login::Key { private_key })) => Login::Key { private_key: private_key.clone() },
                ("key", _) => {
                    let comment = format!("uwumail-backup@{}", web.settings().hostname);
                    let (private_key, _) = uwumail_backup::sftp::generate_key(&comment).map_err(api_error)?;
                    Login::Key { private_key }
                }
                ("password", old_login) => match (new.password.filter(|password| !password.is_empty()), old_login) {
                    (Some(password), _) => Login::Password { password },
                    (None, Some(Login::Password { password })) if same_login => {
                        Login::Password { password: password.clone() }
                    }
                    (None, Some(Login::Password { .. })) => {
                        return Err(ApiError::Invalid(
                            "the password is needed again for another server or user".into(),
                        ));
                    }
                    (None, _) => return Err(ApiError::Invalid("a password is needed".into())),
                },
                _ => return Err(ApiError::Invalid("the login method is key or password".into())),
            };
            Ok(Target::Sftp(SftpTarget {
                host,
                port: new.port,
                user,
                path,
                login,
                host_key: old.filter(|_| same_server).and_then(|old| old.host_key),
            }))
        }
        "s3" => {
            let access_key = new.access_key.trim().to_owned();
            let secret_key = match (new.secret_key.filter(|secret| !secret.is_empty()), old) {
                (Some(secret), _) => secret,
                (None, Some(Target::S3(old))) if old.access_key == access_key => old.secret_key,
                (None, _) => return Err(ApiError::Invalid("the secret key is needed".into())),
            };
            let region = match new.region.trim() {
                "" => "us-east-1".to_owned(),
                region => region.to_owned(),
            };
            let target = S3Target {
                endpoint: new.endpoint.trim().trim_end_matches('/').to_owned(),
                region,
                bucket: new.bucket.trim().to_owned(),
                prefix: new.prefix.trim().trim_matches('/').to_owned(),
                access_key,
                secret_key,
                path_style: new.path_style,
            };
            // Checks the address, the bucket name and the keys without sending anything.
            uwumail_backup::s3::S3::new(&target).map_err(api_error)?;
            Ok(Target::S3(target))
        }
        "folder" => {
            let path = new.path.trim().trim_end_matches('/').to_owned();
            let dir = std::path::Path::new(&path);
            if !dir.is_absolute() || dir.components().any(|part| matches!(part, std::path::Component::ParentDir)) {
                return Err(ApiError::Invalid(format!("'{path}' is not a full path like /backup")));
            }
            // A backup inside the data directory would be backed up into itself, and lost with it.
            let data = web.store().data_dir();
            if dir.starts_with(data) || data.starts_with(dir) {
                return Err(ApiError::Rule(
                    "backupFolderInData",
                    "the backup folder must be outside the server's data directory".into(),
                ));
            }
            Ok(Target::Folder(FolderTarget { path }))
        }
        other => Err(ApiError::Invalid(format!("unknown kind of backup target: {other}"))),
    }
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
        Some(new) => Some(target_of(&web, new, settings.target.take())?),
    };
    settings.enabled = body.enabled && settings.target.is_some();
    settings.hour = body.hour;
    settings.minute = body.minute;
    settings.retention = body.retention;
    backups.save_settings(&settings).await.map_err(api_error)?;

    let details = json!({
        "enabled": settings.enabled,
        "kind": settings.target.as_ref().map(Target::kind),
        "target": settings.target.as_ref().map(Target::shown),
        "encrypted": settings.key.is_some(),
    });
    audit(&web, &session, "backup.settings", "server", details).await;
    let mut value = view(&web).await?;
    if let Some(key) = recovery_key {
        value["recoveryKey"] = json!(key);
    }
    Ok(Json(value))
}

/// Connects once. For SFTP: shows the host key the first time and whether logging in works. For S3
/// and a folder: whether a file can be written, read back and removed there.
pub async fn test(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let backups = backups(&web)?;
    let settings = backups.settings().await.map_err(api_error)?;
    let target = settings.target.ok_or_else(|| ApiError::Invalid("no backup server is set up".into()))?;
    let storage = Storage::open(&target).await.map_err(api_error)?;
    let Target::Sftp(sftp) = &target else {
        let written = storage.check_writable().await;
        storage.close().await;
        written.map_err(api_error)?;
        return Ok(Json(json!({ "kind": target.kind(), "hostKey": null, "known": true })));
    };
    let host_key = storage.host_key().unwrap_or_default().to_owned();
    storage.close().await;
    // Like ssh on the first connection: the admin sees the key and it is remembered from now on.
    let known = sftp.host_key.is_some();
    if !known {
        let mut settings = backups.settings().await.map_err(api_error)?;
        if let Some(saved) = settings.target.as_mut().and_then(Target::as_sftp_mut) {
            saved.host_key = Some(host_key.clone());
        }
        backups.save_settings(&settings).await.map_err(api_error)?;
    }
    Ok(Json(json!({ "kind": "sftp", "hostKey": host_key, "known": known })))
}

/// Trusts the host key the backup server shows now, after it changed on purpose.
pub async fn forget_host_key(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<Value>> {
    let backups = backups(&web)?;
    let mut settings = backups.settings().await.map_err(api_error)?;
    if let Some(target) = settings.target.as_mut().and_then(Target::as_sftp_mut) {
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
    if backups.mailbox_busy() {
        return Err(ApiError::Rule("backupBusy", "a mailbox is being restored from a snapshot right now".into()));
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
    // After it started, not before: an entry for a restore that was refused would make the one
    // record of what happened to this server say something that did not.
    backups.start_restore(&body.snapshot, body.keep_gateway, &session.account.login).await.map_err(api_error)?;
    audit(&web, &session, "backups.restore", &body.snapshot, json!({ "keepGateway": body.keep_gateway })).await;
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

#[derive(Deserialize)]
pub struct OpenSnapshot {
    /// A snapshot's name, or `latest`.
    snapshot: String,
}

/// Opens a snapshot to take single mailboxes out of it: its database is fetched in the background,
/// and the page shows who is in it once it is here.
pub async fn open_snapshot(
    State(web): State<Web>,
    _admin: Admin,
    Json(body): Json<OpenSnapshot>,
) -> ApiResult<Json<Value>> {
    let backups = backups(&web)?;
    if backups.settings().await.map_err(api_error)?.target.is_none() {
        return Err(ApiError::Invalid("no backup server is set up".into()));
    }
    if backups.is_running() {
        return Err(ApiError::Rule("backupBusy", "a backup is running right now".into()));
    }
    backups.open_snapshot(body.snapshot.trim()).await.map_err(api_error)?;
    Ok(Json(view(&web).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailboxRestoreBody {
    /// The person as the snapshot knows them.
    account: String,
    /// The mailbox here it goes into; the same address when left out.
    #[serde(default)]
    into: Option<String>,
    /// The snapshot's folders to bring back; all of them when left out.
    #[serde(default)]
    folders: Option<Vec<i64>>,
}

/// Puts one person's mail from the open snapshot back, into a new folder of their mailbox.
pub async fn restore_mailbox(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(body): Json<MailboxRestoreBody>,
) -> ApiResult<Json<Value>> {
    let backups = backups(&web)?;
    if body.folders.as_ref().is_some_and(Vec::is_empty) {
        return Err(ApiError::Invalid("choose at least one folder".into()));
    }
    if backups.is_running() {
        return Err(ApiError::Rule("backupBusy", "a backup is running right now".into()));
    }
    backups
        .start_mailbox_restore(&body.account, body.into.as_deref(), body.folders.clone(), &session.account.login)
        .await
        .map_err(api_error)?;
    let details = json!({
        "snapshot": backups.mailbox_restore().snapshot,
        "into": body.into,
        "folders": body.folders.as_ref().map(Vec::len),
    });
    audit(&web, &session, "backup.restoreMailbox", &body.account, details).await;
    Ok(Json(view(&web).await?))
}

/// Closes the open snapshot and removes its database from this server.
pub async fn close_snapshot(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    backups(&web)?.close_snapshot().await.map_err(api_error)?;
    Ok(Json(view(&web).await?))
}
