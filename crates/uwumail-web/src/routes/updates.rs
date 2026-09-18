//! Updates in the admin panel: the running version, what is newer, the button that installs it and
//! the schedule that does it without being asked.
//!
//! The installing itself is not done here -- the container cannot recreate itself, on purpose. It
//! goes through the helper on the machine (`deploy/host/`). Without one, this is what it always
//! was: the version, the news, and two commands to copy.

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::{Value, json};

use super::audit;
use super::security::confirm_identity;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::health::unix_now;
use crate::session::Admin;
use crate::updates::{Channel, UpdateSettings, build, image};

/// The image tag a server on this channel follows.
fn tag(release: bool, channel: Channel) -> &'static str {
    match (release, channel) {
        (false, _) => "edge",
        (true, Channel::Stable) => "latest",
        (true, Channel::Beta) => "beta",
    }
}

async fn view(web: &Web) -> ApiResult<Value> {
    let build = build();
    let settings = web.update_settings().await;
    let info = web.update_info().await;
    let tag = tag(build.release, settings.channel);
    let gateway_version = info.releases.first().map(|release| release.version.clone());
    let gateway = web.gateway().map(|gateway| gateway.view()).filter(|view| view.software.is_some());
    // A release carries a ready gateway for amd64; the command checks its checksum before installing.
    let gateway_command = match (&gateway, gateway_version.filter(|_| build.release)) {
        (Some(_), Some(version)) => {
            let base = format!("{}/releases/download/v{version}", env!("CARGO_PKG_REPOSITORY"));
            let file = "uwumail-gateway-linux-amd64.tar.gz";
            Some(format!(
                "cd /tmp && curl -fsSLO {base}/{file} && curl -fsSLO {base}/{file}.sha256 && sha256sum -c {file}.sha256 \
                 && tar -xzf {file} && sudo bash uwumail-gateway/install.sh uwumail-gateway/uwumail-gateway"
            ))
        }
        _ => None,
    };
    // What one click would install: the newest on the channel, or nothing to name for an edge
    // build, which follows its tag and has no version of its own to go to.
    let target = if build.release { info.releases.first().map(|release| release.version.clone()) } else { None };
    let status = web.update_status().await;
    // The log of the job doing this update, while it is the one the helper has in hand.
    let host = web.host().map(|host| host.view());
    let log = host
        .as_ref()
        .filter(|view| view.job.as_ref().map(|job| &job.id) == status.job.as_ref())
        .map(|view| view.log.clone())
        .unwrap_or_default();
    let backups = match web.backups() {
        Some(backups) => backups.settings().await.ok(),
        None => None,
    };
    Ok(json!({
        "build": build,
        "settings": settings,
        "info": info,
        "image": format!("{}:{tag}", image()),
        "serverCommand": "docker compose pull && docker compose up -d",
        "gateway": gateway.map(|view| json!({ "software": view.software })),
        "gatewayCommand": gateway_command,
        // Whether the portal can do it itself, or only say how.
        "canInstall": web.host().is_some_and(|host| host.view().available),
        "target": target,
        "status": status,
        "log": log,
        // Whether there is anywhere to back up to at all; without one, updating means saying so.
        "backupReady": backups.as_ref().is_some_and(|settings| settings.target.is_some()),
        "nearBackup": web.update_near_backup(unix_now()).await,
    }))
}

pub async fn show(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(view(&web).await?))
}

pub async fn save(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(settings): Json<UpdateSettings>,
) -> ApiResult<Json<Value>> {
    if settings.hour > 23 || settings.minute > 59 {
        return Err(ApiError::Invalid("the time has to be a real one".into()));
    }
    if settings.weekday.is_some_and(|day| day > 6) {
        return Err(ApiError::Invalid("the weekday has to be between 0 and 6".into()));
    }
    let before = web.update_settings().await;
    web.save_update_settings(&settings).await?;
    audit(
        &web,
        &session,
        "updates.settings",
        "server",
        json!({
            "check": settings.check,
            "channel": settings.channel,
            "auto": settings.auto,
            "weekday": settings.weekday,
            "hour": settings.hour,
            "minute": settings.minute,
            "backupFirst": settings.backup_first,
        }),
    )
    .await;
    // Another channel sees other releases.
    if settings.check && before.channel != settings.channel {
        web.check_updates().await;
    }
    Ok(Json(view(&web).await?))
}

pub async fn check(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    web.check_updates().await;
    Ok(Json(view(&web).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    /// Which version; left out means the newest the last check found.
    #[serde(default)]
    version: Option<String>,
    /// Back up first. Left out follows the setting; `false` is the deliberate way out for a server
    /// that has nowhere to back up to.
    #[serde(default)]
    backup: Option<bool>,
    #[serde(default)]
    password: Option<String>,
}

/// Updates now. Answers as soon as it has started, because the rest of it takes this process down.
pub async fn run(State(web): State<Web>, Admin(session): Admin, Json(ask): Json<Run>) -> ApiResult<Json<Value>> {
    let settings = web.update_settings().await;
    let backup = ask.backup.unwrap_or(settings.backup_first);
    // The server is about to go away for a minute or two and come back as another binary. That is
    // worth the password again, the same as pairing a gateway or asking the machine for anything.
    confirm_identity(&web, &session, ask.password.as_deref()).await?;
    let status = web
        .start_update(ask.version.as_deref(), backup, "hand")
        .await
        .map_err(|message| ApiError::Rule("updateRefused", message))?;
    audit(&web, &session, "updates.run", "server", json!({ "to": status.to, "backup": backup })).await;
    Ok(Json(view(&web).await?))
}

/// Puts the last result away, once it has been read. Never touches one that is still going.
pub async fn forget(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    web.forget_update().await.map_err(|message| ApiError::Rule("updateRefused", message))?;
    Ok(Json(view(&web).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_build_follows_its_tag() {
        assert_eq!(tag(false, Channel::Stable), "edge");
        assert_eq!(tag(true, Channel::Stable), "latest");
        assert_eq!(tag(true, Channel::Beta), "beta");
    }
}
