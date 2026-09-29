//! "Fetched mailboxes" in My account: mailboxes at other providers that this server empties into
//! the person's own mailbox.
//!
//! The password belongs to the provider, not to us. It goes in and never comes out again: no
//! endpoint here returns it, and an update that leaves it out keeps the one that is stored.
//!
//! Microsoft and Google are signed in at instead (docs/fetch.md, "Microsoft and Google"): the
//! device code flow for Microsoft, the authorization code flow for Google, whose way back is
//! [`oauth_callback`]. A sign-in is proven by a real login, like a password, before it is saved.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_jmap::ClientInfo;
use uwumail_smtp::provider_oauth::{
    CALLBACK_PATH, FlowPoll, Granted, Provider, ProviderServers, provider_of_domain, provider_of_mx,
};
use uwumail_store::{
    AfterFetch, FetchAccount, FetchAccountUpdate, FetchGrant, FetchSecurity, NewFetchAccount, SendSecurity,
};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Session;

/// The cookie that ties Google's way back to the browser that set off: the session cookie is
/// `SameSite=Strict` and does not come along on a navigation from Google.
const BINDING_COOKIE: &str = "uwumail-fetch-oauth";
const SECURE_BINDING_COOKIE: &str = "__Host-uwumail-fetch-oauth";

/// The usual IMAP port with TLS from the first byte.
const DEFAULT_PORT: u16 = 993;

fn security_of(value: Option<&str>) -> ApiResult<FetchSecurity> {
    match value {
        None => Ok(FetchSecurity::Tls),
        Some(value) => FetchSecurity::parse(value)
            .ok_or_else(|| ApiError::Rule("badSecurity", format!("'{value}' is not a way to connect"))),
    }
}

/// What to do at the provider, as the page spells it -- the same words the API sends back.
fn after_of(value: Option<&str>) -> ApiResult<AfterFetch> {
    match value {
        None | Some("markRead") => Ok(AfterFetch::MarkRead),
        Some("delete") => Ok(AfterFetch::Delete),
        Some(value) => Err(ApiError::Rule("badAfterFetch", format!("'{value}' is not something to do"))),
    }
}

/// Who runs a mailbox's mail, when it is Microsoft or Google: by the address's domain, and failing
/// that by its mail servers (Microsoft 365, Google Workspace). `consumer` is Outlook.com and Hotmail.
async fn provider_of(web: &Web, address: &str) -> Option<(Provider, bool)> {
    let (_, domain) = address.trim().rsplit_once('@')?;
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() || !domain.contains('.') {
        return None;
    }
    if let Some(found) = provider_of_domain(&domain) {
        return Some(found);
    }
    provider_of_mx(&web.smtp().mx_hosts(&domain).await)
}

/// Which provider a mailbox that logs in with a password could sign in at instead: its domain, or
/// the servers it was set up with.
fn sign_in_for(account: &FetchAccount) -> Option<Provider> {
    if account.auth.is_oauth() {
        return None;
    }
    if account.password_refused {
        return Some(Provider::Microsoft);
    }
    let domain = account.address.rsplit_once('@').map(|(_, domain)| domain).unwrap_or_default();
    if let Some((provider, _)) = provider_of_domain(domain) {
        return Some(provider);
    }
    let host = account.host.to_ascii_lowercase();
    if host.ends_with(".outlook.com") || host.ends_with("office365.com") {
        Some(Provider::Microsoft)
    } else if host == "imap.gmail.com" || host == "imap.googlemail.com" {
        Some(Provider::Google)
    } else {
        None
    }
}

fn redirect_uri(web: &Web) -> String {
    format!("https://{}{CALLBACK_PATH}", web.settings().hostname)
}

/// Everything the page shows, plus what it needs to keep its own limits.
pub async fn list(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    let accounts = web.store().fetch_accounts(Some(session.account.id)).await?;
    let oauth = web.smtp().provider_oauth();
    let accounts: Vec<Value> = accounts
        .iter()
        .map(|account| {
            let mut value = json!(account);
            value["signIn"] = json!(sign_in_for(account));
            value
        })
        .collect();
    Ok(Json(json!({
        "accounts": accounts,
        "signIn": {
            "microsoft": oauth.ready(Provider::Microsoft),
            "google": oauth.ready(Provider::Google),
            "redirectUri": redirect_uri(&web),
        },
        "max": uwumail_store::MAX_FETCH_ACCOUNTS,
        "defaultPort": DEFAULT_PORT,
        "defaultIntervalSecs": uwumail_store::DEFAULT_FETCH_INTERVAL_SECS,
        "minIntervalSecs": uwumail_store::MIN_FETCH_INTERVAL_SECS,
        "maxIntervalSecs": uwumail_store::MAX_FETCH_INTERVAL_SECS,
    })))
}

#[derive(Deserialize)]
pub struct Unknown {
    address: String,
    password: String,
}

/// Works out how a provider's mailbox is reached, from the address and the password alone, so
/// nobody has to know what their provider calls its servers.
///
/// Nothing here is guessed at the person: the settings come back only once a login has really
/// worked with them, so the page can fill its fields with something that has been tried rather
/// than with something that sounded likely. What did not work comes back as a code the page turns
/// into a sentence -- which of the four sources answered is not the person's business, and saying
/// "your password is wrong" only when the server said so keeps the two apart.
pub async fn discover(
    State(web): State<Web>,
    session: Session,
    Json(unknown): Json<Unknown>,
) -> ApiResult<Json<Value>> {
    // Every mailbox this person has already set up counts, so the discovery cannot be used to knock
    // on providers' doors past the limit that holds for keeping one.
    let held = web.store().fetch_accounts(Some(session.account.id)).await?.len();
    if held >= uwumail_store::MAX_FETCH_ACCOUNTS {
        return Err(ApiError::Rule(
            "fetchLimit",
            format!("at most {} fetched mailboxes", uwumail_store::MAX_FETCH_ACCOUNTS),
        ));
    }
    let found =
        uwumail_smtp::autoconfig::discover(web.smtp(), web.dns(), &unknown.address, &unknown.password, true).await;
    match found {
        Ok(settings) => Ok(Json(json!(settings))),
        Err(code) => Err(match code.as_str() {
            "wrongPassword" => ApiError::Rule("wrongPassword", "the provider refused this password".into()),
            "passwordsRefused" => ApiError::Rule(
                "passwordsRefused",
                "Microsoft no longer accepts passwords for this mailbox; sign in with Microsoft instead".into(),
            ),
            "notAnAddress" => ApiError::Rule("senderInvalid", format!("'{}' is not an address", unknown.address)),
            _ => ApiError::Rule("providerNotFound", "no settings of this provider answered".into()),
        }),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMailbox {
    address: String,
    host: String,
    port: Option<u16>,
    security: Option<String>,
    /// Usually the address itself; some providers want something else.
    username: Option<String>,
    password: String,
    after_fetch: Option<String>,
    fetch_junk: Option<bool>,
    interval_secs: Option<i64>,
    auth_serv_id: Option<String>,
    /// Whether the mail that is already in the mailbox comes too, not only what arrives from now on.
    take_existing: Option<bool>,
    /// A proven sign-in at Microsoft or Google to log in with, instead of a password. The servers
    /// then come from it, whatever else says.
    oauth_flow: Option<String>,
}

/// Takes a proven sign-in of this person out, for the mailbox with `address` (and, when it switches
/// an existing one, that one). A sign-in with another account than the address typed would not have
/// proven, so this is about a flow meant for something else.
fn take_sign_in(web: &Web, session: &Session, flow: &str, address: &str, switch_id: Option<i64>) -> ApiResult<Granted> {
    let granted = web
        .smtp()
        .provider_oauth()
        .take(session.account.id, flow)
        .ok_or_else(|| ApiError::Rule("signInExpired", "the sign-in expired; sign in again".into()))?;
    if !granted.address.trim().eq_ignore_ascii_case(address.trim()) || granted.switch_id != switch_id {
        return Err(ApiError::Rule("signInExpired", "the sign-in was for another mailbox".into()));
    }
    Ok(granted)
}

fn sending_of_settings(settings: &uwumail_smtp::autoconfig::Settings) -> (Option<String>, Option<u16>, Option<SendSecurity>) {
    match &settings.smtp {
        Some(server) => (
            Some(server.host.clone()),
            Some(server.port),
            Some(match server.security {
                uwumail_smtp::autoconfig::Security::Tls => SendSecurity::Tls,
                uwumail_smtp::autoconfig::Security::Starttls => SendSecurity::Starttls,
            }),
        ),
        None => (None, None, None),
    }
}

pub async fn create(
    State(web): State<Web>,
    session: Session,
    Json(new): Json<NewMailbox>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if let Some(flow) = new.oauth_flow.as_deref() {
        let granted = take_sign_in(&web, &session, flow, &new.address, None)?;
        let settings = granted.settings.clone().ok_or(ApiError::Internal)?;
        let created = web
            .store()
            .create_fetch_account_with(
                NewFetchAccount {
                    account_id: session.account.id,
                    address: granted.address.clone(),
                    host: settings.imap.host.clone(),
                    port: settings.imap.port,
                    security: FetchSecurity::Tls,
                    username: granted.address.clone(),
                    password: String::new(),
                    after_fetch: after_of(new.after_fetch.as_deref())?,
                    fetch_junk: new.fetch_junk.unwrap_or(true),
                    interval_secs: new.interval_secs.unwrap_or(uwumail_store::DEFAULT_FETCH_INTERVAL_SECS),
                    auth_serv_id: new.auth_serv_id.unwrap_or_default(),
                },
                Some(FetchGrant { provider: granted.provider.auth(), tokens: granted.tokens }),
            )
            .await?;
        // The outgoing server the sign-in proved is kept right away; answering from the address
        // still waits for one successful fetch, as with a password.
        let (smtp_host, smtp_port, smtp_security) = sending_of_settings(&settings);
        if smtp_host.is_some() {
            let update = FetchAccountUpdate { smtp_host, smtp_port, smtp_security, ..Default::default() };
            web.store().update_fetch_account(session.account.id, created.id, update).await?;
        }
        if new.take_existing == Some(true) {
            web.store().request_fetch_backlog(session.account.id, created.id).await?;
        }
        let saved = web.store().fetch_account(session.account.id, created.id).await?.unwrap_or(created);
        return Ok((StatusCode::CREATED, Json(json!(saved))));
    }
    let username = new.username.unwrap_or_else(|| new.address.clone());
    let created = web
        .store()
        .create_fetch_account(NewFetchAccount {
            account_id: session.account.id,
            address: new.address,
            host: new.host,
            port: new.port.unwrap_or(DEFAULT_PORT),
            security: security_of(new.security.as_deref())?,
            username,
            password: new.password,
            after_fetch: after_of(new.after_fetch.as_deref())?,
            fetch_junk: new.fetch_junk.unwrap_or(true),
            interval_secs: new.interval_secs.unwrap_or(uwumail_store::DEFAULT_FETCH_INTERVAL_SECS),
            auth_serv_id: new.auth_serv_id.unwrap_or_default(),
        })
        .await?;
    if new.take_existing == Some(true) {
        web.store().request_fetch_backlog(session.account.id, created.id).await?;
        let asked = web.store().fetch_account(session.account.id, created.id).await?.unwrap_or(created);
        return Ok((StatusCode::CREATED, Json(json!(asked))));
    }
    Ok((StatusCode::CREATED, Json(json!(created))))
}

/// Brings over the mail that was already in the mailbox, for one that was set up without it. The
/// next runs work through it next to the new mail; what is already here is not brought twice.
pub async fn take_existing(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    web.store().request_fetch_backlog(session.account.id, id).await?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Changes {
    host: Option<String>,
    port: Option<u16>,
    security: Option<String>,
    username: Option<String>,
    /// Left out to keep the password that is stored.
    password: Option<String>,
    after_fetch: Option<String>,
    fetch_junk: Option<bool>,
    interval_secs: Option<i64>,
    enabled: Option<bool>,
    auth_serv_id: Option<String>,
    /// Where the provider takes outgoing mail, for answering from this address.
    smtp_host: Option<String>,
    smtp_port: Option<u16>,
    smtp_security: Option<String>,
    send_enabled: Option<bool>,
    /// A proven sign-in at Microsoft or Google: the mailbox logs in with it from now on, and its
    /// password is forgotten.
    oauth_flow: Option<String>,
}

/// How the provider's outgoing server is reached. Never unencrypted.
fn sending_of(value: Option<&str>) -> ApiResult<SendSecurity> {
    match value {
        None | Some("starttls") => Ok(SendSecurity::Starttls),
        Some("tls") => Ok(SendSecurity::Tls),
        Some(value) => Err(ApiError::Rule("badSecurity", format!("'{value}' is not a way to connect"))),
    }
}

// Whose mailbox it is goes into every call below, so the id from the URL can only ever reach one
// of the caller's own: the store asks for both and finds nothing otherwise.
pub async fn update(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
    Json(changes): Json<Changes>,
) -> ApiResult<Json<Value>> {
    let mut signed_in = None;
    if let Some(flow) = changes.oauth_flow.as_deref() {
        let account = web
            .store()
            .fetch_account(session.account.id, id)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("fetched mailbox {id}")))?;
        signed_in = Some(take_sign_in(&web, &session, flow, &account.address, Some(id))?);
    }
    if let Some(granted) = signed_in {
        let settings = granted.settings.clone().ok_or(ApiError::Internal)?;
        let (smtp_host, smtp_port, smtp_security) = sending_of_settings(&settings);
        let update = FetchAccountUpdate {
            host: Some(settings.imap.host.clone()),
            port: Some(settings.imap.port),
            security: Some(FetchSecurity::Tls),
            username: Some(granted.address.clone()),
            smtp_host,
            smtp_port,
            smtp_security,
            oauth: Some(FetchGrant { provider: granted.provider.auth(), tokens: granted.tokens }),
            ..Default::default()
        };
        return Ok(Json(json!(web.store().update_fetch_account(session.account.id, id, update).await?)));
    }
    let update = FetchAccountUpdate {
        host: changes.host,
        port: changes.port,
        security: changes.security.as_deref().map(|value| security_of(Some(value))).transpose()?,
        username: changes.username,
        password: changes.password,
        after_fetch: changes.after_fetch.as_deref().map(|value| after_of(Some(value))).transpose()?,
        fetch_junk: changes.fetch_junk,
        interval_secs: changes.interval_secs,
        enabled: changes.enabled,
        auth_serv_id: changes.auth_serv_id,
        smtp_host: changes.smtp_host,
        smtp_port: changes.smtp_port,
        smtp_security: changes.smtp_security.as_deref().map(|value| sending_of(Some(value))).transpose()?,
        send_enabled: changes.send_enabled,
        oauth: None,
    };
    Ok(Json(json!(web.store().update_fetch_account(session.account.id, id, update).await?)))
}

pub async fn delete(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    web.store().delete_fetch_account(session.account.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Fetches now instead of waiting for the interval. The run itself happens in the background, so
/// this only moves it to the front of the queue; the page shows what came of it afterwards.
pub async fn fetch_now(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    web.store().fetch_account_due_now(session.account.id, id).await?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
pub struct Detect {
    address: String,
}

/// Whether an address is at Microsoft or Google, for the dialog to offer signing in there first.
pub async fn detect(State(web): State<Web>, _session: Session, Json(detect): Json<Detect>) -> ApiResult<Json<Value>> {
    let found = provider_of(&web, &detect.address).await;
    let oauth = web.smtp().provider_oauth();
    Ok(Json(json!({
        "provider": found.map(|(provider, _)| provider),
        "ready": found.is_some_and(|(provider, _)| oauth.ready(provider)),
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartSignIn {
    address: String,
    provider: Provider,
    /// The fetched mailbox that switches to signing in, when it is not a new one.
    switch_id: Option<i64>,
}

fn binding_cookie(client: ClientInfo, value: &str, max_age: u32) -> HeaderValue {
    let cookie = if client.https {
        format!("{SECURE_BINDING_COOKIE}={value}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Lax")
    } else {
        format!("{BINDING_COOKIE}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Lax")
    };
    HeaderValue::from_str(&cookie).expect("bindings are valid header values")
}

fn sent_binding(headers: &HeaderMap, client: ClientInfo) -> String {
    let name = if client.https { SECURE_BINDING_COOKIE } else { BINDING_COOKIE };
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(cookie, _)| *cookie == name)
        .map(|(_, value)| value.to_owned())
        .unwrap_or_default()
}

/// Starts signing in at Microsoft (a code to type at Microsoft's page) or Google (an address to send
/// the browser to), for a new mailbox or for one that switches from its password.
pub async fn start_sign_in(
    State(web): State<Web>,
    session: Session,
    client: Option<Extension<ClientInfo>>,
    Json(start): Json<StartSignIn>,
) -> ApiResult<Response> {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let address = start.address.trim().to_lowercase();
    let (_, domain) = uwumail_store::normalize_address(&address)
        .map_err(|_| ApiError::Rule("senderInvalid", format!("'{address}' is not an address")))?;
    match start.switch_id {
        Some(id) => {
            let account = web
                .store()
                .fetch_account(session.account.id, id)
                .await?
                .ok_or_else(|| ApiError::NotFound(format!("fetched mailbox {id}")))?;
            if !account.address.eq_ignore_ascii_case(&address) {
                return Err(ApiError::Invalid("the sign-in is for another address".into()));
            }
        }
        None => {
            // Signing in knocks at the provider as much as a discovery does: the same limit.
            let held = web.store().fetch_accounts(Some(session.account.id)).await?.len();
            if held >= uwumail_store::MAX_FETCH_ACCOUNTS {
                return Err(ApiError::Rule(
                    "fetchLimit",
                    format!("at most {} fetched mailboxes", uwumail_store::MAX_FETCH_ACCOUNTS),
                ));
            }
            if web.store().is_local_domain(&domain).await? {
                return Err(ApiError::Invalid(
                    "this address is hosted on this server; a fetched mailbox is for a mailbox elsewhere".into(),
                ));
            }
        }
    }
    let oauth = web.smtp().provider_oauth();
    let refused = |code: String| match code.as_str() {
        "oauthNotConfigured" => ApiError::Rule("oauthNotConfigured", "this server has no client for the provider".into()),
        "oauthClientRejected" => {
            ApiError::Rule("oauthClientRejected", "the provider refused this server's client ID".into())
        }
        "busy" => ApiError::Busy,
        _ => ApiError::Rule("providerUnreachable", "the provider's sign-in could not be reached".into()),
    };
    match start.provider {
        Provider::Microsoft => {
            let consumer = matches!(provider_of_domain(&domain), Some((Provider::Microsoft, true)));
            let started = oauth.start_microsoft(session.account.id, &address, consumer, start.switch_id).await.map_err(refused)?;
            Ok(Json(json!({ "provider": "microsoft", "device": started })).into_response())
        }
        Provider::Google => {
            let started =
                oauth.start_google(session.account.id, &address, start.switch_id, &redirect_uri(&web)).map_err(refused)?;
            let mut response =
                Json(json!({ "provider": "google", "flowId": started.flow_id, "url": started.url })).into_response();
            response.headers_mut().append(header::SET_COOKIE, binding_cookie(client, &started.binding, 600));
            Ok(response)
        }
    }
}

#[derive(Deserialize)]
pub struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Google sends the browser back. Without the session (its cookie stays behind on a navigation from
/// another site), so the sign-in is tied to the browser by the cookie set when it started, traded in
/// here, and picked up by the portal once the browser is back on this server with its session.
pub async fn oauth_callback(
    State(web): State<Web>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    Query(callback): Query<Callback>,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let clear = vec![binding_cookie(client, "", 0)];
    let back = |query: String| super::external_login::onward(&format!("/account/fetch?{query}"), clear.clone());
    if callback.error.is_some() {
        return back("oauthError=declined".into());
    }
    let (Some(state), Some(code)) = (callback.state, callback.code) else {
        return back("oauthError=declined".into());
    };
    let binding = sent_binding(&headers, client);
    match web.smtp().provider_oauth().finish_google(&state, &code, &binding).await {
        Ok(flow) => back(format!("oauth={}", url::form_urlencoded::byte_serialize(flow.as_bytes()).collect::<String>())),
        Err(code) => back(format!("oauthError={code}")),
    }
}

/// How a sign-in is doing. Once the provider handed out tokens, they are proven here with a real
/// login to its servers, the way a password is, before anything can be saved.
pub async fn sign_in_status(State(web): State<Web>, session: Session, Path(flow): Path<String>) -> ApiResult<Json<Value>> {
    let oauth = web.smtp().provider_oauth();
    let ready = |granted: &Granted| {
        json!({
            "status": "ready",
            "provider": granted.provider,
            "address": granted.address,
            "switchId": granted.switch_id,
            "settings": granted.settings,
        })
    };
    let answer = match oauth.poll(session.account.id, &flow).await {
        FlowPoll::Pending(after) => json!({ "status": "pending", "retryIn": after }),
        FlowPoll::Failed(code) => json!({ "status": "failed", "error": code }),
        FlowPoll::Proven(granted) => ready(&granted),
        FlowPoll::Prove(granted) => {
            let servers = ProviderServers::of(granted.provider, granted.consumer);
            let proof = uwumail_smtp::autoconfig::prove_sign_in(
                web.smtp(),
                &servers,
                &granted.address,
                &granted.tokens.access_token,
            )
            .await;
            oauth.settle(session.account.id, &flow, proof.clone());
            match proof {
                Ok(settings) => ready(&Granted { settings: Some(settings), ..granted }),
                Err(code) => json!({ "status": "failed", "error": code }),
            }
        }
    };
    Ok(Json(answer))
}
