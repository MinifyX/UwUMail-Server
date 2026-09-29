//! `POST /jmap/assist/stream`: `Assist/compose` and `Assist/summarize` as server-sent events, so the
//! text appears while the model writes it (docs/jmap-assist.md, "Streaming").
//!
//! The same login as `/jmap/api`. The body is `{using, method, arguments}`; the answer is
//! `subject`, `delta`, then `done` with the method's whole response, or `error`. A comment every 15
//! seconds keeps proxies from closing a quiet connection; a client that goes away stops the request
//! to the provider.

use std::convert::Infallible;
use std::time::Duration;

use axum::Extension;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use uwumail_assist::StreamEvent;

use crate::Jmap;
use crate::api::RequestError;
use crate::auth::ClientInfo;
use crate::error::MethodError;
use crate::methods::Ctx;
use crate::methods::assist::{self, StreamCall};
use crate::session::ASSIST;

/// A streamed request is small: an instruction and a draft at most.
const MAX_BODY: usize = 256 * 1024;
const PING: Duration = Duration::from_secs(15);

fn event(name: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {name}\ndata: {data}\n\n"))
}

pub async fn handle(
    State(jmap): State<Jmap>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let login = match jmap.inner.auth.login_for(&headers, client, true).await {
        Ok(login) => login,
        Err(err) => return err.into_response(),
    };
    let bad = |detail: &str| RequestError::new(StatusCode::BAD_REQUEST, "notRequest", detail).into_response();
    let Ok(body) = axum::body::to_bytes(body, MAX_BODY).await else {
        return bad("The request is too big.");
    };
    let Ok(request) = serde_json::from_slice::<Value>(&body) else {
        return RequestError::new(StatusCode::BAD_REQUEST, "notJSON", "The request is not valid JSON.").into_response();
    };
    let using: Vec<String> = request
        .get("using")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    if !using.iter().any(|capability| capability == ASSIST) {
        return bad("Add urn:uwumail:jmap:assist to using.");
    }
    let method = request.get("method").and_then(Value::as_str).unwrap_or_default().to_owned();
    if !matches!(method.as_str(), "Assist/compose" | "Assist/summarize") {
        return bad("Only Assist/compose and Assist/summarize are streamed.");
    }
    let arguments = request.get("arguments").cloned().unwrap_or(Value::Null);
    let Some(assist) = jmap.inner.assist.clone() else {
        return RequestError::new(StatusCode::NOT_FOUND, "notFound", "The AI assistant is not set up here.")
            .into_response();
    };
    let account = login.account.clone();
    let checked = {
        let ctx = Ctx::new(&jmap.inner, login.account, using, Default::default());
        assist::stream_call(&ctx, &method, &arguments).map(|call| (call, ctx.account_id()))
    };

    let (out, stream) = mpsc::channel::<Result<Bytes, Infallible>>(64);
    tokio::spawn(async move {
        let (call, account_id) = match checked {
            Ok(checked) => checked,
            Err(err) => {
                let _ = out.send(Ok(event("error", &err.to_json()))).await;
                return;
            }
        };
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(64);
        let work = async {
            let result: Result<Value, MethodError> = match call {
                StreamCall::Compose(args) => assist::compose_with(&assist, &account, account_id, args, Some(&tx)).await,
                StreamCall::Summarize(args) => {
                    assist::summarize_with(&assist, &account, account_id, args, Some(&tx)).await
                }
            };
            drop(tx);
            result
        };
        tokio::pin!(work);
        let mut ping = tokio::time::interval(PING);
        ping.tick().await;
        let send = |bytes: Bytes| out.send(Ok(bytes));
        let result = loop {
            tokio::select! {
                result = &mut work => break result,
                Some(piece) = rx.recv() => {
                    let bytes = match piece {
                        StreamEvent::Subject(subject) => event("subject", &json!({ "subject": subject })),
                        StreamEvent::Delta(text) => event("delta", &json!({ "text": text })),
                    };
                    // The client went away: dropping the work stops the provider's request.
                    if send(bytes).await.is_err() {
                        return;
                    }
                }
                _ = ping.tick() => {
                    if send(Bytes::from_static(b": ping\n\n")).await.is_err() {
                        return;
                    }
                }
            }
        };
        while let Ok(piece) = rx.try_recv() {
            let bytes = match piece {
                StreamEvent::Subject(subject) => event("subject", &json!({ "subject": subject })),
                StreamEvent::Delta(text) => event("delta", &json!({ "text": text })),
            };
            if send(bytes).await.is_err() {
                return;
            }
        }
        let last = match result {
            Ok(response) => event("done", &response),
            Err(err) => event("error", &err.to_json()),
        };
        let _ = send(last).await;
    });

    let stream =
        futures_util::stream::unfold(
            stream,
            |mut stream| async move { stream.recv().await.map(|item| (item, stream)) },
        );
    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache, no-store"));
    // nginx and similar proxies pass the events on as they come.
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}
