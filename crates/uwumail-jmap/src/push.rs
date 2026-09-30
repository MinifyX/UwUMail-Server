//! Push over EventSource (RFC 8620, section 7.3), and what the WebSocket push shares with it.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Extension;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{self, Stream};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;
use uwumail_store::{CalendarAlertFired, LiveLogin, StateChange, Store};

use crate::api::RequestError;
use crate::auth::{ClientInfo, Login};
use crate::{Jmap, ids};

/// Event streams and WebSockets one account may have open at once, together. Each watches every
/// change on the server; the webmail has one per tab, an app one per device.
pub const MAX_PUSH_CONNECTIONS: usize = 32;

/// The push connections open per account.
#[derive(Default)]
pub(crate) struct Connections(Arc<Mutex<HashMap<i64, usize>>>);

/// A place among an account's [`MAX_PUSH_CONNECTIONS`], given back when the connection ends.
pub(crate) struct Slot {
    open: Arc<Mutex<HashMap<i64, usize>>>,
    account_id: i64,
}

impl Connections {
    /// A place for one more push connection of the account, or `None` when it has all it may.
    pub fn open(&self, account_id: i64) -> Option<Slot> {
        let mut open = self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = open.entry(account_id).or_default();
        if *count >= MAX_PUSH_CONNECTIONS {
            return None;
        }
        *count += 1;
        Some(Slot { open: self.0.clone(), account_id })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut open = self.open.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = open.get_mut(&self.account_id) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.account_id);
            }
        }
    }
}

/// The answer to one push connection too many (RFC 8620, section 3.6.1, "limit").
pub(crate) fn too_many_connections() -> Response {
    RequestError {
        status: StatusCode::TOO_MANY_REQUESTS,
        body: json!({
            "type": "urn:ietf:params:jmap:error:limit",
            "limit": "maxPushConnections",
            "status": 429,
            "detail": format!("An account may have at most {MAX_PUSH_CONNECTIONS} event streams and WebSockets open."),
        }),
    }
    .into_response()
}

const TYPES: &[&str] = &[
    "Mailbox",
    "Email",
    "Thread",
    "Identity",
    "EmailSubmission",
    "VacationResponse",
    "UserSettings",
    "Calendar",
    "CalendarEvent",
    "CalendarEventNotification",
    "ParticipantIdentity",
    "AddressBook",
    "ContactCard",
    "SieveScript",
    "MaskedEmail",
    "ProfilePicture",
    "AssistLabel",
];

#[derive(Deserialize)]
pub struct PushQuery {
    types: Option<String>,
    closeafter: Option<String>,
    ping: Option<u64>,
}

/// Follows one account's changes for push, over EventSource and WebSocket alike.
pub(crate) struct Watcher {
    store: Store,
    account_id: i64,
    /// The data types asked for, `EmailDelivery` included.
    pub types: Vec<String>,
    changes: broadcast::Receiver<StateChange>,
    /// Everything up to here was reported.
    pub last_modseq: i64,
    /// The last change pushed per account shared with this one.
    shared_modseqs: HashMap<i64, i64>,
    /// Changes still to hand out after missing some: those of the accounts sharing with this one.
    pending: Vec<StateChange>,
    /// Calendar alerts that go off (draft-ietf-jmap-calendars, section 6.4).
    alerts: broadcast::Receiver<CalendarAlertFired>,
    /// For an app allowed masked addresses only: nothing but `MaskedEmail` of the own account is
    /// pushed, whatever types it asks for.
    masked_only: bool,
    /// Whether the login may reach calendars and address books; without, their types and calendar
    /// alerts are not pushed, as their methods do not answer it.
    may_use_dav: bool,
    /// The accounts that share mail with this one.
    owners: Owners,
}

/// The accounts that share mail with the watched one, as of a [`Store::sharing_generation`]. Every
/// change on the server comes by every watcher; asking the database each time who shares with
/// whom made a read per change and watcher.
#[derive(Default)]
struct Owners {
    list: Vec<i64>,
    /// `None` until read, or to read again.
    generation: Option<u64>,
    /// How often they were read, for the tests.
    reads: usize,
}

impl Owners {
    async fn current(&mut self, store: &Store, account_id: i64) -> &[i64] {
        // The generation before the read: a change of sharing committed during it moves it on,
        // and the next change reads again.
        let generation = store.sharing_generation();
        if self.generation != Some(generation) {
            self.reads += 1;
            if let Ok(list) = store.sharing_owners(account_id).await {
                self.list = list;
                self.generation = Some(generation);
            }
        }
        &self.list
    }
}

/// What a watcher has to push.
pub(crate) enum Pushed {
    State(StateChange),
    /// A CalendarAlert of this account, when `CalendarAlert` is among the types asked for.
    Alert(CalendarAlertFired),
}

/// All push types, for a client that asks for everything: the data types, `EmailDelivery` and the
/// `CalendarAlert` pseudo-type.
pub(crate) fn all_types() -> Vec<String> {
    TYPES.iter().map(|t| t.to_string()).chain(["EmailDelivery".to_owned(), "CalendarAlert".to_owned()]).collect()
}

impl Watcher {
    pub async fn new(store: Store, account_id: i64, types: Vec<String>) -> Watcher {
        let changes = store.subscribe_changes();
        let alerts = store.subscribe_calendar_alerts();
        let last_modseq = store.account_modseq(account_id).await.unwrap_or(0);
        // Where each shared account stands now, so what comes later is measured from here.
        let mut owners = Owners::default();
        let mut shared_modseqs = HashMap::new();
        for owner in owners.current(&store, account_id).await.to_vec() {
            if let Ok(modseq) = store.account_modseq(owner).await {
                shared_modseqs.insert(owner, modseq);
            }
        }
        Watcher {
            store,
            account_id,
            types,
            changes,
            last_modseq,
            shared_modseqs,
            pending: Vec::new(),
            alerts,
            masked_only: false,
            may_use_dav: true,
            owners,
        }
    }

    /// Keeps this watcher to what the login may see: `MaskedEmail` of the own account for an app
    /// allowed nothing else, no calendars and contacts without the `dav` scope.
    pub fn allowed_to(mut self, login: &Login) -> Watcher {
        self.masked_only = login.masked_only();
        self.may_use_dav = login.may_use_dav();
        self
    }

    /// Whether a type that changed is pushed: asked for, and allowed to the login.
    fn wants(&self, kind: &str) -> bool {
        let dav = kind == "CalendarAlert" || crate::methods::DAV_TYPES.contains(&kind);
        (!self.masked_only || kind == "MaskedEmail")
            && (self.may_use_dav || !dav)
            && self.types.iter().any(|t| t == kind)
    }

    /// Waits for the next change of this account or of an account that shares mail with it, or a
    /// calendar alert of this account. Safe to cancel: nothing is lost when it is. `None` when
    /// the server shuts down.
    pub async fn wait(&mut self) -> Option<Pushed> {
        let wants_alerts = self.wants("CalendarAlert");
        loop {
            tokio::select! {
                change = next_change(&self.store, self.account_id, &mut self.changes, &mut self.pending, &mut self.owners, self.last_modseq) => {
                    return change.map(Pushed::State);
                }
                alert = self.alerts.recv(), if wants_alerts => match alert {
                    Ok(alert) if alert.account_id == self.account_id => return Some(Pushed::Alert(alert)),
                    // Someone else's, or some went by unseen: alerts are not sent again.
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
            }
        }
    }

    /// The `changed` map of a `StateChange` for everything since the last one, in the types asked
    /// for; `None` when none of them changed.
    pub async fn changed(&mut self, modseq: i64) -> Option<Map<String, Value>> {
        let kinds = self.store.changed_kinds(self.account_id, self.last_modseq).await.unwrap_or_default();
        self.last_modseq = self.last_modseq.max(modseq);
        let changed = type_states(&self.store, self.account_id, &kinds, modseq, false, |kind| self.wants(kind)).await;
        (!changed.is_empty()).then_some(changed)
    }

    /// The account and `changed` map of a `StateChange` for a change [`Watcher::wait`] returned:
    /// this account's own, or a change of someone sharing mail with it, pushed as a change of
    /// their shared account (docs/sharing.md). `None` when nothing asked for changed.
    pub async fn changed_by(&mut self, change: &StateChange) -> Option<(i64, Map<String, Value>)> {
        if change.account_id == self.account_id {
            return self.changed(change.modseq).await.map(|changed| (self.account_id, changed));
        }
        if self.masked_only {
            return None;
        }
        let owner = change.account_id;
        let since = self.shared_modseqs.get(&owner).copied().unwrap_or(change.modseq - 1);
        self.shared_modseqs.insert(owner, since.max(change.modseq));
        let changed = shared_type_states(&self.store, owner, self.account_id, since, |kind| self.wants(kind)).await;
        (!changed.is_empty()).then_some((owner, changed))
    }
}

/// The next change of `account_id` or of an account that shares mail with it; see
/// [`Watcher::wait`].
async fn next_change(
    store: &Store,
    account_id: i64,
    changes: &mut broadcast::Receiver<StateChange>,
    pending: &mut Vec<StateChange>,
    owners: &mut Owners,
    last_modseq: i64,
) -> Option<StateChange> {
    if let Some(change) = pending.pop() {
        return Some(change);
    }
    loop {
        match changes.recv().await {
            Ok(change) if change.account_id == account_id => return Some(change),
            Ok(change) => {
                if owners.current(store, account_id).await.contains(&change.account_id) {
                    return Some(change);
                }
            }
            // Missed some changes: report everything as changed, in the shared accounts too.
            Err(broadcast::error::RecvError::Lagged(_)) => {
                owners.generation = None;
                for owner in owners.current(store, account_id).await.to_vec() {
                    if let Ok(modseq) = store.account_modseq(owner).await {
                        pending.push(StateChange { account_id: owner, modseq });
                    }
                }
                let modseq = store.account_modseq(account_id).await.unwrap_or(last_modseq);
                return Some(StateChange { account_id, modseq });
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

/// The TypeState of a change after `since` in `owner`'s account for `follower`, who has folders of
/// it shared: only what changed in those folders, with the state the follower sees there. Empty
/// when none of it was theirs to know (security-audit-0.16.0 PROTOCOLS-L2).
pub(crate) async fn shared_type_states(
    store: &Store,
    owner: i64,
    follower: i64,
    since: i64,
    wanted: impl Fn(&str) -> bool,
) -> Map<String, Value> {
    let Ok(visible) = crate::sharing::visible_mailboxes(store, follower, owner).await else {
        return Map::new();
    };
    match store.shared_changed_kinds(owner, since, visible).await {
        Ok((kinds, state)) => type_states(store, owner, &kinds, state, true, wanted).await,
        Err(_) => Map::new(),
    }
}

/// What a shared account has.
const SHARED_TYPES: &[&str] = &["Mailbox", "Email", "Thread"];

/// The TypeState of a `StateChange` (RFC 8620, 7.1) for `kinds` that changed in an account, as of
/// `modseq`: every kind in the account's own view, only mail when it is someone else's account
/// shared with the viewer (`shared`). `EmailDelivery` comes along with `Email`. Only the types
/// `wanted` says yes to are in it.
pub(crate) async fn type_states(
    store: &Store,
    account_id: i64,
    kinds: &[String],
    modseq: i64,
    shared: bool,
    wanted: impl Fn(&str) -> bool,
) -> Map<String, Value> {
    let mut changed = Map::new();
    for kind in kinds {
        if (shared && !SHARED_TYPES.contains(&kind.as_str())) || !wanted(kind) {
            continue;
        }
        // UserSettings and ProfilePicture have a state of their own (they do not move with mail),
        // so the client can tell whether it already has it.
        let state = if kind == "UserSettings" {
            store.user_settings_state(account_id).await.unwrap_or_else(|_| modseq.to_string())
        } else if kind == "ProfilePicture" {
            store.profile_settings(account_id).await.map_or_else(|_| modseq.to_string(), |s| s.state.to_string())
        } else if kind == "AssistLabel" {
            store.assist_label_state(account_id).await.unwrap_or_else(|_| modseq.to_string())
        } else {
            modseq.to_string()
        };
        changed.insert(kind.clone(), json!(state));
    }
    // The labels' counts move with the mail (docs/jmap-assist.md, "State and push").
    if !shared
        && kinds.iter().any(|k| k == "Email")
        && !changed.contains_key("AssistLabel")
        && wanted("AssistLabel")
        && store.has_assist_labels(account_id).await.unwrap_or(false)
        && let Ok(state) = store.assist_label_state(account_id).await
    {
        changed.insert("AssistLabel".into(), json!(state));
    }
    if kinds.iter().any(|k| k == "Email") && wanted("EmailDelivery") {
        changed.insert("EmailDelivery".into(), json!(modseq.to_string()));
    }
    changed
}

struct Listener {
    jmap: Jmap,
    /// Its place among the account's push connections, until the stream ends.
    _slot: Slot,
    /// The login the stream was opened with: it ends with it.
    login: LiveLogin,
    watcher: Watcher,
    close_after_state: bool,
    ping: Option<Duration>,
    done: bool,
}

async fn next_event(mut listener: Listener) -> Option<(Result<Event, Infallible>, Listener)> {
    if listener.done {
        return None;
    }
    loop {
        let received = match listener.ping {
            Some(interval) => match tokio::time::timeout(interval, listener.watcher.wait()).await {
                Ok(received) => received,
                Err(_) => {
                    listener.jmap.inner.auth.still_valid(&listener.login).await?;
                    let event =
                        Event::default().event("ping").data(json!({ "interval": interval.as_secs() }).to_string());
                    return Some((Ok(event), listener));
                }
            },
            None => listener.watcher.wait().await,
        };
        let change = match received? {
            Pushed::State(change) => change,
            Pushed::Alert(alert) => {
                listener.jmap.inner.auth.still_valid(&listener.login).await?;
                let data = crate::calendar_alerts::alert_json(&alert);
                return Some((Ok(Event::default().event("calendarAlert").data(data.to_string())), listener));
            }
        };
        let Some((account_id, changed)) = listener.watcher.changed_by(&change).await else {
            continue;
        };
        // A login that ended hears nothing more: the stream closes, and opening it again fails.
        listener.jmap.inner.auth.still_valid(&listener.login).await?;
        let data = json!({
            "@type": "StateChange",
            "changed": { ids::account(account_id): Value::Object(changed) }
        });
        listener.done = listener.close_after_state;
        return Some((Ok(Event::default().event("state").data(data.to_string())), listener));
    }
}

fn events(listener: Listener) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold(listener, next_event)
}

pub async fn handle(
    State(jmap): State<Jmap>,
    Query(query): Query<PushQuery>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let login = match jmap.inner.auth.login_or_masked_for(&headers, client, false).await {
        Ok(login) => login,
        Err(err) => return err.into_response(),
    };
    let account = &login.account;
    let Some(slot) = jmap.inner.push_connections.open(account.id) else {
        return too_many_connections();
    };
    let types: Vec<String> = match query.types.as_deref() {
        None | Some("*") | Some("") => all_types(),
        Some(list) => list.split(',').map(|t| t.trim().to_owned()).collect(),
    };
    let ping = query.ping.filter(|p| *p > 0).map(|p| Duration::from_secs(p.clamp(30, 3600)));
    let watcher = Watcher::new(jmap.inner.store.clone(), account.id, types).await.allowed_to(&login);
    let listener = Listener {
        jmap: jmap.clone(),
        _slot: slot,
        login: login.live(),
        watcher,
        close_after_state: query.closeafter.as_deref() == Some("state"),
        ping,
        done: false,
    };
    Sse::new(events(listener)).keep_alive(KeepAlive::new().interval(Duration::from_secs(300))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role};

    async fn people(store: &Store, names: &[&str]) -> Vec<i64> {
        store.create_domain("example.org").await.unwrap();
        let mut ids = Vec::new();
        for name in names {
            let account = store
                .create_account(NewAccount {
                    address: format!("{name}@example.org"),
                    display_name: name.to_string(),
                    password: Some("katzenpfote-123".into()),
                    role: Role::User,
                    quota_bytes: 0,
                    protocols: None,
                })
                .await
                .unwrap();
            ids.push(account.id);
        }
        ids
    }

    async fn deliver(store: &Store, account_id: i64) {
        let raw = b"From: a@example.net\r\nSubject: hallo\r\n\r\nhallo\r\n".to_vec();
        let mailboxes = vec![MailboxTarget::Role(MailboxRole::Inbox)];
        store.ingest(IngestRequest { account_id, raw, mailboxes, keywords: vec![], received_at: None }).await.unwrap();
    }

    async fn next_state(watcher: &mut Watcher) -> StateChange {
        match tokio::time::timeout(Duration::from_secs(10), watcher.wait()).await.unwrap() {
            Some(Pushed::State(change)) => change,
            _ => panic!("no state change"),
        }
    }

    /// Every change on the server comes by every watcher; who shares with the watched account is
    /// read once, and again only after sharing changed.
    #[tokio::test(flavor = "multi_thread")]
    async fn changes_of_others_do_not_ask_who_shares() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let ids = people(&store, &["mini", "nyu", "kiki"]).await;
        let (mini, nyu, kiki) = (ids[0], ids[1], ids[2]);
        let mut watcher = Watcher::new(store.clone(), mini, all_types()).await;
        assert_eq!(watcher.owners.reads, 1);

        deliver(&store, nyu).await;
        deliver(&store, nyu).await;
        deliver(&store, mini).await;
        assert_eq!(next_state(&mut watcher).await.account_id, mini);
        assert_eq!(watcher.owners.reads, 1, "someone else's mail made the watcher ask who shares");

        // Kiki shares her inbox with Mini: read again, and Kiki's changes come through from then on.
        let inbox = store.mailboxes(kiki).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox));
        store.set_mailbox_acl_for(kiki, inbox.unwrap().id, mini, "lr").await.unwrap();
        assert_eq!(next_state(&mut watcher).await.account_id, kiki);
        assert_eq!(watcher.owners.reads, 2);
        deliver(&store, kiki).await;
        assert_eq!(next_state(&mut watcher).await.account_id, kiki);
        assert_eq!(watcher.owners.reads, 2);
    }

    /// A login without the `dav` scope hears nothing of calendars and address books, alerts
    /// included, as their methods do not answer it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_login_without_dav_hears_no_calendars() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        let mini = people(&store, &["mini"]).await[0];
        let mut watcher = Watcher::new(store, mini, all_types()).await;
        let dav = ["Calendar", "CalendarEvent", "CalendarAlert", "AddressBook", "ContactCard", "ParticipantIdentity"];
        assert!(dav.iter().all(|kind| watcher.wants(kind)));
        watcher.may_use_dav = false;
        assert!(!dav.iter().any(|kind| watcher.wants(kind)));
        assert!(["Email", "Mailbox", "EmailDelivery", "MaskedEmail"].iter().all(|kind| watcher.wants(kind)));
    }
}
