//! Server → Setup: the UwUMail Gateway and where this server stands on the internet.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_smtp::reachability::Reachability;

use super::audit;
use super::security::confirm_identity;
use crate::Web;
use crate::cloudflare::{Cloudflare, HostName};
use crate::error::{ApiError, ApiResult};
use crate::gateway::GatewayView;
use crate::session::Admin;

pub async fn show(State(web): State<Web>, _admin: Admin) -> Json<GatewayView> {
    Json(view(&web).await)
}

async fn view(web: &Web) -> GatewayView {
    let mut view = web.gateway().map(|gateway| gateway.view()).unwrap_or_default();
    // Which gateway there is to install. It comes from the release list rather than from the
    // gateway itself: the gateway has no idea what is newer than it is, and the server asks
    // GitHub once a day anyway.
    view.software_version = newer_gateway(web, view.software.as_deref()).await;
    view
}

/// The version to offer, or `None` when the gateway already runs the newest one.
///
/// The gateway calls itself `uwumail-gateway 0.2.2`; what comes after the space is its version.
async fn newer_gateway(web: &Web, software: Option<&str>) -> Option<String> {
    let newest = web.update_info().await.newest_release?;
    let running = software?.rsplit(' ').next()?.trim().to_owned();
    (running != newest).then_some(newest)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ask {
    /// `os-update`, `reboot` or `gateway-update`.
    verb: String,
    #[serde(default)]
    password: Option<String>,
}

/// Asks the VPS the gateway runs on for something. Each of these takes the gateway off the network
/// for a while, and with it every way into this server from outside -- so each needs the password
/// again, the same as pairing.
pub async fn ask(State(web): State<Web>, Admin(session): Admin, Json(ask): Json<Ask>) -> ApiResult<Json<GatewayView>> {
    let gateway = web.gateway().ok_or_else(|| ApiError::NotFound("the gateway".into()))?.clone();
    if !matches!(ask.verb.as_str(), "os-update" | "reboot" | "gateway-update") {
        return Err(ApiError::Invalid(format!("unknown job: {}", ask.verb)));
    }
    confirm_identity(&web, &session, ask.password.as_deref()).await?;
    // The version is never taken from the request: it is the newest release the server itself
    // found, or nothing. Whoever asks picks the button, not what gets installed.
    let version = match ask.verb.as_str() {
        "gateway-update" => Some(
            newer_gateway(&web, gateway.view().software.as_deref())
                .await
                .ok_or_else(|| ApiError::Rule("gatewayJobRefused", "there is no newer gateway to install".into()))?,
        ),
        _ => None,
    };
    let id = gateway
        .ask(&ask.verb, version.as_deref())
        .await
        .map_err(|message| ApiError::Rule("gatewayJobRefused", message))?;
    audit(&web, &session, "gateway.job", &ask.verb, json!({ "id": id, "version": version })).await;
    Ok(Json(view(&web).await))
}

#[derive(Deserialize)]
pub struct Pairing {
    code: String,
    #[serde(default)]
    password: Option<String>,
}

/// All mail and web traffic will flow through the gateway, so this needs the password again.
pub async fn pair(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(pairing): Json<Pairing>,
) -> ApiResult<Json<GatewayView>> {
    let gateway = web.gateway().ok_or_else(|| ApiError::NotFound("the gateway".into()))?.clone();
    confirm_identity(&web, &session, pairing.password.as_deref()).await?;
    gateway.pair(pairing.code.trim()).await.map_err(|message| ApiError::Rule("gatewayCodeInvalid", message))?;
    let view = gateway.view();
    audit(&web, &session, "gateway.pair", "", json!({ "fingerprint": view.fingerprint, "tunnel": view.tunnel })).await;
    Ok(Json(view))
}

#[derive(Deserialize, Default)]
pub struct Confirmation {
    #[serde(default)]
    password: Option<String>,
}

pub async fn forget(
    State(web): State<Web>,
    Admin(session): Admin,
    body: Option<Json<Confirmation>>,
) -> ApiResult<StatusCode> {
    let gateway = web.gateway().ok_or_else(|| ApiError::NotFound("the gateway".into()))?.clone();
    let confirmation = body.map(|Json(body)| body).unwrap_or_default();
    confirm_identity(&web, &session, confirmation.password.as_deref()).await?;
    let fingerprint = gateway.view().fingerprint;
    gateway.forget().await.map_err(|message| ApiError::Rule("gatewayForgetFailed", message))?;
    audit(&web, &session, "gateway.forget", "", json!({ "fingerprint": fingerprint })).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn reachability(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Reachability>> {
    let Some(dns) = web.dns() else {
        return Err(ApiError::Rule("dnsUnavailable", "the server has no working DNS resolver".into()));
    };
    Ok(Json(web.smtp().check_reachability(dns).await))
}

/// The names mail apps try for every domain; the server's certificate covers them too once they
/// lead here (see acme.rs in the server).
const CLIENT_NAMES: [&str; 5] = ["mail", "imap", "smtp", "autoconfig", "autodiscover"];

/// The host names that have to lead to the gateway: the server's own name first, then
/// `mta-sts.<domain>` for the domains with MTA-STS and the names mail apps use for every domain.
async fn gateway_host_names(web: &Web) -> ApiResult<Vec<HostName>> {
    let hostname = web.settings().hostname.trim_end_matches('.').to_ascii_lowercase();
    let mut hosts = vec![HostName { name: hostname, required: true }];
    let mut add = |name: String| {
        if !hosts.iter().any(|host| host.name == name) {
            hosts.push(HostName { name, required: false });
        }
    };
    for domain in web.store().mta_sts_domains().await? {
        add(format!("mta-sts.{domain}"));
    }
    for domain in web.store().domains().await? {
        for prefix in CLIENT_NAMES {
            add(format!("{prefix}.{}", domain.name));
        }
    }
    Ok(hosts)
}

#[derive(Deserialize)]
pub struct CloudflareHosts {
    token: String,
    /// Carry the plan out; without it the portal only learns what would change.
    #[serde(default)]
    apply: bool,
    /// Replace A and AAAA records that point somewhere else than the gateway.
    #[serde(default)]
    replace: bool,
}

/// Points the server's host names at the gateway's public addresses at Cloudflare, not proxied,
/// with a token that is used for this request only. First it says what would change; records that
/// point elsewhere are only replaced when the admin confirmed exactly that.
pub async fn cloudflare(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(request): Json<CloudflareHosts>,
) -> ApiResult<Json<Value>> {
    if request.token.trim().is_empty() {
        return Err(ApiError::Invalid("a Cloudflare API token is needed".into()));
    }
    let view = web.gateway().map(|gateway| gateway.view()).unwrap_or_default();
    let addresses: Vec<IpAddr> = view.addresses.iter().filter_map(|address| address.parse().ok()).collect();
    let v4: Vec<Ipv4Addr> =
        addresses.iter().filter_map(|ip| if let IpAddr::V4(v4) = ip { Some(*v4) } else { None }).collect();
    let v6: Vec<Ipv6Addr> =
        addresses.iter().filter_map(|ip| if let IpAddr::V6(v6) = ip { Some(*v6) } else { None }).collect();
    if v4.is_empty() && v6.is_empty() {
        return Err(ApiError::Rule("gatewayNoAddresses", "the gateway has not told its public addresses yet".into()));
    }
    let hosts = gateway_host_names(&web).await?;
    let cloudflare = Cloudflare::new(&request.token);
    let plan =
        cloudflare.host_plan(&hosts, &v4, &v6).await.map_err(|message| ApiError::Rule("cloudflareFailed", message))?;
    if !request.apply {
        return Ok(Json(json!({ "plan": plan })));
    }
    let results = cloudflare.apply_hosts(&plan, request.replace).await;
    let changed: Vec<String> = results
        .iter()
        .filter(|result| matches!(result.outcome, "created" | "updated"))
        .map(|result| format!("{} {}", result.record_type, result.name))
        .collect();
    audit(&web, &session, "gateway.cloudflare", "", json!({ "changed": changed, "replace": request.replace })).await;
    // The DNS checks of every domain may read differently now.
    for domain in web.store().domains().await.unwrap_or_default() {
        web.forget_report(&domain.name);
    }
    Ok(Json(json!({ "plan": plan, "results": results })))
}
