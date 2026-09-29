//! Logging in to the portal through another OpenID Connect provider or an LDAP directory
//! (docs/login-oidc-ldap.md), and the admin's side of it: trying the settings, and which accounts
//! check their password at the directory.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uwumail_jmap::ClientInfo;
use uwumail_store::{Account, AuditEntry, NewAccount, Role};

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::external::{AuthConfig, domain_allowed, ldap, oidc};
use crate::session::Admin;

const STATE_COOKIE: &str = "uwumail-oidc";
const SECURE_STATE_COOKIE: &str = "__Host-uwumail-oidc";

/// Where the provider sends the browser back to.
fn redirect_uri(web: &Web) -> String {
    format!("https://{}/api/auth/oidc/callback", web.settings().hostname)
}

/// A path on this server and nothing else, like the login page's `?next=`.
fn safe_next(next: Option<&str>) -> Option<String> {
    let next = next?.trim();
    let safe = next.starts_with('/')
        && !next.starts_with("//")
        && !next.contains('\\')
        // Room for a whole OAuth authorization request coming back to the consent page.
        && next.len() <= 2048
        && !next.chars().any(char::is_control);
    safe.then(|| next.to_owned())
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A page that moves on to `target` by itself. Not an HTTP redirect: coming back from the provider
/// is a navigation from another site, and a `SameSite=Strict` cookie set on it is only sent with
/// the next request once this page (on our own site) asks for it.
pub(super) fn onward(target: &str, cookies: Vec<HeaderValue>) -> Response {
    let target = escape_html(target);
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta http-equiv=\"refresh\" content=\"0;url={target}\">\
         <title>UwUMail</title></head><body><p><a href=\"{target}\">Continue</a></p></body></html>"
    );
    let mut response = (StatusCode::OK, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response();
    for cookie in cookies {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    super::auth::no_store(response)
}

fn failure(code: &str, cookies: Vec<HeaderValue>) -> Response {
    onward(&format!("/login?oidcError={code}"), cookies)
}

fn state_cookie(client: ClientInfo, value: &str, max_age: u32) -> HeaderValue {
    let cookie = if client.https {
        format!("{SECURE_STATE_COOKIE}={value}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Lax")
    } else {
        format!("{STATE_COOKIE}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Lax")
    };
    HeaderValue::from_str(&cookie).expect("states are valid header values")
}

fn sent_state(headers: &HeaderMap, client: ClientInfo) -> Option<String> {
    let name = if client.https { SECURE_STATE_COOKIE } else { STATE_COOKIE };
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(cookie, _)| *cookie == name)
        .map(|(_, value)| value.to_owned())
}

/// Whether accounts can be made on a domain: one of ours, and not one only for masked addresses.
async fn takes_people(web: &Web, domain: &str) -> bool {
    matches!(web.store().domain_kind(domain).await, Ok(uwumail_store::DomainKind::Mail))
}

/// Makes the account for someone who logged in elsewhere, and says so in the change log.
async fn create_account(
    web: &Web,
    address: &str,
    name: &str,
    admin: bool,
    source: &str,
    ip: &str,
) -> ApiResult<Account> {
    let account = web
        .store()
        .create_account(NewAccount {
            address: address.to_owned(),
            display_name: name.to_owned(),
            password: None,
            role: if admin { Role::Admin } else { Role::User },
            quota_bytes: 0,
            protocols: None,
        })
        .await?;
    web.store().set_auth_source(&account.login, source).await?;
    tracing::info!(login = %account.login, source, admin, "account created by a login elsewhere");
    let entry = AuditEntry {
        actor_id: None,
        actor: source.to_owned(),
        action: "account.create".into(),
        target: account.login.clone(),
        details: json!({ "via": source, "admin": admin }),
        ip: ip.to_owned(),
    };
    if let Err(err) = web.store().record_audit(entry).await {
        tracing::error!(%err, "writing the change log failed");
    }
    Ok(web.store().account_by_id(account.id).await?.unwrap_or(account))
}

/// A login the directory knows, for an address without an account here: the account is made when
/// the settings allow it. Accounts that exist are checked by the store itself.
pub(crate) async fn ldap_account(web: &Web, login: &str, password: &str) -> ApiResult<Option<Account>> {
    let config = web.external_login().config();
    if !config.ldap.enabled || !config.ldap.auto_create || password.is_empty() {
        return Ok(None);
    }
    let Ok(address) = uwumail_store::normalize_address(login) else { return Ok(None) };
    let address = format!("{}@{}", address.0, address.1);
    if !domain_allowed(&config.ldap.allowed_domains, &address)
        || !takes_people(web, address.rsplit_once('@').map(|(_, d)| d).unwrap_or_default()).await
        || web.store().account(&address).await?.is_some()
    {
        return Ok(None);
    }
    let user = match web.external_login().ldap_login(&address, password).await {
        // The directory has to know this address for the person, when it keeps addresses.
        Ok(Some(user)) if user.has_address(&address) => user,
        Ok(_) => return Ok(None),
        Err(err) => {
            tracing::warn!(%err, "the directory could not be asked");
            return Ok(None);
        }
    };
    let account = create_account(web, &address, &user.name, user.admin, "ldap", "").await?;
    Ok(Some(account))
}

#[derive(Deserialize)]
pub struct Start {
    next: Option<String>,
}

/// Sends the browser to the provider, with a state it has to bring back to this very browser.
pub async fn oidc_start(
    State(web): State<Web>,
    client: Option<Extension<ClientInfo>>,
    Query(start): Query<Start>,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let next = safe_next(start.next.as_deref());
    let transport = web.oidc_transport();
    match web.external_login().oidc_start(transport.as_ref(), &redirect_uri(&web), next).await {
        Ok((url, pending)) => {
            let mut response = (StatusCode::SEE_OTHER, [(header::LOCATION, url)]).into_response();
            response.headers_mut().insert(header::SET_COOKIE, state_cookie(client, &pending, 600));
            super::auth::no_store(response)
        }
        Err(err) => {
            tracing::warn!(%err, "starting a login at the OpenID Connect provider failed");
            failure("unavailable", Vec::new())
        }
    }
}

#[derive(Deserialize)]
pub struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// The provider sends the browser back: find the account, then log in (or ask for the second factor).
pub async fn oidc_callback(
    State(web): State<Web>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    Query(callback): Query<Callback>,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let clear = vec![state_cookie(client, "", 0)];
    if callback.error.is_some() {
        return failure("refused", clear);
    }
    let Some(code) = callback.code.filter(|code| !code.is_empty() && code.len() < 4096) else {
        return failure("refused", clear);
    };
    let state = callback.state.unwrap_or_default();
    let pending = sent_state(&headers, client).unwrap_or_default();
    let transport = web.oidc_transport();
    let redirect = redirect_uri(&web);
    let identity = match web.external_login().oidc_finish(transport.as_ref(), &redirect, &state, &pending, &code).await
    {
        Ok(identity) => identity,
        Err(oidc::OidcFailure::Expired) => return failure("expired", clear),
        Err(oidc::OidcFailure::Failed(reason)) => {
            tracing::warn!(%reason, "a login at the OpenID Connect provider did not check out");
            return failure("failed", clear);
        }
    };
    match finish_oidc(&web, client, &headers, identity, clear.clone()).await {
        Ok(response) => response,
        Err(err) => {
            tracing::error!(?err, "finishing a login at the OpenID Connect provider failed");
            failure("failed", clear)
        }
    }
}

async fn finish_oidc(
    web: &Web,
    client: ClientInfo,
    headers: &HeaderMap,
    identity: oidc::OidcIdentity,
    clear: Vec<HeaderValue>,
) -> ApiResult<Response> {
    let store = web.store();
    let config = web.external_login().config();
    let ip = client.ip.to_string();
    let account = match store.external_identity(&identity.issuer, &identity.subject).await? {
        Some(id) => store.account_by_id(id).await?,
        None => {
            // The first time: an account with the same address, but only when the provider vouches
            // for the address. Otherwise anyone could claim anyone's address there.
            let Some(email) = identity.email.clone().filter(|_| identity.email_verified) else {
                return Ok(failure("emailNotVerified", clear));
            };
            let account = match store.account(&email).await? {
                Some(account) => Some(account),
                None if config.oidc.auto_create => {
                    let domain = email.rsplit_once('@').map(|(_, domain)| domain).unwrap_or_default();
                    if !domain_allowed(&config.oidc.allowed_domains, &email) || !takes_people(web, domain).await {
                        return Ok(failure("domainNotAllowed", clear));
                    }
                    Some(create_account(web, &email, &identity.name, identity.admin, "oidc", &ip).await?)
                }
                None => None,
            };
            if let Some(account) = account.as_ref().filter(|account| account.can_use_portal()) {
                if !store.link_external_identity(account.id, &identity.issuer, &identity.subject, &email).await? {
                    tracing::warn!(login = %account.login, issuer = %identity.issuer, "a second login at the provider claimed an account");
                    return Ok(failure("alreadyLinked", clear));
                }
                let event = uwumail_store::SecurityEvent {
                    kind: "oidcLinked".into(),
                    actor: String::new(),
                    ip: ip.clone(),
                    details: json!({ "issuer": identity.issuer }),
                };
                store.record_security_event(account.id, event).await?;
            }
            account
        }
    };
    let Some(account) = account.filter(Account::can_use_portal) else {
        return Ok(failure("noAccount", clear));
    };

    let security = store.security_overview(account.id).await?;
    let next = identity.next.clone();
    if security.second_factor {
        // A second factor set up here still counts: the login page asks for it.
        let token = web.login_state().start_via(account.id, "oidc");
        let mut methods = Vec::new();
        if security.totp {
            methods.push("totp");
        }
        if security.passkeys > 0 {
            methods.push("passkey");
        }
        if security.recovery_codes_left > 0 {
            methods.push("recovery");
        }
        let mut target = url::form_urlencoded::Serializer::new(String::new());
        target.append_pair("pending", &token).append_pair("methods", &methods.join(","));
        if let Some(next) = &next {
            target.append_pair("next", next);
        }
        return Ok(onward(&format!("/login?{}", target.finish()), clear));
    }
    web.limiter().record_success(client.ip, &account.login);
    let (cookie, _) = super::auth::start_session(web, &account, client, headers, "oidc").await?;
    let target = next.unwrap_or_else(|| {
        if super::webmail::allowed_for(web, &account) { "/mail".to_owned() } else { "/account".to_owned() }
    });
    let mut cookies = clear;
    cookies.push(cookie);
    Ok(onward(&target, cookies))
}

/// Settings changes not saved yet, the way the Loki test takes them.
#[derive(Deserialize)]
pub struct Changes {
    #[serde(default)]
    changes: Map<String, Value>,
}

/// The settings to try: the saved ones with the changes not saved yet.
async fn settings_to_try(web: &Web, changes: &Map<String, Value>) -> ApiResult<AuthConfig> {
    let Some(backend) = web.settings().config.as_deref() else {
        return Ok((*web.external_login().config()).clone());
    };
    let mut overlay = super::settings::load_overlay(web).await?;
    super::settings::merge_changes(backend, &mut overlay, changes)?;
    backend.auth_config(&overlay).map_err(|err| ApiError::Rule("settingsInvalid", err))
}

/// Tries the directory settings: connection, TLS, service account and search base.
pub async fn test_ldap(State(web): State<Web>, _admin: Admin, Json(request): Json<Changes>) -> ApiResult<Json<Value>> {
    let config = settings_to_try(&web, &request.changes).await?;
    let result = ldap::test(&config.ldap).await.map_err(|err| ApiError::Rule("ldapFailed", err))?;
    Ok(Json(json!({ "ok": true, "detail": result })))
}

/// Tries the provider settings: discovery document and keys.
pub async fn test_oidc(State(web): State<Web>, _admin: Admin, Json(request): Json<Changes>) -> ApiResult<Json<Value>> {
    let config = settings_to_try(&web, &request.changes).await?;
    let transport = web.oidc_transport();
    let result = oidc::test(transport.as_ref(), &config.oidc).await.map_err(|err| ApiError::Rule("oidcFailed", err))?;
    Ok(Json(json!({ "ok": true, "detail": result, "redirectUri": redirect_uri(&web) })))
}

#[derive(Deserialize)]
pub struct AuthSource {
    source: String,
}

/// Where a person's password is checked: here (`local`) or at the directory (`ldap`).
pub async fn set_auth_source(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
    Json(request): Json<AuthSource>,
) -> ApiResult<StatusCode> {
    if !matches!(request.source.as_str(), "local" | "ldap") {
        return Err(ApiError::Invalid("the source is local or ldap".into()));
    }
    let account = web.store().account(&login).await?.ok_or_else(|| ApiError::NotFound(format!("person {login}")))?;
    if account.is_service() {
        return Err(ApiError::Rule("serviceAccount", "a service signs in with app passwords only".into()));
    }
    // Without a directory to ask, nobody moved there could log in any more.
    if request.source == "ldap" && !web.external_login().config().ldap.enabled {
        return Err(ApiError::Rule("ldapOff", "switch logging in with LDAP on first".into()));
    }
    web.store().set_auth_source(&account.login, &request.source).await?;
    audit(&web, &session, "account.authSource", &account.login, json!({ "source": request.source })).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_paths_here_come_next() {
        assert_eq!(safe_next(Some("/mail")), Some("/mail".into()));
        assert_eq!(safe_next(Some("//elsewhere.example")), None);
        assert_eq!(safe_next(Some("https://elsewhere.example")), None);
        assert_eq!(safe_next(Some("/\\elsewhere.example")), None);
        assert_eq!(escape_html("/a?b=1&c=\"x\""), "/a?b=1&amp;c=&quot;x&quot;");
    }
}
