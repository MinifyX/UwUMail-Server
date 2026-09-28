//! The OAuth 2.0 / OpenID Connect provider for mail apps (docs/oauth.md): discovery (RFC 8414 and
//! OpenID Connect Discovery), dynamic registration (RFC 7591), the authorization code flow with
//! PKCE (RFC 6749, RFC 7636), refresh token rotation, revocation (RFC 7009), the signing keys and
//! userinfo. The consent itself is a page of the portal, which asks `/api/oauth/authorize`.
//!
//! Also the list of apps signed in with OAuth, for the person and for their admin.

use std::net::IpAddr;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use data_encoding::BASE64;
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;
use uwumail_jmap::ClientInfo;
use uwumail_store::{
    Account, NewOAuthCode, OAuthClient, OAuthRefusal, OAuthTokens, SecurityEvent, oauth_scopes, oauth_scopes_usable,
    redirect_uri_registered, scopes_for, valid_pkce_challenge,
};

use super::audit;
use super::security::confirm_identity;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::jwt::SigningKey;
use crate::notices::{Notice, Origin, notify};
use crate::session::{Admin, Session};

/// Registrations per network and hour: apps register once per install.
const REGISTRATIONS: Budget = Budget { name: "register", max: 30, window: 3600 };
/// Refused codes, tokens and unknown apps per network, before the endpoints wait.
const FAILURES: Budget = Budget { name: "failures", max: 30, window: 15 * 60 };

/// How many of something a network may do in a while.
#[derive(Clone, Copy)]
pub(crate) struct Budget {
    name: &'static str,
    max: usize,
    window: i64,
}
/// When no scope is asked for: reading and sending mail.
const DEFAULT_SCOPES: &str = "mail smtp";

fn issuer(web: &Web) -> String {
    format!("https://{}", web.settings().hostname)
}

/// Any page may read these answers: they hold nothing a cookie would unlock.
fn open_to_all(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

/// The preflight of a browser app on another site.
pub async fn preflight() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    headers.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST"));
    headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("Authorization, Content-Type"));
    headers.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400"));
    response
}

/// An OAuth error answer (RFC 6749 section 5.2).
fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    let body = json!({ "error": error, "error_description": description });
    let mut response = open_to_all((status, Json(body)).into_response());
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"UwUMail\""));
    }
    response
}

/// Where apps learn everything else: RFC 8414 and OpenID Connect Discovery say the same here.
pub async fn metadata(State(web): State<Web>) -> Response {
    let issuer = issuer(&web);
    let body = json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "registration_endpoint": format!("{issuer}/oauth/register"),
        "revocation_endpoint": format!("{issuer}/oauth/revoke"),
        "jwks_uri": format!("{issuer}/oauth/jwks"),
        "userinfo_endpoint": format!("{issuer}/oauth/userinfo"),
        "scopes_supported": uwumail_store::OAUTH_SCOPES,
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        // Every app is a public client: mail apps run on people's devices and keep no secret.
        "token_endpoint_auth_methods_supported": ["none"],
        "revocation_endpoint_auth_methods_supported": ["none"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "claims_supported": ["sub", "iss", "aud", "exp", "iat", "auth_time", "nonce", "email", "email_verified", "name", "preferred_username"],
        "authorization_response_iss_parameter_supported": true,
        "service_documentation": "https://github.com/MinifyX/UwUMail-Server/blob/main/docs/oauth.md",
    });
    let mut response = open_to_all(Json(body).into_response());
    // Discovery may be cached a little; it changes with the host name only.
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=3600"));
    response
}

async fn signing_key(web: &Web) -> ApiResult<SigningKey> {
    let pkcs8 = web.store().oauth_signing_key().await?;
    SigningKey::from_pkcs8(&pkcs8).map_err(|err| {
        tracing::error!(%err, "the OAuth signing key cannot be used");
        ApiError::Internal
    })
}

pub async fn jwks(State(web): State<Web>) -> ApiResult<Response> {
    let key = signing_key(&web).await?;
    let mut response = open_to_all(Json(json!({ "keys": [key.jwk()] })).into_response());
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=3600"));
    Ok(response)
}

// Registration

#[derive(Deserialize)]
pub struct Registration {
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    client_name: Option<String>,
    #[serde(default)]
    grant_types: Option<Vec<String>>,
    #[serde(default)]
    response_types: Option<Vec<String>>,
}

/// Dynamic client registration (RFC 7591): an app says who it is and where to send people back.
/// Every app becomes a public client: whatever `token_endpoint_auth_method` it asks for, it gets
/// `none` and no secret (RFC 7591 section 2 lets the server replace what it does not offer), and
/// PKCE protects its codes.
pub async fn register(State(web): State<Web>, client: Option<Extension<ClientInfo>>, body: Bytes) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    if !web.oauth_attempt(REGISTRATIONS, client.ip, true) {
        return oauth_error(
            StatusCode::TOO_MANY_REQUESTS,
            "temporarily_unavailable",
            "too many registrations, try later",
        );
    }
    let Ok(request) = serde_json::from_slice::<Registration>(&body) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata", "the registration is not JSON");
    };
    let supported_grants = ["authorization_code", "refresh_token"];
    if request.grant_types.as_ref().is_some_and(|grants| grants.iter().any(|g| !supported_grants.contains(&g.as_str())))
        || request.response_types.as_ref().is_some_and(|types| types.iter().any(|t| t != "code"))
    {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "only the authorization code flow with refresh tokens is offered",
        );
    }
    let name = request.client_name.unwrap_or_default();
    match web.store().register_oauth_client(&name, request.redirect_uris).await {
        Ok(registered) => {
            tracing::info!(client = %registered.client_id, name = %registered.name, ip = %client.ip, "an app registered for OAuth");
            let body = json!({
                "client_id": registered.client_id,
                "client_id_issued_at": registered.created_at,
                "client_name": registered.name,
                "redirect_uris": registered.redirect_uris,
                "token_endpoint_auth_method": "none",
                "grant_types": supported_grants,
                "response_types": ["code"],
            });
            open_to_all((StatusCode::CREATED, Json(body)).into_response())
        }
        Err(uwumail_store::StoreError::Rule { code, message }) => {
            let status = if code == "temporarily_unavailable" {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::BAD_REQUEST
            };
            oauth_error(status, code, &message)
        }
        Err(err) => {
            tracing::error!(%err, "registering an app failed");
            oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", "the app could not be registered")
        }
    }
}

// Authorization, through the portal's consent page

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthorizeParams {
    #[serde(default)]
    response_type: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    redirect_uri: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    code_challenge: Option<String>,
    #[serde(default)]
    code_challenge_method: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
}

/// A request that checked out, with everything the code needs.
struct Checked {
    client: OAuthClient,
    redirect_uri: String,
    scopes: Vec<&'static str>,
    challenge: String,
    /// Errors may go back to the app by themselves (see [`check`]).
    trusted: bool,
}

/// Why a request cannot go on: shown on the page when the app cannot be trusted with the answer,
/// sent back to the app otherwise.
enum Refused {
    Page(&'static str, String),
    App(String),
}

/// The app's redirect address with the answer added (RFC 6749 section 4.1.2, RFC 9207 `iss`).
fn answer(web: &Web, redirect_uri: &str, pairs: &[(&str, &str)], state: Option<&str>) -> String {
    let Ok(mut url) = Url::parse(redirect_uri) else { return redirect_uri.to_owned() };
    {
        let mut query = url.query_pairs_mut();
        for (name, value) in pairs {
            query.append_pair(name, value);
        }
        if let Some(state) = state {
            query.append_pair("state", state);
        }
        query.append_pair("iss", &issuer(web));
    }
    url.to_string()
}

async fn check(web: &Web, account: &Account, params: &AuthorizeParams) -> ApiResult<Result<Checked, Refused>> {
    let Some(client) = web.store().oauth_client(params.client_id.trim()).await? else {
        return Ok(Err(Refused::Page("oauthClientUnknown", "this app is not registered here".into())));
    };
    let redirect_uri = match params.redirect_uri.as_deref().map(str::trim).filter(|uri| !uri.is_empty()) {
        Some(uri) if redirect_uri_registered(&client.redirect_uris, uri) => uri.to_owned(),
        Some(_) => {
            return Ok(Err(Refused::Page(
                "oauthRedirectInvalid",
                "the app asked to go back somewhere it did not register".into(),
            )));
        }
        None if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
        None => return Ok(Err(Refused::Page("oauthRedirectInvalid", "the app did not say where to go back".into()))),
    };
    let state = params.state.as_deref();
    // Anyone can register an app with any https address, so an error only goes back by itself to
    // an app the person allowed in before, or to one on their own device (a loopback address or
    // an app scheme). Anything else would make this page an open redirect: the error is shown
    // instead (RFC 9700 section 4.11.2, WEB-3).
    let trusted = !redirect_is_web(&redirect_uri) || web.store().oauth_consented(account.id, client.id, &[]).await?;
    let refuse = |error: &str, description: &str| {
        if !trusted {
            return Ok(Err(Refused::Page("oauthRequestInvalid", format!("{error}: {description}"))));
        }
        Ok(Err(Refused::App(answer(
            web,
            &redirect_uri,
            &[("error", error), ("error_description", description)],
            state,
        ))))
    };
    if params.response_type != "code" {
        return refuse("unsupported_response_type", "only the authorization code flow is offered");
    }
    let challenge = params.code_challenge.clone().unwrap_or_default();
    if params.code_challenge_method.as_deref() != Some("S256") || !valid_pkce_challenge(&challenge) {
        return refuse("invalid_request", "PKCE with S256 is required");
    }
    if params.nonce.as_ref().is_some_and(|nonce| nonce.len() > 256) || state.is_some_and(|state| state.len() > 1024) {
        return refuse("invalid_request", "the nonce or state is too long");
    }
    // What the app asked for, without protocols this account may not use at all.
    let usable: Vec<&str> = scopes_for(account.protocols)
        .into_iter()
        .map(|scope| match scope {
            uwumail_store::AppScope::Mail => "mail",
            uwumail_store::AppScope::Smtp => "smtp",
            uwumail_store::AppScope::Dav => "dav",
        })
        .collect();
    let asked = params.scope.as_deref().filter(|scope| !scope.trim().is_empty()).unwrap_or(DEFAULT_SCOPES);
    let scopes: Vec<&'static str> = oauth_scopes(asked)
        .into_iter()
        .filter(|scope| !matches!(*scope, "mail" | "smtp" | "dav") || usable.contains(scope))
        .collect();
    if !oauth_scopes_usable(&scopes) {
        return refuse("invalid_scope", "none of the scopes asked for can be given");
    }
    Ok(Ok(Checked { client, redirect_uri, scopes, challenge, trusted }))
}

/// Whether a redirect address leaves the device: an https page anywhere on the web.
fn redirect_is_web(uri: &str) -> bool {
    Url::parse(uri).map_or(true, |url| url.scheme() == "https")
}

fn redirect_host(uri: &str) -> String {
    match Url::parse(uri) {
        Ok(url) => match url.host_str() {
            Some(host) => host.to_owned(),
            None => format!("{}:", url.scheme()),
        },
        Err(_) => uri.to_owned(),
    }
}

/// What the consent page shows: the app, what it asks for, and where the answer goes.
pub async fn authorize_info(
    State(web): State<Web>,
    session: Session,
    Query(params): Query<AuthorizeParams>,
) -> ApiResult<Json<Value>> {
    match check(&web, &session.account, &params).await? {
        Err(Refused::Page(code, detail)) => Err(ApiError::Rule(code, detail)),
        Err(Refused::App(redirect)) => Ok(Json(json!({ "redirect": redirect }))),
        Ok(checked) => {
            let consented = params.prompt.as_deref() != Some("consent")
                && web.store().oauth_consented(session.account.id, checked.client.id, &checked.scopes).await?;
            // An app that asked not to show anything gets its answer right away (OpenID Connect
            // Core section 3.1.2.6): a code only if it was allowed before, which the page then
            // asks for without showing the question.
            if params.prompt.as_deref() == Some("none") && !consented {
                if !checked.trusted {
                    return Err(ApiError::Rule(
                        "oauthRequestInvalid",
                        "consent_required: the person has not allowed this app yet".into(),
                    ));
                }
                let redirect = answer(
                    &web,
                    &checked.redirect_uri,
                    &[("error", "consent_required"), ("error_description", "the person has not allowed this app yet")],
                    params.state.as_deref(),
                );
                return Ok(Json(json!({ "redirect": redirect })));
            }
            Ok(Json(json!({
                "client": {
                    "name": checked.client.name,
                    "clientId": checked.client.client_id,
                    "redirectHost": redirect_host(&checked.redirect_uri),
                },
                "scopes": checked.scopes,
                "consented": consented,
            })))
        }
    }
}

#[derive(Deserialize)]
pub struct Decision {
    #[serde(flatten)]
    params: AuthorizeParams,
    approve: bool,
    /// Letting a new app in needs the password again unless the login is fresh, like a new app
    /// password (WEB-5).
    #[serde(default)]
    password: Option<String>,
}

/// The person's answer: a code for the app, or a refusal. Either way the page sends the browser on.
pub async fn authorize_decide(
    State(web): State<Web>,
    session: Session,
    Json(decision): Json<Decision>,
) -> ApiResult<Json<Value>> {
    let params = &decision.params;
    let checked = match check(&web, &session.account, params).await? {
        Err(Refused::Page(code, detail)) => return Err(ApiError::Rule(code, detail)),
        Err(Refused::App(redirect)) => return Ok(Json(json!({ "redirect": redirect }))),
        Ok(checked) => checked,
    };
    let state = params.state.as_deref();
    if !decision.approve {
        let redirect = answer(
            &web,
            &checked.redirect_uri,
            &[("error", "access_denied"), ("error_description", "the person said no")],
            state,
        );
        return Ok(Json(json!({ "redirect": redirect })));
    }
    if !web.store().oauth_consented(session.account.id, checked.client.id, &checked.scopes).await? {
        confirm_identity(&web, &session, decision.password.as_deref()).await?;
    }
    let code = web
        .store()
        .create_oauth_code(NewOAuthCode {
            client_id: checked.client.id,
            account_id: session.account.id,
            redirect_uri: checked.redirect_uri.clone(),
            scopes: checked.scopes,
            code_challenge: checked.challenge,
            nonce: params.nonce.clone().filter(|nonce| !nonce.is_empty()),
            auth_time: session.created_at,
        })
        .await?;
    tracing::info!(login = %session.account.login, app = %checked.client.name, "an app was allowed in with OAuth");
    Ok(Json(json!({ "redirect": answer(&web, &checked.redirect_uri, &[("code", &code)], state) })))
}

// The token endpoint

fn form(body: &[u8]) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

fn field<'a>(form: &'a [(String, String)], name: &str) -> Option<&'a str> {
    form.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str()).filter(|value| !value.is_empty())
}

/// The app a request comes from: its `client_id` in the form, or as the user name of HTTP Basic
/// (RFC 6749 section 2.3.1), which some libraries send for public clients too. Apps are public
/// clients, so a secret sent along proves nothing and is not looked at.
async fn find_client(web: &Web, headers: &HeaderMap, form: &[(String, String)]) -> Result<OAuthClient, Box<Response>> {
    let invalid =
        || Box::new(oauth_error(StatusCode::UNAUTHORIZED, "invalid_client", "the app is not registered here"));
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
        .and_then(|(_, encoded)| BASE64.decode(encoded.trim().as_bytes()).ok())
        .and_then(|decoded| String::from_utf8(decoded).ok());
    let client_id = match basic {
        Some(pair) => {
            let id = pair.split_once(':').map_or(pair.as_str(), |(id, _)| id);
            url::form_urlencoded::parse(format!("x={id}").as_bytes())
                .next()
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default()
        }
        None => field(form, "client_id").unwrap_or_default().to_owned(),
    };
    if client_id.is_empty() || client_id.len() > 100 {
        return Err(invalid());
    }
    match web.store().oauth_client(&client_id).await {
        Ok(Some(client)) => Ok(client),
        Ok(None) => Err(invalid()),
        Err(err) => {
            tracing::error!(%err, "reading an OAuth app failed");
            Err(Box::new(oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", "try again later")))
        }
    }
}

async fn token_answer(web: &Web, client: &OAuthClient, tokens: &OAuthTokens) -> ApiResult<Response> {
    let mut body = json!({
        "access_token": tokens.access_token,
        "token_type": "Bearer",
        "expires_in": tokens.expires_in,
        "refresh_token": tokens.refresh_token,
        "scope": tokens.scopes.join(" "),
    });
    if tokens.scopes.iter().any(|scope| scope == "openid") {
        let now = crate::health::unix_now();
        let account = &tokens.account;
        let mut claims = json!({
            "iss": issuer(web),
            "sub": account.id.to_string(),
            "aud": client.client_id,
            "azp": client.client_id,
            "iat": now,
            "exp": now + tokens.expires_in,
            "auth_time": tokens.auth_time,
        });
        if let Some(nonce) = &tokens.nonce {
            claims["nonce"] = json!(nonce);
        }
        add_profile_claims(&mut claims, account, &tokens.scopes);
        body["id_token"] = json!(signing_key(web).await?.sign(&claims).map_err(|err| {
            tracing::error!(%err, "signing an ID token failed");
            ApiError::Internal
        })?);
    }
    Ok(open_to_all(Json(body).into_response()))
}

fn add_profile_claims(claims: &mut Value, account: &Account, scopes: &[String]) {
    if scopes.iter().any(|scope| scope == "email") {
        claims["email"] = json!(account.login);
        claims["email_verified"] = json!(true);
    }
    if scopes.iter().any(|scope| scope == "profile") {
        let name = if account.display_name.trim().is_empty() { &account.login } else { &account.display_name };
        claims["name"] = json!(name.trim());
        claims["preferred_username"] = json!(account.login);
    }
}

/// Trades a code or a refresh token in for tokens (RFC 6749 section 3.2).
pub async fn token(
    State(web): State<Web>,
    client_info: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ip = client_info.map(|Extension(c)| c).unwrap_or_default().ip;
    // Wrong codes and tokens slow a network down. Counted apart from logins: an app that keeps
    // trying a token its person revoked must not lock the whole household out of the portal.
    if !web.oauth_attempt(FAILURES, ip, false) {
        return oauth_error(
            StatusCode::TOO_MANY_REQUESTS,
            "temporarily_unavailable",
            "too many failed attempts, try later",
        );
    }
    let form = form(&body);
    let client = match find_client(&web, &headers, &form).await {
        Ok(client) => client,
        Err(response) => {
            web.oauth_attempt(FAILURES, ip, true);
            return *response;
        }
    };
    let result = match field(&form, "grant_type") {
        Some("authorization_code") => {
            let (Some(code), Some(redirect_uri), Some(verifier)) =
                (field(&form, "code"), field(&form, "redirect_uri"), field(&form, "code_verifier"))
            else {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "code, redirect_uri and code_verifier are needed",
                );
            };
            web.store().redeem_oauth_code(code, client.id, redirect_uri, verifier).await
        }
        Some("refresh_token") => {
            let Some(refresh) = field(&form, "refresh_token") else {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "refresh_token is needed");
            };
            web.store().refresh_oauth(refresh, client.id).await
        }
        _ => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                "authorization_code and refresh_token are offered",
            );
        }
    };
    match result {
        Ok(Ok(tokens)) => {
            if tokens.new_grant {
                // notify() writes the activity entry too; a second one here would list the app twice.
                let ip = ip.to_string();
                notify(
                    &web,
                    &tokens.account,
                    Notice::OAuthGranted { name: client.name.clone(), scopes: tokens.scopes.clone() },
                    Origin { actor: "", ip: &ip },
                )
                .await;
            }
            token_answer(&web, &client, &tokens).await.unwrap_or_else(IntoResponse::into_response)
        }
        Ok(Err(OAuthRefusal::Reused { account_id, client_name })) => {
            web.oauth_attempt(FAILURES, ip, true);
            tracing::warn!(client = %client.client_id, %ip, "a used refresh token came back, the grant ended");
            if let Ok(Some(account)) = web.store().account_by_id(account_id).await {
                let ip = ip.to_string();
                notify(&web, &account, Notice::OAuthTokenReused { name: client_name }, Origin { actor: "", ip: &ip })
                    .await;
            }
            oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "the token was used before")
        }
        Ok(Err(OAuthRefusal::InvalidGrant)) => {
            web.oauth_attempt(FAILURES, ip, true);
            oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "the code or token is not valid")
        }
        Err(err) => {
            tracing::error!(%err, "issuing OAuth tokens failed");
            oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", "try again later")
        }
    }
}

/// Revokes a token and with it the app's grant (RFC 7009). Unknown tokens are no error.
pub async fn revoke(
    State(web): State<Web>,
    client_info: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ip = client_info.map(|Extension(c)| c).unwrap_or_default().ip;
    if !web.oauth_attempt(FAILURES, ip, false) {
        return oauth_error(
            StatusCode::TOO_MANY_REQUESTS,
            "temporarily_unavailable",
            "too many failed attempts, try later",
        );
    }
    let form = form(&body);
    let client = match find_client(&web, &headers, &form).await {
        Ok(client) => client,
        Err(response) => {
            web.oauth_attempt(FAILURES, ip, true);
            return *response;
        }
    };
    let Some(token) = field(&form, "token") else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "token is needed");
    };
    match web.store().revoke_oauth_token(token, client.id).await {
        Ok(Some((account_id, name))) => {
            let event = SecurityEvent {
                kind: "oauthRevoked".into(),
                actor: String::new(),
                ip: ip.to_string(),
                details: json!({ "name": name }),
            };
            if let Err(err) = web.store().record_security_event(account_id, event).await {
                tracing::error!(%err, "writing the security activity failed");
            }
            open_to_all(StatusCode::OK.into_response())
        }
        Ok(None) => open_to_all(StatusCode::OK.into_response()),
        Err(err) => {
            tracing::error!(%err, "revoking an OAuth token failed");
            oauth_error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable", "try again later")
        }
    }
}

/// Who the token belongs to (OpenID Connect Core section 5.3), as far as its scopes allow.
pub async fn userinfo(
    State(web): State<Web>,
    client_info: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ip = client_info.map(|Extension(c)| c).unwrap_or_default().ip;
    if !web.oauth_attempt(FAILURES, ip, false) {
        return Err(ApiError::TooManyAttempts);
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim().to_owned())
        .unwrap_or_default();
    let found = web.store().oauth_token_info(&token).await?;
    let Some((account, scopes)) = found.filter(|(_, scopes)| scopes.iter().any(|scope| scope == "openid")) else {
        web.oauth_attempt(FAILURES, ip, true);
        let mut response = open_to_all(StatusCode::UNAUTHORIZED.into_response());
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer error=\"invalid_token\""));
        return Ok(response);
    };
    let mut claims = json!({ "sub": account.id.to_string() });
    add_profile_claims(&mut claims, &account, &scopes);
    Ok(open_to_all(Json(claims).into_response()))
}

// The person's and the admin's list

/// The apps signed in to the account with OAuth.
pub async fn grants(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    Ok(Json(json!(web.store().oauth_grants(session.account.id).await?)))
}

/// Signs an app out: all its tokens stop working.
pub async fn revoke_grant(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let revoked = web.store().revoke_oauth_grant(session.account.id, id).await?;
    let event = SecurityEvent {
        kind: "oauthRevoked".into(),
        actor: String::new(),
        ip: session.client.ip.to_string(),
        details: json!({ "name": revoked.client_name }),
    };
    web.store().record_security_event(session.account.id, event).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// An admin signs one of a person's apps out, e.g. for a lost phone.
pub async fn admin_revoke_grant(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((login, id)): Path<(String, i64)>,
) -> ApiResult<StatusCode> {
    let account = web.store().account(&login).await?.ok_or_else(|| ApiError::NotFound(format!("person {login}")))?;
    let revoked = web.store().revoke_oauth_grant(account.id, id).await?;
    let event = SecurityEvent {
        kind: "oauthRevoked".into(),
        actor: session.account.login.clone(),
        ip: session.client.ip.to_string(),
        details: json!({ "name": revoked.client_name }),
    };
    web.store().record_security_event(account.id, event).await?;
    audit(&web, &session, "account.oauthRevoked", &account.login, json!({ "name": revoked.client_name })).await;
    Ok(StatusCode::NO_CONTENT)
}

impl Web {
    /// Whether a network is within `budget`, counting this attempt when `count` is set (a
    /// registration always, a failure only once it failed).
    pub(crate) fn oauth_attempt(&self, budget: Budget, ip: IpAddr, count: bool) -> bool {
        let now = crate::health::unix_now();
        let network = match ip.to_canonical() {
            IpAddr::V6(v6) => {
                let mut segments = v6.segments();
                segments[4..].fill(0);
                IpAddr::V6(segments.into())
            }
            v4 => v4,
        };
        let mut seen = self.oauth_attempts().lock().expect("OAuth attempts poisoned");
        if seen.len() > 10_000 {
            seen.retain(|(name, _), times| {
                times.last().is_some_and(|last| now - last < if *name == "register" { 3600 } else { 900 })
            });
        }
        let times = seen.entry((budget.name, network)).or_default();
        times.retain(|at| now - at < budget.window);
        if times.len() >= budget.max {
            return false;
        }
        if count {
            times.push(now);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_hosts_are_shown_plainly() {
        assert_eq!(redirect_host("https://app.example.com/cb"), "app.example.com");
        assert_eq!(redirect_host("http://127.0.0.1:4000/"), "127.0.0.1");
        assert_eq!(redirect_host("com.example.mail:/oauth"), "com.example.mail:");
    }
}
