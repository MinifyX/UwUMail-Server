//! **Experimental:** signing in with a ChatGPT subscription, the way OpenAI's Codex CLI does it
//! (`codex login --device-auth`, codex-rs/login): a device code the person confirms at OpenAI, then
//! OAuth tokens that are refreshed shortly before they run out. Requests then go to the Codex
//! backend (the Responses API behind ChatGPT) with the ChatGPT account named in a header.
//!
//! This is not an API OpenAI offers to other programs. It may change or stop working at any time,
//! and using a subscription this way may not be what OpenAI's terms allow; see docs/llm.md.

use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uwumail_smtp::egress::AssistClient;

use crate::llm::shorten;

/// The Codex CLI's OAuth client.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const ISSUER: &str = "https://auth.openai.com";
pub const BACKEND: &str = "https://chatgpt.com/backend-api/codex";
/// A device code is good for this long.
pub const LOGIN_SECS: i64 = 15 * 60;
/// Tokens are refreshed this long before they run out.
const REFRESH_EARLY_SECS: i64 = 5 * 60;
const TIMEOUT: Duration = Duration::from_secs(20);
const MAX_BODY: usize = 64 * 1024;

/// Where the login and the backend are; other addresses only in tests.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub issuer: String,
    pub backend: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Endpoints { issuer: ISSUER.into(), backend: BACKEND.into() }
    }
}

/// A device login that was started and not yet confirmed.
#[derive(Debug, Clone)]
pub struct DeviceCode {
    pub device_auth_id: String,
    pub user_code: String,
    pub interval: u64,
    pub expires_at: i64,
}

/// The tokens of a ChatGPT login, stored sealed as the provider's secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub account_id: Option<String>,
    /// Unix seconds.
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    Pending,
    Connected(Tokens),
    Failed(String),
}

async fn post(client: &AssistClient, url: &str, content_type: &str, body: String) -> Result<(u16, Value), String> {
    let request = Request::post(url)
        .header(CONTENT_TYPE, content_type)
        .header(ACCEPT, "application/json")
        .header(USER_AGENT, "UwUMail")
        .body(Full::new(Bytes::from(body)))
        .map_err(|_| "not a usable address".to_owned())?;
    let work = async {
        let response = client.send(request).await.map_err(|err| err.to_string())?;
        let status = response.status().as_u16();
        let body = http_body_util::Limited::new(response.into_body(), MAX_BODY)
            .collect()
            .await
            .map_err(|_| "the answer was too large or broke off".to_owned())?
            .to_bytes();
        Ok((status, serde_json::from_slice(&body).unwrap_or(Value::Null)))
    };
    tokio::time::timeout(TIMEOUT, work).await.map_err(|_| "OpenAI did not answer in time".to_owned())?
}

fn message(value: &Value) -> String {
    value
        .pointer("/error/message")
        .or_else(|| value.get("error_description"))
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .map(|text| shorten(text, 200))
        .unwrap_or_else(|| "OpenAI refused the login".into())
}

/// Asks for a device code.
pub async fn start(client: &AssistClient, endpoints: &Endpoints, now: i64) -> Result<DeviceCode, String> {
    let url = format!("{}/api/accounts/deviceauth/usercode", endpoints.issuer);
    let (status, value) = post(client, &url, "application/json", json!({ "client_id": CLIENT_ID }).to_string()).await?;
    if status != 200 {
        return Err(message(&value));
    }
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    let (Some(device_auth_id), Some(user_code)) = (text("device_auth_id"), text("user_code").or(text("usercode")))
    else {
        return Err("OpenAI's answer had no device code".into());
    };
    // Sometimes a number, sometimes a string of one.
    let interval = value
        .get("interval")
        .and_then(|interval| interval.as_u64().or_else(|| interval.as_str().and_then(|s| s.trim().parse().ok())))
        .unwrap_or(5)
        .clamp(2, 30);
    Ok(DeviceCode { device_auth_id, user_code, interval, expires_at: now + LOGIN_SECS })
}

/// The page where the person enters the code.
pub fn verification_uri(endpoints: &Endpoints) -> String {
    format!("{}/codex/device", endpoints.issuer)
}

/// Asks whether the person confirmed the code, and trades the code for tokens once they did.
pub async fn poll(client: &AssistClient, endpoints: &Endpoints, code: &DeviceCode, now: i64) -> Poll {
    let url = format!("{}/api/accounts/deviceauth/token", endpoints.issuer);
    let body = json!({ "device_auth_id": code.device_auth_id, "user_code": code.user_code }).to_string();
    let (status, value) = match post(client, &url, "application/json", body).await {
        Ok(answer) => answer,
        Err(err) => return Poll::Failed(err),
    };
    // Not confirmed yet.
    if status == 403 || status == 404 {
        return Poll::Pending;
    }
    if status != 200 {
        return Poll::Failed(message(&value));
    }
    let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    let (code_value, verifier) = (text("authorization_code"), text("code_verifier"));
    if code_value.is_empty() || verifier.is_empty() {
        return Poll::Failed("OpenAI's answer had no authorization code".into());
    }
    let form = format!(
        "grant_type=authorization_code&client_id={}&code={}&redirect_uri={}&code_verifier={}",
        form_encode(CLIENT_ID),
        form_encode(&code_value),
        form_encode(&format!("{}/deviceauth/callback", endpoints.issuer)),
        form_encode(&verifier),
    );
    let url = format!("{}/oauth/token", endpoints.issuer);
    match post(client, &url, "application/x-www-form-urlencoded", form).await {
        Ok((200, value)) => match tokens(&value, None, now) {
            Some(tokens) => Poll::Connected(tokens),
            None => Poll::Failed("OpenAI's answer had no tokens".into()),
        },
        Ok((_, value)) => Poll::Failed(message(&value)),
        Err(err) => Poll::Failed(err),
    }
}

/// Whether the tokens are about to run out.
pub fn needs_refresh(tokens: &Tokens, now: i64) -> bool {
    tokens.expires_at - REFRESH_EARLY_SECS <= now
}

/// New tokens for old ones.
pub async fn refresh(client: &AssistClient, endpoints: &Endpoints, old: &Tokens, now: i64) -> Result<Tokens, String> {
    let url = format!("{}/oauth/token", endpoints.issuer);
    let body = json!({ "grant_type": "refresh_token", "client_id": CLIENT_ID, "refresh_token": old.refresh_token });
    match post(client, &url, "application/json", body.to_string()).await? {
        (200, value) => tokens(&value, Some(old), now).ok_or_else(|| "OpenAI's answer had no tokens".into()),
        (_, value) => Err(message(&value)),
    }
}

fn tokens(value: &Value, old: Option<&Tokens>, now: i64) -> Option<Tokens> {
    let access_token = value.get("access_token").and_then(Value::as_str)?.to_owned();
    let refresh_token = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| old.map(|old| old.refresh_token.clone()))?;
    let id_token = value.get("id_token").and_then(Value::as_str);
    let account_id = id_token
        .and_then(account_of)
        .or_else(|| account_of(&access_token))
        .or_else(|| old.and_then(|old| old.account_id.clone()));
    // Like the Codex CLI: the access token's own `exp`, else `expires_in`, else eight days.
    let expires_at = claims(&access_token)
        .and_then(|claims| claims.get("exp").and_then(Value::as_i64))
        .or_else(|| value.get("expires_in").and_then(Value::as_i64).map(|secs| now + secs))
        .unwrap_or(now + 8 * 86_400);
    Some(Tokens { access_token, refresh_token, account_id, expires_at })
}

/// The claims of a JWT, unchecked: it came straight from OpenAI over TLS, and is only read for the
/// account it names and when it runs out.
fn claims(jwt: &str) -> Option<Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn account_of(jwt: &str) -> Option<String> {
    claims(jwt)?
        .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn form_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: Value) -> String {
        let encode = |value: &Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string());
        format!("{}.{}.sig", encode(&json!({"alg": "none"})), encode(&claims))
    }

    #[test]
    fn tokens_name_the_account_and_when_they_run_out() {
        let id = jwt(json!({ "https://api.openai.com/auth": { "chatgpt_account_id": "acc-1" } }));
        let access = jwt(json!({ "exp": 2_000_000_000 }));
        let got = tokens(&json!({ "access_token": access, "refresh_token": "r1", "id_token": id }), None, 100).unwrap();
        assert_eq!(got.account_id.as_deref(), Some("acc-1"));
        assert_eq!(got.expires_at, 2_000_000_000);
        // A refresh without a new refresh token keeps the old one, and the account.
        let again = tokens(&json!({ "access_token": "opaque", "expires_in": 3600 }), Some(&got), 100).unwrap();
        assert_eq!(again.refresh_token, "r1");
        assert_eq!(again.account_id.as_deref(), Some("acc-1"));
        assert_eq!(again.expires_at, 3700);
        assert!(needs_refresh(&again, 3700 - 60) && !needs_refresh(&again, 100));
        assert_eq!(form_encode("a b/ä"), "a%20b%2F%C3%A4");
    }
}
