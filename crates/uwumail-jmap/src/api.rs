//! `POST /jmap/api`: request parsing, result references and method dispatch (RFC 8620, section 3).

use std::collections::HashMap;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uwumail_store::Account;

use crate::auth::ClientInfo;
use crate::error::MethodError;
use crate::methods::{self, Ctx};
use crate::session::{self, CORE};
use crate::{Jmap, MAX_CALLS_IN_REQUEST};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    using: Vec<String>,
    method_calls: Vec<(String, Value, String)>,
    #[serde(default)]
    created_ids: Option<HashMap<String, String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResultReference {
    result_of: String,
    name: String,
    path: String,
}

/// A request-level error (RFC 8620, section 3.6.1): the whole request failed, no method ran.
pub struct RequestError {
    pub status: StatusCode,
    /// The problem details, `type` included.
    pub body: Value,
}

impl RequestError {
    pub fn new(status: StatusCode, kind: &str, detail: &str) -> RequestError {
        RequestError {
            status,
            body: json!({
                "type": format!("urn:ietf:params:jmap:error:{kind}"),
                "status": status.as_u16(),
                "detail": detail
            }),
        }
    }
}

impl IntoResponse for RequestError {
    fn into_response(self) -> Response {
        (self.status, [(header::CONTENT_TYPE, "application/problem+json")], self.body.to_string()).into_response()
    }
}

pub async fn handle(
    State(jmap): State<Jmap>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let login = match jmap.inner.auth.login_for(&headers, client, true).await {
        Ok(login) => login,
        Err(err) => return err.into_response(),
    };
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return RequestError::new(StatusCode::BAD_REQUEST, "notJSON", "The request is not valid JSON.")
                .into_response();
        }
    };
    match process(&jmap, login.account, Some(login.credential), value).await {
        Ok(response) => ([(header::CACHE_CONTROL, "no-cache, no-store")], Json(response)).into_response(),
        Err(err) => err.into_response(),
    }
}

/// Runs the method calls of one request object and returns the response object. Shared by
/// `POST /jmap/api` and the WebSocket (RFC 8887). `credential` is what the login used, for push
/// subscriptions; without it they cannot be used.
pub async fn process(
    jmap: &Jmap,
    account: Account,
    credential: Option<String>,
    value: Value,
) -> Result<Value, RequestError> {
    let request: Request = serde_json::from_value(value)
        .map_err(|err| RequestError::new(StatusCode::BAD_REQUEST, "notRequest", &err.to_string()))?;
    if let Some(unknown) = request.using.iter().find(|c| !methods::KNOWN_CAPABILITIES.contains(&c.as_str())) {
        return Err(RequestError::new(
            StatusCode::BAD_REQUEST,
            "unknownCapability",
            &format!("Unknown capability {unknown}."),
        ));
    }
    if request.method_calls.len() > MAX_CALLS_IN_REQUEST {
        return Err(RequestError {
            status: StatusCode::BAD_REQUEST,
            body: json!({
                "type": "urn:ietf:params:jmap:error:limit",
                "limit": "maxCallsInRequest",
                "status": 400,
                "detail": "Too many method calls in one request."
            }),
        });
    }

    let echo_created_ids = request.created_ids.is_some();
    let mut ctx = Ctx::new(&jmap.inner, account, request.using, request.created_ids.unwrap_or_default());
    ctx.credential = credential;
    let mut responses: Vec<(String, Value, String)> = Vec::with_capacity(request.method_calls.len());
    let mut reference_budget = MAX_REFERENCED_BYTES;

    for (name, arguments, call_id) in request.method_calls {
        let outcome = match resolve_references(arguments, &responses, &mut reference_budget) {
            Ok(arguments) => methods::dispatch(&mut ctx, &name, arguments).await,
            Err(err) => Err(err),
        };
        match outcome {
            Ok(outputs) => {
                for (method, output) in outputs {
                    responses.push((method, output, call_id.clone()));
                }
            }
            Err(err) => responses.push(("error".into(), err.to_json(), call_id)),
        }
    }

    let mut response = Map::new();
    response.insert("methodResponses".into(), json!(responses));
    if echo_created_ids {
        response.insert("createdIds".into(), json!(ctx.created_ids));
    }
    // The shared accounts are part of the session, so their changes change its state too.
    let shared = crate::sharing::shared_accounts(&jmap.inner.store, ctx.account.id).await;
    let state = format!("{}{}", session::session_state(&ctx.account), crate::sharing::state_suffix(&shared));
    response.insert("sessionState".into(), json!(state));
    Ok(Value::Object(response))
}

/// What result references may copy in one request, all together, counted as JSON. A reference
/// copies what it points at, and Core/echo hands the copy back as a response of its own; two
/// references to the previous echo doubled the answer with every call, so a small request grew
/// without end (security-audit-0.16.0 PANIC-2). Real references name ids, a few kilobytes.
const MAX_REFERENCED_BYTES: usize = crate::MAX_REQUEST_BYTES;
/// `#` arguments one method call may have; methods take one or two.
const MAX_REFERENCES_PER_CALL: usize = 16;

/// Replaces `#name` arguments with the value their result reference points at, taking what the
/// values copy off `budget`.
fn resolve_references(
    arguments: Value,
    responses: &[(String, Value, String)],
    budget: &mut usize,
) -> Result<Value, MethodError> {
    let Value::Object(map) = arguments else {
        return Err(MethodError::invalid_arguments("arguments must be an object"));
    };
    let references = map.keys().filter(|key| key.starts_with('#')).count();
    if references == 0 {
        return Ok(Value::Object(map));
    }
    if references > MAX_REFERENCES_PER_CALL {
        return Err(MethodError::invalid_arguments(format!(
            "a method call may have at most {MAX_REFERENCES_PER_CALL} result references"
        )));
    }
    let mut resolved = Map::with_capacity(map.len());
    for (key, value) in &map {
        let Some(name) = key.strip_prefix('#') else {
            resolved.insert(key.clone(), value.clone());
            continue;
        };
        if map.contains_key(name) {
            return Err(MethodError::invalid_arguments(format!("both {name} and #{name} are given")));
        }
        let reference: ResultReference = serde_json::from_value(value.clone())
            .map_err(|_| MethodError::new("invalidResultReference", format!("#{name} is not a result reference")))?;
        let (response_name, response, _) =
            responses.iter().find(|(_, _, call_id)| *call_id == reference.result_of).ok_or_else(|| {
                MethodError::new("invalidResultReference", format!("no result for {}", reference.result_of))
            })?;
        if *response_name != reference.name {
            return Err(MethodError::new(
                "invalidResultReference",
                format!("{} answered {response_name}, not {}", reference.result_of, reference.name),
            ));
        }
        let value = evaluate_pointer(response, &reference.path)
            .ok_or_else(|| MethodError::new("invalidResultReference", format!("nothing at {}", reference.path)))?;
        *budget = budget.checked_sub(json_size(&value, *budget)).ok_or_else(|| {
            MethodError::new(
                "requestTooLarge",
                format!("the result references of this request copy more than {MAX_REFERENCED_BYTES} bytes"),
            )
        })?;
        resolved.insert(name.to_owned(), value);
    }
    Ok(Value::Object(resolved))
}

/// About how many bytes `value` takes as JSON; the count stops soon after it passes `limit`.
fn json_size(value: &Value, limit: usize) -> usize {
    let mut size = 0usize;
    let mut work = vec![value];
    while let Some(value) = work.pop() {
        size += match value {
            Value::Null | Value::Bool(_) => 5,
            Value::Number(_) => 20,
            Value::String(text) => text.len() + 2,
            Value::Array(items) => {
                work.extend(items);
                items.len() + 2
            }
            Value::Object(map) => {
                size += map.keys().map(|key| key.len() + 4).sum::<usize>();
                work.extend(map.values());
                map.len() + 2
            }
        };
        if size > limit {
            break;
        }
    }
    size
}

/// JSON Pointer with JMAP's `*` extension for arrays.
fn evaluate_pointer(value: &Value, path: &str) -> Option<Value> {
    if path.is_empty() || path == "/" {
        return Some(value.clone());
    }
    let tokens: Vec<String> =
        path.strip_prefix('/')?.split('/').map(|t| t.replace("~1", "/").replace("~0", "~")).collect();
    evaluate_tokens(value, &tokens)
}

fn evaluate_tokens(value: &Value, tokens: &[String]) -> Option<Value> {
    let Some((token, rest)) = tokens.split_first() else {
        return Some(value.clone());
    };
    match value {
        Value::Array(items) if token == "*" => {
            let mut out = Vec::new();
            for item in items {
                match evaluate_tokens(item, rest)? {
                    Value::Array(inner) => out.extend(inner),
                    other => out.push(other),
                }
            }
            Some(Value::Array(out))
        }
        Value::Array(items) => evaluate_tokens(items.get(token.parse::<usize>().ok()?)?, rest),
        Value::Object(map) => evaluate_tokens(map.get(token)?, rest),
        _ => None,
    }
}

pub fn requires(capability: &str, using: &[String]) -> bool {
    capability == CORE || using.iter().any(|c| c == capability)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointers_with_wildcards() {
        let value = json!({ "list": [{ "id": "t1", "emailIds": ["e1", "e2"] }, { "id": "t2", "emailIds": ["e3"] }] });
        assert_eq!(evaluate_pointer(&value, "/list/*/emailIds"), Some(json!(["e1", "e2", "e3"])));
        assert_eq!(evaluate_pointer(&value, "/list/1/id"), Some(json!("t2")));
        assert_eq!(evaluate_pointer(&value, "/nope"), None);
    }

    #[test]
    fn references_are_resolved() {
        let responses = vec![("Email/query".to_string(), json!({ "ids": ["e1"] }), "0".to_string())];
        let args = json!({ "#ids": { "resultOf": "0", "name": "Email/query", "path": "/ids" }, "properties": ["id"] });
        let mut budget = MAX_REFERENCED_BYTES;
        let resolved = resolve_references(args, &responses, &mut budget).unwrap();
        assert_eq!(resolved["ids"], json!(["e1"]));
        assert!(budget < MAX_REFERENCED_BYTES);
        let wrong = json!({ "#ids": { "resultOf": "0", "name": "Mailbox/get", "path": "/ids" } });
        assert_eq!(resolve_references(wrong, &responses, &mut budget).unwrap_err().kind, "invalidResultReference");
    }

    /// Core/echo with two references to the echo before it doubles with every call: a small
    /// request grew without end (security-audit-0.16.0 PANIC-2). The copies are counted now.
    #[test]
    fn references_that_double_run_out() {
        let mut responses = vec![("Core/echo".to_string(), json!({ "a": "x".repeat(1024 * 1024) }), "0".to_string())];
        let mut budget = MAX_REFERENCED_BYTES;
        let mut refused = None;
        for step in 1..8 {
            let previous = (step - 1).to_string();
            let args = json!({
                "#a": { "resultOf": previous, "name": "Core/echo", "path": "/" },
                "#b": { "resultOf": previous, "name": "Core/echo", "path": "/" },
            });
            match resolve_references(args, &responses, &mut budget) {
                Ok(echoed) => responses.push(("Core/echo".into(), echoed, step.to_string())),
                Err(err) => {
                    refused = Some((step, err.kind));
                    break;
                }
            }
        }
        let (step, kind) = refused.expect("the doubling is stopped");
        assert_eq!(kind, "requestTooLarge");
        assert!(step <= 4, "stopped at call {step}");

        let many: Map<String, Value> = (0..=MAX_REFERENCES_PER_CALL)
            .map(|n| (format!("#r{n}"), json!({ "resultOf": "0", "name": "Core/echo", "path": "/a" })))
            .collect();
        let err = resolve_references(Value::Object(many), &responses, &mut MAX_REFERENCED_BYTES.clone()).unwrap_err();
        assert_eq!(err.kind, "invalidArguments");
    }
}
