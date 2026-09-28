//! The JMAP session resource (RFC 8620, section 2).

use axum::Extension;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Json, Response};
use serde_json::{Value, json};
use uwumail_store::Account;

use crate::auth::ClientInfo;
use crate::{
    Jmap, MAX_CALLS_IN_REQUEST, MAX_DELAYED_SEND_SECS, MAX_OBJECTS_IN_GET, MAX_OBJECTS_IN_SET, MAX_REQUEST_BYTES,
    MAX_UPLOAD_BYTES, ids, jscal,
};

pub const CORE: &str = "urn:ietf:params:jmap:core";
pub const MAIL: &str = "urn:ietf:params:jmap:mail";
pub const SUBMISSION: &str = "urn:ietf:params:jmap:submission";
pub const VACATION: &str = "urn:ietf:params:jmap:vacationresponse";
/// Our own extension: one's allowed and blocked senders on this server (docs/jmap-senders.md).
pub const SENDERS: &str = "urn:uwumail:jmap:senders";
/// Our own extension: the settings the webmail and the apps keep in sync (docs/jmap-settings.md).
pub const SETTINGS: &str = "urn:uwumail:jmap:settings";
/// Sieve scripts, the mail rules delivery runs (RFC 9661, docs/sieve.md).
pub const SIEVE: &str = "urn:ietf:params:jmap:sieve";
/// Our own extension: what the webmail needs on top of plain JMAP, currently the cleaned HTML
/// body of a message (`uwuSafeHtml`, `uwuHasRemoteContent`).
pub const WEBMAIL: &str = "urn:uwumail:jmap:webmail";
/// Our own extension: a message's remote pictures, fetched by the server so the sender never sees
/// who reads it (docs/jmap-remote.md).
pub const REMOTE: &str = "urn:uwumail:jmap:remote";
/// JMAP Calendars (draft-ietf-jmap-calendars) on the CalDAV calendars; see docs/jmap-calendars.md.
pub const CALENDARS: &str = "urn:ietf:params:jmap:calendars";
/// When people are busy, `Principal/getAvailability` (draft-ietf-jmap-calendars, section 2.2).
pub const AVAILABILITY: &str = "urn:ietf:params:jmap:principals:availability";
/// Turning iCalendar files into events, `CalendarEvent/parse` (draft-ietf-jmap-calendars, 5.13).
pub const CALENDARS_PARSE: &str = "urn:ietf:params:jmap:calendars:parse";
/// Our own extension: addresses to suggest while writing, from the address books and recent mail
/// (docs/jmap-suggest.md).
pub const SUGGEST: &str = "urn:uwumail:jmap:suggest";
/// Requests and push over a WebSocket (RFC 8887).
pub const WEBSOCKET: &str = "urn:ietf:params:jmap:websocket";
/// JMAP Contacts (RFC 9610) on the CardDAV address books; see docs/jmap-contacts.md.
pub const CONTACTS: &str = "urn:ietf:params:jmap:contacts";
/// Fastmail's masked email extension: random addresses per website (docs/jmap-masked-email.md).
pub const MASKED: &str = "https://www.fastmail.com/dev/maskedemail";
/// The server's VAPID key for Web Push subscriptions (RFC 9749); see docs/jmap-push.md.
pub const WEBPUSH_VAPID: &str = "urn:ietf:params:jmap:webpush-vapid";

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

/// The WebSocket endpoint for the origin the client used: `wss://` for `https://`.
pub fn websocket_url(base: &str) -> String {
    let origin = match base.strip_prefix("https://") {
        Some(host) => format!("wss://{host}"),
        None => format!("ws://{}", base.strip_prefix("http://").unwrap_or(base)),
    };
    format!("{origin}/jmap/ws")
}

pub fn session_state(account: &Account) -> String {
    // Changes whenever something in the session document would change.
    let calendars = if account.protocols.caldav { "-c" } else { "" };
    let contacts = if account.protocols.carddav { "-k" } else { "" };
    format!("{}-{}{calendars}{contacts}", account.id, account.login.len() + account.display_name.len())
}

/// What the account's MaskedEmail capability says (a UwUMail addition to Fastmail's extension):
/// the domains it may make masked addresses on and the one a new one goes to without `domain`.
pub async fn masked_capability(store: &uwumail_store::Store, account_id: i64) -> Value {
    let policy = match store.effective_masked_policy(account_id).await {
        Ok(policy) => policy,
        Err(err) => {
            tracing::warn!(%err, account_id, "reading the masked address policy failed");
            Default::default()
        }
    };
    json!({ "domains": policy.domains, "defaultDomain": policy.default_domain })
}

/// The part of the session state that changes with the masked address policies, so clients fetch
/// the capability again. One counter for all of them: a change anywhere makes every client look.
pub async fn masked_state(store: &uwumail_store::Store) -> String {
    format!("-x{}", store.masked_policy_version().await.unwrap_or(0))
}

/// The session document. `may_use_dav` is whether the login's credential may reach calendars and
/// address books (see [`crate::auth::Login::scopes`]); without it they are left out.
pub fn document(account: &Account, base: &str, may_use_dav: bool) -> Value {
    let account_id = ids::account(account.id);
    let mut document = json!({
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
            WEBSOCKET: { "url": websocket_url(base), "supportsPush": true },
            VACATION: {},
            SENDERS: {},
            SETTINGS: {},
            SUGGEST: {},
            SIEVE: { "implementation": "UwUMail Server" },
            MASKED: {},
            WEBMAIL: {},
            REMOTE: {
                "imageUrl": format!("{base}/jmap/image/{{accountId}}?url={{url}}"),
                "pictureUrl": format!("{base}/jmap/picture/{{accountId}}?email={{email}}"),
                "maxSizeImage": crate::remote::MAX_IMAGE_BYTES
            }
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
                    SUBMISSION: {
                        "maxDelayedSend": MAX_DELAYED_SEND_SECS,
                        // RFC 4865: the longest hold in seconds, and the latest date it may reach.
                        "submissionExtensions": {
                            "FUTURERELEASE": [
                                MAX_DELAYED_SEND_SECS.to_string(),
                                crate::dates::format(crate::methods::unix_now() + MAX_DELAYED_SEND_SECS)
                            ]
                        }
                    },
                    VACATION: {},
                    MASKED: {},
                    SENDERS: { "maxEntries": uwumail_store::SENDER_LIST_PERSONAL_LIMIT },
                    SUGGEST: { "maxLimit": crate::methods::MAX_SUGGESTIONS },
                    SETTINGS: {
                        "maxKeys": uwumail_store::USER_SETTINGS_MAX_KEYS,
                        "maxSize": uwumail_store::USER_SETTINGS_MAX_SIZE,
                        "maxValueSize": uwumail_store::USER_SETTINGS_MAX_VALUE_SIZE
                    },
                    SIEVE: {
                        "maxSizeScriptName": uwumail_store::SIEVE_MAX_NAME_SIZE,
                        "maxSizeScript": uwumail_store::SIEVE_MAX_SCRIPT_SIZE,
                        "maxNumberScripts": uwumail_store::SIEVE_MAX_SCRIPTS,
                        "maxNumberRedirects": uwumail_smtp::sieve::MAX_REDIRECTS,
                        "sieveExtensions": uwumail_smtp::sieve::EXTENSIONS,
                        "notificationMethods": null,
                        "externalLists": null
                    }
                }
            }
        },
        "primaryAccounts": {
            MAIL: account_id.clone(),
            SUBMISSION: account_id.clone(),
            VACATION: account_id.clone(),
            SENDERS: account_id.clone(),
            SETTINGS: account_id.clone(),
            SUGGEST: account_id.clone(),
            SIEVE: account_id.clone(),
            MASKED: account_id.clone()
        },
        "username": account.login,
        "apiUrl": format!("{base}/jmap/api"),
        "downloadUrl": format!("{base}/jmap/download/{{accountId}}/{{blobId}}/{{name}}?accept={{type}}"),
        "uploadUrl": format!("{base}/jmap/upload/{{accountId}}/"),
        "eventSourceUrl": format!("{base}/jmap/eventsource/?types={{types}}&closeafter={{closeafter}}&ping={{ping}}"),
        "state": session_state(account)
    });
    // Calendars are there when the account may use them, as over CalDAV.
    if account.protocols.caldav && may_use_dav {
        document["capabilities"][CALENDARS] = json!({});
        document["accounts"][&account_id]["accountCapabilities"][CALENDARS] = json!({
            "maxCalendarsPerEvent": 1,
            "minDateTime": jscal::MIN_DATE_TIME,
            "maxDateTime": jscal::MAX_DATE_TIME,
            "maxExpandedQueryDuration": jscal::MAX_EXPANDED_DURATION,
            "maxParticipantsPerEvent": null,
            "mayCreateCalendar": true
        });
        document["primaryAccounts"][CALENDARS] = json!(account_id);
        document["capabilities"][AVAILABILITY] = json!({});
        document["accounts"][&account_id]["accountCapabilities"][AVAILABILITY] =
            json!({ "maxAvailabilityDuration": crate::availability::MAX_DURATION });
        document["primaryAccounts"][AVAILABILITY] = json!(account_id);
        document["capabilities"][CALENDARS_PARSE] = json!({});
        document["accounts"][&account_id]["accountCapabilities"][CALENDARS_PARSE] = json!({});
        document["primaryAccounts"][CALENDARS_PARSE] = json!(account_id);
    }
    // Address books too, as over CardDAV.
    if account.protocols.carddav && may_use_dav {
        document["capabilities"][CONTACTS] = json!({});
        document["accounts"][&account_id]["accountCapabilities"][CONTACTS] = json!({
            "maxAddressBooksPerCard": 1,
            "mayCreateAddressBook": true
        });
        document["primaryAccounts"][CONTACTS] = json!(account_id);
    }
    document
}

pub async fn handle(State(jmap): State<Jmap>, client: Option<Extension<ClientInfo>>, headers: HeaderMap) -> Response {
    let client = client.map(|Extension(c)| c).unwrap_or_default();
    match jmap.inner.auth.login_for(&headers, client, false).await {
        Ok(login) => {
            let base = base_url(&headers, client);
            let may_use_dav = login.may_use_dav();
            let account = login.account;
            let mut document = document(&account, &base, may_use_dav);
            // Folders others share with this account, as accounts of their own (docs/sharing.md).
            let shared = crate::sharing::shared_accounts(&jmap.inner.store, account.id).await;
            crate::sharing::add_to_session(&mut document, &account, &shared);
            document["accounts"][ids::account(account.id)]["accountCapabilities"][MASKED] =
                masked_capability(&jmap.inner.store, account.id).await;
            let masked_state = masked_state(&jmap.inner.store).await;
            // The key a browser binds its push subscription to. It never changes, so the session
            // state need not say anything about it.
            if let Some(vapid) = jmap.inner.push.vapid().await {
                document["capabilities"][WEBPUSH_VAPID] = json!({ "applicationServerKey": vapid.public_key() });
            }
            document["state"] =
                json!(format!("{}{}{masked_state}", session_state(&account), crate::sharing::state_suffix(&shared)));
            ([(header::CACHE_CONTROL, "no-cache, no-store")], Json(document)).into_response()
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
        headers.insert(header::HOST, HeaderValue::from_static("mail.example.org"));
        assert_eq!(base_url(&headers, ClientInfo::default()), "http://mail.example.org");
        assert_eq!(base_url(&headers, ClientInfo { https: true, ..ClientInfo::default() }), "https://mail.example.org");

        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert_eq!(base_url(&headers, ClientInfo::default()), "https://mail.example.org");

        headers.remove("x-forwarded-proto");
        headers.insert(header::FORWARDED, HeaderValue::from_static("for=192.0.2.1;proto=https;host=mail.example.org"));
        assert_eq!(base_url(&headers, ClientInfo::default()), "https://mail.example.org");
    }
}
