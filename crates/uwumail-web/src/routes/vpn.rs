//! Settings → VPN & proxy: the gluetun container beside the server, set up from the portal.
//!
//! The settings are stored here, keys included, and shown without them. With the machine's helper the
//! portal hands them over and starts the VPN; without it, it shows `.env.vpn` to copy. Either way the
//! server's pictures (and whatever else the admin chose) then leave through gluetun's proxy.

use axum::Json;
use axum::extract::State;
use serde_json::{Map, Value, json};

use super::audit;
use super::settings::{change_settings, setting_source};
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Admin;
use crate::settings::SettingSource;
use crate::vpn::{GLUETUN_PROXY, PROVIDERS, VPN_KEY, VpnChange, VpnConfig};

async fn load(web: &Web) -> ApiResult<VpnConfig> {
    Ok(web.store().setting(VPN_KEY).await?.and_then(|raw| serde_json::from_str(&raw).ok()).unwrap_or_default())
}

async fn store(web: &Web, config: &VpnConfig) -> ApiResult<()> {
    let raw = serde_json::to_string(config).map_err(|_| ApiError::Internal)?;
    web.store().set_setting(VPN_KEY, &raw).await?;
    Ok(())
}

/// Whether the helper on the machine can do `verb`: it has to be there and new enough.
fn helper_does(web: &Web, verb: &str) -> bool {
    web.host().and_then(|host| host.view().machine).is_some_and(|machine| machine.can(verb))
}

/// Whether the helper on the machine can start the VPN.
fn helper_can(web: &Web) -> bool {
    helper_does(web, "vpn-apply")
}

async fn view(web: &Web) -> ApiResult<Value> {
    let config = load(web).await?;
    let (shown, secrets) = config.shown();
    let host = web.host().map(|host| host.view());
    let machine = host.as_ref().and_then(|host| host.machine.clone());
    let proxy = web.egress().map(|egress| egress.status().proxy).unwrap_or_default();
    let source = setting_source(web, "egress.proxy").await?;
    Ok(json!({
        "config": shown,
        "secrets": secrets,
        "saved": !config.provider.is_empty(),
        "complete": config.check().err(),
        "providers": PROVIDERS,
        "helper": {
            "available": host.as_ref().is_some_and(|host| host.available),
            "canVpn": helper_can(web),
            "canRemove": helper_does(web, "vpn-remove"),
            "canUpdate": helper_does(web, "helper-update"),
            "version": machine.as_ref().map(|machine| machine.helper.clone()),
            "vpn": machine.and_then(|machine| machine.vpn),
        },
        "job": host.as_ref().and_then(|host| host.job.clone()),
        "log": host.map(|host| host.log).unwrap_or_default(),
        "proxy": {
            "current": proxy,
            "gluetun": GLUETUN_PROXY,
            "locked": source == Some(SettingSource::File),
        },
    }))
}

pub async fn show(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(view(&web).await?))
}

/// gluetun wants the address of an own WireGuard server; a provider's file often names it instead, so the
/// name is looked up here. IPv4 first: a VPN container usually has no IPv6 route.
async fn endpoint_address(name: &str, port: u16) -> ApiResult<String> {
    let found: Vec<std::net::SocketAddr> = tokio::net::lookup_host((name, port))
        .await
        .map_err(|_| {
            ApiError::Rule("vpnInvalid", format!("{name} could not be looked up; enter the server's IP address"))
        })?
        .collect();
    found
        .iter()
        .find(|address| address.is_ipv4())
        .or(found.first())
        .map(|address| address.ip().to_string())
        .ok_or_else(|| ApiError::Rule("vpnInvalid", format!("{name} has no address; enter the server's IP address")))
}

/// Stores the VPN's settings. Nothing is started; that is [`apply`].
pub async fn save(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(change): Json<VpnChange>,
) -> ApiResult<Json<Value>> {
    let mut config = load(&web).await?.changed(change);
    if config.provider().is_none() {
        return Err(ApiError::Rule("vpnInvalid", "choose a VPN provider".into()));
    }
    let endpoint = config.wireguard_endpoint_ip.clone();
    if config.provider == "custom" && !endpoint.is_empty() && endpoint.parse::<std::net::IpAddr>().is_err() {
        let port = config.wireguard_endpoint_port.unwrap_or(51820);
        config.wireguard_endpoint_ip = endpoint_address(endpoint.trim_matches(['[', ']']), port).await?;
    }
    store(&web, &config).await?;
    let details = json!({ "provider": config.provider, "type": config.kind });
    audit(&web, &session, "vpn.save", &config.provider, details).await;
    Ok(Json(view(&web).await?))
}

fn proxy_change(value: Value) -> Map<String, Value> {
    let mut changes = Map::new();
    changes.insert("egress.proxy".into(), value);
    changes
}

/// Hands the settings to the helper, which writes `.env.vpn` and (re)starts gluetun, and points the way out
/// at gluetun's proxy.
pub async fn apply(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<Value>> {
    let host = web.host().ok_or_else(|| ApiError::NotFound("the helper on this machine".into()))?.clone();
    if !helper_can(&web) {
        return Err(ApiError::Rule(
            "vpnHelperOld",
            "the helper on this machine does not know the VPN yet; install its new version".into(),
        ));
    }
    let config = load(&web).await?;
    config.check().map_err(|message| ApiError::Rule("vpnInvalid", message))?;
    let request = json!({ "env": config.env(), "ovpn": config.ovpn() });
    host.hand_over("vpn.json", &request.to_string()).map_err(|message| ApiError::Rule("hostJobRefused", message))?;
    let id = host.ask("vpn-apply").await.map_err(|message| ApiError::Rule("hostJobRefused", message))?;
    change_settings(&web, &session, proxy_change(json!(GLUETUN_PROXY))).await?;
    audit(&web, &session, "vpn.apply", &config.provider, json!({ "id": id })).await;
    Ok(Json(view(&web).await?))
}

/// Switches the VPN off: the way out goes straight again at once, so nothing waits for a VPN that is going
/// away, and the helper (where there is one) stops gluetun and keeps it from coming back with the next
/// `docker compose up -d`. Without a helper only the first half happens; the container is stopped by hand.
pub async fn stop(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<Value>> {
    if web.egress().and_then(|egress| egress.status().proxy).as_deref() == Some(GLUETUN_PROXY) {
        change_settings(&web, &session, proxy_change(Value::Null)).await?;
    }
    let mut id = None;
    if helper_can(&web)
        && let Some(host) = web.host().cloned()
    {
        id = Some(host.ask("vpn-stop").await.map_err(|message| ApiError::Rule("hostJobRefused", message))?);
    }
    audit(&web, &session, "vpn.stop", "", json!({ "id": id })).await;
    Ok(Json(view(&web).await?))
}

/// Takes the VPN out entirely: the settings stored here, keys included, and -- through the helper --
/// the container, `.env.vpn` and the OpenVPN file. The way out goes straight again at once. A helper
/// too old to remove things at least stops the container; one that is missing leaves it to the admin.
pub async fn remove(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<Value>> {
    if web.egress().and_then(|egress| egress.status().proxy).as_deref() == Some(GLUETUN_PROXY) {
        change_settings(&web, &session, proxy_change(Value::Null)).await?;
    }
    let verb = ["vpn-remove", "vpn-stop"].into_iter().find(|verb| helper_does(&web, verb));
    let mut id = None;
    if let (Some(verb), Some(host)) = (verb, web.host().cloned()) {
        id = Some(host.ask(verb).await.map_err(|message| ApiError::Rule("hostJobRefused", message))?);
    }
    // Forgotten only once the helper took the job: a refused one leaves everything as it was.
    let provider = load(&web).await?.provider;
    web.store().delete_setting(VPN_KEY).await?;
    audit(&web, &session, "vpn.remove", &provider, json!({ "id": id })).await;
    Ok(Json(view(&web).await?))
}

/// The files to put onto the machine by hand, keys included, for a server without the helper.
pub async fn files(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<Value>> {
    let config = load(&web).await?;
    config.check().map_err(|message| ApiError::Rule("vpnInvalid", message))?;
    audit(&web, &session, "vpn.files", &config.provider, json!({})).await;
    Ok(Json(json!({ "envFile": config.env_file(), "ovpn": config.ovpn() })))
}

/// Points the way out at gluetun without the helper, once the VPN was started by hand.
pub async fn use_gluetun(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<Value>> {
    if !change_settings(&web, &session, proxy_change(json!(GLUETUN_PROXY))).await? {
        return Err(ApiError::Rule("settingLocked", "egress.proxy is set in the config file or .env".into()));
    }
    Ok(Json(view(&web).await?))
}
