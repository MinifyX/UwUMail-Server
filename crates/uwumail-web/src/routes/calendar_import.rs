//! Taking calendars and contacts over from elsewhere, in "My account → Calendars & contacts"
//! (docs/calendar-import.md): an `.ics` or `.vcf` file, a calendar's iCal address once, a
//! subscription that keeps fetching it, and everything at another CalDAV/CardDAV provider at once.
//!
//! Passwords of other providers are used for the one request that needs them and kept nowhere:
//! moving over is a one-time thing. Addresses of subscribed calendars are kept, sealed, since a
//! secret iCal address is as good as a password; the page only ever sees their host.

use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_dav::client::{self, Remote, RemoteError};
use uwumail_store::{
    CalendarSubscriptionUpdate, DavCollection, DavCollectionUpdate, DavImportMode, DavImportReport, DavKind,
    NewCalendarSubscription, NewDavCollection, NewImportCollection, Split, dav_color, decode_text, split_ics,
    split_vcf,
};

use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::Session;

/// The largest file taken at once. A calendar of many years is a few megabytes.
pub const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;
/// Requests to other servers per person and hour: iCal addresses, subscriptions, refreshes and
/// moving over. Enough for anyone moving, too few for knocking on doors.
const REMOTE_CALLS_PER_HOUR: usize = 30;
/// Moving everything over from another provider, at most.
const REMOTE_IMPORT_LIMIT: Duration = Duration::from_secs(300);
/// Fetching one feed, at most.
const FEED_LIMIT: Duration = Duration::from_secs(120);

fn remote_error(err: RemoteError) -> ApiError {
    ApiError::Rule(err.code(), err.to_string())
}

fn kind_of(value: &str) -> ApiResult<DavKind> {
    match value {
        "calendar" => Ok(DavKind::Calendar),
        "addressbook" => Ok(DavKind::Addressbook),
        _ => Err(ApiError::Invalid("kind is calendar or addressbook".into())),
    }
}

fn check_kind(session: &Session, kind: DavKind) -> ApiResult<()> {
    if super::calendars::kinds(session).contains(&kind) {
        Ok(())
    } else {
        Err(ApiError::Rule("davOff", "calendars or contacts are switched off for this account".into()))
    }
}

fn default_of(web: &Web, kind: DavKind) -> NewDavCollection {
    let (calendar, book) = web.smtp().tone().language.collection_names();
    match kind {
        DavKind::Calendar => NewDavCollection::default_calendar(calendar),
        DavKind::Addressbook => NewDavCollection::default_address_book(book),
    }
}

fn polite(web: &Web, session: &Session) -> ApiResult<()> {
    if web.allow_remote_call(session.account.id, REMOTE_CALLS_PER_HOUR) {
        Ok(())
    } else {
        Err(ApiError::TooManyAttempts)
    }
}

/// One of one's own collections to import into (not a subscribed calendar), or a new one.
async fn target(
    web: &Web,
    session: &Session,
    kind: DavKind,
    existing: Option<i64>,
    new: NewImportCollection,
) -> ApiResult<DavCollection> {
    match existing {
        Some(id) => {
            let found = web.store().dav_access(session.account.id, id).await?;
            let Some((collection, access)) = found.filter(|(c, access)| access.is_owner() && c.kind == kind) else {
                return Err(ApiError::NotFound(format!("collection {id}")));
            };
            if !collection.entries_writable(access) {
                return Err(ApiError::Rule("readOnly", "a subscribed calendar is filled by its feed only".into()));
            }
            Ok(collection)
        }
        None => {
            Ok(web.store().dav_create_import_collection(session.account.id, kind, new, default_of(web, kind)).await?)
        }
    }
}

fn split_of(kind: DavKind, text: &str, strip_alarms: bool) -> ApiResult<Split> {
    let split = match kind {
        DavKind::Calendar => split_ics(text, strip_alarms),
        DavKind::Addressbook => split_vcf(text),
    };
    if !split.recognized {
        return Err(match kind {
            DavKind::Calendar => ApiError::Rule("notICalendar", "this is not an iCalendar file".into()),
            DavKind::Addressbook => ApiError::Rule("notVCard", "this is not a vCard file".into()),
        });
    }
    Ok(split)
}

/// Imports what a file or a server brought, with what the cutting left out in the same report.
async fn import(
    web: &Web,
    session: &Session,
    collection: &DavCollection,
    split: Split,
    mode: DavImportMode,
) -> ApiResult<DavImportReport> {
    let mut report = web.store().dav_import(session.account.id, collection.id, split.objects, mode).await?;
    for problem in split.problems {
        report.total += 1;
        report.problem(problem.item, problem.reason);
    }
    Ok(report)
}

/// The page's view, with what the request did.
async fn answer(web: &Web, session: &Session, extra: Value) -> ApiResult<Json<Value>> {
    let Json(mut view) = super::calendars::view(web, session).await?;
    if let (Value::Object(view), Value::Object(extra)) = (&mut view, extra) {
        view.extend(extra);
    }
    Ok(Json(view))
}

fn shown(collection: &DavCollection) -> Value {
    json!({ "id": collection.id, "kind": collection.kind, "name": collection.display_name })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTarget {
    kind: String,
    /// An own collection's id; a new one when left out.
    target: Option<i64>,
    name: Option<String>,
    /// The file's name, for a new collection whose file names none.
    file_name: Option<String>,
    color: Option<String>,
    /// `merge` (the default) or `onlyNew`.
    mode: Option<String>,
}

fn mode_of(mode: Option<&str>) -> DavImportMode {
    if mode == Some("onlyNew") { DavImportMode::OnlyNew } else { DavImportMode::Merge }
}

/// Imports an `.ics` or `.vcf` file, sent as the request body.
pub async fn import_file(
    State(web): State<Web>,
    session: Session,
    Query(query): Query<FileTarget>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let kind = kind_of(&query.kind)?;
    check_kind(&session, kind)?;
    if body.is_empty() {
        return Err(ApiError::Rule("importEmpty", "the file is empty".into()));
    }
    let split = split_of(kind, &decode_text(&body), false)?;
    let name = [query.name.clone(), split.meta.name.clone(), query.file_name.clone()]
        .into_iter()
        .flatten()
        .find(|name| !name.trim().is_empty());
    let new = NewImportCollection {
        name: name.unwrap_or_else(|| default_of(&web, kind).display_name),
        description: split.meta.description.clone().unwrap_or_default(),
        color: query.color.clone().or_else(|| split.meta.color.clone()),
    };
    let collection = target(&web, &session, kind, query.target, new).await?;
    let report = import(&web, &session, &collection, split, mode_of(query.mode.as_deref())).await?;
    answer(&web, &session, json!({ "report": report, "collection": shown(&collection) })).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlImport {
    url: String,
    target: Option<i64>,
    name: Option<String>,
    color: Option<String>,
}

/// Imports a calendar from its iCal address once, such as a Google calendar's secret address.
pub async fn import_url(
    State(web): State<Web>,
    session: Session,
    Json(request): Json<UrlImport>,
) -> ApiResult<Json<Value>> {
    check_kind(&session, DavKind::Calendar)?;
    let url = client::feed_url(&request.url).map_err(remote_error)?;
    polite(&web, &session)?;
    let transport = web.dav_transport();
    let fetched = tokio::time::timeout(FEED_LIMIT, client::fetch_feed(transport.as_ref(), &url, None))
        .await
        .map_err(|_| remote_error(RemoteError::Timeout))?
        .map_err(remote_error)?;
    let split = split_of(DavKind::Calendar, fetched.text.as_deref().unwrap_or_default(), false)?;
    let name = request.name.clone().filter(|name| !name.trim().is_empty()).or_else(|| split.meta.name.clone());
    let new = NewImportCollection {
        name: name.unwrap_or_else(|| uwumail_store::shown_url(&url).trim_end_matches("/…").to_owned()),
        description: split.meta.description.clone().unwrap_or_default(),
        color: request.color.clone().or_else(|| split.meta.color.clone()),
    };
    let collection = target(&web, &session, DavKind::Calendar, request.target, new).await?;
    let report = import(&web, &session, &collection, split, DavImportMode::Merge).await?;
    answer(&web, &session, json!({ "report": report, "collection": shown(&collection) })).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSubscription {
    url: String,
    name: Option<String>,
    color: Option<String>,
    interval_secs: Option<i64>,
    keep_alarms: Option<bool>,
}

/// Subscribes to a calendar feed. It is fetched once right away, so an address that does not
/// work is said at once instead of in an hour, and makes no calendar.
pub async fn subscribe(
    State(web): State<Web>,
    session: Session,
    Json(request): Json<NewSubscription>,
) -> ApiResult<Json<Value>> {
    check_kind(&session, DavKind::Calendar)?;
    let url = client::feed_url(&request.url).map_err(remote_error)?;
    let held = web.store().calendar_subscriptions(session.account.id).await?.len();
    if held >= uwumail_store::MAX_CALENDAR_SUBSCRIPTIONS {
        return Err(ApiError::Rule(
            "subscriptionLimit",
            format!("at most {} subscribed calendars", uwumail_store::MAX_CALENDAR_SUBSCRIPTIONS),
        ));
    }
    polite(&web, &session)?;
    let transport = web.dav_transport();
    let fetched = tokio::time::timeout(FEED_LIMIT, client::fetch_feed(transport.as_ref(), &url, None))
        .await
        .map_err(|_| remote_error(RemoteError::Timeout))?
        .map_err(remote_error)?;
    let keep_alarms = request.keep_alarms.unwrap_or(false);
    let split = split_of(DavKind::Calendar, fetched.text.as_deref().unwrap_or_default(), !keep_alarms)?;
    let interval = request
        .interval_secs
        .or(split.meta.refresh_secs)
        .unwrap_or(uwumail_store::DEFAULT_SUBSCRIPTION_INTERVAL_SECS)
        .clamp(uwumail_store::MIN_SUBSCRIPTION_INTERVAL_SECS, uwumail_store::MAX_SUBSCRIPTION_INTERVAL_SECS);
    let name = request
        .name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| split.meta.name.clone())
        .unwrap_or_else(|| uwumail_store::shown_url(&url).trim_end_matches("/…").to_owned());
    let new = NewCalendarSubscription {
        collection: NewDavCollection {
            slug: format!("subscribed-{}", uwumail_store::dav_etag(&url).trim_matches('"').get(..12).unwrap_or("feed")),
            display_name: name.chars().take(255).collect(),
            description: split.meta.description.clone().unwrap_or_default(),
            color: request.color.as_deref().or(split.meta.color.as_deref()).and_then(dav_color),
            components: vec!["VEVENT".into(), "VTODO".into()],
        },
        url,
        interval_secs: interval,
        keep_alarms,
    };
    let (collection, subscription) =
        web.store().create_calendar_subscription(session.account.id, new, default_of(&web, DavKind::Calendar)).await?;
    // What was fetched to check the address fills the calendar, instead of a second fetch.
    let report = web.store().dav_mirror(session.account.id, collection.id, split.objects).await?;
    web.store().note_subscription_run(subscription.id, Ok((fetched.validator, report.entries))).await?;
    answer(&web, &session, json!({ "report": report, "collection": shown(&collection) })).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionChange {
    url: Option<String>,
    name: Option<String>,
    color: Option<String>,
    interval_secs: Option<i64>,
    keep_alarms: Option<bool>,
    enabled: Option<bool>,
}

pub async fn update_subscription(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
    Json(change): Json<SubscriptionChange>,
) -> ApiResult<Json<Value>> {
    let url = change.url.as_deref().map(client::feed_url).transpose().map_err(remote_error)?;
    let subscription = web
        .store()
        .update_calendar_subscription(
            session.account.id,
            id,
            CalendarSubscriptionUpdate {
                url,
                interval_secs: change.interval_secs,
                keep_alarms: change.keep_alarms,
                enabled: change.enabled,
            },
        )
        .await?;
    if change.name.is_some() || change.color.is_some() {
        let update = DavCollectionUpdate {
            display_name: change
                .name
                .filter(|name| !name.trim().is_empty())
                .map(|name| name.chars().take(255).collect()),
            color: change.color.map(|color| dav_color(&color)),
            ..Default::default()
        };
        web.store().dav_update_collection(session.account.id, subscription.collection_id, update).await?;
    }
    answer(&web, &session, json!({})).await
}

/// Fetches a subscribed calendar now, once a minute at most.
pub async fn refresh(State(web): State<Web>, session: Session, Path(id): Path<i64>) -> ApiResult<Json<Value>> {
    if !web.store().subscription_due_now(session.account.id, id).await? {
        return Err(ApiError::Rule("refreshPause", "this calendar was fetched less than a minute ago".into()));
    }
    polite(&web, &session)?;
    let subscription = web.store().calendar_subscription(session.account.id, id).await?;
    let transport = web.dav_transport();
    let run =
        tokio::time::timeout(FEED_LIMIT, client::refresh_subscription(web.store(), transport.as_ref(), &subscription));
    let failed = match run.await {
        Ok(Ok(_)) => None,
        Ok(Err(err)) => Some(err),
        Err(_) => Some(RemoteError::Timeout),
    };
    if let Some(err) = failed {
        web.store().note_subscription_run(id, Err(err.code().to_owned())).await?;
    }
    answer(&web, &session, json!({})).await
}

#[derive(Deserialize)]
pub struct Unsubscribe {
    /// Keep the calendar with what it holds, as an ordinary one.
    keep: Option<bool>,
}

pub async fn unsubscribe(
    State(web): State<Web>,
    session: Session,
    Path(id): Path<i64>,
    Query(query): Query<Unsubscribe>,
) -> ApiResult<Json<Value>> {
    web.store().delete_calendar_subscription(session.account.id, id, query.keep.unwrap_or(false)).await?;
    answer(&web, &session, json!({})).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteImport {
    address: String,
    password: String,
    /// The provider's server, for those that cannot be found from the address.
    server: Option<String>,
    /// `calendar` and/or `addressbook`.
    kinds: Vec<String>,
}

/// Moves everything of the given kinds over from another CalDAV/CardDAV provider in one go: each
/// calendar and address book there becomes a new one here. One request, because some providers'
/// app passwords work for one setup only.
pub async fn import_remote(
    State(web): State<Web>,
    session: Session,
    Json(request): Json<RemoteImport>,
) -> ApiResult<Json<Value>> {
    let mut kinds = Vec::new();
    for kind in &request.kinds {
        let kind = kind_of(kind)?;
        check_kind(&session, kind)?;
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    if kinds.is_empty() {
        return Err(ApiError::Invalid("choose calendars, contacts or both".into()));
    }
    let address = request.address.trim().to_lowercase();
    if request.server.as_deref().is_none_or(|server| server.trim().is_empty()) && !address.contains('@') {
        return Err(ApiError::Rule("senderInvalid", format!("'{address}' is not an address")));
    }
    if request.password.is_empty() {
        return Err(ApiError::Invalid("the password is missing".into()));
    }
    // What is fetched is held until all of it is stored: one move per account at a time, with one
    // byte budget for all its collections together.
    let Some(_running) = web.start_remote_import(session.account.id) else {
        return Err(ApiError::Rule("importRunning", "a move from another provider is running already".into()));
    };
    polite(&web, &session)?;
    let transport = web.dav_transport();
    let work = async {
        let mut remote = Remote::with_login(transport.as_ref(), &address, &request.password);
        let found = client::discover(&mut remote, web.dns(), &address, request.server.as_deref(), &kinds).await?;
        let mut fetched = Vec::with_capacity(found.collections.len());
        let mut budget = client::MAX_IMPORT_BYTES;
        for remote_collection in &found.collections {
            match client::fetch_collection(&mut remote.fork(), remote_collection, &mut budget).await {
                Err(err @ (RemoteError::WrongPassword | RemoteError::RedirectedElsewhere)) => return Err(err),
                other => fetched.push(other),
            }
        }
        Ok((found, fetched))
    };
    let (found, fetched) = tokio::time::timeout(REMOTE_IMPORT_LIMIT, work)
        .await
        .map_err(|_| remote_error(RemoteError::Timeout))?
        .map_err(remote_error)?;
    let mut results = Vec::new();
    for (remote_collection, texts) in found.collections.iter().zip(fetched) {
        let kind = remote_collection.kind;
        let texts = match texts {
            Ok(texts) => texts,
            Err(err) => {
                results.push(json!({ "kind": kind, "name": remote_collection.name, "error": err.code() }));
                continue;
            }
        };
        let joined = texts.concat();
        let split = match kind {
            DavKind::Calendar => split_ics(&joined, false),
            DavKind::Addressbook => split_vcf(&joined),
        };
        let new = NewImportCollection {
            name: remote_collection.name.clone(),
            description: remote_collection.description.clone(),
            color: remote_collection.color.clone(),
        };
        let collection = target(&web, &session, kind, None, new).await?;
        let report = import(&web, &session, &collection, split, DavImportMode::Merge).await?;
        results.push(
            json!({ "kind": kind, "name": remote_collection.name, "collection": shown(&collection), "report": report }),
        );
    }
    tracing::info!(account = %session.account.login, provider = ?found.provider, collections = results.len(), "calendars and contacts moved over");
    answer(&web, &session, json!({ "provider": found.provider, "results": results })).await
}
