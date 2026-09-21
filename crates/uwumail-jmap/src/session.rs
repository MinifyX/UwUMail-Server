//! The JMAP session resource (RFC 8620, section 2).

use axum::Extension;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Json, Response};
use serde_json::{Value, json};
use uwumail_store::Account;

use crate::auth::ClientInfo;
use crate::{
    Jmap, MAX_CALLS_IN_REQUEST, MAX_OBJECTS_IN_GET, MAX_OBJECTS_IN_SET, MAX_REQUEST_BYTES, MAX_UPLOAD_BYTES, ids,
};

pub const CORE: &str = "urn:ietf:params:jmap:core";
pub const MAIL: &str = "urn:ietf:params:jmap:mail";
pub const SUBMISSION: &str = "urn:ietf:params:jmap:submission";
pub const VACATION: &str = "urn:ietf:params:jmap:vacationresponse";
/// Our own extension: one's allowed and blocked senders on this server (docs/jmap-senders.md).
pub const SENDERS: &str = "urn:uwumail:jmap:senders";
/// Our own extension: the settings the webmail and the apps keep in sync (docs/jmap-settings.md).
pub const SETTINGS: &str = "urn:uwumail:jmap:settings";
/// Our own extension: what the webmail needs on top of plain JMAP, currently the cleaned HTML
/// body of a message (`uwuSafeHtml`, `uwuHasRemoteContent`).
pub const WEBMAIL: &str = "urn:uwumail:jmap:webmail";

/// Origin the client used, so every URL in the session works from where it is.
///
/// A reverse proxy's `X-Forwarded-Proto` (or `Forwarded: proto=`) is followed even when the
/// proxy is not configured as trusted: it only decides the scheme of these URLs, and an
/// https client must never be handed http endpoints.
pub fn base_url(headers: &HeaderMap, client: ClientInfo) -> String {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
    let forwarded_proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_ascii_lowercase())
        .or_else(|| {
            let forwarded = headers.get(header::FORWARDED)?.to_str().ok()?;
            forwarded
                .split([';', ','])
                .filter_map(|pair| pair.trim().split_once('='))
                .find(|(key, _)| key.eq_ignore_ascii_case("proto"))
                .map(|(_, value)| value.trim_matches('"').to_ascii_lowercase())
        });
    let https = client.https || forwarded_proto.as_deref() == Some("https");
    let scheme = if https { "https" } else { "http" };
    format!("{scheme}://{host}")
}

pub fn session_state(account: &Account) -> String {
    // Changes whenever something in the session document would change.
    format!("{}-{}", account.id, account.login.len() + account.display_name.len())
}

pub fn document(account: &Account, base: &str) -> Value {
    let account_id = ids::account(account.id);
    json!({
        "capabilities": {
            CORE: {
                "maxSizeUpload": MAX_UPLOAD_BYTES,
                "maxConcurrentUpload": 4,
                "maxSizeRequest": MAX_REQUEST_BYTES,
                "maxConcurrentRequests": 8,
                "maxCallsInRequest": MAX_CALLS_IN_REQUEST,
                "maxObjectsInGet": MAX_OBJECTS_IN_GET,
                "maxObjectsInSet": MAX_OBJECTS_IN_SET,
                "collationAlgorithms": ["i;ascii-casemap", "i;octet", "i;unicode-casemap"]
            },
            MAIL: {},
            SUBMISSION: {},
            VACATION: {},
            SENDERS: {},
            SETTINGS: {},
            WEBMAIL: {}
        },
        "accounts": {
            account_id.clone(): {
                "name": account.login,
                "isPersonal": true,
                "isReadOnly": false,
                "accountCapabilities": {
                    MAIL: {
                        "maxMailboxesPerEmail": null,
                        "maxMailboxDepth": 64,
                        "maxSizeMailboxName": 255,
                        "maxSizeAttachmentsPerEmail": MAX_UPLOAD_BYTES,
                        "emailQuerySortOptions": ["receivedAt", "sentAt", "size", "from", "to", "subject", "hasKeyword", "allInThreadHaveKeyword", "someInThreadHaveKeyword"],
                        "mayCreateTopLevelMailbox": true
                    },
                    SUBMISSION: { "maxDelayedSend": 0, "submissionExtensions": {} },
                    VACATION: {},
                    SENDERS: { "maxEntries": uwumail_store::SENDER_LIST_PERSONAL_LIMIT },
                    SETTINGS: {
                        "maxKeys": uwumail_store::USER_SETTINGS_MAX_KEYS,
                        "maxSize": uwumail_store::USER_SETTINGS_MAX_SIZE,
                        "maxValueSize": uwumail_store::USER_SETTINGS_MAX_VALUE_SIZE
                    }
                }
            }
        },
        "primaryAccounts": {
            MAIL: account_id.clone(),
            SUBMISSION: account_id.clone(),
            VACATION: account_id.clone(),
            SENDERS: account_id.clone(),
            SETTINGS: account_id
        },
        "username": account.login,
        "apiUrl": format!("{base}/jmap/api"),
        "downloadUrl": format!("{base}/jmap/download/{{accountId}}/{{blobId}}/{{name}}?accept={{type}}"),
        "uploadUrl": format!("{base}/jmap/upload/{{accountId}}/"),
        "eventSourceUrl": format!("{base}/jmap/eventsource/?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": session_state(account)
    })
}

pub async fn handle(State(jmap): State<Jmap>, client: Option<Extension<ClientInfo>>, headers: HeaderMap) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    match jmap.inner.auth.account_for(&headers, client, false).await {
        Ok(account) => {
            let base = base_url(&headers, client);
            ([(header::CACHE_CONTROL, "no-cache, no-store")], Json(document(&account, &base))).into_response()
        }
        Err(err) => err.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn follows_the_scheme_the_client_used() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("mail.example.de"));
        assert_eq!(base_url(&headers, ClientInfo::default()), "http://mail.example.de");
        assert_eq!(base_url(&headers, ClientInfo { https: true, ..ClientInfo::default() }), "https://mail.example.de");

        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert_eq!(base_url(&headers, ClientInfo::default()), "https://mail.example.de");

        headers.remove("x-forwarded-proto");
        headers.insert(header::FORWARDED, HeaderValue::from_static("for=192.0.2.1;proto=https;host=mail.example.de"));
        assert_eq!(base_url(&headers, ClientInfo::default()), "https://mail.example.de");
    }
}
