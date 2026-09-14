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

/// Origin the client used, so every URL in the session works from where it is.
pub fn base_url(headers: &HeaderMap, client: ClientInfo) -> String {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("localhost");
    let scheme = if client.https { "https" } else { "http" };
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
            VACATION: {}
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
                    VACATION: {}
                }
            }
        },
        "primaryAccounts": {
            MAIL: account_id.clone(),
            SUBMISSION: account_id.clone(),
            VACATION: account_id
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
    match jmap.inner.auth.account(&headers, client).await {
        Ok(account) => {
            let base = base_url(&headers, client);
            ([(header::CACHE_CONTROL, "no-cache, no-store")], Json(document(&account, &base))).into_response()
        }
        Err(err) => err.into_response(),
    }
}
