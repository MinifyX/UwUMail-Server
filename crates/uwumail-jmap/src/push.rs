//! Push over EventSource (RFC 8620, section 7.3).

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
use uwumail_store::{StateChange, Store};

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
];

#[derive(Deserialize)]
pub struct PushQuery {
    types: Option<String>,
    closeafter: Option<String>,
    ping: Option<u64>,
}

struct Listener {
    store: Store,
    account_id: i64,
    types: Vec<String>,
    close_after_state: bool,
    ping: Option<Duration>,
    changes: broadcast::Receiver<StateChange>,
    last_modseq: i64,
    /// The last change pushed per account shared with the listener.
    shared_modseqs: std::collections::HashMap<i64, i64>,
    done: bool,
}

async fn next_event(mut listener: Listener) -> Option<(Result<Event, Infallible>, Listener)> {
    if listener.done {
        return None;
    }
    loop {
        let received = match listener.ping {
            Some(interval) => match tokio::time::timeout(interval, listener.changes.recv()).await {
                Ok(received) => received,
                Err(_) => {
                    let event =
                        Event::default().event("ping").data(json!({ "interval": interval.as_secs() }).to_string());
                    return Some((Ok(event), listener));
                }
            },
            None => listener.changes.recv().await,
        };
        let change = match received {
            Ok(change) if change.account_id == listener.account_id => change,
            Ok(change) => match shared_change(&mut listener, change).await {
                Some(data) => {
                    listener.done = listener.close_after_state;
                    return Some((Ok(Event::default().event("state").data(data.to_string())), listener));
                }
                None => continue,
            },
            // Missed some changes: report everything as changed.
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let modseq = listener.store.account_modseq(listener.account_id).await.unwrap_or(listener.last_modseq);
                StateChange { account_id: listener.account_id, modseq }
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        };
        let kinds = listener.store.changed_kinds(listener.account_id, listener.last_modseq).await.unwrap_or_default();
        listener.last_modseq = listener.last_modseq.max(change.modseq);
        let mut changed = Map::new();
        for kind in kinds.iter().filter(|k| listener.types.iter().any(|t| t == *k)) {
            // UserSettings has a state of its own (it does not move with mail), so the client can
            // tell whether it already has it.
            let state = if kind == "UserSettings" {
                listener
                    .store
                    .user_settings_state(listener.account_id)
                    .await
                    .unwrap_or_else(|_| change.modseq.to_string())
            } else {
                change.modseq.to_string()
            };
            changed.insert(kind.clone(), json!(state));
        }
        if kinds.iter().any(|k| k == "Email") && listener.types.iter().any(|t| t == "EmailDelivery") {
            changed.insert("EmailDelivery".into(), json!(change.modseq.to_string()));
        }
        if changed.is_empty() {
            continue;
        }
        let data = json!({
            "@type": "StateChange",
            "changed": { ids::account(listener.account_id): Value::Object(changed) }
        });
        listener.done = listener.close_after_state;
        return Some((Ok(Event::default().event("state").data(data.to_string())), listener));
    }
}

/// A change in someone else's account: pushed when they share mail with the listener, as a
/// change of their shared account (docs/sharing.md).
async fn shared_change(listener: &mut Listener, change: StateChange) -> Option<Value> {
    let owner = change.account_id;
    if !listener.store.sharing_owners(listener.account_id).await.ok()?.contains(&owner) {
        return None;
    }
    let since = listener.shared_modseqs.get(&owner).copied().unwrap_or(change.modseq - 1);
    listener.shared_modseqs.insert(owner, since.max(change.modseq));
    let kinds = listener.store.changed_kinds(owner, since).await.ok()?;
    let mut changed = Map::new();
    for kind in kinds.iter().filter(|k| SHARED_TYPES.contains(&k.as_str())) {
        if listener.types.iter().any(|t| t == kind) {
            changed.insert(kind.clone(), json!(change.modseq.to_string()));
        }
    }
    if kinds.iter().any(|k| k == "Email") && listener.types.iter().any(|t| t == "EmailDelivery") {
        changed.insert("EmailDelivery".into(), json!(change.modseq.to_string()));
    }
    if changed.is_empty() {
        return None;
    }
    Some(json!({ "@type": "StateChange", "changed": { ids::account(owner): Value::Object(changed) } }))
}

/// What a shared account has.
const SHARED_TYPES: &[&str] = &["Mailbox", "Email", "Thread"];

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
    let account = match jmap.inner.auth.account_for(&headers, client, false).await {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    let store = jmap.inner.store.clone();
    let changes = store.subscribe_changes();
    let last_modseq = store.account_modseq(account.id).await.unwrap_or(0);
    let types: Vec<String> = match query.types.as_deref() {
        None | Some("*") | Some("") => {
            TYPES.iter().map(|t| t.to_string()).chain(["EmailDelivery".to_owned()]).collect()
        }
        Some(list) => list.split(',').map(|t| t.trim().to_owned()).collect(),
    };
    let ping = query.ping.filter(|p| *p > 0).map(|p| Duration::from_secs(p.clamp(30, 3600)));
    let listener = Listener {
        store,
        account_id: account.id,
        types,
        close_after_state: query.closeafter.as_deref() == Some("state"),
        ping,
        changes,
        last_modseq,
        shared_modseqs: Default::default(),
        done: false,
    };
    Sse::new(events(listener)).keep_alive(KeepAlive::new().interval(Duration::from_secs(300))).into_response()
}
