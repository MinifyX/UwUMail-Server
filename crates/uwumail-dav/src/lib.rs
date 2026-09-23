//! CalDAV (RFC 4791) and CardDAV (RFC 6352) for calendars and contacts, with sync-collection
//! (RFC 6578) and the extras Apple's apps and DAVx5 look for.
//!
//! URLs:
//!
//! - `/.well-known/caldav`, `/.well-known/carddav` lead to `/dav/`
//! - `/dav/principals/<login>/`: who is logged in and where the homes are
//! - `/dav/calendars/<login>/<calendar>/<event>` and `/dav/addressbooks/<login>/<book>/<contact>`

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
use uwumail_store::{
    Account, AppScope, DavCollection, DavCollectionUpdate, DavKind, DavPrecondition, DavWrite, DavWriteOutcome,
    NewDavCollection, Store, StoreError,
};

use crate::props::{Requested, Target};
use crate::xml::{APPLE, CALDAV, CARDDAV, DAV, Element};

const DAV_HEADER: &str = "1, 3, access-control, calendar-access, addressbook, extended-mkcol";
const ALLOW: &str = "OPTIONS, GET, HEAD, PUT, DELETE, PROPFIND, PROPPATCH, REPORT, MKCALENDAR, MKCOL";

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
}

impl Dav {
    pub fn new(store: Store, settings: DavSettings) -> Dav {
        let auth = Authenticator::for_protocol(store.clone(), AppScope::Dav, "dav");
        Dav { inner: Arc::new(Inner { store, auth, settings }) }
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
            DavKind::Addressbook => NewDavCollection {
                slug: "contacts".into(),
                display_name: self.inner.settings.addressbook_name.clone(),
                ..Default::default()
            },
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

fn etag_header(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
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
    let session = Session { dav: &dav, account: &account, login: &login };
    match method.as_str() {
        "PROPFIND" => session.propfind(&target, &headers, &body).await,
        "PROPPATCH" => session.proppatch(&target, &body).await,
        "REPORT" => session.report(&target, &body).await,
        "GET" | "HEAD" => session.get(&target, method == Method::HEAD).await,
        "PUT" => session.put(&target, &headers, &body).await,
        "DELETE" => session.delete(&target, &headers).await,
        "MKCALENDAR" | "MKCOL" => session.make_collection(&target, method.as_str() == "MKCALENDAR", &body).await,
        _ => (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, ALLOW)]).into_response(),
    }
}

struct Session<'a> {
    dav: &'a Dav,
    account: &'a Account,
    login: &'a str,
}

impl Session<'_> {
    fn store(&self) -> &Store {
        self.dav.store()
    }

    async fn collections(&self, kind: DavKind) -> Result<Vec<DavCollection>, StoreError> {
        self.store().dav_collections(self.account.id, kind, self.dav.default_collection(kind)).await
    }

    async fn collection(&self, kind: DavKind, slug: &str) -> Result<Option<DavCollection>, StoreError> {
        // Listing first makes the default collection exist before a client asks for it by name.
        self.collections(kind).await?;
        self.store().dav_collection(self.account.id, kind, slug).await
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
                        for collection in self.collections(*kind).await? {
                            targets.push(Target::Collection(collection));
                        }
                    }
                }
                Path::Collection(kind, _, slug) => {
                    let Some(collection) = self.collection(*kind, slug).await? else {
                        return Err(StoreError::NotFound(slug.clone()));
                    };
                    targets.push(Target::Collection(collection.clone()));
                    if depth > 0 {
                        let with_content = requested.wants_data();
                        if with_content {
                            for resource in
                                self.store().dav_resource_contents(self.account.id, collection.id, None).await?
                            {
                                targets.push(Target::Resource(
                                    collection.clone(),
                                    resource.info,
                                    Some(resource.content),
                                ));
                            }
                        } else {
                            for info in self.store().dav_resources(self.account.id, collection.id).await? {
                                targets.push(Target::Resource(collection.clone(), info, None));
                            }
                        }
                    }
                }
                Path::Resource(kind, _, slug, name) => {
                    let Some(collection) = self.collection(*kind, slug).await? else {
                        return Err(StoreError::NotFound(slug.clone()));
                    };
                    let mut found = self
                        .store()
                        .dav_resource_contents(self.account.id, collection.id, Some(vec![name.clone()]))
                        .await?;
                    let Some(resource) = found.pop() else {
                        return Err(StoreError::NotFound(name.clone()));
                    };
                    targets.push(Target::Resource(collection, resource.info, Some(resource.content)));
                }
            }
            Ok(())
        }
        .await;
        if let Err(err) = result {
            return store_failure(err);
        }
        let responses =
            targets.iter().map(|target| props::response(target, &requested, self.account, self.login)).collect();
        multistatus(responses, "")
    }

    async fn proppatch(&self, target: &Path, body: &Bytes) -> Response {
        let Path::Collection(kind, _, slug) = target else {
            return simple(StatusCode::FORBIDDEN, "Only calendars and address books have properties to change");
        };
        let Ok(Some(root)) = xml::parse(body) else {
            return simple(StatusCode::BAD_REQUEST, "PROPPATCH needs a propertyupdate body");
        };
        let collection = match self.collection(*kind, slug).await {
            Ok(Some(collection)) => collection,
            Ok(None) => return simple(StatusCode::NOT_FOUND, "Not found"),
            Err(err) => return store_failure(err),
        };
        let (update, names) = collection_update(&root);
        if let Err(err) = self.store().dav_update_collection(self.account.id, collection.id, update).await {
            return store_failure(err);
        }
        // Properties this server does not keep are accepted too: clients like to store their own.
        let props: String = names.iter().map(|(ns, name)| xml::empty_element(ns, name)).collect();
        let response = format!(
            "<d:response><d:href>{}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
            xml::escape(&collection_href(*kind, self.login, slug))
        );
        multistatus(vec![response], "")
    }

    async fn make_collection(&self, target: &Path, calendar: bool, body: &Bytes) -> Response {
        let Path::Collection(kind, _, slug) = target else {
            return simple(StatusCode::FORBIDDEN, "Calendars and address books go into your home");
        };
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
        if self.collections(*kind).await.is_err() {
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

    async fn get(&self, target: &Path, head: bool) -> Response {
        let Path::Resource(kind, _, slug, name) = target else {
            return simple(StatusCode::METHOD_NOT_ALLOWED, "Use PROPFIND for collections");
        };
        let collection = match self.collection(*kind, slug).await {
            Ok(Some(collection)) => collection,
            Ok(None) => return simple(StatusCode::NOT_FOUND, "Not found"),
            Err(err) => return store_failure(err),
        };
        let mut found =
            match self.store().dav_resource_contents(self.account.id, collection.id, Some(vec![name.clone()])).await {
                Ok(found) => found,
                Err(err) => return store_failure(err),
            };
        let Some(resource) = found.pop() else { return simple(StatusCode::NOT_FOUND, "Not found") };
        let content_type = props::content_type(*kind, &resource.info.component);
        let mut response =
            if head { StatusCode::OK.into_response() } else { (StatusCode::OK, resource.content).into_response() };
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(&content_type).expect("a plain content type"));
        if let Ok(etag) = HeaderValue::from_str(&resource.info.etag) {
            headers.insert(header::ETAG, etag);
        }
        response
    }

    async fn put(&self, target: &Path, headers: &HeaderMap, body: &Bytes) -> Response {
        let Path::Resource(kind, _, slug, name) = target else {
            return simple(StatusCode::METHOD_NOT_ALLOWED, "Only entries can be stored");
        };
        let collection = match self.collection(*kind, slug).await {
            Ok(Some(collection)) => collection,
            Ok(None) => return simple(StatusCode::CONFLICT, "The calendar or address book does not exist"),
            Err(err) => return store_failure(err),
        };
        let Ok(content) = String::from_utf8(body.to_vec()) else {
            return simple(StatusCode::BAD_REQUEST, "The data is not UTF-8");
        };
        let checked = match kind {
            DavKind::Calendar => objects::check_calendar(&content, &collection.components),
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
        let condition = DavPrecondition {
            if_match: etag_header(headers, header::IF_MATCH),
            if_none_match_any: etag_header(headers, header::IF_NONE_MATCH).is_some_and(|value| value == "*"),
        };
        let write = DavWrite {
            name: name.clone(),
            content,
            uid: checked.uid,
            component: checked.component,
            starts_at: checked.starts_at,
            ends_at: checked.ends_at,
        };
        match self.store().dav_put(self.account.id, collection.id, write, condition).await {
            Ok(DavWriteOutcome::Created { etag }) => with_etag(StatusCode::CREATED, &etag),
            Ok(DavWriteOutcome::Updated { etag }) => with_etag(StatusCode::NO_CONTENT, &etag),
            Ok(DavWriteOutcome::PreconditionFailed) => {
                simple(StatusCode::PRECONDITION_FAILED, "The entry changed meanwhile")
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
                precondition(StatusCode::FORBIDDEN, ns, prop, &href)
            }
            Err(StoreError::QuotaExceeded) => {
                let (ns, prop) = match kind {
                    DavKind::Calendar => (CALDAV, "max-resource-size"),
                    DavKind::Addressbook => (CARDDAV, "max-resource-size"),
                };
                precondition(StatusCode::FORBIDDEN, ns, prop, "")
            }
            Err(err) => store_failure(err),
        }
    }

    async fn delete(&self, target: &Path, headers: &HeaderMap) -> Response {
        match target {
            Path::Collection(kind, _, slug) => match self.collection(*kind, slug).await {
                Ok(Some(collection)) => {
                    match self.store().dav_delete_collection(self.account.id, collection.id).await {
                        Ok(()) => StatusCode::NO_CONTENT.into_response(),
                        Err(err) => store_failure(err),
                    }
                }
                Ok(None) => simple(StatusCode::NOT_FOUND, "Not found"),
                Err(err) => store_failure(err),
            },
            Path::Resource(kind, _, slug, name) => {
                let collection = match self.collection(*kind, slug).await {
                    Ok(Some(collection)) => collection,
                    Ok(None) => return simple(StatusCode::NOT_FOUND, "Not found"),
                    Err(err) => return store_failure(err),
                };
                let if_match = etag_header(headers, header::IF_MATCH);
                match self.store().dav_delete(self.account.id, collection.id, name, if_match.clone()).await {
                    Ok(true) => StatusCode::NO_CONTENT.into_response(),
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
        let collection = match self.collection(kind, &slug).await {
            Ok(Some(collection)) => collection,
            Ok(None) => return simple(StatusCode::NOT_FOUND, "Not found"),
            Err(err) => return store_failure(err),
        };
        let requested = Self::requested(&Some(root.clone()));
        let base = collection_href(kind, self.login, &slug);
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
            let found =
                match self.store().dav_resource_contents(self.account.id, collection.id, Some(names.clone())).await {
                    Ok(found) => found,
                    Err(err) => return store_failure(err),
                };
            let mut responses = Vec::new();
            for name in names {
                match found.iter().find(|resource| resource.info.name == name) {
                    Some(resource) => responses.push(props::response(
                        &Target::Resource(collection.clone(), resource.info.clone(), Some(resource.content.clone())),
                        &requested,
                        self.account,
                        self.login,
                    )),
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
                    Some((id, change)) if id == collection.id && change <= collection.change => change,
                    _ => return precondition(StatusCode::FORBIDDEN, DAV, "valid-sync-token", ""),
                }
            };
            let changes = match self.store().dav_changes(self.account.id, collection.id, since).await {
                Ok(changes) => changes,
                Err(err) => return store_failure(err),
            };
            let mut responses = Vec::new();
            if requested.wants_data() {
                let names = changes.changed.iter().map(|info| info.name.clone()).collect();
                match self.store().dav_resource_contents(self.account.id, collection.id, Some(names)).await {
                    Ok(found) => {
                        for resource in found {
                            responses.push(props::response(
                                &Target::Resource(collection.clone(), resource.info, Some(resource.content)),
                                &requested,
                                self.account,
                                self.login,
                            ));
                        }
                    }
                    Err(err) => return store_failure(err),
                }
            } else {
                for info in changes.changed {
                    responses.push(props::response(
                        &Target::Resource(collection.clone(), info, None),
                        &requested,
                        self.account,
                        self.login,
                    ));
                }
            }
            if since > 0 {
                for name in changes.deleted {
                    responses.push(props::not_found(&format!("{base}{name}")));
                }
            }
            let extra = format!(
                "<d:sync-token>{}</d:sync-token>",
                xml::escape(&props::sync_token(collection.id, changes.change))
            );
            return multistatus(responses, &extra);
        }
        if root.is(CALDAV, "calendar-query") || root.is(CARDDAV, "addressbook-query") {
            let filter = props::QueryFilter::from_report(&root);
            let all = match self.store().dav_resource_contents(self.account.id, collection.id, None).await {
                Ok(all) => all,
                Err(err) => return store_failure(err),
            };
            let responses = all
                .into_iter()
                .filter(|resource| filter.matches(&resource.info))
                .map(|resource| {
                    props::response(
                        &Target::Resource(collection.clone(), resource.info, Some(resource.content)),
                        &requested,
                        self.account,
                        self.login,
                    )
                })
                .collect();
            return multistatus(responses, "");
        }
        precondition(StatusCode::FORBIDDEN, DAV, "supported-report", "")
    }
}

fn with_etag(status: StatusCode, etag: &str) -> Response {
    let mut response = status.into_response();
    if let Ok(value) = HeaderValue::from_str(etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
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
        assert_eq!(parse_path("/dav/principals/Mini%40example.de/"), Some(Path::Principal("mini@example.de".into())));
        assert_eq!(
            parse_path("/dav/calendars/mini@example.de/"),
            Some(Path::Home(DavKind::Calendar, "mini@example.de".into()))
        );
        assert_eq!(
            parse_path("/dav/addressbooks/mini@example.de/contacts/nyu.vcf"),
            Some(Path::Resource(DavKind::Addressbook, "mini@example.de".into(), "contacts".into(), "nyu.vcf".into()))
        );
        assert_eq!(parse_path("/dav/other/x/"), None);
        assert_eq!(parse_path("/dav/calendars/a/b/c/d"), None);
        assert_eq!(parse_path("/dav/calendars/%zz/"), None);
    }
}
