//! CalDAV (RFC 4791) and CardDAV (RFC 6352) for calendars and contacts, with sync-collection
//! (RFC 6578), scheduling (RFC 6638) and the extras Apple's apps and DAVx5 look for.
//!
//! URLs:
//!
//! - `/.well-known/caldav`, `/.well-known/carddav` lead to `/dav/`
//! - `/dav/principals/<login>/`: who is logged in and where the homes are
//! - `/dav/calendars/<login>/<calendar>/<event>` and `/dav/addressbooks/<login>/<book>/<contact>`
//! - `/dav/calendars/<login>/shared~<id>/`: a calendar someone else shares with the login, and the
//!   same for address books
//! - `/dav/calendars/<login>/inbox/` and `.../outbox/`: the scheduling inbox and outbox
//!
//! Changing an event one organizes or is invited to tells the others (implicit scheduling): see
//! `uwumail_smtp::scheduling`.

pub mod objects;
mod props;
pub mod xml;

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use uwumail_jmap::{Authenticator, ClientInfo};
use uwumail_smtp::Smtp;
use uwumail_store::itip::{self, Component};
use uwumail_store::{
    Account, AppScope, DavAccess, DavCollectionUpdate, DavKind, DavPrecondition, DavWrite,
    DavWriteOutcome, NewDavCollection, Store, StoreError,
};

use crate::props::{Requested, Target, View, Who};
use crate::xml::{APPLE, CALDAV, CARDDAV, DAV, Element};

const DAV_HEADER: &str = "1, 3, access-control, calendar-access, calendar-auto-schedule, addressbook, extended-mkcol";
const ALLOW: &str = "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, REPORT, MKCALENDAR, MKCOL, POST";

/// The last segments of the scheduling inbox and outbox in the calendar home.
pub(crate) const INBOX: &str = "inbox";
pub(crate) const OUTBOX: &str = "outbox";
/// How the calendar home names a collection shared with the login: `shared~<id>`.
const SHARED_PREFIX: &str = "shared~";
/// The longest free-busy lookup, as clients ask for weeks at most.
const MAX_FREE_BUSY_SECS: i64 = 400 * 86_400;
/// Attendees one free-busy lookup may ask about.
const MAX_FREE_BUSY_ATTENDEES: usize = 100;

/// Names of the collections every account gets, in the server's language.
#[derive(Debug, Clone)]
pub struct DavSettings {
    pub calendar_name: String,
    pub addressbook_name: String,
}

#[derive(Clone)]
pub struct Dav {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    auth: Authenticator,
    settings: DavSettings,
    /// Sends invitations, answers and cancellations; without it, events are only stored.
    smtp: Option<Smtp>,
}

impl Dav {
    pub fn new(store: Store, settings: DavSettings) -> Dav {
        let auth = Authenticator::for_protocol(store.clone(), AppScope::Dav, "dav");
        Dav { inner: Arc::new(Inner { store, auth, settings, smtp: None }) }
    }

    /// Lets changes to scheduled events reach the organizer and attendees (RFC 6638).
    pub fn with_scheduling(self, smtp: Smtp) -> Dav {
        let store = self.inner.store.clone();
        let auth = Authenticator::for_protocol(store.clone(), AppScope::Dav, "dav");
        Dav { inner: Arc::new(Inner { store, auth, settings: self.inner.settings.clone(), smtp: Some(smtp) }) }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/.well-known/caldav", any(well_known))
            .route("/.well-known/carddav", any(well_known))
            .route("/dav", any(handle))
            .route("/dav/", any(handle))
            .route("/dav/{*path}", any(handle))
            .with_state(self.clone())
    }

    fn store(&self) -> &Store {
        &self.inner.store
    }

    /// The calendar or address book everyone starts with.
    pub fn default_collection(&self, kind: DavKind) -> NewDavCollection {
        match kind {
            DavKind::Calendar => NewDavCollection::default_calendar(&self.inner.settings.calendar_name),
            DavKind::Addressbook => NewDavCollection::default_address_book(&self.inner.settings.addressbook_name),
        }
    }
}

async fn well_known() -> Response {
    (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, "/dav/")]).into_response()
}

/// Where a request points.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Path {
    Root,
    Principals,
    Principal(String),
    Home(DavKind, String),
    Collection(DavKind, String, String),
    Resource(DavKind, String, String, String),
}

fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn parse_path(path: &str) -> Option<Path> {
    let rest = path.strip_prefix("/dav")?;
    let segments: Vec<String> =
        rest.split('/').filter(|segment| !segment.is_empty()).map(percent_decode).collect::<Option<_>>()?;
    let kind = |name: &str| match name {
        "calendars" => Some(DavKind::Calendar),
        "addressbooks" => Some(DavKind::Addressbook),
        _ => None,
    };
    Some(match segments.as_slice() {
        [] => Path::Root,
        [p] if p == "principals" => Path::Principals,
        [p, login] if p == "principals" => Path::Principal(login.to_lowercase()),
        [k, login] => Path::Home(kind(k)?, login.to_lowercase()),
        [k, login, collection] => Path::Collection(kind(k)?, login.to_lowercase(), collection.clone()),
        [k, login, collection, name] => {
            Path::Resource(kind(k)?, login.to_lowercase(), collection.clone(), name.clone())
        }
        _ => return None,
    })
}

pub(crate) fn kind_segment(kind: DavKind) -> &'static str {
    match kind {
        DavKind::Calendar => "calendars",
        DavKind::Addressbook => "addressbooks",
    }
}

pub(crate) fn principal_href(login: &str) -> String {
    format!("/dav/principals/{login}/")
}

pub(crate) fn home_href(kind: DavKind, login: &str) -> String {
    format!("/dav/{}/{login}/", kind_segment(kind))
}

pub(crate) fn collection_href(kind: DavKind, login: &str, slug: &str) -> String {
    format!("/dav/{}/{login}/{slug}/", kind_segment(kind))
}

/// Whether a segment of the calendar home is the scheduling inbox or outbox.
fn scheduling_box(kind: DavKind, segment: &str) -> Option<&'static str> {
    match (kind, segment) {
        (DavKind::Calendar, INBOX) => Some(INBOX),
        (DavKind::Calendar, OUTBOX) => Some(OUTBOX),
        _ => None,
    }
}

/// Segments an own collection may not take, as the server uses them itself.
fn reserved(kind: DavKind, segment: &str) -> bool {
    segment.starts_with(SHARED_PREFIX) || scheduling_box(kind, segment).is_some()
}

fn simple(status: StatusCode, text: &str) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text.to_owned()).into_response()
}

/// A precondition error body, like `<c:no-uid-conflict/>`.
fn precondition(status: StatusCode, ns: &str, name: &str, inner: &str) -> Response {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:error xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\" \
xmlns:card=\"urn:ietf:params:xml:ns:carddav\">{}</d:error>\n",
        if inner.is_empty() { xml::empty_element(ns, name) } else { xml::element(ns, name, inner) }
    );
    (status, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response()
}

/// Refused for lack of a privilege (RFC 3744, 7.1.1).
fn not_allowed(privilege: &str) -> Response {
    precondition(StatusCode::FORBIDDEN, DAV, "need-privileges", &format!("<d:resource><d:privilege>{privilege}</d:privilege></d:resource>"))
}

fn multistatus(responses: Vec<String>, extra: &str) -> Response {
    let mut body = String::from(xml::MULTISTATUS_START);
    for response in responses {
        body.push_str(&response);
    }
    body.push_str(extra);
    body.push_str(xml::MULTISTATUS_END);
    (StatusCode::MULTI_STATUS, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response()
}

fn store_failure(err: StoreError) -> Response {
    match err {
        StoreError::NotFound(_) => simple(StatusCode::NOT_FOUND, "Not found"),
        StoreError::Conflict(_) => simple(StatusCode::METHOD_NOT_ALLOWED, "It exists already"),
        StoreError::Invalid(message) => simple(StatusCode::BAD_REQUEST, &message),
        StoreError::QuotaExceeded => simple(StatusCode::INSUFFICIENT_STORAGE, "Too much data"),
        StoreError::Rule { message, .. } => simple(StatusCode::FORBIDDEN, &message),
        other => {
            tracing::error!(%other, "a DAV request failed in the store");
            simple(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server")
        }
    }
}

fn depth(headers: &HeaderMap) -> u8 {
    match headers.get("depth").and_then(|value| value.to_str().ok()).map(str::trim) {
        Some("0") => 0,
        _ => 1,
    }
}

fn etag_header(headers: &HeaderMap, name: impl header::AsHeaderName) -> Option<String> {
    headers.get(name).and_then(|value| value.to_str().ok()).map(|value| {
        let value = value.trim();
        value.strip_prefix("W/").unwrap_or(value).to_owned()
    })
}

async fn handle(State(dav): State<Dav>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let method = parts.method.clone();
    let headers = parts.headers;
    let path = parts.uri.path().to_owned();
    if method == Method::OPTIONS {
        return (
            StatusCode::OK,
            [
                (header::HeaderName::from_static("dav"), HeaderValue::from_static(DAV_HEADER)),
                (header::ALLOW, HeaderValue::from_static(ALLOW)),
            ],
        )
            .into_response();
    }
    let client = parts.extensions.get::<ClientInfo>().copied().unwrap_or_default();
    let account = match dav.inner.auth.account(&headers, client).await {
        Ok(account) => account,
        Err(err) => return err.into_response(),
    };
    let body = match axum::body::to_bytes(body, xml::MAX_BODY).await {
        Ok(body) => body,
        Err(_) => return simple(StatusCode::PAYLOAD_TOO_LARGE, "The request is too big"),
    };
    let Some(target) = parse_path(&path) else {
        return simple(StatusCode::NOT_FOUND, "Not found");
    };
    let login = account.login.to_lowercase();
    let owner_ok = match &target {
        Path::Root | Path::Principals => true,
        Path::Principal(owner)
        | Path::Home(_, owner)
        | Path::Collection(_, owner, _)
        | Path::Resource(_, owner, _, _) => *owner == login,
    };
    if !owner_ok {
        return simple(StatusCode::FORBIDDEN, "Only your own calendars and contacts");
    }
    // Calendars and address books are switched on one at a time, and one password covers both --
    // so which of the two a request may touch is decided here, not where it logged in.
    let kind = match &target {
        Path::Root | Path::Principals | Path::Principal(_) => None,
        Path::Home(kind, _) | Path::Collection(kind, _, _) | Path::Resource(kind, _, _, _) => Some(*kind),
    };
    let allowed = match kind {
        Some(DavKind::Calendar) => account.protocols.caldav,
        Some(DavKind::Addressbook) => account.protocols.carddav,
        // The root and the principal say what there is; with neither switch on there is nothing.
        None => account.protocols.caldav || account.protocols.carddav,
    };
    if !allowed {
        return simple(StatusCode::FORBIDDEN, "This account does not use this");
    }
    let mut addresses: Vec<String> = dav.store().addresses(&account.login).await.unwrap_or_default();
    addresses.push(login.clone());
    let mut addresses: Vec<String> = addresses.into_iter().map(|a| a.to_lowercase()).collect();
    addresses.sort();
    addresses.dedup();
    let session = Session { dav: &dav, account: &account, login: &login, addresses };
    match method.as_str() {
        "PROPFIND" => session.propfind(&target, &headers, &body).await,
        "PROPPATCH" => session.proppatch(&target, &body).await,
        "REPORT" => session.report(&target, &body).await,
        "GET" | "HEAD" => session.get(&target, method == Method::HEAD).await,
        "PUT" => session.put(&target, &headers, &body).await,
        "DELETE" => session.delete(&target, &headers).await,
        "POST" => session.post(&target, &body).await,
        "MKCALENDAR" | "MKCOL" => session.make_collection(&target, method.as_str() == "MKCALENDAR", &body).await,
        _ => (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, ALLOW)]).into_response(),
    }
}

struct Session<'a> {
    dav: &'a Dav,
    account: &'a Account,
    login: &'a str,
    /// The account's addresses, lower case: who it is in events.
    addresses: Vec<String>,
}

/// What a collection path names.
enum Found {
    Collection(View),
    Inbox,
    Outbox,
}

impl Session<'_> {
    fn store(&self) -> &Store {
        self.dav.store()
    }

    fn who(&self, default_calendar: Option<String>) -> Who<'_> {
        Who { account: self.account, login: self.login, addresses: &self.addresses, default_calendar }
    }

    /// The account's own collections of a kind, then those shared with it.
    async fn collections(&self, kind: DavKind) -> Result<Vec<View>, StoreError> {
        let own = self.store().dav_collections(self.account.id, kind, self.dav.default_collection(kind)).await?;
        let mut views: Vec<View> = own.into_iter().map(|c| View::own(c, self.login)).collect();
        for shared in self.store().dav_shared_with(self.account.id, kind).await? {
            views.push(View {
                segment: format!("{SHARED_PREFIX}{}", shared.collection.id),
                collection: shared.collection,
                access: DavAccess::Shared(shared.rights),
                owner_login: shared.owner_login,
                owner_name: shared.owner_name,
            });
        }
        Ok(views)
    }

    /// The collection a path segment names, for this login.
    async fn collection(&self, kind: DavKind, segment: &str) -> Result<Option<View>, StoreError> {
        if let Some(id) = segment.strip_prefix(SHARED_PREFIX) {
            let Ok(id) = id.parse::<i64>() else { return Ok(None) };
            return Ok(self.collections(kind).await?.into_iter().find(|v| v.collection.id == id && v.segment == segment));
        }
        // Listing first makes the default collection exist before a client asks for it by name.
        self.store().dav_collections(self.account.id, kind, self.dav.default_collection(kind)).await?;
        Ok(self.store().dav_collection(self.account.id, kind, segment).await?.map(|c| View::own(c, self.login)))
    }

    async fn find(&self, kind: DavKind, segment: &str) -> Result<Option<Found>, StoreError> {
        match scheduling_box(kind, segment) {
            Some(INBOX) => Ok(Some(Found::Inbox)),
            Some(_) => Ok(Some(Found::Outbox)),
            None => Ok(self.collection(kind, segment).await?.map(Found::Collection)),
        }
    }

    /// Where invitations go: the default calendar's URL.
    async fn default_calendar_href(&self) -> Option<String> {
        let own = self.store().dav_collections(self.account.id, DavKind::Calendar, self.dav.default_collection(DavKind::Calendar)).await.ok()?;
        let calendar = own.iter().find(|c| c.is_default).or(own.first())?;
        Some(collection_href(DavKind::Calendar, self.login, &calendar.slug))
    }

    fn requested(body: &Option<Element>) -> Requested {
        let Some(root) = body else { return Requested::AllProp };
        if root.child(DAV, "propname").is_some() {
            return Requested::PropName;
        }
        match root.child(DAV, "prop") {
            Some(prop) => {
                Requested::Props(prop.children.iter().map(|child| (child.ns.clone(), child.name.clone())).collect())
            }
            None => Requested::AllProp,
        }
    }

    async fn propfind(&self, target: &Path, headers: &HeaderMap, body: &Bytes) -> Response {
        let parsed = match xml::parse(body) {
            Ok(parsed) => parsed,
            Err(message) => return simple(StatusCode::BAD_REQUEST, &message),
        };
        let requested = Self::requested(&parsed);
        let depth = depth(headers);
        let mut targets = Vec::new();
        let result: Result<(), StoreError> = async {
            match target {
                Path::Root => targets.push(Target::Root),
                Path::Principals => targets.push(Target::Principals),
                Path::Principal(_) => targets.push(Target::Principal),
                Path::Home(kind, _) => {
                    targets.push(Target::Home(*kind));
                    if depth > 0 {
                        for view in self.collections(*kind).await? {
                            targets.push(Target::Collection(view));
                        }
                        if *kind == DavKind::Calendar {
                            targets.push(Target::Inbox);
                            targets.push(Target::Outbox);
                        }
                    }
                }
                Path::Collection(kind, _, segment) => match self.find(*kind, segment).await? {
                    None => return Err(StoreError::NotFound(segment.clone())),
                    Some(Found::Inbox) => targets.push(Target::Inbox),
                    Some(Found::Outbox) => targets.push(Target::Outbox),
                    Some(Found::Collection(view)) => {
                        let owner = view.collection.account_id;
                        let id = view.collection.id;
                        targets.push(Target::Collection(view.clone()));
                        if depth > 0 {
                            if requested.wants_data() {
                                for resource in self.store().dav_resource_contents(owner, id, None).await? {
                                    targets.push(Target::Resource(view.clone(), resource.info, Some(resource.content)));
                                }
                            } else {
                                for info in self.store().dav_resources(owner, id).await? {
                                    targets.push(Target::Resource(view.clone(), info, None));
                                }
                            }
                        }
                    }
                },
                Path::Resource(kind, _, segment, name) => {
                    let Some(view) = self.collection(*kind, segment).await? else {
                        return Err(StoreError::NotFound(segment.clone()));
                    };
                    let mut found = self
                        .store()
                        .dav_resource_contents(view.collection.account_id, view.collection.id, Some(vec![name.clone()]))
                        .await?;
                    let Some(resource) = found.pop() else {
                        return Err(StoreError::NotFound(name.clone()));
                    };
                    targets.push(Target::Resource(view, resource.info, Some(resource.content)));
                }
            }
            Ok(())
        }
        .await;
        if let Err(err) = result {
            return store_failure(err);
        }
        let default_calendar =
            if targets.iter().any(|t| matches!(t, Target::Inbox)) { self.default_calendar_href().await } else { None };
        let who = self.who(default_calendar);
        let responses = targets.iter().map(|target| props::response(target, &requested, &who)).collect();
        multistatus(responses, "")
    }

    async fn proppatch(&self, target: &Path, body: &Bytes) -> Response {
        let Path::Collection(kind, _, segment) = target else {
            return simple(StatusCode::FORBIDDEN, "Only calendars and address books have properties to change");
        };
        let Ok(Some(root)) = xml::parse(body) else {
            return simple(StatusCode::BAD_REQUEST, "PROPPATCH needs a propertyupdate body");
        };
        let view = match self.collection(*kind, segment).await {
            Ok(Some(view)) => view,
            Ok(None) if scheduling_box(*kind, segment).is_some() => {
                return not_allowed("<d:write-properties/>");
            }
            Ok(None) => return simple(StatusCode::NOT_FOUND, "Not found"),
            Err(err) => return store_failure(err),
        };
        if !view.access.may_admin() {
            return not_allowed("<d:write-properties/>");
        }
        let (update, names) = collection_update(&root);
        if let Err(err) =
            self.store().dav_update_collection(view.collection.account_id, view.collection.id, update).await
        {
            return store_failure(err);
        }
        // Properties this server does not keep are accepted too: clients like to store their own.
        let props: String = names.iter().map(|(ns, name)| xml::empty_element(ns, name)).collect();
        let response = format!(
            "<d:response><d:href>{}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
            xml::escape(&collection_href(*kind, self.login, segment))
        );
        multistatus(vec![response], "")
    }

    async fn make_collection(&self, target: &Path, calendar: bool, body: &Bytes) -> Response {
        let Path::Collection(kind, _, slug) = target else {
            return simple(StatusCode::FORBIDDEN, "Calendars and address books go into your home");
        };
        if reserved(*kind, slug) {
            return simple(StatusCode::METHOD_NOT_ALLOWED, "This name is taken by the server");
        }
        let root = match xml::parse(body) {
            Ok(root) => root,
            Err(message) => return simple(StatusCode::BAD_REQUEST, &message),
        };
        let wanted = if calendar { DavKind::Calendar } else { *kind };
        if wanted != *kind {
            return simple(StatusCode::FORBIDDEN, "Calendars go under /dav/calendars/");
        }
        let mut new = NewDavCollection { slug: slug.clone(), ..Default::default() };
        if *kind == DavKind::Calendar {
            new.components = vec!["VEVENT".into(), "VTODO".into()];
        }
        let mut update = DavCollectionUpdate::default();
        if let Some(root) = &root {
            let (patch, _) = collection_update(root);
            new.display_name = patch.display_name.clone().unwrap_or_default();
            new.description = patch.description.clone().unwrap_or_default();
            new.color = patch.color.clone().flatten();
            if let Some(components) = find_components(root) {
                new.components = components;
            }
            update.sort_order = patch.sort_order;
            update.timezone = patch.timezone;
        }
        if self.store().dav_collections(self.account.id, *kind, self.dav.default_collection(*kind)).await.is_err() {
            return simple(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server");
        }
        match self.store().dav_create_collection(self.account.id, *kind, new).await {
            Ok(collection) => {
                if update.sort_order.is_some() || update.timezone.is_some() {
                    let _ = self.store().dav_update_collection(self.account.id, collection.id, update).await;
                }
                StatusCode::CREATED.into_response()
            }
            Err(err) => store_failure(err),
        }
    }

    /// One entry with its content, in a collection the login may read.
    async fn entry(&self, view: &View, name: &str) -> Result<Option<uwumail_store::DavResource>, StoreError> {
        let mut found = self
            .store()
            .dav_resource_contents(view.collection.account_id, view.collection.id, Some(vec![name.to_owned()]))
            .await?;
        Ok(found.pop())
    }

    async fn get(&self, target: &Path, head: bool) -> Response {
        let Path::Resource(kind, _, slug, name) = target else {
            return simple(StatusCode::METHOD_NOT_ALLOWED, "Use PROPFIND for collections");
        };
        let view = match self.collection(*kind, slug).await {
            Ok(Some(view)) => view,
            Ok(None) => return simple(StatusCode::NOT_FOUND, "Not found"),
            Err(err) => return store_failure(err),
        };
        let resource = match self.entry(&view, name).await {
            Ok(Some(resource)) => resource,
            Ok(None) => return simple(StatusCode::NOT_FOUND, "Not found"),
            Err(err) => return store_failure(err),
        };
        let content_type = props::content_type(*kind, &resource.info.component);
        let mut response =
            if head { StatusCode::OK.into_response() } else { (StatusCode::OK, resource.content).into_response() };
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(&content_type).expect("a plain content type"));
        if let Ok(etag) = HeaderValue::from_str(&resource.info.etag) {
            headers.insert(header::ETAG, etag);
        }
        if let Some(tag) = resource.info.schedule_tag.as_deref().and_then(|tag| HeaderValue::from_str(tag).ok()) {
            headers.insert("schedule-tag", tag);
        }
        response
    }

    /// Tells attendees or the organizer about a change to an own calendar's event.
    async fn schedule(&self, view: &View, old: Option<&str>, new: Option<&str>) {
        if !view.access.is_owner() || view.kind() != DavKind::Calendar {
            return;
        }
        if let Some(smtp) = &self.dav.inner.smtp {
            smtp.schedule_change(self.account, old, new).await;
        }
    }

    async fn put(&self, target: &Path, headers: &HeaderMap, body: &Bytes) -> Response {
        let Path::Resource(kind, _, slug, name) = target else {
            return simple(StatusCode::METHOD_NOT_ALLOWED, "Only entries can be stored");
        };
        let view = match self.collection(*kind, slug).await {
            Ok(Some(view)) => view,
            Ok(None) => return simple(StatusCode::CONFLICT, "The calendar or address book does not exist"),
            Err(err) => return store_failure(err),
        };
        if !view.access.may_write() {
            return not_allowed("<d:write-content/>");
        }
        let Ok(mut content) = String::from_utf8(body.to_vec()) else {
            return simple(StatusCode::BAD_REQUEST, "The data is not UTF-8");
        };
        let checked = match kind {
            DavKind::Calendar => objects::check_calendar(&content, &view.collection.components),
            DavKind::Addressbook => objects::check_contact(&content, name),
        };
        let checked = match checked {
            Ok(checked) => checked,
            Err(objects::Refused::UnsupportedComponent(_)) => {
                return precondition(StatusCode::FORBIDDEN, CALDAV, "supported-calendar-component", "");
            }
            Err(objects::Refused::InvalidData(_) | objects::Refused::InvalidObject(_)) => {
                return match kind {
                    DavKind::Calendar => {
                        precondition(StatusCode::FORBIDDEN, CALDAV, "valid-calendar-object-resource", "")
                    }
                    DavKind::Addressbook => precondition(StatusCode::FORBIDDEN, CARDDAV, "valid-address-data", ""),
                };
            }
        };
        let old = if *kind == DavKind::Calendar {
            match self.entry(&view, name).await {
                Ok(old) => old,
                Err(err) => return store_failure(err),
            }
        } else {
            None
        };
        let mut condition = DavPrecondition {
            if_match: etag_header(headers, header::IF_MATCH),
            if_none_match_any: etag_header(headers, header::IF_NONE_MATCH).is_some_and(|value| value == "*"),
        };
        // RFC 6638, 3.2.10: a client that saw the organizer's copy at this Schedule-Tag may store
        // over answers that came in since; they are carried over into what it stores.
        let mut merged = false;
        if let Some(wanted) = etag_header(headers, "if-schedule-tag-match") {
            let Some(old) = &old else {
                return simple(StatusCode::PRECONDITION_FAILED, "The entry is not there");
            };
            if old.info.schedule_tag.as_deref() != Some(wanted.as_str()) {
                return simple(StatusCode::PRECONDITION_FAILED, "The entry changed meanwhile");
            }
            condition.if_match = Some(old.info.etag.clone());
            if let (Some(stored), Some(mut new)) = (Component::parse(&old.content), Component::parse(&content)) {
                let before = new.clone();
                itip::keep_answers(&mut new, &stored, &self.addresses);
                if new != before {
                    content = new.to_ics();
                    merged = true;
                }
            }
        }
        let write = DavWrite {
            name: name.clone(),
            content: content.clone(),
            uid: checked.uid,
            component: checked.component,
            starts_at: checked.starts_at,
            ends_at: checked.ends_at,
        };
        let outcome = self.store().dav_put(view.collection.account_id, view.collection.id, write, condition).await;
        let (status, etag) = match outcome {
            Ok(DavWriteOutcome::Created { etag }) => (StatusCode::CREATED, etag),
            Ok(DavWriteOutcome::Updated { etag }) => (StatusCode::NO_CONTENT, etag),
            Ok(DavWriteOutcome::PreconditionFailed) => {
                return simple(StatusCode::PRECONDITION_FAILED, "The entry changed meanwhile");
            }
            Ok(DavWriteOutcome::UidTaken { name: other }) => {
                let (ns, prop) = match kind {
                    DavKind::Calendar => (CALDAV, "no-uid-conflict"),
                    DavKind::Addressbook => (CARDDAV, "no-uid-conflict"),
                };
                let href = format!(
                    "<d:href>{}{}</d:href>",
                    xml::escape(&collection_href(*kind, self.login, slug)),
                    xml::escape(&other)
                );
                return precondition(StatusCode::FORBIDDEN, ns, prop, &href);
            }
            Err(StoreError::QuotaExceeded) => {
                let (ns, prop) = match kind {
                    DavKind::Calendar => (CALDAV, "max-resource-size"),
                    DavKind::Addressbook => (CARDDAV, "max-resource-size"),
                };
                return precondition(StatusCode::FORBIDDEN, ns, prop, "");
            }
            Err(err) => return store_failure(err),
        };
        self.schedule(&view, old.as_ref().map(|old| old.content.as_str()), Some(&content)).await;
        let mut response = status.into_response();
        // What was stored is not what the client sent when answers were merged in: without an
        // ETag it reads the entry again.
        if !merged && let Ok(value) = HeaderValue::from_str(&etag) {
            response.headers_mut().insert(header::ETAG, value);
        }
        if *kind == DavKind::Calendar
            && let Ok(value) = HeaderValue::from_str(&etag)
        {
            response.headers_mut().insert("schedule-tag", value);
        }
        response
    }

    async fn delete(&self, target: &Path, headers: &HeaderMap) -> Response {
        match target {
            Path::Collection(kind, _, slug) => match self.collection(*kind, slug).await {
                // A shared collection is left, not deleted: it stays its owner's.
                Ok(Some(view)) if !view.access.is_owner() => {
                    match self.store().dav_unshare(self.account.id, view.collection.id, self.account.id).await {
                        Ok(()) => StatusCode::NO_CONTENT.into_response(),
                        Err(err) => store_failure(err),
                    }
                }
                Ok(Some(view)) => match self.store().dav_delete_collection(self.account.id, view.collection.id).await {
                    Ok(()) => StatusCode::NO_CONTENT.into_response(),
                    Err(err) => store_failure(err),
                },
                Ok(None) if scheduling_box(*kind, slug).is_some() => not_allowed("<d:unbind/>"),
                Ok(None) => simple(StatusCode::NOT_FOUND, "Not found"),
                Err(err) => store_failure(err),
            },
            Path::Resource(kind, _, slug, name) => {
                let view = match self.collection(*kind, slug).await {
                    Ok(Some(view)) => view,
                    Ok(None) => return simple(StatusCode::NOT_FOUND, "Not found"),
                    Err(err) => return store_failure(err),
                };
                if !view.access.may_write() {
                    return not_allowed("<d:unbind/>");
                }
                let old = if *kind == DavKind::Calendar { self.entry(&view, name).await.ok().flatten() } else { None };
                let if_match = etag_header(headers, header::IF_MATCH);
                match self.store().dav_delete(view.collection.account_id, view.collection.id, name, if_match.clone()).await
                {
                    Ok(true) => {
                        self.schedule(&view, old.as_ref().map(|old| old.content.as_str()), None).await;
                        StatusCode::NO_CONTENT.into_response()
                    }
                    Ok(false) if if_match.is_some() => {
                        simple(StatusCode::PRECONDITION_FAILED, "The entry changed meanwhile")
                    }
                    Ok(false) => simple(StatusCode::NOT_FOUND, "Not found"),
                    Err(err) => store_failure(err),
                }
            }
            _ => simple(StatusCode::FORBIDDEN, "This cannot be deleted"),
        }
    }

    /// `POST` to the outbox: a free-busy lookup (RFC 6638, 5).
    async fn post(&self, target: &Path, body: &Bytes) -> Response {
        let Path::Collection(DavKind::Calendar, _, segment) = target else {
            return simple(StatusCode::METHOD_NOT_ALLOWED, "POST goes to the scheduling outbox");
        };
        if segment != OUTBOX {
            return simple(StatusCode::METHOD_NOT_ALLOWED, "POST goes to the scheduling outbox");
        }
        let Some(request) = std::str::from_utf8(body).ok().and_then(Component::parse) else {
            return precondition(StatusCode::BAD_REQUEST, CALDAV, "valid-calendar-data", "");
        };
        let Some(query) = request.components.iter().find(|c| c.name == "VFREEBUSY") else {
            return precondition(StatusCode::FORBIDDEN, CALDAV, "valid-scheduling-message", "");
        };
        if itip::method(&request).as_deref() != Some("REQUEST") {
            return precondition(StatusCode::FORBIDDEN, CALDAV, "valid-scheduling-message", "");
        }
        let organizer = query.property("ORGANIZER").and_then(|p| p.address());
        if !organizer.as_ref().is_some_and(|o| self.addresses.contains(o)) {
            return precondition(StatusCode::FORBIDDEN, CALDAV, "organizer-allowed", "");
        }
        let (Some(start), Some(end)) = (
            query.value("DTSTART").and_then(|v| props::utc_time(v.trim())),
            query.value("DTEND").and_then(|v| props::utc_time(v.trim())),
        ) else {
            return precondition(StatusCode::FORBIDDEN, CALDAV, "valid-scheduling-message", "");
        };
        if end <= start || end - start > MAX_FREE_BUSY_SECS {
            return precondition(StatusCode::FORBIDDEN, CALDAV, "valid-scheduling-message", "");
        }
        let attendees: Vec<String> = query
            .properties_named("ATTENDEE")
            .filter_map(|p| p.address())
            .take(MAX_FREE_BUSY_ATTENDEES)
            .collect();
        let mut responses = String::new();
        for attendee in attendees {
            let busy = self.busy(&attendee, start, end).await;
            let (status, data) = match busy {
                Some(periods) => {
                    let mut reply = Component::new("VCALENDAR");
                    reply.properties.push(itip::Property::new("VERSION", "2.0"));
                    reply.properties.push(itip::Property::new("PRODID", "-//UwUMail//Server//EN"));
                    reply.properties.push(itip::Property::new("METHOD", "REPLY"));
                    let mut answer = Component::new("VFREEBUSY");
                    for name in ["UID", "ORGANIZER"] {
                        if let Some(p) = query.property(name) {
                            answer.properties.push(p.clone());
                        }
                    }
                    answer.properties.push(itip::Property::new("ATTENDEE", format!("mailto:{attendee}")));
                    answer.properties.push(itip::Property::new("DTSTAMP", itip::stamp(now())));
                    answer.properties.push(itip::Property::new("DTSTART", itip::stamp(start)));
                    answer.properties.push(itip::Property::new("DTEND", itip::stamp(end)));
                    for (from, to) in periods {
                        answer
                            .properties
                            .push(itip::Property::new("FREEBUSY", format!("{}/{}", itip::stamp(from), itip::stamp(to))));
                    }
                    reply.components.push(answer);
                    (
                        "2.0;Success",
                        format!("<c:calendar-data>{}</c:calendar-data>", xml::escape(&reply.to_ics())),
                    )
                }
                None => ("3.7;Invalid calendar user", String::new()),
            };
            responses.push_str(&format!(
                "<c:response><c:recipient><d:href>{}</d:href></c:recipient><c:request-status>{status}</c:request-status>{data}</c:response>",
                xml::escape(&format!("mailto:{attendee}"))
            ));
        }
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<c:schedule-response xmlns:d=\"DAV:\" \
xmlns:c=\"urn:ietf:params:xml:ns:caldav\">{responses}</c:schedule-response>\n"
        );
        (StatusCode::OK, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response()
    }

    /// When someone of this server is busy between `start` and `end`, merged; `None` for anyone
    /// else. Only one's own calendars count, not those shared with them.
    async fn busy(&self, address: &str, start: i64, end: i64) -> Option<Vec<(i64, i64)>> {
        let id = self.store().resolve_recipient(address).await.ok()??;
        let id = self.store().delivery_target(id).await.ok()??;
        let account = self.store().account_by_id(id).await.ok()??;
        if !account.protocols.caldav {
            return None;
        }
        let own: Vec<i64> = self
            .store()
            .dav_collections(id, DavKind::Calendar, self.dav.default_collection(DavKind::Calendar))
            .await
            .ok()?
            .into_iter()
            .map(|c| c.id)
            .collect();
        let events = self.store().calendar_events_between(id, None, Some(start), Some(end)).await.ok()?;
        let mut periods: Vec<(i64, i64)> = events
            .iter()
            .filter(|event| own.contains(&event.calendar_id))
            .flat_map(|event| uwumail_store::ical::busy_periods(&event.content, start, end))
            .map(|(from, to)| (from.max(start), to.min(end)))
            .collect();
        periods.sort_unstable();
        let mut merged: Vec<(i64, i64)> = Vec::new();
        for (from, to) in periods {
            match merged.last_mut() {
                Some(last) if from <= last.1 => last.1 = last.1.max(to),
                _ => merged.push((from, to)),
            }
        }
        Some(merged)
    }

    async fn report(&self, target: &Path, body: &Bytes) -> Response {
        let root = match xml::parse(body) {
            Ok(Some(root)) => root,
            Ok(None) => return simple(StatusCode::BAD_REQUEST, "REPORT needs a body"),
            Err(message) => return simple(StatusCode::BAD_REQUEST, &message),
        };
        let (kind, slug) = match target {
            Path::Collection(kind, _, slug) => (*kind, slug.clone()),
            // Principal searches and the like: nothing to find here.
            Path::Principal(_) | Path::Principals | Path::Root | Path::Home(..) => return multistatus(Vec::new(), ""),
            Path::Resource(..) => return precondition(StatusCode::FORBIDDEN, DAV, "supported-report", ""),
        };
        if scheduling_box(kind, &slug).is_some() {
            // Nothing waits in the inbox: invitations go straight into the calendars.
            let extra = if root.is(DAV, "sync-collection") {
                format!("<d:sync-token>{}</d:sync-token>", xml::escape(&props::sync_token(0, 0)))
            } else {
                String::new()
            };
            return multistatus(Vec::new(), &extra);
        }
        let view = match self.collection(kind, &slug).await {
            Ok(Some(view)) => view,
            Ok(None) => return simple(StatusCode::NOT_FOUND, "Not found"),
            Err(err) => return store_failure(err),
        };
        let (owner, collection_id) = (view.collection.account_id, view.collection.id);
        let who = self.who(None);
        let requested = Self::requested(&Some(root.clone()));
        let base = collection_href(kind, self.login, &slug);
        let resource = |resource: uwumail_store::DavResource| {
            props::response(&Target::Resource(view.clone(), resource.info, Some(resource.content)), &requested, &who)
        };
        if root.is(CALDAV, "calendar-multiget") || root.is(CARDDAV, "addressbook-multiget") {
            let names: Vec<String> = root
                .children_named(DAV, "href")
                .filter_map(|href| {
                    let path = href.all_text();
                    let path = path.trim();
                    let path =
                        path.split_once("://").map_or(path, |(_, rest)| rest.find('/').map_or("", |i| &rest[i..]));
                    let decoded = percent_decode(path)?;
                    decoded
                        .strip_prefix(&base)
                        .filter(|name| !name.is_empty() && !name.contains('/'))
                        .map(str::to_owned)
                })
                .collect();
            let found = match self.store().dav_resource_contents(owner, collection_id, Some(names.clone())).await {
                Ok(found) => found,
                Err(err) => return store_failure(err),
            };
            let mut responses = Vec::new();
            for name in names {
                match found.iter().find(|resource| resource.info.name == name) {
                    Some(found) => responses.push(resource(found.clone())),
                    None => responses.push(props::not_found(&format!("{base}{name}"))),
                }
            }
            return multistatus(responses, "");
        }
        if root.is(DAV, "sync-collection") {
            let token =
                root.child(DAV, "sync-token").map(|token| token.all_text().trim().to_owned()).unwrap_or_default();
            let since = if token.is_empty() {
                0
            } else {
                match props::parse_sync_token(&token) {
                    Some((id, change)) if id == collection_id && change <= view.collection.change => change,
                    _ => return precondition(StatusCode::FORBIDDEN, DAV, "valid-sync-token", ""),
                }
            };
            let changes = match self.store().dav_changes(owner, collection_id, since).await {
                Ok(changes) => changes,
                Err(err) => return store_failure(err),
            };
            let mut responses = Vec::new();
            if requested.wants_data() {
                let names = changes.changed.iter().map(|info| info.name.clone()).collect();
                match self.store().dav_resource_contents(owner, collection_id, Some(names)).await {
                    Ok(found) => responses.extend(found.into_iter().map(resource)),
                    Err(err) => return store_failure(err),
                }
            } else {
                for info in changes.changed {
                    responses.push(props::response(&Target::Resource(view.clone(), info, None), &requested, &who));
                }
            }
            if since > 0 {
                for name in changes.deleted {
                    responses.push(props::not_found(&format!("{base}{name}")));
                }
            }
            let extra = format!(
                "<d:sync-token>{}</d:sync-token>",
                xml::escape(&props::sync_token(collection_id, changes.change))
            );
            return multistatus(responses, &extra);
        }
        if root.is(CALDAV, "calendar-query") || root.is(CARDDAV, "addressbook-query") {
            let filter = props::QueryFilter::from_report(&root);
            let all = match self.store().dav_resource_contents(owner, collection_id, None).await {
                Ok(all) => all,
                Err(err) => return store_failure(err),
            };
            let responses = all.into_iter().filter(|r| filter.matches(&r.info)).map(resource).collect();
            return multistatus(responses, "");
        }
        precondition(StatusCode::FORBIDDEN, DAV, "supported-report", "")
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The properties of a PROPPATCH or extended MKCOL body this server keeps, and the names of all set
/// or removed ones.
fn collection_update(root: &Element) -> (DavCollectionUpdate, Vec<(String, String)>) {
    let mut update = DavCollectionUpdate::default();
    let mut names = Vec::new();
    for (instruction, removing) in
        root.children.iter().filter_map(|child| match (child.ns.as_str(), child.name.as_str()) {
            (DAV, "set") => Some((child, false)),
            (DAV, "remove") => Some((child, true)),
            _ => None,
        })
    {
        for prop in instruction.children_named(DAV, "prop") {
            for property in &prop.children {
                names.push((property.ns.clone(), property.name.clone()));
                let text = property.all_text().trim().to_owned();
                match (property.ns.as_str(), property.name.as_str()) {
                    (DAV, "displayname") => update.display_name = Some(if removing { String::new() } else { text }),
                    (CALDAV, "calendar-description") | (CARDDAV, "addressbook-description") => {
                        update.description = Some(if removing { String::new() } else { text })
                    }
                    (APPLE, "calendar-color") => {
                        update.color = Some((!removing && !text.is_empty()).then_some(text));
                    }
                    (APPLE, "calendar-order") => update.sort_order = text.parse().ok(),
                    (CALDAV, "calendar-timezone") => {
                        update.timezone = Some((!removing && !text.is_empty()).then_some(text));
                    }
                    _ => {}
                }
            }
        }
    }
    (update, names)
}

fn find_components(root: &Element) -> Option<Vec<String>> {
    fn find(element: &Element) -> Option<&Element> {
        if element.is(CALDAV, "supported-calendar-component-set") {
            return Some(element);
        }
        element.children.iter().find_map(find)
    }
    let set = find(root)?;
    let components: Vec<String> = set
        .children_named(CALDAV, "comp")
        .filter_map(|comp| comp.attribute("name"))
        .map(|name| name.to_ascii_uppercase())
        .collect();
    (!components.is_empty()).then_some(components)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(parse_path("/dav/"), Some(Path::Root));
        assert_eq!(parse_path("/dav/principals/Mini%40example.org/"), Some(Path::Principal("mini@example.org".into())));
        assert_eq!(
            parse_path("/dav/calendars/mini@example.org/"),
            Some(Path::Home(DavKind::Calendar, "mini@example.org".into()))
        );
        assert_eq!(
            parse_path("/dav/addressbooks/mini@example.org/contacts/nyu.vcf"),
            Some(Path::Resource(DavKind::Addressbook, "mini@example.org".into(), "contacts".into(), "nyu.vcf".into()))
        );
        assert_eq!(parse_path("/dav/other/x/"), None);
        assert_eq!(parse_path("/dav/calendars/a/b/c/d"), None);
        assert_eq!(parse_path("/dav/calendars/%zz/"), None);
        assert!(reserved(DavKind::Calendar, "inbox") && reserved(DavKind::Addressbook, "shared~4"));
        assert!(!reserved(DavKind::Addressbook, "inbox"));
    }
}
