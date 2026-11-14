//! The update centre of the admin panel: what runs, what is newer, and the one place where it gets
//! installed -- on this machine and on the gateway's.
//!
//! The container never replaces itself, and the server installs nothing. It asks the helper on
//! each machine for a job, one word from a fixed list, and keeps the order (see `update_run`). The
//! helper fetches what it installs from the project's releases -- never from anywhere this server
//! names. Without a helper the portal shows the command to run by hand.

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::{Value, json};

use super::audit;
use super::security::confirm_identity;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Admin;
use crate::update_run::{StepAction, StepKind};
use crate::updates::{UpdateSettings, build, image, tag};

async fn view(web: &Web) -> ApiResult<Value> {
    let build = build();
    let settings = web.update_settings().await;
    let info = web.update_info().await;
    let run = web.advance_update_run().await;
    let (plan, possible) = web.update_steps().await;
    let gateway = web.gateway().map(|gateway| gateway.view()).filter(|view| view.software.is_some());
    let gateway_version = match &gateway {
        Some(view) => super::gateway::newer_gateway(web, view.software.as_deref()).await,
        None => None,
    };
    // For a gateway from before its helper: the release's amd64 build, checked before it installs.
    // That also brings the helper, and from then on the gateway updates from here.
    let gateway_command = match (&gateway, &gateway_version) {
        (Some(view), Some(version)) if !view.can_install => {
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
        "image": format!("{}:{}", image(), tag(build.release, settings.channel)),
        // For a machine without a helper. update.sh updates itself, the compose file and the
        // image, in that order.
        "serverCommand": "sudo bash update.sh",
        "host": web.host().map(|host| host.view()).unwrap_or_default(),
        "gateway": gateway,
        "gatewayVersion": gateway_version,
        "gatewayCommand": gateway_command,
        "plan": plan,
        "possible": possible,
        "run": run,
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Start {
    /// The steps to take; without it, everything that waits. Restarts are only ever asked for by name.
    #[serde(default)]
    steps: Option<Vec<StepKind>>,
    #[serde(default)]
    password: Option<String>,
}

/// Every step can take the server or the gateway off the network for a while, so a run needs the
/// password again -- the same rule as pairing a gateway.
pub async fn start(State(web): State<Web>, Admin(session): Admin, Json(start): Json<Start>) -> ApiResult<Json<Value>> {
    confirm_identity(&web, &session, start.password.as_deref()).await?;
    let run = web
        .start_update_run(start.steps, &session.account.login)
        .await
        .map_err(|message| ApiError::Rule("updateRefused", message))?;
    let steps: Vec<StepKind> = run.steps.iter().map(|step| step.kind).collect();
    audit(&web, &session, "updates.run", "server", json!({ "steps": steps })).await;
    Ok(Json(view(&web).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    action: StepAction,
    /// With `retry` after the compose file stood in the way: `keep` or `replace`.
    #[serde(default)]
    compose: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

/// What to do with the step that failed: once more, without it, or not at all.
pub async fn step(State(web): State<Web>, Admin(session): Admin, Json(answer): Json<Answer>) -> ApiResult<Json<Value>> {
    // Going on can start the next step, a restart among them; only giving up needs no password.
    if answer.action != StepAction::Stop {
        confirm_identity(&web, &session, answer.password.as_deref()).await?;
    }
    web.answer_update_step(answer.action, answer.compose.as_deref())
        .await
        .map_err(|message| ApiError::Rule("updateRefused", message))?;
    audit(
        &web,
        &session,
        "updates.step",
        "server",
        json!({ "action": format!("{:?}", answer.action), "compose": answer.compose }),
    )
    .await;
    Ok(Json(view(&web).await?))
}
