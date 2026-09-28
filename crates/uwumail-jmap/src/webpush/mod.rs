//! Web Push (RFC 8030) for JMAP `PushSubscription` (RFC 8620, section 7.2): a device registers an
//! address at its push service, and the server POSTs a `StateChange` there when something changed,
//! so an app hears of new mail while it is closed. See docs/jmap-push.md.
//!
//! Only the state strings leave the server, never content: encrypted for the device when it gave
//! keys (RFC 8291), and signed with the server's VAPID key (RFC 8292, RFC 9749). Changes are
//! collected for a moment and a subscription gets at most one push every few seconds; a push
//! service that answers 404 or 410 ends the subscription, and one that fails is asked again later.

pub(crate) mod ece;
pub(crate) mod vapid;

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use serde_json::{Map, Value, json};
use tokio::sync::{OnceCell, broadcast, watch};
use uwumail_smtp::egress::Egress;
use uwumail_store::{PushTarget, Store};

use crate::push::{all_types, shared_type_states, type_states};
use crate::{Jmap, ids};
use vapid::Vapid;

/// How changes are bundled: collected for `debounce` after the first one before they are pushed,
/// and at most one push per subscription every `min_interval`; what comes in between waits and goes
/// along with the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushTiming {
    pub debounce: Duration,
    pub min_interval: Duration,
}

impl Default for PushTiming {
    fn default() -> Self {
        PushTiming { debounce: Duration::from_secs(2), min_interval: Duration::from_secs(5) }
    }
}
/// How long push services keep a message for a device that is away. Whatever is older is found by
/// syncing anyway.
const STATE_TTL_SECS: u32 = 12 * 3600;
/// The verification code is only of use while the subscription is young (it goes after a day).
const VERIFICATION_TTL_SECS: u32 = 24 * 3600;
/// A later StateChange replaces an earlier one still waiting at the push service (RFC 8030, 5.4).
const TOPIC: &str = "jmap-state";
/// Expired subscriptions and those whose login ended are dropped this often.
const PURGE_INTERVAL: Duration = Duration::from_secs(3600);
/// New subscriptions per account and hour: each sends one request to an address of the client's
/// choosing.
const MAX_CREATES_PER_HOUR: u32 = 30;

/// One push message: where to, the headers, and the body as it goes out.
#[derive(Debug, Clone)]
pub struct PushMessage {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// How push messages leave the server. The server's own goes over [`Egress::post`]: https only, to
/// public addresses only. Tests hand in their own to reach a push service on the same machine.
pub trait PushTransport: Send + Sync + 'static {
    /// Whether this address may be subscribed at all; the reason why not, when not.
    fn check_url(&self, url: &str) -> Result<(), String>;
    /// POSTs the message; answers the HTTP status, or why there was none.
    fn post(&self, message: PushMessage) -> Pin<Box<dyn Future<Output = Result<u16, String>> + Send + '_>>;
}

/// The way out for the server: [`Egress::post`].
struct EgressTransport(Egress);

impl PushTransport for EgressTransport {
    fn check_url(&self, url: &str) -> Result<(), String> {
        uwumail_smtp::fetch::check_url(url, false).map(|_| ())
    }

    fn post(&self, message: PushMessage) -> Pin<Box<dyn Future<Output = Result<u16, String>> + Send + '_>> {
        Box::pin(async move {
            self.0.post(&message.url, &message.headers, message.body).await.map_err(|err| err.to_string())
        })
    }
}

/// What the JMAP service keeps for Web Push. Cheap to clone.
#[derive(Clone)]
pub(crate) struct WebPush {
    store: Store,
    transport: Arc<dyn PushTransport>,
    vapid: Arc<OnceCell<Arc<Vapid>>>,
    /// Who the pushes come from, in the VAPID token: the server's own address.
    subject: String,
    /// New subscriptions per account in the current hour.
    creates: Arc<Mutex<HashMap<i64, (u32, Instant)>>>,
    timing: PushTiming,
}

/// What became of a push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Delivered,
    /// The push service no longer knows the subscription.
    Gone,
    Failed,
}

impl WebPush {
    pub fn new(store: Store, egress: Egress, hostname: &str) -> WebPush {
        WebPush {
            store,
            transport: Arc::new(EgressTransport(egress)),
            vapid: Arc::default(),
            subject: format!("https://{hostname}"),
            creates: Arc::default(),
            timing: PushTiming::default(),
        }
    }

    pub fn with_timing(self, timing: PushTiming) -> WebPush {
        WebPush { timing, ..self }
    }

    pub fn with_transport(self, transport: Arc<dyn PushTransport>) -> WebPush {
        WebPush { transport, ..self }
    }

    pub fn with_egress(self, egress: Egress) -> WebPush {
        self.with_transport(Arc::new(EgressTransport(egress)))
    }

    pub fn check_url(&self, url: &str) -> Result<(), String> {
        self.transport.check_url(url)
    }

    /// The server's VAPID key, read (or made) once.
    pub async fn vapid(&self) -> Option<Arc<Vapid>> {
        let store = self.store.clone();
        let loaded = self
            .vapid
            .get_or_try_init(|| async move {
                let pkcs8 = store.push_vapid_key().await.map_err(|err| err.to_string())?;
                Vapid::from_pkcs8(&pkcs8).map(Arc::new).ok_or_else(|| "the stored VAPID key is unusable".to_owned())
            })
            .await;
        match loaded {
            Ok(vapid) => Some(vapid.clone()),
            Err(err) => {
                tracing::error!(%err, "the VAPID key could not be read");
                None
            }
        }
    }

    /// Counts a new subscription; false when the account made too many this hour.
    pub fn may_create(&self, account_id: i64) -> bool {
        let mut creates = self.creates.lock().unwrap_or_else(|e| e.into_inner());
        creates.retain(|_, (_, since)| since.elapsed() < Duration::from_secs(3600));
        let entry = creates.entry(account_id).or_insert((0, Instant::now()));
        entry.0 += 1;
        entry.0 <= MAX_CREATES_PER_HOUR
    }

    /// Sends a new subscription its verification code (RFC 8620, 7.2.2). Not retried: a code that
    /// does not arrive leaves the subscription unverified, and it goes after a day.
    pub fn send_verification(&self, target: PushTarget) {
        let push = self.clone();
        tokio::spawn(async move {
            let body = json!({
                "@type": "PushVerification",
                "pushSubscriptionId": ids::push_subscription(target.id),
                "verificationCode": target.verification_code,
            });
            let outcome = push.send(&target, &body, "high", VERIFICATION_TTL_SECS, None).await;
            if outcome == Outcome::Gone {
                let _ = push.store.push_gone(target.id).await;
            }
        });
    }

    /// Encrypts, signs and POSTs one message.
    async fn send(&self, target: &PushTarget, body: &Value, urgency: &str, ttl: u32, topic: Option<&str>) -> Outcome {
        let plain = body.to_string().into_bytes();
        let mut headers = vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("ttl".to_owned(), ttl.to_string()),
            ("urgency".to_owned(), urgency.to_owned()),
        ];
        if let Some(topic) = topic {
            headers.push(("topic".to_owned(), topic.to_owned()));
        }
        let body = match &target.keys {
            Some(keys) => {
                let (Some(p256dh), Some(auth)) = (decode(&keys.p256dh), decode(&keys.auth)) else {
                    tracing::warn!(subscription = target.id, "a push subscription has keys that do not decode");
                    return Outcome::Gone;
                };
                match ece::encrypt(&plain, &p256dh, &auth) {
                    Ok(sealed) => {
                        headers.push(("content-encoding".to_owned(), "aes128gcm".to_owned()));
                        sealed
                    }
                    Err(err) => {
                        tracing::warn!(subscription = target.id, %err, "a push could not be encrypted");
                        return if err == ece::EceError::BadKeys { Outcome::Gone } else { Outcome::Failed };
                    }
                }
            }
            None => plain,
        };
        let Some(vapid) = self.vapid().await else {
            return Outcome::Failed;
        };
        match vapid.authorization(&target.url, &self.subject, crate::methods::unix_now()) {
            Some(authorization) => headers.push(("authorization".to_owned(), authorization)),
            None => return Outcome::Gone,
        }
        let message = PushMessage { url: target.url.clone(), headers, body };
        match self.transport.post(message).await {
            Ok(status) if (200..300).contains(&status) => Outcome::Delivered,
            Ok(404 | 410) => {
                tracing::info!(subscription = target.id, host = %target.url_shown, "the push service no longer knows a subscription");
                Outcome::Gone
            }
            Ok(status) => {
                tracing::warn!(subscription = target.id, host = %target.url_shown, status, "a push was refused");
                Outcome::Failed
            }
            Err(err) => {
                tracing::warn!(subscription = target.id, host = %target.url_shown, %err, "a push did not go through");
                Outcome::Failed
            }
        }
    }

    /// Sends a StateChange and notes how it went.
    async fn deliver(&self, target: &PushTarget, changed: &Map<String, Value>) {
        let urgent = changed.values().any(|types| types.get("EmailDelivery").is_some());
        let body = json!({ "@type": "StateChange", "changed": changed });
        let outcome =
            self.send(target, &body, if urgent { "high" } else { "normal" }, STATE_TTL_SECS, Some(TOPIC)).await;
        let result = match outcome {
            Outcome::Delivered => self.store.push_delivered(target.id).await,
            Outcome::Gone => self.store.push_gone(target.id).await,
            Outcome::Failed => self.store.push_failed(target.id).await.map(|dropped| {
                if dropped {
                    tracing::info!(subscription = target.id, "a push subscription failed too often and was dropped");
                }
            }),
        };
        if let Err(err) = result {
            tracing::warn!(%err, "noting how a push went failed");
        }
    }
}

fn decode(value: &str) -> Option<Vec<u8>> {
    let normalized: String = value
        .trim()
        .trim_end_matches('=')
        .chars()
        .map(|c| {
            if c == '+' {
                '-'
            } else if c == '/' {
                '_'
            } else {
                c
            }
        })
        .collect();
    B64.decode(normalized).ok()
}

/// Whether a subscription's keys, as the client sent them, can be encrypted for (RFC 8291): an
/// uncompressed point on P-256 and a 16-byte auth secret.
pub(crate) fn check_keys(p256dh: &str, auth: &str) -> bool {
    match (decode(p256dh), decode(auth)) {
        (Some(p256dh), Some(auth)) => ece::keys_look_right(&p256dh, &auth) && ece::encrypt(b"", &p256dh, &auth).is_ok(),
        _ => false,
    }
}

/// A change waiting to be pushed: the account it happened in, from which state on.
#[derive(Default)]
struct Pending {
    /// Per account: the state before the first change, and the newest.
    accounts: HashMap<i64, (i64, i64)>,
    since: Option<Instant>,
}

/// A StateChange held back for a subscription pushed to a moment ago.
struct Held {
    account_id: i64,
    changed: Map<String, Value>,
    due: Instant,
}

impl Jmap {
    /// Runs until `shutdown`: pushes changes to the verified push subscriptions and drops the ones
    /// that ended.
    pub async fn run_web_push(self, mut shutdown: watch::Receiver<bool>) {
        let push = self.inner.push.clone();
        let PushTiming { debounce, min_interval } = push.timing;
        let store = self.inner.store.clone();
        let mut changes = store.subscribe_changes();
        let mut pending = Pending::default();
        let mut held: HashMap<i64, Held> = HashMap::new();
        let mut last_sent: HashMap<i64, Instant> = HashMap::new();
        let mut next_purge = Instant::now();
        loop {
            let mut wake = next_purge;
            if let Some(since) = pending.since {
                wake = wake.min(since + debounce);
            }
            if let Some(due) = held.values().map(|h| h.due).min() {
                wake = wake.min(due);
            }
            tokio::select! {
                received = changes.recv() => match received {
                    Ok(change) => {
                        let entry = pending.accounts.entry(change.account_id).or_insert((change.modseq - 1, change.modseq));
                        entry.0 = entry.0.min(change.modseq - 1);
                        entry.1 = entry.1.max(change.modseq);
                        // The wait starts with the first change, not with the loop turn.
                        pending.since.get_or_insert_with(Instant::now);
                    }
                    // Some changes went by unseen; the next ones are pushed as usual, and whoever
                    // missed something finds it with the next sync.
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::debug!(missed, "web push missed some changes");
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                _ = tokio::time::sleep_until(wake.into()) => {}
                _ = shutdown.changed() => return,
            }
            let now = Instant::now();
            if now >= next_purge {
                next_purge = now + PURGE_INTERVAL;
                match store.purge_push_subscriptions().await {
                    Ok(0) => {}
                    Ok(purged) => tracing::info!(purged, "push subscriptions that ended were dropped"),
                    Err(err) => tracing::warn!(%err, "dropping ended push subscriptions failed"),
                }
                last_sent.retain(|_, at| at.elapsed() < min_interval);
            }
            if pending.since.is_some_and(|since| now >= since + debounce) {
                let accounts = std::mem::take(&mut pending).accounts;
                push.flush(&store, accounts, &mut held, &mut last_sent).await;
            }
            if held.values().any(|h| h.due <= now) {
                push.release(&store, &mut held, &mut last_sent).await;
            }
        }
    }
}

impl WebPush {
    /// Pushes what changed in `accounts` to everyone who follows them.
    async fn flush(
        &self,
        store: &Store,
        accounts: HashMap<i64, (i64, i64)>,
        held: &mut HashMap<i64, Held>,
        last_sent: &mut HashMap<i64, Instant>,
    ) {
        if !store.has_push_subscriptions().await.unwrap_or(false) {
            return;
        }
        let known = all_types();
        // Per account that follows: its StateChange, by the account id it names.
        let mut by_follower: HashMap<i64, Map<String, Value>> = HashMap::new();
        for (account_id, (since, modseq)) in accounts {
            let Ok(kinds) = store.changed_kinds(account_id, since).await else { continue };
            if kinds.is_empty() {
                continue;
            }
            // EmailDelivery only for new mail (RFC 8621, 1.5), and only to those who may read the
            // folder it came into: a browser has to show something for every push, and a message
            // marked as read, a draft or a copy in Sent is nothing to show.
            let delivered = if kinds.iter().any(|kind| kind == "Email") {
                store.push_deliveries(account_id, since).await.unwrap_or_default()
            } else {
                Vec::new()
            };
            let Ok(audience) = store.push_audience(account_id).await else { continue };
            for follower in audience {
                let wanted = |kind: &str| {
                    (kind != "EmailDelivery" || delivered.contains(&follower)) && known.iter().any(|k| k == kind)
                };
                let changed = if follower == account_id {
                    type_states(store, account_id, &kinds, modseq, false, wanted).await
                } else {
                    shared_type_states(store, account_id, follower, since, wanted).await
                };
                if !changed.is_empty() {
                    by_follower.entry(follower).or_default().insert(ids::account(account_id), Value::Object(changed));
                }
            }
        }
        if by_follower.is_empty() {
            return;
        }
        let targets = match store.push_targets(by_follower.keys().copied().collect()).await {
            Ok(targets) => targets,
            Err(err) => {
                tracing::warn!(%err, "reading the push subscriptions failed");
                return;
            }
        };
        let now = Instant::now();
        let mut sends = Vec::new();
        for target in targets {
            let Some(changed) = by_follower.get(&target.account_id).map(|changed| wanted_by(&target, changed)) else {
                continue;
            };
            if changed.is_empty() {
                continue;
            }
            match last_sent.get(&target.id) {
                Some(at) if now < *at + self.timing.min_interval => {
                    let due = *at + self.timing.min_interval;
                    let entry = held.entry(target.id).or_insert_with(|| Held {
                        account_id: target.account_id,
                        changed: Map::new(),
                        due,
                    });
                    merge(&mut entry.changed, changed);
                }
                _ => {
                    // Anything held for it goes along now.
                    let mut changed = changed;
                    if let Some(earlier) = held.remove(&target.id) {
                        let mut all = earlier.changed;
                        merge(&mut all, changed);
                        changed = all;
                    }
                    last_sent.insert(target.id, now);
                    sends.push((target, changed));
                }
            }
        }
        self.send_all(sends);
    }

    /// Pushes what was held back and is due now, to the subscriptions that are still there.
    async fn release(&self, store: &Store, held: &mut HashMap<i64, Held>, last_sent: &mut HashMap<i64, Instant>) {
        let now = Instant::now();
        let due: Vec<i64> = held.iter().filter(|(_, h)| h.due <= now).map(|(id, _)| *id).collect();
        let accounts: HashSet<i64> = due.iter().filter_map(|id| held.get(id)).map(|h| h.account_id).collect();
        let targets = store.push_targets(accounts.into_iter().collect()).await.unwrap_or_default();
        let mut sends = Vec::new();
        for id in due {
            let Some(entry) = held.remove(&id) else { continue };
            if let Some(target) = targets.iter().find(|target| target.id == id) {
                last_sent.insert(id, now);
                sends.push((target.clone(), entry.changed));
            }
        }
        self.send_all(sends);
    }

    /// Sends to several push services at once, each on its own, without waiting for them: a slow
    /// push service holds up nobody else's pushes.
    fn send_all(&self, sends: Vec<(PushTarget, Map<String, Value>)>) {
        for (target, changed) in sends {
            let push = self.clone();
            tokio::spawn(async move { push.deliver(&target, &changed).await });
        }
    }
}

/// The part of a StateChange a subscription asked for with its `types`.
fn wanted_by(target: &PushTarget, changed: &Map<String, Value>) -> Map<String, Value> {
    let Some(types) = &target.types else {
        return changed.clone();
    };
    let mut out = Map::new();
    for (account, states) in changed {
        let Some(states) = states.as_object() else { continue };
        let kept: Map<String, Value> = states
            .iter()
            .filter(|(kind, _)| types.iter().any(|t| t == *kind))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !kept.is_empty() {
            out.insert(account.clone(), Value::Object(kept));
        }
    }
    out
}

/// Adds `newer` to `into`: per account, the newer state of each type wins.
fn merge(into: &mut Map<String, Value>, newer: Map<String, Value>) {
    for (account, states) in newer {
        match (into.get_mut(&account), states) {
            (Some(Value::Object(existing)), Value::Object(states)) => existing.extend(states),
            (_, states) => {
                into.insert(account, states);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(types: Option<Vec<&str>>) -> PushTarget {
        PushTarget {
            id: 1,
            account_id: 1,
            url: "https://push.example.net/a".into(),
            url_shown: "push.example.net".into(),
            keys: None,
            types: types.map(|types| types.into_iter().map(str::to_owned).collect()),
            verification_code: String::new(),
        }
    }

    #[test]
    fn subscriptions_get_the_types_they_asked_for() {
        let Value::Object(changed) = json!({ "a1": { "Email": "5", "Mailbox": "5" }, "a2": { "Mailbox": "9" } }) else {
            unreachable!()
        };
        assert_eq!(wanted_by(&target(None), &changed), changed);
        let only_mail = wanted_by(&target(Some(vec!["Email"])), &changed);
        assert_eq!(Value::Object(only_mail), json!({ "a1": { "Email": "5" } }));
        assert!(wanted_by(&target(Some(vec!["CalendarEvent"])), &changed).is_empty());
    }

    #[test]
    fn held_changes_are_merged() {
        let Value::Object(mut into) = json!({ "a1": { "Email": "5", "Mailbox": "5" } }) else { unreachable!() };
        let Value::Object(newer) = json!({ "a1": { "Email": "7" }, "a2": { "Thread": "3" } }) else { unreachable!() };
        merge(&mut into, newer);
        assert_eq!(Value::Object(into), json!({ "a1": { "Email": "7", "Mailbox": "5" }, "a2": { "Thread": "3" } }));
    }

    #[test]
    fn keys_are_read_with_or_without_padding() {
        assert!(check_keys(
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4=",
            "BTBZMqHH6r4Tts7J_aSIgg=="
        ));
        assert!(check_keys(
            "BCVxsr7N/eNgVRqvHtD0zTZsEc6+VV+JvLexhqUzORcxaOzi6+AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
            "BTBZMqHH6r4Tts7J_aSIgg"
        ));
        assert!(!check_keys("BCVx", "BTBZMqHH6r4Tts7J_aSIgg"));
        // The right length, but not a point on the curve.
        assert!(!check_keys(&B64.encode([4u8; 65]), "BTBZMqHH6r4Tts7J_aSIgg"));
        assert!(!check_keys("not base64!", "BTBZMqHH6r4Tts7J_aSIgg"));
    }
}
