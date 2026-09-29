//! Signing in at Microsoft and Google for fetched mailboxes (docs/fetch.md, "Microsoft and
//! Google").
//!
//! Microsoft has switched plain passwords off for IMAP and SMTP at most mailboxes, Outlook.com and
//! Hotmail included: a login with one is answered "Basic authentication is disabled", app password
//! or not. What still opens them is OAuth 2.0, the token then sent as SASL XOAUTH2. Google takes
//! app passwords still, but only with two-step verification switched on; OAuth works for everyone.
//!
//! * **Microsoft** signs in with the device code flow (RFC 8628): this server is a public client
//!   without a secret, the person types a short code at microsoft.com/devicelogin, and nothing has
//!   to come back to this server's address -- which may not even be reachable from the internet.
//!   MinifyX ships a client ID for it; an admin can register their own and set it instead.
//! * **Google** has no device flow for mail, so it is the authorization code flow with PKCE (RFC
//!   7636) and the admin's own "Web application" client, which comes back to
//!   [`CALLBACK_PATH`] on this server.
//!
//! What comes out is a refresh token, sealed in the store like a password, and short-lived access
//! tokens made from it a little before the last one runs out. A provider that ends the grant makes
//! the mailbox say so and wait for a new sign-in; one that is merely down is asked again later, and
//! later still the more often it failed, never every run.
//!
//! Every request to the providers leaves through the egress the way fetching does, through the
//! proxy when fetching takes it: HTTPS with a valid certificate to public addresses only.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use aws_lc_rs::{constant_time, digest};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
// Tokio's clock, so the waits a sign-in keeps to can be run through in tests.
use tokio::time::Instant;
use uwumail_store::{FetchAuth, FetchTokens, Store};

use crate::egress::{Egress, Purpose};

/// The client ID MinifyX registered with Microsoft Entra for UwUMail: a public client for the device
/// code flow, for personal accounts and every organisation, so signing in at Microsoft works without
/// anything set up. An admin's own (`fetch.oauth.microsoft_client_id`) wins over it.
pub const MICROSOFT_DEFAULT_CLIENT_ID: &str = "f4b09124-76e0-44a5-b675-2b35a898f0d7";

/// Where Google sends the browser back to, on this server.
pub const CALLBACK_PATH: &str = "/api/account/fetch/oauth/callback";

/// IMAP and SMTP with the grant, and a refresh token to keep it (Microsoft's own list).
const MICROSOFT_SCOPES: &str =
    "https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send offline_access";
/// All of Gmail over IMAP and SMTP; Google has no narrower scope for either.
const GOOGLE_SCOPE: &str = "https://mail.google.com/";
/// The longest answer read from a token endpoint. Tokens are a few kilobytes at most.
const MAX_ANSWER: usize = 64 * 1024;
/// An access token is renewed when less than this is left of it, so a run or a delivery never
/// starts with one that runs out halfway.
const RENEW_BEFORE: i64 = 5 * 60;
/// The longest wait after a token endpoint failed, and the first one.
const MAX_BACKOFF: i64 = 6 * 3600;
const FIRST_BACKOFF: i64 = 60;
/// A sign-in has this long to finish, whatever the provider says.
const MAX_FLOW_LIFETIME: Duration = Duration::from_secs(15 * 60);
/// Google's way round: the person has this long to come back.
const CODE_FLOW_LIFETIME: Duration = Duration::from_secs(10 * 60);
/// A finished sign-in waits this long to be saved.
const SETTLED_LIFETIME: Duration = Duration::from_secs(10 * 60);
/// Sign-ins on their way, for everyone together and per person.
const MAX_FLOWS: usize = 256;
const MAX_FLOWS_PER_PERSON: usize = 3;
/// A token endpoint that did not answer this often while a sign-in waited ends it.
const MAX_POLL_ERRORS: u32 = 3;

/// The two providers this server signs in at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Microsoft,
    Google,
}

impl Provider {
    pub fn auth(self) -> FetchAuth {
        match self {
            Provider::Microsoft => FetchAuth::Microsoft,
            Provider::Google => FetchAuth::Google,
        }
    }

    pub fn of_auth(auth: FetchAuth) -> Option<Provider> {
        match auth {
            FetchAuth::Microsoft => Some(Provider::Microsoft),
            FetchAuth::Google => Some(Provider::Google),
            FetchAuth::Password => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Microsoft => "Microsoft",
            Provider::Google => "Google",
        }
    }
}

/// `[fetch.oauth]`: the clients this server signs in at the providers with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FetchOAuthConfig {
    /// An own Microsoft Entra app (public client, device code flow). Empty takes MinifyX's.
    pub microsoft_client_id: String,
    /// A Google "Web application" client; both are needed for Google.
    pub google_client_id: String,
    pub google_client_secret: String,
}

/// Where the providers are. Only tests point these elsewhere.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// `https://login.microsoftonline.com`; the tenant and the path follow.
    pub microsoft: String,
    pub google_authorize: String,
    pub google_token: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints {
            microsoft: "https://login.microsoftonline.com".into(),
            google_authorize: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            google_token: "https://oauth2.googleapis.com/token".into(),
        }
    }
}

pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// How a form reaches a token endpoint: through the egress, or a stand-in in tests.
pub trait TokenTransport: Send + Sync {
    /// The status and the body of the answer.
    fn post_form(&self, url: &str, form: String) -> BoxFuture<'_, Result<(u16, Vec<u8>), String>>;
}

/// The egress, the way fetching leaves: through the proxy when fetching takes it.
pub struct EgressTransport(pub Egress);

impl TokenTransport for EgressTransport {
    fn post_form(&self, url: &str, form: String) -> BoxFuture<'_, Result<(u16, Vec<u8>), String>> {
        let url = url.to_owned();
        Box::pin(async move {
            self.0
                .post_form(Purpose::Fetch, &url, form, MAX_ANSWER)
                .await
                .map(|(status, body)| (status, body.to_vec()))
                .map_err(|err| err.to_string())
        })
    }
}

/// Where a provider's mailbox is reached with a grant. Fixed per provider: signing in says nothing
/// about servers, and these are the ones the grant is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderServers {
    pub imap_host: String,
    pub imap_port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
    /// TLS from the first byte; STARTTLS otherwise.
    pub smtp_tls: bool,
}

impl ProviderServers {
    pub fn of(provider: Provider, consumer: bool) -> ProviderServers {
        match provider {
            Provider::Microsoft => ProviderServers {
                imap_host: "outlook.office365.com".into(),
                imap_port: 993,
                // Outlook.com and Hotmail send through their own name, Microsoft 365 through its.
                smtp_host: if consumer { "smtp-mail.outlook.com" } else { "smtp.office365.com" }.into(),
                smtp_port: 587,
                smtp_tls: false,
            },
            Provider::Google => ProviderServers {
                imap_host: "imap.gmail.com".into(),
                imap_port: 993,
                smtp_host: "smtp.gmail.com".into(),
                smtp_port: 465,
                smtp_tls: true,
            },
        }
    }
}

/// Microsoft's own address domains, and Google's: `outlook.com`, `hotmail.de`, `live.co.uk`,
/// `gmail.com` and the like. `consumer` tells Outlook.com and Hotmail apart from Microsoft 365,
/// which is only ever found by the domain's mail servers.
pub fn provider_of_domain(domain: &str) -> Option<(Provider, bool)> {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if matches!(domain.as_str(), "gmail.com" | "googlemail.com") {
        return Some((Provider::Google, true));
    }
    if matches!(domain.as_str(), "msn.com" | "windowslive.com" | "passport.com") {
        return Some((Provider::Microsoft, true));
    }
    // hotmail.*, live.*, outlook.*: the name, then a country's ending of one or two labels.
    let (name, suffix) = domain.split_once('.')?;
    let suffix_ok = !suffix.is_empty()
        && suffix.split('.').count() <= 2
        && suffix
            .split('.')
            .all(|label| (2..=3).contains(&label.len()) && label.chars().all(|c| c.is_ascii_lowercase()));
    (matches!(name, "hotmail" | "live" | "outlook") && suffix_ok).then_some((Provider::Microsoft, true))
}

/// What a domain's mail servers say about who runs its mail: Microsoft 365 receives under
/// `*.mail.protection.outlook.com`, Google Workspace under `aspmx.l.google.com` and the like.
pub fn provider_of_mx(hosts: &[String]) -> Option<(Provider, bool)> {
    hosts.iter().find_map(|host| {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        if host.ends_with(".mail.protection.outlook.com") {
            Some((Provider::Microsoft, false))
        } else if host.ends_with(".google.com") || host.ends_with(".googlemail.com") {
            Some((Provider::Google, false))
        } else {
            None
        }
    })
}

/// Whether a refusal is Microsoft's "Basic authentication is disabled": not a wrong password, but a
/// mailbox that takes none at all any more.
pub fn is_basic_auth_disabled(text: &str) -> bool {
    text.to_ascii_lowercase().contains("basic authentication is disabled")
}

/// The login message of SASL XOAUTH2, base64-encoded: `user=` address `^Aauth=Bearer ` token `^A^A`.
pub fn xoauth2(user: &str, token: &str) -> String {
    STANDARD.encode(format!("user={user}\x01auth=Bearer {token}\x01\x01"))
}

/// Why no access token came.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    /// The provider ended the grant: only signing in again helps.
    Expired(Provider),
    /// The token endpoint failed a moment ago and is not asked again before this time.
    Waiting(i64),
    /// This server has no client for the provider (any more).
    NotConfigured(Provider),
    /// Anything else, said in a sentence.
    Failed(String),
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TokenError::Expired(provider) => {
                write!(f, "the sign-in at {} has expired or was revoked; sign in again", provider.name())
            }
            TokenError::Waiting(_) => {
                f.write_str("the provider's sign-in service failed a moment ago; trying again later")
            }
            TokenError::NotConfigured(provider) => {
                write!(f, "this server has no client for signing in at {} (fetch.oauth)", provider.name())
            }
            TokenError::Failed(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for TokenError {}

/// Something the person has to hear about a fetched mailbox, sent as a notice into their inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchNotice {
    /// Whose mailbox it is.
    pub account_id: i64,
    pub address: String,
    pub kind: FetchNoticeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchNoticeKind {
    /// The provider ended the grant.
    LoginExpired(Provider),
    /// The provider takes no passwords any more (Microsoft).
    PasswordRefused,
}

type NoticeHook = Arc<dyn Fn(FetchNotice) + Send + Sync>;

/// How a sign-in on its way is doing, for the portal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowPoll {
    /// Still waiting for the person; ask again after this many seconds.
    Pending(u64),
    /// The provider handed out tokens: prove them with a real login, then [`ProviderOAuth::settle`].
    /// Handed out once; asked again meanwhile, the flow says it is pending.
    Prove(Granted),
    /// Proven, and waiting to be saved.
    Proven(Granted),
    /// It did not work; a code the portal turns into a sentence.
    Failed(String),
}

/// A sign-in that came through.
#[derive(Clone, PartialEq, Eq)]
pub struct Granted {
    pub provider: Provider,
    pub address: String,
    /// Outlook.com or Hotmail rather than Microsoft 365, which only changes the outgoing server.
    pub consumer: bool,
    /// The fetched mailbox that switches to this sign-in, when it is not a new one.
    pub switch_id: Option<i64>,
    pub tokens: FetchTokens,
    /// What the login proved, once it did.
    pub settings: Option<crate::autoconfig::Settings>,
}

impl std::fmt::Debug for Granted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Granted")
            .field("provider", &self.provider)
            .field("address", &self.address)
            .field("switch_id", &self.switch_id)
            .finish_non_exhaustive()
    }
}

/// What Microsoft's device authorization answered, for the person to act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceStart {
    pub flow_id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Google's way round: where to send the browser, and the value of the cookie that binds the way
/// back to this browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeStart {
    pub flow_id: String,
    pub url: String,
    pub binding: String,
}

enum Kind {
    Device { device_code: String, tenant: &'static str, interval: Duration, next_poll: Instant, errors: u32 },
    Code { state: String, verifier: String, binding: String, redirect_uri: String },
}

enum Stage {
    Waiting,
    Granted(FetchTokens),
    Proving(FetchTokens),
    Proven(FetchTokens, crate::autoconfig::Settings),
    Failed(String),
}

struct Flow {
    owner: i64,
    provider: Provider,
    address: String,
    consumer: bool,
    switch_id: Option<i64>,
    expires: Instant,
    kind: Kind,
    stage: Stage,
}

impl Flow {
    fn granted(&self, tokens: &FetchTokens, settings: Option<crate::autoconfig::Settings>) -> Granted {
        Granted {
            provider: self.provider,
            address: self.address.clone(),
            consumer: self.consumer,
            switch_id: self.switch_id,
            tokens: tokens.clone(),
            settings,
        }
    }
}

struct Inner {
    config: RwLock<FetchOAuthConfig>,
    endpoints: RwLock<Endpoints>,
    transport: RwLock<Arc<dyn TokenTransport>>,
    flows: Mutex<HashMap<String, Flow>>,
    /// One renewal at a time per mailbox: the fetch run and a delivery may want a token at once, and
    /// a provider that rotates refresh tokens must not see the same one twice.
    renewing: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
    notices: RwLock<Option<NoticeHook>>,
}

/// Signing in at the providers, and the tokens that come of it. Cheap to clone.
#[derive(Clone)]
pub struct ProviderOAuth {
    inner: Arc<Inner>,
}

impl Default for ProviderOAuth {
    fn default() -> Self {
        ProviderOAuth::new(Arc::new(EgressTransport(Egress::direct())))
    }
}

fn random(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("the system RNG failed");
    URL_SAFE_NO_PAD.encode(buffer)
}

fn now() -> i64 {
    crate::now()
}

fn form(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish()
}

/// The part after the `@`, lowercased.
fn domain_of(address: &str) -> Option<String> {
    let (_, domain) = address.trim().rsplit_once('@')?;
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    (!domain.is_empty() && domain.contains('.')).then_some(domain)
}

/// Microsoft's tenant for an address: `consumers` for Outlook.com and Hotmail, whose accounts are
/// personal ones, `common` for everyone else.
fn tenant_for(address: &str) -> &'static str {
    match domain_of(address).and_then(|domain| provider_of_domain(&domain)) {
        Some((Provider::Microsoft, true)) => "consumers",
        _ => "common",
    }
}

/// A token endpoint's answer, read.
enum Answer {
    Tokens {
        access: String,
        refresh: Option<String>,
        expires_in: i64,
    },
    /// The OAuth error code, like `invalid_grant` or `authorization_pending`.
    Error(String),
}

fn text(value: &Value, name: &str, max: usize) -> Option<String> {
    value.get(name).and_then(Value::as_str).filter(|text| !text.is_empty() && text.len() <= max).map(str::to_owned)
}

fn read_answer(status: u16, body: &[u8]) -> Result<Answer, String> {
    let value: Value = serde_json::from_slice(body).map_err(|_| format!("the provider answered {status}, not JSON"))?;
    if status == 200 {
        let access = text(&value, "access_token", 16 * 1024).ok_or("the provider handed out no access token")?;
        let refresh = text(&value, "refresh_token", 16 * 1024);
        let expires_in = value.get("expires_in").and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()));
        return Ok(Answer::Tokens { access, refresh, expires_in: expires_in.unwrap_or(3600).clamp(60, 24 * 3600) });
    }
    match text(&value, "error", 128) {
        Some(error) => Ok(Answer::Error(error)),
        None => Err(format!("the provider answered {status}")),
    }
}

/// Errors that end a grant for good: only a new sign-in helps.
fn grant_ended(error: &str) -> bool {
    matches!(error, "invalid_grant" | "interaction_required" | "consent_required" | "login_required")
}

impl ProviderOAuth {
    pub fn new(transport: Arc<dyn TokenTransport>) -> ProviderOAuth {
        ProviderOAuth {
            inner: Arc::new(Inner {
                config: RwLock::default(),
                endpoints: RwLock::default(),
                transport: RwLock::new(transport),
                flows: Mutex::default(),
                renewing: Mutex::default(),
                notices: RwLock::default(),
            }),
        }
    }

    /// Puts the clients from the settings into effect.
    pub fn configure(&self, config: FetchOAuthConfig) {
        *self.inner.config.write().unwrap_or_else(|e| e.into_inner()) = config;
    }

    /// Asks the token endpoints through `transport` from now on: the egress, or a stand-in in tests.
    pub fn set_transport(&self, transport: Arc<dyn TokenTransport>) {
        *self.inner.transport.write().unwrap_or_else(|e| e.into_inner()) = transport;
    }

    /// Points the endpoints elsewhere. For tests.
    pub fn set_endpoints(&self, endpoints: Endpoints) {
        *self.inner.endpoints.write().unwrap_or_else(|e| e.into_inner()) = endpoints;
    }

    /// Who hears about a grant that ended or a password the provider refused: the portal, which puts
    /// a notice into the person's inbox.
    pub fn on_notice(&self, hook: impl Fn(FetchNotice) + Send + Sync + 'static) {
        *self.inner.notices.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(hook));
    }

    /// Tells the person, through the hook, if there is one.
    pub fn notify(&self, notice: FetchNotice) {
        let hook = self.inner.notices.read().unwrap_or_else(|e| e.into_inner()).clone();
        match hook {
            Some(hook) => hook(notice),
            None => {
                tracing::info!(address = %notice.address, kind = ?notice.kind, "nobody to tell about a fetched mailbox")
            }
        }
    }

    fn config(&self) -> FetchOAuthConfig {
        self.inner.config.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn endpoints(&self) -> Endpoints {
        self.inner.endpoints.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn transport(&self) -> Arc<dyn TokenTransport> {
        self.inner.transport.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The Microsoft client this server signs in with: the admin's own, else MinifyX's.
    pub fn microsoft_client_id(&self) -> Option<String> {
        let own = self.config().microsoft_client_id.trim().to_owned();
        let id = if own.is_empty() { MICROSOFT_DEFAULT_CLIENT_ID.trim().to_owned() } else { own };
        (!id.is_empty()).then_some(id)
    }

    fn google_client(&self) -> Option<(String, String)> {
        let config = self.config();
        let (id, secret) = (config.google_client_id.trim().to_owned(), config.google_client_secret.trim().to_owned());
        (!id.is_empty() && !secret.is_empty()).then_some((id, secret))
    }

    /// Whether signing in at a provider can work on this server at all.
    pub fn ready(&self, provider: Provider) -> bool {
        match provider {
            Provider::Microsoft => self.microsoft_client_id().is_some(),
            Provider::Google => self.google_client().is_some(),
        }
    }

    async fn post(&self, url: &str, form: String) -> Result<Answer, String> {
        let (status, body) = self.transport().post_form(url, form).await?;
        read_answer(status, &body)
    }

    /// Makes room for a new sign-in: the ones that ran out go, and a person with too many on their
    /// way loses the oldest.
    fn admit(&self, flows: &mut HashMap<String, Flow>, owner: i64) -> Result<(), String> {
        let now = Instant::now();
        flows.retain(|_, flow| flow.expires > now);
        let mine: Vec<(String, Instant)> =
            flows.iter().filter(|(_, flow)| flow.owner == owner).map(|(id, flow)| (id.clone(), flow.expires)).collect();
        if mine.len() >= MAX_FLOWS_PER_PERSON
            && let Some((oldest, _)) = mine.iter().min_by_key(|(_, expires)| *expires)
        {
            flows.remove(oldest);
        }
        if flows.len() >= MAX_FLOWS {
            return Err("busy".into());
        }
        Ok(())
    }

    /// Starts signing in at Microsoft for `address`: asks for a device code the person types at
    /// Microsoft's page. Errors are codes for the portal.
    pub async fn start_microsoft(
        &self,
        owner: i64,
        address: &str,
        consumer: bool,
        switch_id: Option<i64>,
    ) -> Result<DeviceStart, String> {
        let client_id = self.microsoft_client_id().ok_or("oauthNotConfigured")?;
        let tenant = if consumer { "consumers" } else { tenant_for(address) };
        let url = format!("{}/{tenant}/oauth2/v2.0/devicecode", self.endpoints().microsoft.trim_end_matches('/'));
        let (status, body) = self
            .transport()
            .post_form(&url, form(&[("client_id", &client_id), ("scope", MICROSOFT_SCOPES)]))
            .await
            .map_err(|err| {
                tracing::warn!(%err, "asking Microsoft for a device code failed");
                "providerUnreachable".to_owned()
            })?;
        let value: Value = serde_json::from_slice(&body).map_err(|_| "providerError".to_owned())?;
        if status != 200 {
            let error = text(&value, "error", 128).unwrap_or_default();
            tracing::warn!(status, %error, "Microsoft refused to hand out a device code");
            return Err(if error.contains("client") { "oauthClientRejected" } else { "providerError" }.into());
        }
        let device_code = text(&value, "device_code", 4096).ok_or("providerError")?;
        let user_code = text(&value, "user_code", 64).ok_or("providerError")?;
        let verification_uri = text(&value, "verification_uri", 512)
            .filter(|uri| uri.starts_with("https://"))
            .unwrap_or_else(|| "https://microsoft.com/devicelogin".into());
        let lifetime = value.get("expires_in").and_then(Value::as_u64).unwrap_or(900);
        let lifetime = Duration::from_secs(lifetime.clamp(60, MAX_FLOW_LIFETIME.as_secs()));
        let interval = Duration::from_secs(value.get("interval").and_then(Value::as_u64).unwrap_or(5).clamp(1, 60));

        let flow_id = random(18);
        let mut flows = self.inner.flows.lock().unwrap_or_else(|e| e.into_inner());
        self.admit(&mut flows, owner)?;
        flows.insert(
            flow_id.clone(),
            Flow {
                owner,
                provider: Provider::Microsoft,
                address: address.to_owned(),
                consumer,
                switch_id,
                expires: Instant::now() + lifetime,
                kind: Kind::Device { device_code, tenant, interval, next_poll: Instant::now() + interval, errors: 0 },
                stage: Stage::Waiting,
            },
        );
        Ok(DeviceStart {
            flow_id,
            user_code,
            verification_uri,
            expires_in: lifetime.as_secs(),
            interval: interval.as_secs(),
        })
    }

    /// Starts signing in at Google: the address to send the browser to. `redirect_uri` is
    /// [`CALLBACK_PATH`] on this server's public name.
    pub fn start_google(
        &self,
        owner: i64,
        address: &str,
        switch_id: Option<i64>,
        redirect_uri: &str,
    ) -> Result<CodeStart, String> {
        let (client_id, _) = self.google_client().ok_or("oauthNotConfigured")?;
        let (state, verifier, binding) = (random(24), random(48), random(24));
        let challenge = URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()).as_ref());
        let mut url = url::Url::parse(&self.endpoints().google_authorize).map_err(|_| "providerError".to_owned())?;
        url.query_pairs_mut()
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", GOOGLE_SCOPE)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("login_hint", address)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        let flow_id = random(18);
        let mut flows = self.inner.flows.lock().unwrap_or_else(|e| e.into_inner());
        self.admit(&mut flows, owner)?;
        flows.insert(
            flow_id.clone(),
            Flow {
                owner,
                provider: Provider::Google,
                address: address.to_owned(),
                consumer: provider_of_domain(&domain_of(address).unwrap_or_default()).is_some(),
                switch_id,
                expires: Instant::now() + CODE_FLOW_LIFETIME,
                kind: Kind::Code { state, verifier, binding: binding.clone(), redirect_uri: redirect_uri.to_owned() },
                stage: Stage::Waiting,
            },
        );
        Ok(CodeStart { flow_id, url: url.to_string(), binding })
    }

    /// Google sends the browser back. The state names the sign-in; the cookie the browser carries has
    /// to be the one set when it was started, so an answer cannot be sent to somebody else's browser
    /// (or theirs to ours). Answers the flow, for the portal to pick up once the browser is back on
    /// this server with its session.
    pub async fn finish_google(&self, state: &str, code: &str, binding: &str) -> Result<String, String> {
        if state.is_empty() || state.len() > 128 || code.is_empty() || code.len() > 4096 {
            return Err("expired".into());
        }
        let (flow_id, verifier, redirect_uri) = {
            let mut flows = self.inner.flows.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            flows.retain(|_, flow| flow.expires > now);
            let found = flows.iter_mut().find(|(_, flow)| {
                matches!(&flow.kind, Kind::Code { state: wanted, .. } if !wanted.is_empty() && wanted == state)
            });
            let Some((flow_id, flow)) = found else {
                return Err("expired".into());
            };
            let Kind::Code { state: wanted, verifier, binding: bound, redirect_uri } = &mut flow.kind else {
                return Err("expired".into());
            };
            if constant_time::verify_slices_are_equal(bound.as_bytes(), binding.as_bytes()).is_err() {
                return Err("expired".into());
            }
            // One way back per sign-in.
            wanted.clear();
            (flow_id.clone(), verifier.clone(), redirect_uri.clone())
        };
        let outcome = match self.google_client() {
            None => Err("oauthNotConfigured".to_owned()),
            Some((client_id, secret)) => {
                let body = form(&[
                    ("grant_type", "authorization_code"),
                    ("code", code),
                    ("redirect_uri", &redirect_uri),
                    ("client_id", &client_id),
                    ("client_secret", &secret),
                    ("code_verifier", &verifier),
                ]);
                match self.post(&self.endpoints().google_token, body).await {
                    Ok(Answer::Tokens { access, refresh: Some(refresh), expires_in }) => {
                        Ok(FetchTokens { access_token: access, expires_at: now() + expires_in, refresh_token: refresh })
                    }
                    Ok(Answer::Tokens { refresh: None, .. }) => Err("noRefreshToken".to_owned()),
                    Ok(Answer::Error(error)) => {
                        tracing::warn!(%error, "Google refused to trade in a sign-in");
                        Err(if error.contains("client") { "oauthClientRejected" } else { "failed" }.to_owned())
                    }
                    Err(err) => {
                        tracing::warn!(%err, "trading in a sign-in at Google failed");
                        Err("providerUnreachable".to_owned())
                    }
                }
            }
        };
        let mut flows = self.inner.flows.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(flow) = flows.get_mut(&flow_id) {
            flow.stage = match outcome {
                Ok(tokens) => Stage::Granted(tokens),
                Err(code) => Stage::Failed(code),
            };
            flow.expires = Instant::now() + SETTLED_LIFETIME;
        }
        Ok(flow_id)
    }

    /// How a sign-in of `owner` is doing. For Microsoft this asks the token endpoint, but never more
    /// often than Microsoft asked to be asked, however often the portal looks.
    pub async fn poll(&self, owner: i64, flow_id: &str) -> FlowPoll {
        // What to ask Microsoft, taken out of the lock.
        let ask = {
            let mut flows = self.inner.flows.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            flows.retain(|_, flow| flow.expires > now);
            let Some(flow) = flows.get_mut(flow_id).filter(|flow| flow.owner == owner) else {
                return FlowPoll::Failed("expired".into());
            };
            match &flow.stage {
                Stage::Failed(code) => return FlowPoll::Failed(code.clone()),
                Stage::Proven(tokens, settings) => {
                    return FlowPoll::Proven(flow.granted(tokens, Some(settings.clone())));
                }
                Stage::Proving(_) => return FlowPoll::Pending(1),
                Stage::Granted(tokens) => {
                    let granted = flow.granted(tokens, None);
                    flow.stage = Stage::Proving(tokens.clone());
                    return FlowPoll::Prove(granted);
                }
                Stage::Waiting => {}
            }
            match &mut flow.kind {
                Kind::Code { .. } => return FlowPoll::Pending(2),
                Kind::Device { device_code, tenant, interval, next_poll, .. } => {
                    if *next_poll > now {
                        return FlowPoll::Pending(next_poll.duration_since(now).as_secs().max(1));
                    }
                    // Whoever asks first asks Microsoft; the next may only after the interval.
                    *next_poll = now + *interval;
                    (device_code.clone(), *tenant)
                }
            }
        };
        let (device_code, tenant) = ask;
        let Some(client_id) = self.microsoft_client_id() else {
            return self.fail(flow_id, "oauthNotConfigured");
        };
        let url = format!("{}/{tenant}/oauth2/v2.0/token", self.endpoints().microsoft.trim_end_matches('/'));
        let body = form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", &client_id),
            ("device_code", &device_code),
        ]);
        let answer = self.post(&url, body).await;
        let mut flows = self.inner.flows.lock().unwrap_or_else(|e| e.into_inner());
        let Some(flow) = flows.get_mut(flow_id) else {
            return FlowPoll::Failed("expired".into());
        };
        let Kind::Device { interval, next_poll, errors, .. } = &mut flow.kind else {
            return FlowPoll::Failed("expired".into());
        };
        match answer {
            Ok(Answer::Tokens { access, refresh: Some(refresh), expires_in }) => {
                let tokens =
                    FetchTokens { access_token: access, expires_at: now() + expires_in, refresh_token: refresh };
                let granted = flow.granted(&tokens, None);
                flow.stage = Stage::Proving(tokens);
                flow.expires = Instant::now() + SETTLED_LIFETIME;
                FlowPoll::Prove(granted)
            }
            Ok(Answer::Tokens { refresh: None, .. }) => {
                flow.stage = Stage::Failed("noRefreshToken".into());
                FlowPoll::Failed("noRefreshToken".into())
            }
            Ok(Answer::Error(error)) => match error.as_str() {
                "authorization_pending" => FlowPoll::Pending(interval.as_secs()),
                // RFC 8628 3.5: five seconds more, from now on.
                "slow_down" => {
                    *interval += Duration::from_secs(5);
                    *next_poll = Instant::now() + *interval;
                    FlowPoll::Pending(interval.as_secs())
                }
                "authorization_declined" | "access_denied" => {
                    flow.stage = Stage::Failed("declined".into());
                    FlowPoll::Failed("declined".into())
                }
                "expired_token" | "bad_verification_code" => {
                    flow.stage = Stage::Failed("expired".into());
                    FlowPoll::Failed("expired".into())
                }
                other => {
                    tracing::warn!(error = %other, "Microsoft refused a sign-in");
                    let code = if other.contains("client") { "oauthClientRejected" } else { "failed" };
                    flow.stage = Stage::Failed(code.into());
                    FlowPoll::Failed(code.into())
                }
            },
            Err(err) => {
                tracing::warn!(%err, "asking Microsoft whether a sign-in is done failed");
                *errors += 1;
                if *errors >= MAX_POLL_ERRORS {
                    flow.stage = Stage::Failed("providerUnreachable".into());
                    return FlowPoll::Failed("providerUnreachable".into());
                }
                FlowPoll::Pending(interval.as_secs())
            }
        }
    }

    fn fail(&self, flow_id: &str, code: &str) -> FlowPoll {
        let mut flows = self.inner.flows.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(flow) = flows.get_mut(flow_id) {
            flow.stage = Stage::Failed(code.into());
        }
        FlowPoll::Failed(code.into())
    }

    /// What the login with the tokens proved: the settings to save, or a code for why it failed.
    pub fn settle(&self, owner: i64, flow_id: &str, proof: Result<crate::autoconfig::Settings, String>) {
        let mut flows = self.inner.flows.lock().unwrap_or_else(|e| e.into_inner());
        let Some(flow) = flows.get_mut(flow_id).filter(|flow| flow.owner == owner) else { return };
        let Stage::Proving(tokens) = &flow.stage else { return };
        flow.stage = match proof {
            Ok(settings) => Stage::Proven(tokens.clone(), settings),
            Err(code) => Stage::Failed(code),
        };
    }

    /// Takes a proven sign-in of `owner` out, to save it. Only once.
    pub fn take(&self, owner: i64, flow_id: &str) -> Option<Granted> {
        let mut flows = self.inner.flows.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let flow = flows.get(flow_id).filter(|flow| flow.owner == owner && flow.expires > now)?;
        let Stage::Proven(tokens, settings) = &flow.stage else { return None };
        let granted = flow.granted(tokens, Some(settings.clone()));
        flows.remove(flow_id);
        Some(granted)
    }

    fn renewal_lock(&self, fetch_id: i64) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.inner.renewing.lock().unwrap_or_else(|e| e.into_inner());
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        locks.entry(fetch_id).or_default().clone()
    }

    /// An access token for a fetched mailbox that signs in: the one kept while it has a few minutes
    /// left, a new one otherwise, made from the refresh token (and the new refresh token kept, where
    /// the provider rotates them).
    pub async fn access_token(
        &self,
        store: &Store,
        account_id: i64,
        fetch_id: i64,
        address: &str,
    ) -> Result<String, TokenError> {
        let failed = |err: uwumail_store::StoreError| TokenError::Failed(err.to_string());
        let lock = self.renewal_lock(fetch_id);
        let _renewing = lock.lock().await;
        // Read under the lock: a renewal that just finished is found here, not done again.
        let oauth = store
            .fetch_oauth(account_id, fetch_id)
            .await
            .map_err(failed)?
            .ok_or_else(|| TokenError::Failed("the fetched mailbox is gone".into()))?;
        let provider = Provider::of_auth(oauth.provider)
            .ok_or_else(|| TokenError::Failed("this mailbox logs in with a password".into()))?;
        if oauth.expired {
            return Err(TokenError::Expired(provider));
        }
        let at = now();
        if let (Some(access), Some(expires_at)) = (&oauth.access_token, oauth.expires_at)
            && expires_at - RENEW_BEFORE > at
        {
            return Ok(access.clone());
        }
        if let Some(retry_at) = oauth.retry_at.filter(|retry_at| *retry_at > at) {
            return Err(TokenError::Waiting(retry_at));
        }
        let Some(refresh) = oauth.refresh_token else {
            self.ended(store, account_id, fetch_id, address, provider).await;
            return Err(TokenError::Expired(provider));
        };
        let endpoints = self.endpoints();
        let (url, body) = match provider {
            Provider::Microsoft => {
                let client_id = self.microsoft_client_id().ok_or(TokenError::NotConfigured(provider))?;
                let url =
                    format!("{}/{}/oauth2/v2.0/token", endpoints.microsoft.trim_end_matches('/'), tenant_for(address));
                let body = form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", &client_id),
                    ("refresh_token", &refresh),
                    ("scope", MICROSOFT_SCOPES),
                ]);
                (url, body)
            }
            Provider::Google => {
                let (client_id, secret) = self.google_client().ok_or(TokenError::NotConfigured(provider))?;
                let body = form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", &client_id),
                    ("client_secret", &secret),
                    ("refresh_token", &refresh),
                ]);
                (endpoints.google_token, body)
            }
        };
        match self.post(&url, body).await {
            Ok(Answer::Tokens { access, refresh, expires_in }) => {
                store.store_fetch_tokens(fetch_id, access.clone(), at + expires_in, refresh).await.map_err(failed)?;
                Ok(access)
            }
            Ok(Answer::Error(error)) if grant_ended(&error) => {
                tracing::info!(%address, provider = provider.name(), %error, "the provider ended a fetched mailbox's sign-in");
                self.ended(store, account_id, fetch_id, address, provider).await;
                Err(TokenError::Expired(provider))
            }
            Ok(Answer::Error(error)) => {
                let wait = self.back_off(store, fetch_id, oauth.failures).await;
                Err(TokenError::Failed(format!(
                    "{} refused to renew the sign-in ({error}); trying again in {} minutes",
                    provider.name(),
                    wait / 60
                )))
            }
            Err(err) => {
                let wait = self.back_off(store, fetch_id, oauth.failures).await;
                Err(TokenError::Failed(format!(
                    "{}'s sign-in service could not be asked ({err}); trying again in {} minutes",
                    provider.name(),
                    wait / 60
                )))
            }
        }
    }

    /// The wait before the next attempt, doubling with every failure in a row.
    async fn back_off(&self, store: &Store, fetch_id: i64, failures: i64) -> i64 {
        let wait = (FIRST_BACKOFF << failures.clamp(0, 16)).min(MAX_BACKOFF);
        if let Err(err) = store.note_fetch_token_failure(fetch_id, false, now() + wait).await {
            tracing::warn!(%err, "writing down a failed token renewal failed");
        }
        wait
    }

    async fn ended(&self, store: &Store, account_id: i64, fetch_id: i64, address: &str, provider: Provider) {
        match store.note_fetch_token_failure(fetch_id, true, 0).await {
            Ok(true) => self.notify(FetchNotice {
                account_id,
                address: address.to_owned(),
                kind: FetchNoticeKind::LoginExpired(provider),
            }),
            Ok(false) => {}
            Err(err) => tracing::warn!(%err, "writing down an ended sign-in failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microsoft_and_google_are_known_by_their_domains() {
        for domain in ["outlook.com", "hotmail.com", "hotmail.de", "hotmail.co.uk", "live.de", "msn.com", "OUTLOOK.DE."]
        {
            assert_eq!(provider_of_domain(domain), Some((Provider::Microsoft, true)), "{domain}");
        }
        for domain in ["gmail.com", "googlemail.com"] {
            assert_eq!(provider_of_domain(domain), Some((Provider::Google, true)), "{domain}");
        }
        for domain in ["example.com", "hotmail.example.com", "livemail.test", "outlook.co.uk.example", "gmail.de"] {
            assert_eq!(provider_of_domain(domain), None, "{domain}");
        }
        assert_eq!(tenant_for("mini@hotmail.de"), "consumers");
        assert_eq!(tenant_for("mini@example.com"), "common");
    }

    #[test]
    fn microsoft_365_and_google_workspace_are_known_by_their_mail_servers() {
        let hosts = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            provider_of_mx(&hosts(&["example-com.mail.protection.outlook.com."])),
            Some((Provider::Microsoft, false))
        );
        assert_eq!(provider_of_mx(&hosts(&["mx.example.net", "aspmx.l.google.com"])), Some((Provider::Google, false)));
        assert_eq!(provider_of_mx(&hosts(&["smtp.google.com"])), Some((Provider::Google, false)));
        assert_eq!(provider_of_mx(&hosts(&["mx.example.net", "google.com.example.net"])), None);
        assert_eq!(ProviderServers::of(Provider::Microsoft, true).smtp_host, "smtp-mail.outlook.com");
        assert_eq!(ProviderServers::of(Provider::Microsoft, false).smtp_host, "smtp.office365.com");
    }

    #[test]
    fn microsofts_refusal_of_passwords_is_told_apart() {
        assert!(is_basic_auth_disabled("LOGIN: NO Basic authentication is disabled."));
        assert!(is_basic_auth_disabled("a1 NO BASIC AUTHENTICATION IS DISABLED"));
        assert!(!is_basic_auth_disabled("a1 NO [AUTHENTICATIONFAILED] Invalid credentials"));
        assert_eq!(
            xoauth2("mini@example.com", "t0k3n"),
            STANDARD.encode("user=mini@example.com\x01auth=Bearer t0k3n\x01\x01")
        );
    }

    type Answerer = Box<dyn Fn(&str, &HashMap<String, String>) -> Result<(u16, Value), String> + Send + Sync>;

    /// The providers' token endpoints, answered by the test: every request is written down, the
    /// form read back into its fields.
    struct FakeProvider {
        seen: Mutex<Vec<(String, HashMap<String, String>)>>,
        answer: Mutex<Answerer>,
    }

    impl FakeProvider {
        fn new(
            answer: impl Fn(&str, &HashMap<String, String>) -> Result<(u16, Value), String> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(FakeProvider { seen: Mutex::default(), answer: Mutex::new(Box::new(answer)) })
        }

        fn answer(
            &self,
            answer: impl Fn(&str, &HashMap<String, String>) -> Result<(u16, Value), String> + Send + Sync + 'static,
        ) {
            *self.answer.lock().unwrap() = Box::new(answer);
        }

        fn seen(&self) -> Vec<(String, HashMap<String, String>)> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl TokenTransport for FakeProvider {
        fn post_form(&self, url: &str, form: String) -> BoxFuture<'_, Result<(u16, Vec<u8>), String>> {
            let fields: HashMap<String, String> = url::form_urlencoded::parse(form.as_bytes()).into_owned().collect();
            let answer = (self.answer.lock().unwrap())(url, &fields);
            self.seen.lock().unwrap().push((url.to_owned(), fields));
            Box::pin(async move { answer.map(|(status, body)| (status, body.to_string().into_bytes())) })
        }
    }

    fn oauth_with(fake: &Arc<FakeProvider>) -> ProviderOAuth {
        let oauth = ProviderOAuth::new(fake.clone());
        oauth.set_endpoints(Endpoints {
            microsoft: "https://login.test".into(),
            google_authorize: "https://accounts.test/auth".into(),
            google_token: "https://oauth.test/token".into(),
        });
        oauth
    }

    fn proof() -> crate::autoconfig::Settings {
        let server = |host: &str, port| crate::autoconfig::Server {
            host: host.into(),
            port,
            security: crate::autoconfig::Security::Tls,
            login: crate::autoconfig::Login::WholeAddress,
        };
        crate::autoconfig::Settings {
            imap: server("outlook.office365.com", 993),
            smtp: None,
            source: crate::autoconfig::Source::SignIn,
        }
    }

    /// RFC 8628 as Microsoft speaks it: the code, then waiting as long as Microsoft asks -- longer
    /// after a "slow_down" -- however often the portal looks, then tokens that are proven once and
    /// saved once, by the person who started it.
    #[tokio::test(start_paused = true)]
    async fn microsofts_device_flow_waits_as_asked_and_hands_out_the_sign_in_once() {
        let fake = FakeProvider::new(|url, _| {
            assert!(url.ends_with("/consumers/oauth2/v2.0/devicecode"), "{url}");
            Ok((
                200,
                serde_json::json!({
                    "device_code": "dc-1", "user_code": "KX7PQ4M", "verification_uri": "https://microsoft.com/devicelogin",
                    "expires_in": 900, "interval": 5
                }),
            ))
        });
        let oauth = oauth_with(&fake);
        let started = oauth.start_microsoft(7, "mini@hotmail.de", true, None).await.unwrap();
        assert_eq!((started.user_code.as_str(), started.interval), ("KX7PQ4M", 5));
        let form = &fake.seen()[0].1;
        assert_eq!(form["client_id"], MICROSOFT_DEFAULT_CLIENT_ID);
        assert!(form["scope"].contains("IMAP.AccessAsUser.All") && form["scope"].contains("offline_access"));

        // Asked at once: not yet, and Microsoft is not asked either.
        assert_eq!(oauth.poll(7, &started.flow_id).await, FlowPoll::Pending(5));
        assert_eq!(fake.seen().len(), 1);
        // Somebody else's sign-in is none of their business.
        assert_eq!(oauth.poll(8, &started.flow_id).await, FlowPoll::Failed("expired".into()));

        fake.answer(|_, _| Ok((400, serde_json::json!({ "error": "authorization_pending" }))));
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(oauth.poll(7, &started.flow_id).await, FlowPoll::Pending(5));
        fake.answer(|_, _| Ok((400, serde_json::json!({ "error": "slow_down" }))));
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(oauth.poll(7, &started.flow_id).await, FlowPoll::Pending(10), "five seconds more from now on");
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(matches!(oauth.poll(7, &started.flow_id).await, FlowPoll::Pending(_)));
        assert_eq!(fake.seen().len(), 3, "not asked again before the longer interval");

        fake.answer(|url, form| {
            assert!(url.ends_with("/consumers/oauth2/v2.0/token"), "{url}");
            assert_eq!(form["grant_type"], "urn:ietf:params:oauth:grant-type:device_code");
            assert_eq!(form["device_code"], "dc-1");
            Ok((200, serde_json::json!({ "access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600 })))
        });
        tokio::time::advance(Duration::from_secs(5)).await;
        let FlowPoll::Prove(granted) = oauth.poll(7, &started.flow_id).await else { panic!("no tokens") };
        assert_eq!((granted.tokens.access_token.as_str(), granted.tokens.refresh_token.as_str()), ("at-1", "rt-1"));
        assert!(granted.consumer);
        // Proven once: whoever asks while the login runs waits.
        assert_eq!(oauth.poll(7, &started.flow_id).await, FlowPoll::Pending(1));
        assert!(oauth.take(7, &started.flow_id).is_none(), "not before it is proven");
        oauth.settle(7, &started.flow_id, Ok(proof()));
        assert!(matches!(oauth.poll(7, &started.flow_id).await, FlowPoll::Proven(_)));
        assert!(oauth.take(8, &started.flow_id).is_none());
        let taken = oauth.take(7, &started.flow_id).expect("the proven sign-in");
        assert_eq!(taken.address, "mini@hotmail.de");
        assert!(oauth.take(7, &started.flow_id).is_none(), "only once");
    }

    #[tokio::test(start_paused = true)]
    async fn a_declined_or_failed_device_sign_in_says_why() {
        let fake = FakeProvider::new(|_, _| {
            Ok((200, serde_json::json!({ "device_code": "dc", "user_code": "C", "expires_in": 900, "interval": 1 })))
        });
        let oauth = oauth_with(&fake);
        // Microsoft 365 (found by the domain's mail servers) goes to the tenant for everyone.
        let declined = oauth.start_microsoft(1, "mini@example.com", false, None).await.unwrap();
        assert!(fake.seen()[0].0.contains("/common/"), "{}", fake.seen()[0].0);
        fake.answer(|_, _| Ok((400, serde_json::json!({ "error": "authorization_declined" }))));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(oauth.poll(1, &declined.flow_id).await, FlowPoll::Failed("declined".into()));

        fake.answer(|_, _| Ok((200, serde_json::json!({ "device_code": "dc", "user_code": "C", "interval": 1 }))));
        let unreachable = oauth.start_microsoft(1, "mini@example.com", false, None).await.unwrap();
        fake.answer(|_, _| Err("connection refused".into()));
        for _ in 0..MAX_POLL_ERRORS - 1 {
            tokio::time::advance(Duration::from_secs(1)).await;
            assert!(matches!(oauth.poll(1, &unreachable.flow_id).await, FlowPoll::Pending(_)));
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(oauth.poll(1, &unreachable.flow_id).await, FlowPoll::Failed("providerUnreachable".into()));

        // An admin's own client ID wins over the one that is shipped.
        oauth.configure(FetchOAuthConfig { microsoft_client_id: "own-client".into(), ..Default::default() });
        fake.answer(|_, _| Ok((400, serde_json::json!({ "error": "unauthorized_client" }))));
        assert_eq!(oauth.start_microsoft(1, "mini@example.com", false, None).await, Err("oauthClientRejected".into()));
        assert_eq!(fake.seen().last().unwrap().1["client_id"], "own-client");
    }

    /// Google's way round: PKCE, the admin's client with its secret, offline access with consent,
    /// and the way back only for the browser that set off -- once.
    #[tokio::test]
    async fn googles_code_flow_is_bound_to_the_browser_that_started_it() {
        let fake = FakeProvider::new(|_, _| Err("not yet".into()));
        let oauth = oauth_with(&fake);
        let redirect = "https://mail.example.org/api/account/fetch/oauth/callback";
        assert_eq!(oauth.start_google(3, "mini@gmail.com", None, redirect).unwrap_err(), "oauthNotConfigured");
        oauth.configure(FetchOAuthConfig {
            google_client_id: "gid.apps.googleusercontent.com".into(),
            google_client_secret: "g-secret".into(),
            ..Default::default()
        });
        let started = oauth.start_google(3, "mini@gmail.com", Some(12), redirect).unwrap();
        let url = url::Url::parse(&started.url).unwrap();
        let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(url.as_str().split('?').next(), Some("https://accounts.test/auth"));
        assert_eq!(query["scope"], "https://mail.google.com/");
        assert_eq!((query["access_type"].as_str(), query["prompt"].as_str()), ("offline", "consent"));
        assert_eq!((query["redirect_uri"].as_str(), query["code_challenge_method"].as_str()), (redirect, "S256"));
        let (state, challenge) = (query["state"].clone(), query["code_challenge"].clone());

        // Another browser, or no cookie: nothing happens, and the state stays good for the right one.
        assert_eq!(oauth.finish_google(&state, "code-1", "someone-else").await, Err("expired".into()));
        assert_eq!(oauth.finish_google(&state, "code-1", "").await, Err("expired".into()));
        assert!(fake.seen().is_empty(), "Google is not even asked");

        fake.answer(|url, _| {
            assert_eq!(url, "https://oauth.test/token");
            Ok((200, serde_json::json!({ "access_token": "g-at", "refresh_token": "g-rt", "expires_in": 3599 })))
        });
        let flow = oauth.finish_google(&state, "code-1", &started.binding).await.unwrap();
        assert_eq!(flow, started.flow_id);
        let form = &fake.seen()[0].1;
        assert_eq!((form["grant_type"].as_str(), form["code"].as_str()), ("authorization_code", "code-1"));
        assert_eq!((form["client_secret"].as_str(), form["redirect_uri"].as_str()), ("g-secret", redirect));
        let verified =
            URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, form["code_verifier"].as_bytes()).as_ref());
        assert_eq!(verified, challenge, "PKCE: the verifier belongs to the challenge");
        assert_eq!(oauth.finish_google(&state, "code-1", &started.binding).await, Err("expired".into()), "once");

        let FlowPoll::Prove(granted) = oauth.poll(3, &flow).await else { panic!("no tokens") };
        assert_eq!((granted.provider, granted.switch_id), (Provider::Google, Some(12)));
        assert_eq!(granted.tokens.refresh_token, "g-rt");
        oauth.settle(3, &flow, Err("signInRefused".into()));
        assert_eq!(oauth.poll(3, &flow).await, FlowPoll::Failed("signInRefused".into()));
    }

    async fn store_with_grant(
        dir: &std::path::Path,
        address: &str,
        provider: FetchAuth,
        expires_in: i64,
    ) -> (Store, i64, i64) {
        let store = Store::open(dir).await.unwrap();
        store.create_domain("example.org").await.unwrap();
        let account = store
            .create_account(uwumail_store::NewAccount {
                address: "mini@example.org".into(),
                display_name: String::new(),
                password: None,
                role: uwumail_store::Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;
        let fetched = store
            .create_fetch_account_with(
                uwumail_store::NewFetchAccount {
                    account_id: account,
                    address: address.into(),
                    host: "outlook.office365.com".into(),
                    port: 993,
                    security: uwumail_store::FetchSecurity::Tls,
                    username: address.into(),
                    password: String::new(),
                    after_fetch: uwumail_store::AfterFetch::MarkRead,
                    fetch_junk: true,
                    interval_secs: uwumail_store::DEFAULT_FETCH_INTERVAL_SECS,
                    auth_serv_id: String::new(),
                },
                Some(uwumail_store::FetchGrant {
                    provider,
                    tokens: FetchTokens {
                        access_token: "at-0".into(),
                        expires_at: now() + expires_in,
                        refresh_token: "rt-0".into(),
                    },
                }),
            )
            .await
            .unwrap();
        (store, account, fetched.id)
    }

    /// The token kept while it has time left; a new one a little before it runs out, and the new
    /// refresh token of a provider that rotates them kept -- the old one when none came.
    #[tokio::test]
    async fn access_tokens_are_renewed_before_they_run_out_and_rotated_refresh_tokens_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (store, account, id) = store_with_grant(dir.path(), "mini@outlook.com", FetchAuth::Microsoft, 3600).await;
        let fake = FakeProvider::new(|_, _| Err("must not be asked".into()));
        let oauth = oauth_with(&fake);
        assert_eq!(oauth.access_token(&store, account, id, "mini@outlook.com").await.unwrap(), "at-0");
        assert!(fake.seen().is_empty());

        // Two minutes left: renewed.
        store.store_fetch_tokens(id, "at-0".into(), now() + 120, None).await.unwrap();
        fake.answer(|url, form| {
            assert!(url.ends_with("/consumers/oauth2/v2.0/token"), "{url}");
            assert_eq!((form["grant_type"].as_str(), form["refresh_token"].as_str()), ("refresh_token", "rt-0"));
            Ok((200, serde_json::json!({ "access_token": "at-1", "refresh_token": "rt-1", "expires_in": 3600 })))
        });
        assert_eq!(oauth.access_token(&store, account, id, "mini@outlook.com").await.unwrap(), "at-1");
        let kept = store.fetch_oauth(account, id).await.unwrap().unwrap();
        assert_eq!((kept.access_token.as_deref(), kept.refresh_token.as_deref()), (Some("at-1"), Some("rt-1")));
        assert!(kept.expires_at.unwrap() > now() + 3000);
        assert_eq!(fake.seen().len(), 1);
        // Asked again at once: the new one, not another renewal.
        assert_eq!(oauth.access_token(&store, account, id, "mini@outlook.com").await.unwrap(), "at-1");
        assert_eq!(fake.seen().len(), 1);

        // No new refresh token this time: the last one stays.
        store.store_fetch_tokens(id, "at-1".into(), now(), None).await.unwrap();
        fake.answer(|_, form| {
            assert_eq!(form["refresh_token"], "rt-1");
            Ok((200, serde_json::json!({ "access_token": "at-2", "expires_in": 3600 })))
        });
        assert_eq!(oauth.access_token(&store, account, id, "mini@outlook.com").await.unwrap(), "at-2");
        assert_eq!(store.fetch_oauth(account, id).await.unwrap().unwrap().refresh_token.as_deref(), Some("rt-1"));

        // Sealed at rest: neither token is in the database as it is.
        let mut raw = std::fs::read(dir.path().join("uwumail.db")).unwrap();
        raw.extend(std::fs::read(dir.path().join("uwumail.db-wal")).unwrap_or_default());
        assert!(!raw.windows(4).any(|w| w == b"rt-1"), "the refresh token is sealed");
    }

    /// A grant the provider ended: the mailbox says so, the person hears it once, and the provider
    /// is not asked again until they sign in anew. A provider that is down is asked later, not on
    /// every run.
    #[tokio::test]
    async fn an_ended_grant_waits_for_a_new_sign_in_and_a_provider_that_is_down_is_given_time() {
        let dir = tempfile::tempdir().unwrap();
        let (store, account, id) = store_with_grant(dir.path(), "mini@gmail.com", FetchAuth::Google, 0).await;
        let fake = FakeProvider::new(|_, _| Ok((400, serde_json::json!({ "error": "invalid_grant" }))));
        let oauth = oauth_with(&fake);
        let heard = Arc::new(Mutex::new(Vec::new()));
        let ear = heard.clone();
        oauth.on_notice(move |notice| ear.lock().unwrap().push(notice));

        // Without a client for Google there is nothing to renew with.
        assert_eq!(
            oauth.access_token(&store, account, id, "mini@gmail.com").await,
            Err(TokenError::NotConfigured(Provider::Google))
        );
        oauth.configure(FetchOAuthConfig {
            google_client_id: "gid".into(),
            google_client_secret: "gsecret".into(),
            ..Default::default()
        });
        assert_eq!(
            oauth.access_token(&store, account, id, "mini@gmail.com").await,
            Err(TokenError::Expired(Provider::Google))
        );
        assert_eq!(fake.seen()[0].1["client_secret"], "gsecret");
        let fetched = store.fetch_account(account, id).await.unwrap().unwrap();
        assert!(fetched.login_expired, "the mailbox says so");
        assert!(!store.fetch_accounts_due().await.unwrap().iter().any(|due| due.id == id), "and runs leave it be");
        assert_eq!(
            oauth.access_token(&store, account, id, "mini@gmail.com").await,
            Err(TokenError::Expired(Provider::Google))
        );
        assert_eq!(fake.seen().len(), 1, "Google is not asked again");
        assert_eq!(
            *heard.lock().unwrap(),
            [FetchNotice {
                account_id: account,
                address: "mini@gmail.com".into(),
                kind: FetchNoticeKind::LoginExpired(Provider::Google)
            }],
            "told once"
        );

        // A new sign-in starts it afresh.
        let grant = uwumail_store::FetchGrant {
            provider: FetchAuth::Google,
            tokens: FetchTokens {
                access_token: "at-new".into(),
                expires_at: now() + 3600,
                refresh_token: "rt-new".into(),
            },
        };
        store
            .update_fetch_account(
                account,
                id,
                uwumail_store::FetchAccountUpdate { oauth: Some(grant), ..Default::default() },
            )
            .await
            .unwrap();
        assert!(!store.fetch_account(account, id).await.unwrap().unwrap().login_expired);
        assert_eq!(oauth.access_token(&store, account, id, "mini@gmail.com").await.unwrap(), "at-new");

        // Down: a wait, and no second request before it is over.
        store.store_fetch_tokens(id, "at-new".into(), now(), None).await.unwrap();
        fake.answer(|_, _| Err("connection refused".into()));
        let Err(TokenError::Failed(reason)) = oauth.access_token(&store, account, id, "mini@gmail.com").await else {
            panic!("a failure")
        };
        assert!(reason.contains("trying again in 1 minutes"), "{reason}");
        assert!(matches!(
            oauth.access_token(&store, account, id, "mini@gmail.com").await,
            Err(TokenError::Waiting(at)) if at > now()
        ));
        assert_eq!(fake.seen().len(), 2);
        assert_eq!(store.fetch_oauth(account, id).await.unwrap().unwrap().failures, 1);
        assert_eq!(heard.lock().unwrap().len(), 1, "being down is not an ended sign-in");
    }
}
