//! "My account": what every logged-in person can see and change about themselves.

use axum::Json;
use axum::extract::State;
use serde_json::{Map, Value, json};

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
    ("motion", &["system", "on", "off"]),
    ("mailConversations", &["on", "off"]),
    ("mailDensity", &["relaxed", "compact"]),
    ("mailRemoteImages", &["ask", "always"]),
    ("mailAppearance", &["auto", "light", "dark"]),
    ("mailSenderPictures", &["on", "off"]),
    ("mailSwipeRight", &["read", "archive", "trash", "flag", "spam", "none"]),
    ("mailSwipeLeft", &["read", "archive", "trash", "flag", "spam", "none"]),
];

pub async fn update_preferences(
    State(web): State<Web>,
    session: Session,
    Json(changes): Json<Map<String, Value>>,
) -> ApiResult<Json<Value>> {
    for (key, value) in &changes {
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
