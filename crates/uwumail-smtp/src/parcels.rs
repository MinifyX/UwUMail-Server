//! Asking carriers where a parcel is, by its number only (docs/jmap-shipments.md, "Asking the
//! carrier"). Never a link from a mail: the carrier and the number go to the fixed endpoints below,
//! nothing else about the person. Off unless the admin switches it on, and each person can say no.
//!
//! Only DHL, UPS and FedEx are asked, through their official APIs with the admin's keys. Other
//! carriers keep the link from the mail.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use base64::Engine;
use bytes::Bytes;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use chrono_tz::Europe::Berlin;
use http_body_util::Full;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use uwumail_labels::ShipmentStatus;
use uwumail_store::{
    EmailUpdate, MailboxRole, MailboxesChange, NewShipmentEvent, NewSpamLogEntry, ShipmentExpected,
    ShipmentLookupResult, ShipmentRefresh, SpamLogRecipient, Store,
};

use crate::Smtp;
use crate::egress::{Egress, Purpose};
use crate::provider_oauth::BoxFuture;

/// A refresh an app asked for waits this long for the carrier at most.
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(10);
/// The worker's share of a carrier's daily budget, in percent; the rest is for refreshes and
/// suspicious mail.
const BACKGROUND_SHARE: u32 = 80;
/// What refreshes and the check of suspicious mail may take of the day at most, in percent, so
/// neither leaves the worker without (security audit 0.38.0 S-2).
const PERSON_SHARE: u32 = 40;
const CROSS_CHECK_SHARE: u32 = 10;
/// Suspicious mails of one sender (domain or network) checked with carriers per day.
const CHECKS_PER_SENDER: u32 = 5;
/// Parcels one account may refresh per window.
const REFRESHES_PER_ACCOUNT: u32 = 20;
const REFRESH_WINDOW: i64 = 300;
/// Parcels the worker looks at per round.
const BATCH: i64 = 50;
const MAX_ANSWER: usize = 1024 * 1024;

const DHL_URL: &str = "https://api-eu.dhl.com/track/shipments";
const UPS_TOKEN_URL: &str = "https://onlinetools.ups.com/security/v1/oauth/token";
const UPS_TRACK_URL: &str = "https://onlinetools.ups.com/api/track/v1/details/";
const FEDEX_TOKEN_URL: &str = "https://apis.fedex.com/oauth/token";
const FEDEX_TRACK_URL: &str = "https://apis.fedex.com/track/v1/trackingnumbers";

/// `[shipments.lookup]` in the configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LookupConfig {
    pub enabled: bool,
    /// developer.dhl.com, "Shipment Tracking - Unified"; also answers for Deutsche Post.
    pub dhl_key: String,
    pub ups_client_id: String,
    pub ups_client_secret: String,
    pub fedex_client_id: String,
    pub fedex_client_secret: String,
}

/// A request to a carrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParcelRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Option<String>,
}

/// How a request reaches a carrier: through the egress, or a stand-in in tests.
pub trait ParcelTransport: Send + Sync {
    /// The status and the body of the answer.
    fn send(&self, request: ParcelRequest) -> BoxFuture<'_, Result<(u16, Vec<u8>), String>>;
}

/// The egress, with [`Purpose::Parcels`]: through the proxy when the admin sends it there.
pub struct EgressTransport(pub Egress);

impl ParcelTransport for EgressTransport {
    fn send(&self, request: ParcelRequest) -> BoxFuture<'_, Result<(u16, Vec<u8>), String>> {
        Box::pin(async move {
            let mut builder = hyper::Request::builder().method(request.method).uri(&request.url);
            for (name, value) in &request.headers {
                builder = builder.header(*name, value);
            }
            let request = builder
                .body(Full::new(Bytes::from(request.body.unwrap_or_default())))
                .map_err(|err| err.to_string())?;
            self.0
                .exchange(Purpose::Parcels, request, MAX_ANSWER)
                .await
                .map(|(status, body)| (status, body.to_vec()))
                .map_err(|err| err.to_string())
        })
    }
}

/// Who answers for a carrier, with its own budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Service {
    Dhl,
    Ups,
    Fedex,
}

impl Service {
    fn of(carrier: &str) -> Option<Service> {
        Some(match carrier {
            "dhl" | "deutschePost" => Service::Dhl,
            "ups" => Service::Ups,
            "fedex" => Service::Fedex,
            _ => return None,
        })
    }

    /// Seconds between two requests and requests per day (UTC). DHL's are its free plan's.
    fn limits(self) -> (i64, u32) {
        match self {
            Service::Dhl => (5, 250),
            Service::Ups | Service::Fedex => (1, 1000),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Budget {
    next_at: i64,
    day: i64,
    used: u32,
    by_people: u32,
    by_cross_check: u32,
}

/// Who asks a carrier, for its share of the budget.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Asker {
    Worker,
    Person,
    CrossCheck,
}

/// Asking carriers; one per server, changed in place from the admin panel.
pub struct Parcels {
    config: RwLock<LookupConfig>,
    transport: RwLock<Arc<dyn ParcelTransport>>,
    budgets: Mutex<HashMap<Service, Budget>>,
    /// Today's suspicious mails checked per sender, with the day.
    checked_senders: Mutex<(i64, HashMap<String, u32>)>,
    /// Refreshes per account in the current window, with its start.
    refreshes: Mutex<HashMap<i64, (i64, u32)>>,
    /// Access tokens of UPS and FedEx with when they run out.
    tokens: Mutex<HashMap<Service, (String, i64)>>,
}

impl Default for Parcels {
    fn default() -> Self {
        Parcels::new(Arc::new(EgressTransport(Egress::direct())))
    }
}

/// What went wrong, as `lookupError` names it.
type Failure = &'static str;
const NOT_FOUND: Failure = "notFound";
const UNAVAILABLE: Failure = "unavailable";
const RATE_LIMITED: Failure = "rateLimited";

impl Parcels {
    pub fn new(transport: Arc<dyn ParcelTransport>) -> Parcels {
        Parcels {
            config: RwLock::default(),
            transport: RwLock::new(transport),
            budgets: Mutex::default(),
            checked_senders: Mutex::default(),
            refreshes: Mutex::default(),
            tokens: Mutex::default(),
        }
    }

    pub fn configure(&self, config: LookupConfig) {
        let mut current = self.config.write().unwrap_or_else(|e| e.into_inner());
        if *current != config {
            self.tokens.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
        *current = config;
    }

    pub fn set_transport(&self, transport: Arc<dyn ParcelTransport>) {
        *self.transport.write().unwrap_or_else(|e| e.into_inner()) = transport;
    }

    fn config(&self) -> LookupConfig {
        self.config.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The carriers asked on this server ([`uwumail_labels::Carrier::id`]s); empty when it is off.
    pub fn carriers(&self) -> Vec<&'static str> {
        let config = self.config();
        let mut carriers = Vec::new();
        if !config.enabled {
            return carriers;
        }
        if !config.dhl_key.trim().is_empty() {
            carriers.extend(["dhl", "deutschePost"]);
        }
        if !config.ups_client_id.trim().is_empty() && !config.ups_client_secret.trim().is_empty() {
            carriers.push("ups");
        }
        if !config.fedex_client_id.trim().is_empty() && !config.fedex_client_secret.trim().is_empty() {
            carriers.push("fedex");
        }
        carriers
    }

    /// Whether a carrier's official API answers here, so that "unknown" from it means unknown.
    pub fn answers_for_sure(&self, carrier: &str) -> bool {
        self.carriers().contains(&carrier)
    }

    /// Takes one request from a carrier's budget at `now`; the worker leaves a share of the day for
    /// the rest, and people and the check of suspicious mail each leave the worker some.
    fn take(&self, service: Service, now: i64, asker: Asker) -> bool {
        let (gap, per_day) = service.limits();
        let mut budgets = self.budgets.lock().unwrap_or_else(|e| e.into_inner());
        let budget = budgets.entry(service).or_default();
        let day = now.div_euclid(86_400);
        if budget.day != day {
            *budget = Budget { next_at: budget.next_at, day, ..Default::default() };
        }
        let within = match asker {
            Asker::Worker => budget.used < per_day * BACKGROUND_SHARE / 100,
            Asker::Person => budget.used < per_day && budget.by_people < per_day * PERSON_SHARE / 100,
            Asker::CrossCheck => budget.used < per_day && budget.by_cross_check < per_day * CROSS_CHECK_SHARE / 100,
        };
        if now < budget.next_at || !within {
            return false;
        }
        budget.used += 1;
        budget.by_people += u32::from(asker == Asker::Person);
        budget.by_cross_check += u32::from(asker == Asker::CrossCheck);
        budget.next_at = now + gap;
        true
    }

    /// Counts one refresh of `account_id` at `now`; false once it had its share of the window.
    fn take_refresh(&self, account_id: i64, now: i64) -> bool {
        let mut refreshes = self.refreshes.lock().unwrap_or_else(|e| e.into_inner());
        let (since, count) = refreshes.entry(account_id).or_insert((now, 0));
        if now - *since >= REFRESH_WINDOW {
            (*since, *count) = (now, 0);
        }
        *count += 1;
        *count <= REFRESHES_PER_ACCOUNT
    }

    /// Counts one checked mail of `sender` at `now`; false once the sender had its share of the day.
    fn take_sender(&self, sender: &str, now: i64) -> bool {
        let mut checked = self.checked_senders.lock().unwrap_or_else(|e| e.into_inner());
        let day = now.div_euclid(86_400);
        if checked.0 != day {
            *checked = (day, HashMap::new());
        }
        let count = checked.1.entry(sender.to_owned()).or_default();
        *count += 1;
        *count <= CHECKS_PER_SENDER
    }

    /// Asks the carrier about a parcel at `now`. Never fails: what went wrong is in `error`.
    async fn lookup(&self, carrier: &str, number: &str, now: i64, asker: Asker) -> ShipmentLookupResult {
        let failed = |error| ShipmentLookupResult { error: Some(error), ..Default::default() };
        let Some(service) = Service::of(carrier).filter(|_| self.carriers().contains(&carrier)) else {
            return failed(UNAVAILABLE);
        };
        // Only what a number is made of goes into a URL.
        if number.is_empty() || number.len() > 40 || !number.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return failed(UNAVAILABLE);
        }
        if !self.take(service, now, asker) {
            return failed(RATE_LIMITED);
        }
        let answer = match service {
            Service::Dhl => self.dhl(number).await,
            Service::Ups => self.ups(number, now).await,
            Service::Fedex => self.fedex(number, now).await,
        };
        match answer {
            Ok((events, expected)) => ShipmentLookupResult { events, expected, error: None },
            Err(error) => {
                if error == UNAVAILABLE {
                    tracing::info!(?service, "a carrier did not answer about a parcel");
                }
                failed(error)
            }
        }
    }

    /// The parcels due at `now`, asked about one after the other within each carrier's budget.
    /// Returns whether some were held back by a budget.
    pub async fn run_due(&self, store: &Store, now: i64) -> bool {
        let due = match store.shipment_lookups_due(self.carriers(), now, BATCH).await {
            Ok(due) => due,
            Err(err) => {
                tracing::error!(%err, "listing the parcels to ask carriers about failed");
                return false;
            }
        };
        let mut held = false;
        for lookup in due {
            let result = self.lookup(&lookup.carrier, &lookup.number, now, Asker::Worker).await;
            // Held back by the budget, it stays due and is asked about next round.
            if result.error == Some(RATE_LIMITED) {
                held = true;
                continue;
            }
            if let Err(err) = store.record_shipment_lookup(lookup.account_id, lookup.id, now, result).await {
                tracing::error!(%err, "noting what a carrier said failed");
            }
        }
        held
    }

    /// `Shipment/refresh` for one parcel: `Ok(())` when the carrier was asked and answered,
    /// else the `notRefreshed` type. A parcel that is not there is the store's `NotFound`.
    pub async fn refresh(
        &self,
        store: &Store,
        account_id: i64,
        id: i64,
        now: i64,
    ) -> uwumail_store::Result<Result<(), &'static str>> {
        if !self.take_refresh(account_id, now) {
            return Ok(Err("tooSoon"));
        }
        let lookup = match store.claim_shipment_refresh(account_id, id, now, self.carriers()).await? {
            ShipmentRefresh::Go(lookup) => lookup,
            ShipmentRefresh::TooSoon => return Ok(Err("tooSoon")),
            ShipmentRefresh::NotSupported => return Ok(Err("notSupported")),
        };
        let result =
            tokio::time::timeout(REFRESH_TIMEOUT, self.lookup(&lookup.carrier, &lookup.number, now, Asker::Person))
                .await
                .unwrap_or_else(|_| ShipmentLookupResult { error: Some(UNAVAILABLE), ..Default::default() });
        let outcome = match result.error {
            None => Ok(()),
            Some(NOT_FOUND) => Err(NOT_FOUND),
            Some(_) => Err(UNAVAILABLE),
        };
        store.record_shipment_lookup(account_id, id, now, result).await?;
        Ok(outcome)
    }

    async fn send(&self, request: ParcelRequest) -> Result<(u16, Value), Failure> {
        let transport = self.transport.read().unwrap_or_else(|e| e.into_inner()).clone();
        let (status, body) = transport.send(request).await.map_err(|err| {
            tracing::debug!(%err, "a request to a carrier failed");
            UNAVAILABLE
        })?;
        match status {
            // Only the carrier's own answer says it does not know a parcel; a bare 404 may be a proxy's
            // (security audit 0.38.0 S-4).
            404 if serde_json::from_slice::<Value>(&body).is_ok_and(|json| json.is_object()) => return Err(NOT_FOUND),
            404 => return Err(UNAVAILABLE),
            429 => return Err(RATE_LIMITED),
            401 | 403 => {
                tracing::warn!(status, "a carrier refused the key of the parcel lookup");
                return Err(UNAVAILABLE);
            }
            200..=299 => {}
            // Redirects are not followed: a tracking page that sends elsewhere is gone.
            _ => return Err(UNAVAILABLE),
        }
        serde_json::from_slice(&body).map(|json| (status, json)).map_err(|_| UNAVAILABLE)
    }

    async fn token(&self, service: Service, now: i64) -> Result<String, Failure> {
        if let Some((token, until)) = self.tokens.lock().unwrap_or_else(|e| e.into_inner()).get(&service)
            && *until > now
        {
            return Ok(token.clone());
        }
        let config = self.config();
        let form =
            |pairs: &[(&str, &str)]| url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish();
        let request = match service {
            Service::Ups => {
                let basic = base64::engine::general_purpose::STANDARD.encode(format!(
                    "{}:{}",
                    config.ups_client_id.trim(),
                    config.ups_client_secret.trim()
                ));
                ParcelRequest {
                    method: "POST",
                    url: UPS_TOKEN_URL.into(),
                    headers: vec![
                        ("authorization", format!("Basic {basic}")),
                        ("content-type", "application/x-www-form-urlencoded".into()),
                    ],
                    body: Some(form(&[("grant_type", "client_credentials")])),
                }
            }
            _ => ParcelRequest {
                method: "POST",
                url: FEDEX_TOKEN_URL.into(),
                headers: vec![("content-type", "application/x-www-form-urlencoded".into())],
                body: Some(form(&[
                    ("grant_type", "client_credentials"),
                    ("client_id", config.fedex_client_id.trim()),
                    ("client_secret", config.fedex_client_secret.trim()),
                ])),
            },
        };
        let (_, json) = self.send(request).await.map_err(|_| UNAVAILABLE)?;
        let token = json["access_token"].as_str().filter(|token| !token.is_empty()).ok_or(UNAVAILABLE)?.to_owned();
        // UPS sends the seconds as a string, FedEx as a number.
        let lasts = json["expires_in"].as_i64().or_else(|| json["expires_in"].as_str()?.parse().ok()).unwrap_or(600);
        self.tokens.lock().unwrap_or_else(|e| e.into_inner()).insert(service, (token.clone(), now + lasts - 60));
        Ok(token)
    }

    async fn dhl(&self, number: &str) -> Answer {
        let key = self.config().dhl_key.trim().to_owned();
        let request = ParcelRequest {
            method: "GET",
            url: format!("{DHL_URL}?trackingNumber={number}"),
            headers: vec![("dhl-api-key", key), ("accept", "application/json".into())],
            body: None,
        };
        let (_, json) = self.send(request).await?;
        parse_dhl(&json)
    }

    async fn ups(&self, number: &str, now: i64) -> Answer {
        let token = self.token(Service::Ups, now).await?;
        let request = ParcelRequest {
            method: "GET",
            url: format!("{UPS_TRACK_URL}{number}?locale=de_DE&returnSignature=false"),
            headers: vec![
                ("authorization", format!("Bearer {token}")),
                ("transid", hex::encode(rand_bytes())),
                ("transactionsrc", "uwumail".into()),
                ("accept", "application/json".into()),
            ],
            body: None,
        };
        let answer = self.send(request).await;
        if answer == Err(UNAVAILABLE) {
            self.tokens.lock().unwrap_or_else(|e| e.into_inner()).remove(&Service::Ups);
        }
        parse_ups(&answer?.1)
    }

    async fn fedex(&self, number: &str, now: i64) -> Answer {
        let token = self.token(Service::Fedex, now).await?;
        let body = json!({
            "includeDetailedScans": true,
            "trackingInfo": [{ "trackingNumberInfo": { "trackingNumber": number } }],
        });
        let request = ParcelRequest {
            method: "POST",
            url: FEDEX_TRACK_URL.into(),
            headers: vec![
                ("authorization", format!("Bearer {token}")),
                ("content-type", "application/json".into()),
                ("x-locale", "de_DE".into()),
            ],
            body: Some(body.to_string()),
        };
        let answer = self.send(request).await;
        if answer == Err(UNAVAILABLE) {
            self.tokens.lock().unwrap_or_else(|e| e.into_inner()).remove(&Service::Fedex);
        }
        parse_fedex(&answer?.1)
    }
}

/// The finding a mail gets when its carrier does not know its parcel number.
pub const PARCEL_NUMBER_UNKNOWN: &str = "PARCEL_NUMBER_UNKNOWN";

/// The parcel a doubtful, unauthenticated mail told one person of, as it was stored for them.
pub(crate) struct ParcelCheck {
    pub account_id: i64,
    pub address: String,
    pub email_ids: Vec<i64>,
    pub carrier: &'static str,
    pub numbers: Vec<String>,
}

/// Puts the numbers of a doubtful mail to their carriers at `now`. Where the carrier's official
/// API says for sure that it does not know every one of them, the mail goes to Junk and the spam
/// history gets `entry` with [`PARCEL_NUMBER_UNKNOWN`]. An error or a timeout changes nothing.
pub(crate) async fn cross_check(
    smtp: Smtp,
    entry: Option<NewSpamLogEntry>,
    sender: Option<String>,
    checks: Vec<ParcelCheck>,
    now: i64,
) {
    let (parcels, store) = (smtp.parcels(), smtp.store());
    // One sender must not take the whole share of the day.
    if !parcels.take_sender(sender.as_deref().unwrap_or_default(), now) {
        return;
    }
    let mut known: HashMap<(&'static str, String), bool> = HashMap::new();
    let mut moved = Vec::new();
    for check in checks {
        if check.numbers.is_empty()
            || !parcels.answers_for_sure(check.carrier)
            || !store.shipment_lookup_enabled(check.account_id).await.unwrap_or(false)
        {
            continue;
        }
        let mut unknown = true;
        for number in check.numbers.iter().take(3) {
            let key = (check.carrier, number.clone());
            let is_unknown = match known.get(&key) {
                Some(is_unknown) => *is_unknown,
                None => {
                    let asked = tokio::time::timeout(
                        REFRESH_TIMEOUT,
                        parcels.lookup(check.carrier, number, now, Asker::CrossCheck),
                    );
                    let is_unknown = asked.await.is_ok_and(|result| result.error == Some(NOT_FOUND));
                    known.insert(key, is_unknown);
                    is_unknown
                }
            };
            unknown &= is_unknown;
        }
        if !unknown {
            continue;
        }
        let junk = match store.mailboxes(check.account_id).await {
            Ok(mailboxes) => mailboxes.into_iter().find(|mailbox| mailbox.role == Some(MailboxRole::Junk)),
            Err(err) => {
                tracing::warn!(account = check.account_id, %err, "finding the Junk mailbox failed");
                None
            }
        };
        let Some(junk) = junk else { continue };
        let updates = check
            .email_ids
            .iter()
            .map(|&id| EmailUpdate { id, mailboxes: MailboxesChange::Replace(vec![junk.id]), ..Default::default() })
            .collect();
        match store.update_emails_by_server(check.account_id, updates).await {
            Ok(_) => {
                tracing::info!(
                    account = check.account_id,
                    carrier = check.carrier,
                    "the carrier does not know the parcel of a doubtful mail; it went to Junk"
                );
                moved.push(SpamLogRecipient {
                    address: check.address,
                    action: "junk".into(),
                    mailbox: Some("junk".into()),
                });
            }
            Err(err) => {
                tracing::warn!(account = check.account_id, %err, "moving a mail with an unknown parcel to Junk failed")
            }
        }
    }
    if let Some(mut entry) = entry.filter(|_| !moved.is_empty()) {
        entry.recipients = moved;
        if let Err(err) = store.add_spam_log(entry).await {
            tracing::warn!(%err, "writing the spam history failed");
        }
    }
}

fn rand_bytes() -> [u8; 16] {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes
}

/// Asks carriers about the parcels that are due, until `shutdown` changes: every minute, or every
/// 15 seconds while a budget holds some back.
pub async fn run_parcel_lookups(smtp: Smtp, mut shutdown: watch::Receiver<bool>) {
    loop {
        let held = smtp.parcels().run_due(smtp.store(), crate::now()).await;
        let wait = if held { Duration::from_secs(15) } else { Duration::from_secs(60) };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shutdown.changed() => break,
        }
    }
}

type Answer = Result<(Vec<NewShipmentEvent>, Option<ShipmentExpected>), Failure>;

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned)
}

/// A carrier's time: with an offset as it says, without one as the wall clock in Germany.
fn instant(value: &str) -> Option<i64> {
    let value = value.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(value) {
        return Some(at.timestamp());
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%d.%m.%Y, %H:%M",
        "%d.%m.%Y %H:%M",
    ] {
        if let Ok(at) = NaiveDateTime::parse_from_str(value, format) {
            return local(at);
        }
    }
    ["%Y-%m-%d", "%d.%m.%Y", "%Y%m%d"]
        .iter()
        .find_map(|format| NaiveDate::parse_from_str(value, format).ok())
        .and_then(|day| local(day.and_time(NaiveTime::MIN)))
}

fn local(at: NaiveDateTime) -> Option<i64> {
    Berlin.from_local_datetime(&at).earliest().map(|at| at.timestamp())
}

/// What a carrier's words say beyond its code: out for delivery, waiting for pickup, on its way back.
fn by_text(text: &str) -> Option<ShipmentStatus> {
    let text = text.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| text.contains(word));
    if has(&["rücksendung", "zurück an den absender", "zurückgesendet", "retoure", "returned to", "return to sender"])
    {
        Some(ShipmentStatus::Returned)
    } else if has(&[
        "nicht zugestellt",
        "nicht angetroffen",
        "zustellung nicht möglich",
        "not delivered",
        "delivery attempt",
    ]) {
        Some(ShipmentStatus::DeliveryFailed)
    } else if has(&[
        "zur abholung bereit",
        "abholbereit",
        "liegt zur abholung",
        "ready for pickup",
        "ready for collection",
    ]) {
        Some(ShipmentStatus::ReadyForPickup)
    } else if has(&["in zustellung", "zustellfahrzeug", "zustelltour", "out for delivery", "on the way to you"]) {
        Some(ShipmentStatus::OutForDelivery)
    } else {
        None
    }
}

fn dhl_status(code: &str, words: &str) -> Option<ShipmentStatus> {
    match code {
        "delivered" => Some(ShipmentStatus::Delivered),
        "failure" => Some(by_text(words).unwrap_or(ShipmentStatus::DeliveryFailed)),
        "pre-transit" => Some(ShipmentStatus::Shipped),
        "transit" => Some(by_text(words).unwrap_or(ShipmentStatus::InTransit)),
        _ => by_text(words),
    }
}

/// DHL's Unified API: `shipments[0]` with `events` (newest first) and an estimated delivery.
fn parse_dhl(json: &Value) -> Answer {
    let shipment = json["shipments"].get(0).ok_or(NOT_FOUND)?;
    let event_of = |event: &Value| {
        let words = [text(&event["status"]), text(&event["description"])].into_iter().flatten().collect::<Vec<_>>();
        let status = dhl_status(event["statusCode"].as_str().unwrap_or(""), &words.join(" "))?;
        Some(NewShipmentEvent {
            at: instant(event["timestamp"].as_str()?)?,
            status,
            text: text(&event["description"]).or_else(|| text(&event["status"])),
            location: text(&event["location"]["address"]["addressLocality"]),
        })
    };
    let mut events: Vec<NewShipmentEvent> =
        shipment["events"].as_array().into_iter().flatten().filter_map(event_of).collect();
    if events.is_empty() {
        events.extend(event_of(&shipment["status"]));
    }
    let frame = &shipment["estimatedDeliveryTimeFrame"];
    let expected =
        match (frame["estimatedFrom"].as_str().and_then(instant), shipment["estimatedTimeOfDelivery"].as_str()) {
            (Some(from), _) => Some(ShipmentExpected {
                from,
                to: frame["estimatedThrough"].as_str().and_then(instant),
                day_only: false,
            }),
            (None, Some(at)) => instant(at).map(|from| ShipmentExpected { from, to: None, day_only: at.len() <= 10 }),
            _ => None,
        };
    Ok((events, expected))
}

/// UPS's Track API: `trackResponse.shipment[0].package[0].activity`, a warning when it does not
/// know the number.
fn parse_ups(json: &Value) -> Answer {
    let shipment = &json["trackResponse"]["shipment"][0];
    let unknown = shipment["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|warning| warning["code"].as_str().is_some_and(|code| code.starts_with("TW0001")));
    let package = &shipment["package"][0];
    if unknown || package.is_null() {
        return Err(NOT_FOUND);
    }
    let events = package["activity"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|activity| {
            let words = text(&activity["status"]["description"]);
            let status = match activity["status"]["type"].as_str()? {
                "M" | "P" => ShipmentStatus::Shipped,
                "I" => words.as_deref().and_then(by_text).unwrap_or(ShipmentStatus::InTransit),
                "O" => ShipmentStatus::OutForDelivery,
                "D" => ShipmentStatus::Delivered,
                "X" => words.as_deref().and_then(by_text).unwrap_or(ShipmentStatus::DeliveryFailed),
                "RS" => ShipmentStatus::Returned,
                _ => return None,
            };
            // shortcut: UPS gives the wall clock where the scan was, read as Germany's; off by the
            // time zone for scans abroad, which only shifts their order among themselves.
            let at = format!("{} {}", activity["date"].as_str()?, activity["time"].as_str().unwrap_or("000000"));
            let at = local(NaiveDateTime::parse_from_str(&at, "%Y%m%d %H%M%S").ok()?)?;
            Some(NewShipmentEvent { at, status, text: words, location: text(&activity["location"]["address"]["city"]) })
        })
        .collect();
    let expected = package["deliveryDate"].as_array().into_iter().flatten().find_map(|date| {
        if date["type"].as_str() == Some("DEL") {
            return None;
        }
        let day = NaiveDate::parse_from_str(date["date"].as_str()?, "%Y%m%d").ok()?;
        let time = |key: &str| {
            let raw = package["deliveryTime"][key].as_str()?;
            NaiveTime::parse_from_str(raw, "%H%M%S").ok().and_then(|time| local(day.and_time(time)))
        };
        match time("startTime") {
            Some(from) => Some(ShipmentExpected { from, to: time("endTime"), day_only: false }),
            None => local(day.and_time(NaiveTime::MIN)).map(|from| ShipmentExpected { from, to: None, day_only: true }),
        }
    });
    Ok((events, expected))
}

/// FedEx's Track API: `output.completeTrackResults[0].trackResults[0]` with `scanEvents`.
fn parse_fedex(json: &Value) -> Answer {
    let result = &json["output"]["completeTrackResults"][0]["trackResults"][0];
    if result.is_null() || result["error"]["code"].as_str().is_some_and(|code| code.ends_with("NOTFOUND")) {
        return Err(NOT_FOUND);
    }
    let events = result["scanEvents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|scan| {
            let words = text(&scan["eventDescription"]);
            let status = match scan["eventType"].as_str()? {
                "OC" | "PU" => ShipmentStatus::Shipped,
                "AR" | "DP" | "IT" | "AF" | "CC" | "PM" => ShipmentStatus::InTransit,
                "OD" => ShipmentStatus::OutForDelivery,
                "DL" => ShipmentStatus::Delivered,
                "DE" | "DY" => ShipmentStatus::DeliveryFailed,
                "RS" => ShipmentStatus::Returned,
                "CA" => ShipmentStatus::Cancelled,
                "HP" => ShipmentStatus::ReadyForPickup,
                _ => return None,
            };
            Some(NewShipmentEvent {
                at: instant(scan["date"].as_str()?)?,
                status,
                text: words,
                location: text(&scan["scanLocation"]["city"]),
            })
        })
        .collect();
    let window = &result["estimatedDeliveryTimeWindow"]["window"];
    let expected = window["begins"].as_str().and_then(instant).map(|from| ShipmentExpected {
        from,
        to: window["ends"].as_str().and_then(instant),
        day_only: false,
    });
    Ok((events, expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_without_an_offset_are_germanys() {
        // 10:15 in winter in Berlin is 09:15 UTC.
        assert_eq!(instant("2026-01-12T10:15:00"), Some(1_768_209_300));
        assert_eq!(instant("2026-01-12T10:15:00+01:00"), Some(1_768_209_300));
        assert_eq!(instant("12.01.2026, 10:15"), Some(1_768_209_300));
        assert_eq!(instant("2026-01-12 10:15:00"), Some(1_768_209_300));
        assert_eq!(instant("soon"), None);
    }

    #[test]
    fn budgets_keep_the_gap_and_a_share_for_people() {
        let parcels = Parcels::default();
        let day = 20_000 * 86_400;
        assert!(parcels.take(Service::Dhl, day, Asker::Worker));
        assert!(!parcels.take(Service::Dhl, day + 4, Asker::Person), "5 s between two");
        assert!(parcels.take(Service::Dhl, day + 5, Asker::Person));
        let mut at = day + 10;
        let mut background = 2;
        while parcels.take(Service::Dhl, at, Asker::Worker) {
            background += 1;
            at += 5;
        }
        assert_eq!(background, 200, "the worker stops at 80 % of 250");
        assert!(parcels.take(Service::Dhl, at, Asker::Person), "a person still gets one");
        assert!(parcels.take(Service::Dhl, day + 86_400, Asker::Worker), "a new day starts again");

        // People and the check of suspicious mail each stop at their share, and leave the worker some.
        let mut at = 2 * day;
        let mut taken = |asker| {
            let mut n = 0;
            while parcels.take(Service::Dhl, at, asker) {
                n += 1;
                at += 5;
            }
            at += 5;
            n
        };
        assert_eq!((taken(Asker::Person), taken(Asker::CrossCheck), taken(Asker::Worker)), (100, 25, 75));
    }

    struct Answer(u16, &'static str);

    impl ParcelTransport for Answer {
        fn send(&self, _: ParcelRequest) -> BoxFuture<'_, Result<(u16, Vec<u8>), String>> {
            let answer = (self.0, self.1.as_bytes().to_vec());
            Box::pin(async move { Ok(answer) })
        }
    }

    #[tokio::test]
    async fn only_the_carriers_own_answer_says_not_found() {
        for (body, expected) in
            [(r#"{"title":"No shipment found"}"#, NOT_FOUND), ("<html>Not Found</html>", UNAVAILABLE)]
        {
            let parcels = Parcels::new(Arc::new(Answer(404, body)));
            let answer = parcels
                .send(ParcelRequest {
                    method: "GET",
                    url: "https://carrier.example/".into(),
                    headers: Vec::new(),
                    body: None,
                })
                .await;
            assert_eq!(answer.err(), Some(expected), "{body}");
        }
    }

    #[test]
    fn refreshes_and_checks_have_limits_of_their_own() {
        let parcels = Parcels::default();
        assert!((0..20).all(|_| parcels.take_refresh(1, 1_000)));
        assert!(!parcels.take_refresh(1, 1_299), "20 parcels in five minutes per account");
        assert!(parcels.take_refresh(2, 1_299), "another account is not held back");
        assert!(parcels.take_refresh(1, 1_300));
        assert!((0..5).all(|_| parcels.take_sender("domain:example.com", 1_000)));
        assert!(!parcels.take_sender("domain:example.com", 1_000));
        assert!(parcels.take_sender("domain:example.com", 1_000 + 86_400), "a new day starts again");
    }

    #[test]
    fn words_refine_a_code() {
        assert_eq!(
            dhl_status("transit", "Die Sendung wurde in das Zustellfahrzeug geladen."),
            Some(ShipmentStatus::OutForDelivery)
        );
        assert_eq!(
            dhl_status("transit", "Die Sendung liegt zur Abholung bereit."),
            Some(ShipmentStatus::ReadyForPickup)
        );
        assert_eq!(
            dhl_status("transit", "Die Sendung wurde im Paketzentrum bearbeitet."),
            Some(ShipmentStatus::InTransit)
        );
        assert_eq!(dhl_status("unknown", "Sendung angekündigt"), None);
    }
}
