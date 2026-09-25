//! JMAP over WebSocket (RFC 8887): requests and responses as text messages, and push of
//! `StateChange` objects once the client sends `WebSocketPushEnable`.
//!
//! Programs authenticate the handshake like any other JMAP request (Basic or Bearer). The webmail,
//! whose browser cannot set headers on a WebSocket, uses the portal's session cookie; then the
//! handshake must come from this server's own origin and carry the CSRF token as `?csrf=`.

use axum::Extension;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::api::{self, RequestError};
use crate::auth::{AuthError, CSRF_HEADER, ClientInfo};
use crate::push::{Watcher, all_types};
use crate::{Jmap, MAX_REQUEST_BYTES, ids};

/// The subprotocol a JMAP client asks for (RFC 8887, section 4.2).
const SUBPROTOCOL: &str = "jmap";

#[derive(Deserialize)]
pub struct WsQuery {
    csrf: Option<String>,
}

/// Whether the handshake's `Origin` is this server itself. Browsers always send one; a page on
/// another site must not be able to open a connection with the person's cookie.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else {
        return false;
    };
    let Some(origin) = headers.get(header::ORIGIN).and_then(|o| o.to_str().ok()) else {
        return false;
    };
    let authority = origin.strip_prefix("https://").or_else(|| origin.strip_prefix("http://"));
    authority.is_some_and(|authority| authority.eq_ignore_ascii_case(host))
}

pub async fn handle(
    State(jmap): State<Jmap>,
    Query(query): Query<WsQuery>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let offered = headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|protocol| protocol.trim().eq_ignore_ascii_case(SUBPROTOCOL));
    if !offered {
        return RequestError::new(StatusCode::BAD_REQUEST, "notRequest", "Ask for the WebSocket subprotocol jmap.")
            .into_response();
    }
    let account = if headers.contains_key(header::AUTHORIZATION) {
        jmap.inner.auth.account_for(&headers, client, true).await
    } else if !same_origin(&headers) {
        Err(AuthError::Missing)
    } else {
        // The cookie login changes things only with the CSRF token, which a browser cannot send
        // as a header here.
        let mut headers = headers.clone();
        if let Some(csrf) = query.csrf.as_deref().and_then(|csrf| HeaderValue::from_str(csrf).ok()) {
            headers.insert(CSRF_HEADER, csrf);
        }
        jmap.inner.auth.account_for(&headers, client, true).await
    };
    let account = match account {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    upgrade
        .protocols([SUBPROTOCOL])
        .max_message_size(MAX_REQUEST_BYTES)
        .on_upgrade(move |socket| serve(jmap, account.id, socket))
}

fn request_error(request_id: Option<&Value>, error: RequestError) -> Value {
    let mut body = match error.body {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    body.insert("@type".into(), json!("RequestError"));
    if let Some(id) = request_id {
        body.insert("requestId".into(), id.clone());
    }
    Value::Object(body)
}

async fn send(socket: &mut WebSocket, value: Value) -> bool {
    socket.send(Message::Text(value.to_string().into())).await.is_ok()
}

async fn serve(jmap: Jmap, account_id: i64, mut socket: WebSocket) {
    let store = jmap.inner.store.clone();
    let mut watcher = Watcher::new(store.clone(), account_id, all_types()).await;
    let mut push = false;
    loop {
        tokio::select! {
            message = socket.recv() => {
                let text = match message {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Binary(_))) => {
                        let error = RequestError::new(StatusCode::BAD_REQUEST, "notJSON", "Send JSON as text messages.");
                        if !send(&mut socket, request_error(None, error)).await {
                            return;
                        }
                        continue;
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    Some(Ok(_)) => continue,
                };
                let Ok(Value::Object(mut object)) = serde_json::from_str::<Value>(&text) else {
                    let error = RequestError::new(StatusCode::BAD_REQUEST, "notJSON", "The message is not a JSON object.");
                    if !send(&mut socket, request_error(None, error)).await {
                        return;
                    }
                    continue;
                };
                let kind = object.remove("@type").and_then(|t| t.as_str().map(str::to_owned)).unwrap_or_default();
                let request_id = object.remove("id");
                let reply = match kind.as_str() {
                    "Request" => {
                        // Every request sees the account as it is now: a login that was switched off
                        // or a protocol that was taken away ends the connection.
                        let account = match store.account_by_id(account_id).await {
                            Ok(Some(account)) if account.can_log_in() => account,
                            _ => return,
                        };
                        match api::process(&jmap, account, Value::Object(object)).await {
                            Ok(Value::Object(mut response)) => {
                                response.insert("@type".into(), json!("Response"));
                                if let Some(id) = &request_id {
                                    response.insert("requestId".into(), id.clone());
                                }
                                Some(Value::Object(response))
                            }
                            Ok(other) => Some(other),
                            Err(error) => Some(request_error(request_id.as_ref(), error)),
                        }
                    }
                    "WebSocketPushEnable" => {
                        watcher.types = match object.get("dataTypes") {
                            Some(Value::Array(types)) => {
                                types.iter().filter_map(Value::as_str).map(str::to_owned).collect()
                            }
                            _ => all_types(),
                        };
                        let current = store.account_modseq(account_id).await.unwrap_or(0);
                        // With a pushState from before, everything since is pushed at once;
                        // otherwise push starts from now.
                        let since = object
                            .get("pushState")
                            .and_then(Value::as_str)
                            .and_then(|state| state.parse::<i64>().ok())
                            .filter(|state| (0..=current).contains(state));
                        push = true;
                        match since {
                            Some(since) if since < current => {
                                watcher.last_modseq = since;
                                watcher.changed(current).await.map(|changed| state_change(account_id, changed, current))
                            }
                            _ => {
                                watcher.last_modseq = current;
                                None
                            }
                        }
                    }
                    "WebSocketPushDisable" => {
                        push = false;
                        None
                    }
                    _ => Some(request_error(
                        request_id.as_ref(),
                        RequestError::new(StatusCode::BAD_REQUEST, "notRequest", "Unknown @type."),
                    )),
                };
                if let Some(reply) = reply
                    && !send(&mut socket, reply).await
                {
                    return;
                }
            }
            change = watcher.wait(), if push => {
                let Some(change) = change else { return };
                if let Some((changed_account, changed)) = watcher.changed_by(&change).await
                    && !send(&mut socket, state_change(changed_account, changed, watcher.last_modseq)).await
                {
                    return;
                }
            }
        }
    }
}

fn state_change(account_id: i64, changed: Map<String, Value>, modseq: i64) -> Value {
    json!({
        "@type": "StateChange",
        "changed": { ids::account(account_id): Value::Object(changed) },
        "pushState": modseq.to_string(),
    })
}
