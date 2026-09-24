//! Server → the machine this runs on: what it has waiting, and the buttons that install it -- the
//! system's updates, a new UwUMail, a new helper.
//!
//! Only reachable when a helper is installed beside the container (`deploy/host/`). Without one,
//! [`show`] says so and the portal goes on showing the commands to copy.

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::json;

use super::audit;
use super::security::confirm_identity;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::host::HostView;
use crate::session::Admin;

pub async fn show(State(web): State<Web>, _admin: Admin) -> Json<HostView> {
    Json(web.host().map(|host| host.view()).unwrap_or_default())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ask {
    /// `os-update`, `reboot`, `uwumail-update` or `helper-update`.
    verb: String,
    #[serde(default)]
    password: Option<String>,
}

/// Every one of these can take the server off the network for a while, so each needs the password
/// again -- the same rule as pairing a gateway.
pub async fn ask(State(web): State<Web>, Admin(session): Admin, Json(ask): Json<Ask>) -> ApiResult<Json<HostView>> {
    let host = web.host().ok_or_else(|| ApiError::NotFound("the helper on this machine".into()))?.clone();
    if !matches!(ask.verb.as_str(), "os-update" | "reboot" | "uwumail-update" | "helper-update") {
        return Err(ApiError::Invalid(format!("unknown job: {}", ask.verb)));
    }
    // An older helper would only answer that it does not know the job; better to say so now.
    if host.view().machine.is_some_and(|machine| !machine.can(&ask.verb)) {
        return Err(ApiError::Rule(
            "hostHelperOld",
            format!("the helper on this machine cannot do {} yet; bring it up to date first", ask.verb),
        ));
    }
    // Updating a machine that also runs other things restarts those too, and a restart takes them
    // with it. The portal says so above the button, plainly, and asks for the password before it
    // happens -- but it does not stand in the way. It is the admin's machine.
    confirm_identity(&web, &session, ask.password.as_deref()).await?;

    let id = host.ask(&ask.verb).await.map_err(|message| ApiError::Rule("hostJobRefused", message))?;
    audit(&web, &session, "host.job", &ask.verb, json!({ "id": id })).await;
    Ok(Json(host.view()))
}
