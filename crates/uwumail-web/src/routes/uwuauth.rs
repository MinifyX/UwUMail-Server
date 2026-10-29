//! Pairing with UwUAuth (docs/uwuauth.md): the admin types UwUAuth's address and a pairing code, and
//! the server asks UwUAuth for an OpenID Connect client and a SCIM token. The client goes into the
//! usual `auth.oidc.*` settings, so the login button works as with any provider; the token's SHA-256
//! is kept for `/scim/v2` (routes/scim.rs).

use aws_lc_rs::digest;
use axum::Json;
use axum::extract::State;
use axum::http::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use axum::http::{Method, Request, StatusCode};
use bytes::Bytes;
use data_encoding::HEXLOWER;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uwumail_dav::client::Transport;

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::external::{checked_issuer_url, oidc};
use crate::session::Admin;
use crate::settings::{SettingSource, SettingsBackend};

/// Where the pairing is kept in the store.
const PAIRING_KEY: &str = "uwuauth.pairing";
const MAX_ANSWER: usize = 256 * 1024;
/// What the server asks UwUAuth for at login.
const SCOPES: [&str; 5] = ["openid", "email", "profile", "groups", "roles"];

/// The settings pairing fills in, and unpairing empties.
const PAIRED_SETTINGS: [&str; 11] = [
    "auth.oidc.enabled",
    "auth.oidc.issuer",
    "auth.oidc.client_id",
    "auth.oidc.client_secret",
    "auth.oidc.button_label",
    "auth.oidc.auto_create",
    "auth.oidc.admin_group_claim",
    "auth.oidc.admin_group_value",
    "auth.oidc.scopes",
    "auth.oidc.private_network",
    "auth.oidc.sync_admin",
];

/// A pairing with UwUAuth, as kept here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Pairing {
    /// UwUAuth's address, as the admin typed it.
    pub url: String,
    /// The issuer in the provider's own spelling: ID tokens and linked logins carry exactly this.
    pub issuer: String,
    /// What UwUAuth calls itself.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub manage_url: String,
    pub paired_at: i64,
    pub paired_by: String,
    /// Hex SHA-256 of the SCIM token. The token itself is not kept.
    pub scim_token_hash: String,
    #[serde(default)]
    pub private_network: bool,
}

/// The pairing in effect, if any.
pub(crate) async fn pairing(web: &Web) -> ApiResult<Option<Pairing>> {
    let Some(stored) = web.store().setting(PAIRING_KEY).await? else { return Ok(None) };
    Ok(serde_json::from_str(&stored).ok())
}

pub(crate) fn token_hash(token: &str) -> String {
    HEXLOWER.encode(digest::digest(&digest::SHA256, token.as_bytes()).as_ref())
}

fn base_url(web: &Web) -> String {
    format!("https://{}", web.settings().hostname)
}

fn backend(web: &Web) -> ApiResult<&dyn SettingsBackend> {
    web.settings().config.as_deref().ok_or_else(|| ApiError::NotFound("server settings".into()))
}

/// The pairing settings the config file holds: those cannot be filled in from here.
async fn locked_settings(web: &Web) -> ApiResult<Vec<String>> {
    let Some(backend) = web.settings().config.as_deref() else { return Ok(Vec::new()) };
    let overlay = super::settings::load_overlay(web).await?;
    let current = backend.view(&overlay).map_err(|_| ApiError::Internal)?;
    Ok(current
        .iter()
        .filter(|setting| setting.source == SettingSource::File && PAIRED_SETTINGS.contains(&setting.key))
        .map(|setting| setting.key.to_owned())
        .collect())
}

/// Where the pairing stands, for the admin panel. Never the token.
pub async fn status(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    let pairing = pairing(&web).await?;
    let config = web.external_login().config();
    let base = base_url(&web);
    let login_active = pairing.as_ref().is_some_and(|pairing| {
        config.oidc.enabled && config.oidc.issuer.trim().trim_end_matches('/') == pairing.issuer.trim_end_matches('/')
    });
    Ok(Json(json!({
        "paired": pairing.is_some(),
        "pairing": pairing.map(|pairing| json!({
            "url": pairing.url,
            "issuer": pairing.issuer,
            "name": pairing.name,
            "version": pairing.version,
            "appId": pairing.app_id,
            "manageUrl": pairing.manage_url,
            "pairedAt": pairing.paired_at,
            "pairedBy": pairing.paired_by,
            "privateNetwork": pairing.private_network,
        })),
        "loginActive": login_active,
        "scimUrl": format!("{base}/scim/v2"),
        "redirectUri": super::external_login::redirect_uri(&web),
        "locked": locked_settings(&web).await?,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairRequest {
    #[serde(default)]
    url: Option<String>,
    code: String,
    /// UwUAuth lives in the local network: its host may have a private address.
    #[serde(default)]
    private_network: bool,
    /// The admin's password, unless the login is fresh (as for every security change).
    #[serde(default)]
    password: Option<String>,
}

/// UwUAuth's address and the code from what the admin typed: the address field, or the whole text
/// of the QR code (`https://auth.example.com/#pair=CODE`) in the code field.
fn pairing_input(url: Option<&str>, code: &str) -> Result<(String, String), String> {
    let code = code.trim();
    let (url, code) = match code.split_once("#pair=") {
        Some((from_qr, code)) => (url.map(str::trim).filter(|url| !url.is_empty()).unwrap_or(from_qr), code),
        None => (url.map(str::trim).unwrap_or_default(), code),
    };
    let url = url.trim().trim_end_matches('/').to_owned();
    if url.is_empty() {
        return Err("enter UwUAuth's address".into());
    }
    let parsed = url::Url::parse(&url).map_err(|_| "UwUAuth's address is not a web address".to_owned())?;
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("UwUAuth's address has no ? or # part".into());
    }
    let code = code.trim().to_owned();
    if code.is_empty() || code.len() > 64 || code.chars().any(char::is_control) {
        return Err("enter the pairing code from UwUAuth".into());
    }
    Ok((url, code))
}

fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(str::to_ascii_lowercase)
}

/// A JSON request to UwUAuth: the status and the answer, JSON or not.
async fn call(
    transport: &dyn Transport,
    method: Method,
    url: &str,
    lan: Option<&str>,
    body: Option<&Value>,
) -> ApiResult<(u16, Value)> {
    let url = checked_issuer_url(url, lan).map_err(|err| ApiError::Rule("uwuauthAddress", err))?;
    let mut request = Request::builder()
        .method(method)
        .uri(url.as_str())
        .header(USER_AGENT, "UwUMail")
        .header(ACCEPT, "application/json");
    let payload = match body {
        Some(body) => {
            request = request.header(CONTENT_TYPE, "application/json");
            Bytes::from(serde_json::to_vec(body).map_err(|_| ApiError::Internal)?)
        }
        None => Bytes::new(),
    };
    let request = request.body(payload).map_err(|_| ApiError::Internal)?;
    let answer = transport
        .send(request, MAX_ANSWER)
        .await
        .map_err(|err| ApiError::Rule("uwuauthUnreachable", format!("UwUAuth did not answer: {err}")))?;
    Ok((answer.status, serde_json::from_slice(&answer.body).unwrap_or(Value::Null)))
}

fn text(value: &Value, name: &str) -> String {
    value.get(name).and_then(Value::as_str).unwrap_or_default().trim().to_owned()
}

/// What pairing puts into the settings.
fn paired_settings(
    issuer: &str,
    client_id: &str,
    secret: &str,
    scopes: &[String],
    private: bool,
) -> Map<String, Value> {
    let mut changes = Map::new();
    changes.insert("auth.oidc.enabled".into(), json!(true));
    changes.insert("auth.oidc.issuer".into(), json!(issuer));
    changes.insert("auth.oidc.client_id".into(), json!(client_id));
    changes.insert("auth.oidc.client_secret".into(), json!(secret));
    changes.insert("auth.oidc.button_label".into(), json!("UwUAuth"));
    // People come over SCIM; nobody gets an account by logging in.
    changes.insert("auth.oidc.auto_create".into(), json!(false));
    changes.insert("auth.oidc.admin_group_claim".into(), json!("roles"));
    changes.insert("auth.oidc.admin_group_value".into(), json!("admin"));
    changes.insert("auth.oidc.scopes".into(), json!(scopes));
    changes.insert("auth.oidc.private_network".into(), json!(private));
    changes.insert("auth.oidc.sync_admin".into(), json!(true));
    changes
}

/// Pairs with UwUAuth: asks what it is, checks its OpenID Connect discovery, uses the code, and
/// takes the client and the SCIM token over.
pub async fn pair(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(request): Json<PairRequest>,
) -> ApiResult<Json<Value>> {
    super::security::confirm_identity(&web, &session, request.password.as_deref()).await?;
    let (base, code) = pairing_input(request.url.as_deref(), &request.code).map_err(ApiError::Invalid)?;
    let backend = backend(&web)?;
    let private = request.private_network;
    let base_host = host_of(&base).ok_or_else(|| ApiError::Invalid("UwUAuth's address has no host".into()))?;
    let base_lan = private.then_some(base_host.as_str());
    checked_issuer_url(&base, base_lan).map_err(|err| ApiError::Rule("uwuauthAddress", err))?;

    // 1. Is it UwUAuth, and does it pair?
    let transport = web.oidc_transport_for(base_lan.map(str::to_owned).into_iter().collect());
    let (status, server) =
        call(transport.as_ref(), Method::GET, &format!("{base}/uwu/v1/server"), base_lan, None).await?;
    let pairs = server.get("pairing").and_then(Value::as_u64).is_some_and(|version| version >= 1);
    if status != 200 || text(&server, "product") != "UwUAuth" || !pairs {
        return Err(ApiError::Rule("notUwuauth", "this is not a UwUAuth that can pair (0.4 or newer)".into()));
    }
    let issuer = text(&server, "issuer");
    let issuer_host = host_of(&issuer).ok_or_else(|| ApiError::Rule("notUwuauth", "UwUAuth names no issuer".into()))?;
    // In the local network only the host the admin typed may be private: an issuer elsewhere would
    // let UwUAuth's answer point this server at any machine in the network.
    if private && issuer_host != base_host {
        return Err(ApiError::Rule(
            "uwuauthAddress",
            format!("UwUAuth names {issuer_host} as its issuer; in the local network it has to be {base_host}"),
        ));
    }
    let lan_hosts: Vec<String> = if private { vec![base_host.clone()] } else { Vec::new() };
    let transport = web.oidc_transport_for(lan_hosts);
    let issuer_lan = private.then_some(issuer_host.as_str());

    // 2. Logging in there has to work before the code is used up.
    let discovery = oidc::discover(transport.as_ref(), &issuer, issuer_lan)
        .await
        .map_err(|err| ApiError::Rule("oidcFailed", err))?;

    // 3. The settings have to take it: none of them may come from the config file.
    let scopes: Vec<String> = SCOPES.iter().map(|scope| (*scope).to_owned()).collect();
    let mut overlay = super::settings::load_overlay(&web).await?;
    let mut trial = overlay.clone();
    super::settings::merge_changes(
        backend,
        &mut trial,
        &paired_settings(&discovery.issuer, "pending", "pending", &scopes, private),
    )?;

    // 4. Use the code. The pairing endpoint is the one UwUAuth names, on its own host.
    let pair_url = Some(text(&server, "pair"))
        .filter(|url| host_of(url).is_some_and(|host| host == base_host || host == issuer_host))
        .unwrap_or_else(|| format!("{base}/uwu/v1/pair"));
    let pair_lan = private.then(|| host_of(&pair_url)).flatten();
    let base_url = base_url(&web);
    let app = json!({
        "code": code,
        "app": {
            "product": "UwUMail",
            "version": env!("CARGO_PKG_VERSION"),
            "name": format!("UwUMail ({})", web.settings().hostname),
            "url": base_url,
            "redirectUris": [super::external_login::redirect_uri(&web)],
            "postLogoutRedirectUris": [format!("{base_url}/")],
            "scopes": scopes,
            "roles": [
                { "id": "admin", "name": "Administrator", "description": "Uses UwUMail's admin panel" },
                { "id": "user", "name": "User", "description": "Has a mailbox" },
            ],
            "scim": { "baseUrl": format!("{base_url}/scim/v2"), "resources": ["User", "Group"], "userName": "email" },
        },
    });
    let (answered, answer) = call(transport.as_ref(), Method::POST, &pair_url, pair_lan.as_deref(), Some(&app)).await?;
    if answered != 200 {
        let error = text(&answer, "error");
        let message = text(&answer, "message");
        return Err(match (answered, error.as_str()) {
            (400, "invalid_code") => {
                ApiError::Rule("pairingCodeInvalid", "the code is unknown, used, withdrawn or ran out".into())
            }
            (429, _) => ApiError::Rule("pairingTooMany", "UwUAuth refuses more tries for a while".into()),
            (400, _) => {
                let field = answer.pointer("/detail/field").and_then(Value::as_str).unwrap_or_default();
                ApiError::Rule("pairingRefused", format!("UwUAuth refused the pairing: {error} {field} {message}"))
            }
            _ => ApiError::Rule("uwuauthUnreachable", format!("UwUAuth answered {answered}")),
        });
    }
    let client_id = text(&answer, "clientId");
    let secret = answer.get("clientSecret").and_then(Value::as_str).unwrap_or_default().to_owned();
    let token = answer.get("scimToken").and_then(Value::as_str).unwrap_or_default().to_owned();
    let answered_issuer = text(&answer, "issuer");
    if client_id.is_empty() || secret.is_empty() || token.len() < 16 {
        return Err(ApiError::Rule("pairingRefused", "UwUAuth's answer lacks the client or the SCIM token".into()));
    }
    if answered_issuer.trim_end_matches('/') != discovery.issuer.trim_end_matches('/') {
        return Err(ApiError::Rule("pairingRefused", format!("UwUAuth paired as {answered_issuer}, not {issuer}")));
    }
    let granted: Vec<String> = answer
        .get("scopes")
        .and_then(Value::as_array)
        .map(|scopes| scopes.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .filter(|scopes: &Vec<String>| !scopes.is_empty())
        .unwrap_or(scopes);

    // 5. Into effect.
    let before = pairing(&web).await?;
    let changes = paired_settings(&discovery.issuer, &client_id, &secret, &granted, private);
    let details = super::settings::merge_changes(backend, &mut overlay, &changes)?;
    backend.apply(&overlay).map_err(|err| ApiError::Rule("settingsInvalid", err))?;
    crate::settings::save_overlay(web.store(), &overlay).await?;
    let pairing = Pairing {
        url: base.clone(),
        issuer: discovery.issuer.clone(),
        name: text(&server, "name"),
        version: text(&server, "version"),
        app_id: text(&answer, "appId"),
        manage_url: text(&answer, "manageUrl"),
        paired_at: crate::health::unix_now(),
        paired_by: session.account.login.clone(),
        scim_token_hash: token_hash(&token),
        private_network: private,
    };
    web.store().set_setting(PAIRING_KEY, &serde_json::to_string(&pairing).map_err(|_| ApiError::Internal)?).await?;
    // Another UwUAuth than before: the last one's people and groups are not the new one's. Only
    // now, so a pairing that failed above leaves the old one whole.
    if before.is_some_and(|before| before.issuer.trim_end_matches('/') != pairing.issuer.trim_end_matches('/')) {
        web.store().scim_forget().await?;
    }
    audit(
        &web,
        &session,
        "uwuauth.pair",
        &pairing.issuer,
        json!({ "url": base, "appId": pairing.app_id, "privateNetwork": private, "settings": details }),
    )
    .await;
    tracing::info!(issuer = %pairing.issuer, "paired with UwUAuth");
    self::status(State(web), Admin(session)).await
}

/// Forgets the pairing: logging in with UwUAuth goes off, and SCIM requests are refused. Accounts
/// stay as they are. The app in UwUAuth is deleted there.
pub async fn unpair(State(web): State<Web>, Admin(session): Admin) -> ApiResult<StatusCode> {
    let Some(pairing) = pairing(&web).await? else { return Err(ApiError::NotFound("pairing with UwUAuth".into())) };
    let config = web.external_login().config();
    // Only the settings pairing made: an issuer the admin changed since stays.
    if config.oidc.issuer.trim().trim_end_matches('/') == pairing.issuer.trim_end_matches('/') {
        let backend = backend(&web)?;
        let mut overlay = super::settings::load_overlay(&web).await?;
        let changes: Map<String, Value> = PAIRED_SETTINGS.iter().map(|key| ((*key).to_owned(), Value::Null)).collect();
        super::settings::merge_changes(backend, &mut overlay, &changes)?;
        backend.apply(&overlay).map_err(|err| ApiError::Rule("settingsInvalid", err))?;
        crate::settings::save_overlay(web.store(), &overlay).await?;
    }
    web.store().delete_setting(PAIRING_KEY).await?;
    web.store().scim_forget().await?;
    audit(&web, &session, "uwuauth.unpair", &pairing.issuer, json!({ "appId": pairing.app_id })).await;
    tracing::info!(issuer = %pairing.issuer, "unpaired from UwUAuth");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_qr_text_counts_as_address_and_code() {
        assert_eq!(
            pairing_input(None, " https://auth.example.com/#pair=7KQ4-M2XD-9HFT "),
            Ok(("https://auth.example.com".into(), "7KQ4-M2XD-9HFT".into()))
        );
        assert_eq!(
            pairing_input(Some("https://auth.example.com/"), "7kq4 m2xd 9hft"),
            Ok(("https://auth.example.com".into(), "7kq4 m2xd 9hft".into()))
        );
        assert!(pairing_input(None, "7KQ4-M2XD-9HFT").is_err(), "an address is needed");
        assert!(pairing_input(Some("https://auth.example.com/?x=1"), "7KQ4").is_err());
        assert!(pairing_input(Some("https://auth.example.com"), "").is_err());
        assert!(pairing_input(Some("not an address"), "7KQ4").is_err());
    }

    #[test]
    fn tokens_are_kept_as_their_hash() {
        assert_eq!(token_hash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
