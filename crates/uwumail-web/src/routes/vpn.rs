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

/// Whether the helper on the machine can start the VPN: it has to be there and new enough.
fn helper_can(web: &Web) -> bool {
    web.host()
        .and_then(|host| host.view().machine)
        .is_some_and(|machine| machine.verbs.iter().any(|verb| verb == "vpn-apply"))
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

/// Stores the VPN's settings. Nothing is started; that is [`apply`].
pub async fn save(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(change): Json<VpnChange>,
) -> ApiResult<Json<Value>> {
    let config = load(&web).await?.changed(change);
    if config.provider().is_none() {
        return Err(ApiError::Rule("vpnInvalid", "choose a VPN provider".into()));
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

/// Stops gluetun and lets the way out go straight again, so pictures don't wait for a VPN that is gone.
pub async fn stop(State(web): State<Web>, Admin(session): Admin) -> ApiResult<Json<Value>> {
    let host = web.host().ok_or_else(|| ApiError::NotFound("the helper on this machine".into()))?.clone();
    if !helper_can(&web) {
        return Err(ApiError::Rule("vpnHelperOld", "the helper on this machine does not know the VPN yet".into()));
    }
    let id = host.ask("vpn-stop").await.map_err(|message| ApiError::Rule("hostJobRefused", message))?;
    if web.egress().and_then(|egress| egress.status().proxy).as_deref() == Some(GLUETUN_PROXY) {
        change_settings(&web, &session, proxy_change(Value::Null)).await?;
    }
    audit(&web, &session, "vpn.stop", "", json!({ "id": id })).await;
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
