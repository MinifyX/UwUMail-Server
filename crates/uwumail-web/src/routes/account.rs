//! "My account": what every logged-in person can see and change about themselves.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uwumail_store::IdentityUpdate;

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Session;

pub async fn profile(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    let account = session.account;
    let addresses = web.store().addresses(&account.login).await?;
    Ok(Json(json!({
        "login": account.login,
        "name": account.display_name,
        "role": account.role,
        "addresses": addresses,
        "quotaBytes": account.quota_bytes,
        "usedBytes": account.used_bytes,
        "createdAt": account.created_at,
    })))
}

/// The allowed values of each portal preference. The `mail…` ones are the webmail's; language,
/// tone, theme and some of the webmail's are also synced to the apps as JMAP `UserSettings`
/// (docs/jmap-settings.md), which checks the same values.
const PREFERENCES: &[(&str, &[&str])] = &[
    ("language", &["system", "de", "en", "fr", "nl", "ja", "zh"]),
    ("tone", &["playful", "neutral"]),
    ("mode", &["simple", "pro"]),
    ("theme", &["system", "light", "dark"]),
    ("contrast", &["system", "normal", "high"]),
    ("motion", &["system", "on", "off"]),
    ("mailConversations", &["on", "off"]),
    ("mailDensity", &["relaxed", "compact"]),
    ("mailRemoteImages", &["ask", "always"]),
    ("mailAppearance", &["auto", "light", "dark"]),
    ("mailSenderPictures", &["on", "off"]),
    // Seconds a message waits before it goes, so it can be taken back; the server applies it.
    ("mailUndoSend", &["0", "5", "10", "20", "30"]),
    ("mailSwipeRight", &["read", "archive", "trash", "flag", "spam", "none"]),
    ("mailSwipeLeft", &["read", "archive", "trash", "flag", "spam", "none"]),
    // Verification codes (docs/verification-codes.md): also guess codes in mails without the
    // header, and when a code mail leaves the inbox once it has expired.
    ("mailVerificationHeuristics", &["on", "off"]),
    ("mailVerificationCleanup", &["off", "immediately", "24h", "7d"]),
    // Admins only: the calm view with just the traffic light, or everything (docs/admin-alerts.md).
    ("adminView", &["simple", "full"]),
    // Admins only: which alert mails they get.
    ("adminAlerts", &["all", "problems", "none"]),
];

/// How many features `seenFeatures` may list: the "New" tags someone has already clicked.
const SEEN_FEATURES_MAX: usize = 64;

/// `seenFeatures`: distinct short ids like `codes`, at most [`SEEN_FEATURES_MAX`].
fn valid_seen_features(value: &Value) -> bool {
    let Some(list) = value.as_array() else { return false };
    list.len() <= SEEN_FEATURES_MAX
        && list.iter().enumerate().all(|(index, id)| {
            id.as_str().is_some_and(|id| {
                (1..=32).contains(&id.len())
                    && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            }) && !list[..index].contains(id)
        })
}

pub async fn update_preferences(
    State(web): State<Web>,
    session: Session,
    Json(changes): Json<Map<String, Value>>,
) -> ApiResult<Json<Value>> {
    for (key, value) in &changes {
        if key == "seenFeatures" {
            if !value.is_null() && !valid_seen_features(value) {
                return Err(ApiError::Invalid(format!(
                    "seenFeatures must be a list of at most {SEEN_FEATURES_MAX} distinct feature ids"
                )));
            }
            continue;
        }
        let Some((_, allowed)) = PREFERENCES.iter().find(|(name, _)| name == key) else {
            return Err(ApiError::Invalid(format!("unknown preference {key}")));
        };
        let valid = value.is_null() || value.as_str().is_some_and(|v| allowed.contains(&v));
        if !valid {
            return Err(ApiError::Invalid(format!("{key} must be one of {}", allowed.join(", "))));
        }
    }
    let preferences = web.store().update_preferences(session.account.id, changes).await?;
    Ok(Json(Value::Object(preferences)))
}

/// The sending identities with their signatures: the same ones JMAP's `Identity` has, so a
/// signature set here is the one mail programs and the webmail get.
pub async fn identities(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    let list = web.store().identities(session.account.id).await?;
    let list: Vec<Value> = list
        .iter()
        .map(|identity| {
            json!({
                "id": identity.id,
                "name": identity.name,
                "email": identity.email,
                "textSignature": identity.text_signature,
                "htmlSignature": identity.html_signature,
            })
        })
        .collect();
    Ok(Json(Value::Array(list)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityChange {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    text_signature: Option<String>,
    #[serde(default)]
    html_signature: Option<String>,
}

pub async fn update_identity(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
    Json(change): Json<IdentityChange>,
) -> ApiResult<StatusCode> {
    let update = IdentityUpdate {
        name: change.name,
        text_signature: change.text_signature,
        html_signature: change.html_signature,
        ..IdentityUpdate::default()
    };
    web.store().update_identity(session.account.id, id, update).await?;
    Ok(StatusCode::NO_CONTENT)
}
