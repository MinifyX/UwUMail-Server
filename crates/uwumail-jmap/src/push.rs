//! Push over EventSource (RFC 8620, section 7.3), and what the WebSocket push shares with it.

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;

use axum::Extension;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{self, Stream};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;
use uwumail_store::{LiveLogin, StateChange, Store};

use crate::auth::ClientInfo;
use crate::{Jmap, ids};

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
    "ParticipantIdentity",
    "AddressBook",
    "ContactCard",
    "SieveScript",
    "MaskedEmail",
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
}

/// All push types, for a client that asks for everything.
pub(crate) fn all_types() -> Vec<String> {
    TYPES.iter().map(|t| t.to_string()).chain(["EmailDelivery".to_owned()]).collect()
}

impl Watcher {
    pub async fn new(store: Store, account_id: i64, types: Vec<String>) -> Watcher {
        let changes = store.subscribe_changes();
        let last_modseq = store.account_modseq(account_id).await.unwrap_or(0);
        // Where each shared account stands now, so what comes later is measured from here.
        let mut shared_modseqs = HashMap::new();
        for owner in store.sharing_owners(account_id).await.unwrap_or_default() {
            if let Ok(modseq) = store.account_modseq(owner).await {
                shared_modseqs.insert(owner, modseq);
            }
        }
        Watcher { store, account_id, types, changes, last_modseq, shared_modseqs, pending: Vec::new() }
    }

    /// Waits for the next change of this account or of an account that shares mail with it. Safe
    /// to cancel: nothing is lost when it is. `None` when the server shuts down.
    pub async fn wait(&mut self) -> Option<StateChange> {
        if let Some(change) = self.pending.pop() {
            return Some(change);
        }
        loop {
            match self.changes.recv().await {
                Ok(change) if change.account_id == self.account_id => return Some(change),
                Ok(change) => {
                    let owners = self.store.sharing_owners(self.account_id).await.unwrap_or_default();
                    if owners.contains(&change.account_id) {
                        return Some(change);
                    }
                }
                // Missed some changes: report everything as changed, in the shared accounts too.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    for owner in self.store.sharing_owners(self.account_id).await.unwrap_or_default() {
                        if let Ok(modseq) = self.store.account_modseq(owner).await {
                            self.pending.push(StateChange { account_id: owner, modseq });
                        }
                    }
                    let modseq = self.store.account_modseq(self.account_id).await.unwrap_or(self.last_modseq);
                    return Some(StateChange { account_id: self.account_id, modseq });
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// The `changed` map of a `StateChange` for everything since the last one, in the types asked
    /// for; `None` when none of them changed.
    pub async fn changed(&mut self, modseq: i64) -> Option<Map<String, Value>> {
        let kinds = self.store.changed_kinds(self.account_id, self.last_modseq).await.unwrap_or_default();
        self.last_modseq = self.last_modseq.max(modseq);
        let changed = type_states(&self.store, self.account_id, &kinds, modseq, false, |kind| {
            self.types.iter().any(|t| t == kind)
        })
        .await;
        (!changed.is_empty()).then_some(changed)
    }

    /// The account and `changed` map of a `StateChange` for a change [`Watcher::wait`] returned:
    /// this account's own, or a change of someone sharing mail with it, pushed as a change of
    /// their shared account (docs/sharing.md). `None` when nothing asked for changed.
    pub async fn changed_by(&mut self, change: &StateChange) -> Option<(i64, Map<String, Value>)> {
        if change.account_id == self.account_id {
            return self.changed(change.modseq).await.map(|changed| (self.account_id, changed));
        }
        let owner = change.account_id;
        let since = self.shared_modseqs.get(&owner).copied().unwrap_or(change.modseq - 1);
        self.shared_modseqs.insert(owner, since.max(change.modseq));
        let changed =
            shared_type_states(&self.store, owner, self.account_id, since, |kind| self.types.iter().any(|t| t == kind))
                .await;
        (!changed.is_empty()).then_some((owner, changed))
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
        // UserSettings has a state of its own (it does not move with mail), so the client can
        // tell whether it already has it.
        let state = if kind == "UserSettings" {
            store.user_settings_state(account_id).await.unwrap_or_else(|_| modseq.to_string())
        } else {
            modseq.to_string()
        };
        changed.insert(kind.clone(), json!(state));
    }
    if kinds.iter().any(|k| k == "Email") && wanted("EmailDelivery") {
        changed.insert("EmailDelivery".into(), json!(modseq.to_string()));
    }
    changed
}

struct Listener {
    jmap: Jmap,
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
        let change = received?;
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
    let login = match jmap.inner.auth.login_for(&headers, client, false).await {
        Ok(login) => login,
        Err(err) => return err.into_response(),
    };
    let account = &login.account;
    let types: Vec<String> = match query.types.as_deref() {
        None | Some("*") | Some("") => all_types(),
        Some(list) => list.split(',').map(|t| t.trim().to_owned()).collect(),
    };
    let ping = query.ping.filter(|p| *p > 0).map(|p| Duration::from_secs(p.clamp(30, 3600)));
    let watcher = Watcher::new(jmap.inner.store.clone(), account.id, types).await;
    let listener = Listener {
        jmap: jmap.clone(),
        login: login.live(),
        watcher,
        close_after_state: query.closeafter.as_deref() == Some("state"),
        ping,
        done: false,
    };
    Sse::new(events(listener)).keep_alive(KeepAlive::new().interval(Duration::from_secs(300))).into_response()
}
