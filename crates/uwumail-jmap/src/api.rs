//! `POST /jmap/api`: request parsing, result references and method dispatch (RFC 8620, section 3).

use std::collections::HashMap;

use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};

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

fn request_error(status: StatusCode, kind: &str, detail: &str) -> Response {
    let body =
        json!({ "type": format!("urn:ietf:params:jmap:error:{kind}"), "status": status.as_u16(), "detail": detail });
    (status, [(header::CONTENT_TYPE, "application/problem+json")], body.to_string()).into_response()
}

pub async fn handle(
    State(jmap): State<Jmap>,
    client: Option<Extension<ClientInfo>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    let account = match jmap.inner.auth.account_for(&headers, client, true).await {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return request_error(StatusCode::BAD_REQUEST, "notJSON", "The request is not valid JSON."),
    };
    let request: Request = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(err) => return request_error(StatusCode::BAD_REQUEST, "notRequest", &err.to_string()),
    };
    if let Some(unknown) = request.using.iter().find(|c| !methods::KNOWN_CAPABILITIES.contains(&c.as_str())) {
        return request_error(StatusCode::BAD_REQUEST, "unknownCapability", &format!("Unknown capability {unknown}."));
    }
    if request.method_calls.len() > MAX_CALLS_IN_REQUEST {
        let body = json!({
            "type": "urn:ietf:params:jmap:error:limit",
            "limit": "maxCallsInRequest",
            "status": 400,
            "detail": "Too many method calls in one request."
        });
        return (StatusCode::BAD_REQUEST, Json(body)).into_response();
    }

    let echo_created_ids = request.created_ids.is_some();
    let mut ctx = Ctx::new(&jmap.inner, account, request.using, request.created_ids.unwrap_or_default());
    let mut responses: Vec<(String, Value, String)> = Vec::with_capacity(request.method_calls.len());

    for (name, arguments, call_id) in request.method_calls {
        let outcome = match resolve_references(arguments, &responses) {
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
    response.insert("sessionState".into(), json!(session::session_state(&ctx.account)));
    ([(header::CACHE_CONTROL, "no-cache, no-store")], Json(Value::Object(response))).into_response()
}

/// Replaces `#name` arguments with the value their result reference points at.
fn resolve_references(arguments: Value, responses: &[(String, Value, String)]) -> Result<Value, MethodError> {
    let Value::Object(map) = arguments else {
        return Err(MethodError::invalid_arguments("arguments must be an object"));
    };
    if !map.keys().any(|key| key.starts_with('#')) {
        return Ok(Value::Object(map));
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
        resolved.insert(name.to_owned(), value);
    }
    Ok(Value::Object(resolved))
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
        let resolved = resolve_references(args, &responses).unwrap();
        assert_eq!(resolved["ids"], json!(["e1"]));
        let wrong = json!({ "#ids": { "resultOf": "0", "name": "Mailbox/get", "path": "/ids" } });
        assert_eq!(resolve_references(wrong, &responses).unwrap_err().kind, "invalidResultReference");
    }
}
