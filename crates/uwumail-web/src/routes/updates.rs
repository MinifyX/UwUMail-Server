//! Updates in the admin panel: the running version, what is newer, and how it gets installed.
//!
//! The container never replaces itself: `update.sh` beside the compose file does, because it brings
//! a new compose file along, which a container replacing itself never could. With the machine's
//! helper the portal's button asks for that run (`uwumail-update`, see routes::host), and the helper
//! fetches update.sh from the project's releases -- never from anywhere this server names. Without
//! the helper the command is shown to run by hand.

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use super::audit;
use crate::Web;
use crate::error::ApiResult;
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
    Ok(json!({
        "build": build,
        "settings": settings,
        "info": info,
        "image": format!("{}:{tag}", image()),
        // What to run on the machine the server lives on. update.sh updates itself, the compose
        // file and the image, in that order.
        "serverCommand": "sudo bash update.sh",
        "gateway": gateway.map(|view| json!({ "software": view.software })),
        "gatewayCommand": gateway_command,
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
    let before = web.update_settings().await;
    web.save_update_settings(&settings).await?;
    audit(
        &web,
        &session,
        "updates.settings",
        "server",
        json!({ "check": settings.check, "channel": settings.channel }),
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
