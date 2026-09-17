//! CalDAV and CardDAV the way the iPhone, Thunderbird and DAVx5 use them.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;
use uwumail_dav::{Dav, DavSettings};
use uwumail_jmap::ClientInfo;
use uwumail_store::{NewAccount, Role, Store};

const PASSWORD: &str = "katzenpfote-123";

struct Reply {
    status: StatusCode,
    etag: Option<String>,
    location: Option<String>,
    body: String,
}

async fn setup() -> (Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.de").await.unwrap();
    for login in ["mini", "leni"] {
        store
            .create_account(NewAccount {
                address: format!("{login}@example.de"),
                display_name: login.to_uppercase(),
                password: Some(PASSWORD.into()),
                role: Role::User,
                quota_bytes: 0,
            })
            .await
            .unwrap();
    }
    let dav = Dav::new(store, DavSettings { calendar_name: "Kalender".into(), addressbook_name: "Kontakte".into() });
    (dav.router(), dir)
}

fn basic(login: &str) -> String {
    use base64::Engine as _;
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{login}:{PASSWORD}")))
}

async fn send(app: &Router, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Reply {
    let mut request = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut request = request.body(Body::from(body.to_owned())).unwrap();
    request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let header_text = |name| response.headers().get(name).map(|v: &header::HeaderValue| v.to_str().unwrap().to_owned());
    let etag = header_text(header::ETAG);
    let location = header_text(header::LOCATION);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22).await.unwrap();
    Reply { status, etag, location, body: String::from_utf8_lossy(&bytes).into_owned() }
}

async fn as_mini(app: &Router, method: &str, path: &str, extra: &[(&str, &str)], body: &str) -> Reply {
    let auth = basic("mini@example.de");
    let mut headers = vec![("authorization", auth.as_str())];
    headers.extend_from_slice(extra);
    send(app, method, path, &headers, body).await
}

fn event(uid: &str, summary: &str, start: &str, end: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//DE\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260917T080000Z\r\n\
DTSTART:{start}\r\nDTEND:{end}\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let from = text.find(start).unwrap_or_else(|| panic!("{start} not in {text}")) + start.len();
    let to = text[from..].find(end).unwrap_or_else(|| panic!("{end} not after {start} in {text}")) + from;
    &text[from..to]
}

const PROPFIND_PRINCIPAL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<A:propfind xmlns:A="DAV:"><A:prop><A:current-user-principal/></A:prop></A:propfind>"#;

#[tokio::test]
async fn an_iphone_finds_the_calendar_and_keeps_events_in_sync() {
    let (app, _dir) = setup().await;

    let redirect = send(&app, "PROPFIND", "/.well-known/caldav", &[], "").await;
    assert_eq!((redirect.status, redirect.location.as_deref()), (StatusCode::MOVED_PERMANENTLY, Some("/dav/")));
    let options = send(&app, "OPTIONS", "/dav/", &[], "").await;
    assert_eq!(options.status, StatusCode::OK);
    let denied = send(&app, "PROPFIND", "/dav/", &[("depth", "0")], PROPFIND_PRINCIPAL).await;
    assert_eq!(denied.status, StatusCode::UNAUTHORIZED);

    let root = as_mini(&app, "PROPFIND", "/dav/", &[("depth", "0")], PROPFIND_PRINCIPAL).await;
    assert_eq!(root.status, StatusCode::MULTI_STATUS, "{}", root.body);
    let principal = between(&root.body, "<d:current-user-principal><d:href>", "</d:href>").to_owned();
    assert_eq!(principal, "/dav/principals/mini@example.de/");

    let homes = as_mini(
        &app,
        "PROPFIND",
        &principal,
        &[("depth", "0")],
        r#"<propfind xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CR="urn:ietf:params:xml:ns:carddav">
<prop><C:calendar-home-set/><CR:addressbook-home-set/><displayname/><C:schedule-inbox-URL/></prop></propfind>"#,
    )
    .await;
    let home = between(&homes.body, "<c:calendar-home-set><d:href>", "</d:href>").to_owned();
    assert_eq!(home, "/dav/calendars/mini@example.de/");
    assert!(homes.body.contains("<d:displayname>MINI</d:displayname>"));
    assert!(homes.body.contains("<c:schedule-inbox-URL/>") && homes.body.contains("404 Not Found"), "{}", homes.body);

    let calendars = as_mini(
        &app,
        "PROPFIND",
        &home,
        &[("depth", "1")],
        r#"<propfind xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:CS="http://calendarserver.org/ns/" xmlns:I="http://apple.com/ns/ical/">
<prop><resourcetype/><displayname/><CS:getctag/><sync-token/><C:supported-calendar-component-set/><I:calendar-color/></prop></propfind>"#,
    )
    .await;
    assert!(calendars.body.contains("<d:href>/dav/calendars/mini@example.de/personal/</d:href>"), "{}", calendars.body);
    assert!(calendars.body.contains("<d:resourcetype><d:collection/><c:calendar/></d:resourcetype>"));
    assert!(calendars.body.contains("<d:displayname>Kalender</d:displayname>"));
    assert!(calendars.body.contains("<c:comp name=\"VEVENT\"/><c:comp name=\"VTODO\"/>"));
    let calendar = "/dav/calendars/mini@example.de/personal/";

    let created = as_mini(
        &app,
        "PUT",
        &format!("{calendar}tierarzt.ics"),
        &[("if-none-match", "*"), ("content-type", "text/calendar")],
        &event("tierarzt", "Tierarzt", "20260920T090000Z", "20260920T100000Z"),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let etag = created.etag.clone().expect("an ETag for the new event");
    let again = as_mini(
        &app,
        "PUT",
        &format!("{calendar}tierarzt.ics"),
        &[("if-none-match", "*")],
        &event("tierarzt", "Tierarzt", "20260920T090000Z", "20260920T100000Z"),
    )
    .await;
    assert_eq!(again.status, StatusCode::PRECONDITION_FAILED);
    let broken =
        as_mini(&app, "PUT", &format!("{calendar}kaputt.ics"), &[], "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").await;
    assert_eq!(broken.status, StatusCode::FORBIDDEN);
    assert!(broken.body.contains("valid-calendar-object-resource"));
    let conflict = as_mini(
        &app,
        "PUT",
        &format!("{calendar}doppelt.ics"),
        &[],
        &event("tierarzt", "Doppelt", "20260920T090000Z", "20260920T100000Z"),
    )
    .await;
    assert!(
        conflict
            .body
            .contains("<c:no-uid-conflict><d:href>/dav/calendars/mini@example.de/personal/tierarzt.ics</d:href>")
    );

    let fetched = as_mini(&app, "GET", &format!("{calendar}tierarzt.ics"), &[], "").await;
    assert_eq!((fetched.status, fetched.etag.as_deref()), (StatusCode::OK, Some(etag.as_str())));
    assert!(fetched.body.contains("SUMMARY:Tierarzt"));

    // The first sync gets everything and a token.
    let sync_body = |token: &str| {
        format!(
            r#"<sync-collection xmlns="DAV:"><sync-token>{token}</sync-token><sync-level>1</sync-level><prop><getetag/></prop></sync-collection>"#
        )
    };
    let first = as_mini(&app, "REPORT", calendar, &[], &sync_body("")).await;
    assert_eq!(first.status, StatusCode::MULTI_STATUS, "{}", first.body);
    assert!(first.body.contains(&format!("<d:getetag>{}</d:getetag>", etag.replace('"', "&quot;"))), "{}", first.body);
    let token = between(&first.body, "<d:sync-token>", "</d:sync-token>").to_owned();

    // Another event, one change and one deletion later, the next sync gets only those.
    as_mini(
        &app,
        "PUT",
        &format!("{calendar}friseur.ics"),
        &[],
        &event("friseur", "Friseur", "20261001T150000Z", "20261001T160000Z"),
    )
    .await;
    let updated = as_mini(
        &app,
        "PUT",
        &format!("{calendar}tierarzt.ics"),
        &[("if-match", &etag)],
        &event("tierarzt", "Tierarzt, 10 Uhr", "20260920T100000Z", "20260920T110000Z"),
    )
    .await;
    assert_eq!(updated.status, StatusCode::NO_CONTENT);
    let stale = as_mini(&app, "DELETE", &format!("{calendar}tierarzt.ics"), &[("if-match", &etag)], "").await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED, "the old ETag no longer fits");
    assert_eq!(
        as_mini(&app, "DELETE", &format!("{calendar}tierarzt.ics"), &[], "").await.status,
        StatusCode::NO_CONTENT
    );
    let second = as_mini(&app, "REPORT", calendar, &[], &sync_body(&token)).await;
    assert!(second.body.contains("<d:href>/dav/calendars/mini@example.de/personal/friseur.ics</d:href><d:propstat>"));
    assert!(second.body.contains(
        "<d:href>/dav/calendars/mini@example.de/personal/tierarzt.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status>"
    ));
    let bad_token = as_mini(&app, "REPORT", calendar, &[], &sync_body("urn:uwumail:dav:sync:999:1")).await;
    assert_eq!(bad_token.status, StatusCode::FORBIDDEN);
    assert!(bad_token.body.contains("valid-sync-token"));

    let multiget = as_mini(
        &app,
        "REPORT",
        calendar,
        &[],
        r#"<C:calendar-multiget xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><D:getetag/><C:calendar-data/></D:prop>
<D:href>/dav/calendars/mini%40example.de/personal/friseur.ics</D:href><D:href>/dav/calendars/mini@example.de/personal/weg.ics</D:href></C:calendar-multiget>"#,
    )
    .await;
    assert!(multiget.body.contains("SUMMARY:Friseur"), "{}", multiget.body);
    assert!(multiget.body.contains("weg.ics</d:href><d:status>HTTP/1.1 404 Not Found"));

    let query = |start: &str, end: &str| {
        format!(
            r#"<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><D:getetag/></D:prop>
<C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VEVENT"><C:time-range start="{start}" end="{end}"/></C:comp-filter></C:comp-filter></C:filter></C:calendar-query>"#
        )
    };
    let october =
        as_mini(&app, "REPORT", calendar, &[("depth", "1")], &query("20261001T000000Z", "20261101T000000Z")).await;
    assert!(october.body.contains("friseur.ics"), "{}", october.body);
    let november =
        as_mini(&app, "REPORT", calendar, &[("depth", "1")], &query("20261101T000000Z", "20261201T000000Z")).await;
    assert!(!november.body.contains("friseur.ics"), "{}", november.body);
}

#[tokio::test]
async fn calendars_come_and_go_and_contacts_have_their_own_home() {
    let (app, _dir) = setup().await;
    let made = as_mini(
        &app,
        "MKCALENDAR",
        "/dav/calendars/mini@example.de/arbeit/",
        &[],
        r#"<C:mkcalendar xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:I="http://apple.com/ns/ical/"><D:set><D:prop>
<D:displayname>Arbeit</D:displayname><I:calendar-color>#00AAFFFF</I:calendar-color>
<C:supported-calendar-component-set><C:comp name="VTODO"/></C:supported-calendar-component-set></D:prop></D:set></C:mkcalendar>"#,
    )
    .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.body);
    let patched = as_mini(
        &app,
        "PROPPATCH",
        "/dav/calendars/mini@example.de/arbeit/",
        &[],
        r#"<D:propertyupdate xmlns:D="DAV:" xmlns:X="urn:example"><D:set><D:prop><D:displayname>Büro</D:displayname><X:own>1</X:own></D:prop></D:set></D:propertyupdate>"#,
    )
    .await;
    assert_eq!(patched.status, StatusCode::MULTI_STATUS);
    let listed = as_mini(
        &app,
        "PROPFIND",
        "/dav/calendars/mini@example.de/",
        &[("depth", "1")],
        r#"<propfind xmlns="DAV:" xmlns:I="http://apple.com/ns/ical/"><prop><displayname/><I:calendar-color/></prop></propfind>"#,
    )
    .await;
    assert!(
        listed.body.contains("<d:displayname>Büro</d:displayname><ical:calendar-color>#00AAFFFF</ical:calendar-color>"),
        "{}",
        listed.body
    );
    let refused = as_mini(
        &app,
        "PUT",
        "/dav/calendars/mini@example.de/arbeit/termin.ics",
        &[],
        &event("termin", "Termin", "20260920T090000Z", "20260920T100000Z"),
    )
    .await;
    assert!(refused.body.contains("supported-calendar-component"), "only tasks in this calendar");
    assert_eq!(
        as_mini(&app, "DELETE", "/dav/calendars/mini@example.de/arbeit/", &[], "").await.status,
        StatusCode::NO_CONTENT
    );

    let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:nyu\r\nFN:Nyu Katze\r\nEMAIL:nyu@example.org\r\nEND:VCARD\r\n";
    let stored = as_mini(&app, "PUT", "/dav/addressbooks/mini@example.de/contacts/nyu.vcf", &[], card).await;
    assert_eq!(stored.status, StatusCode::CREATED, "{}", stored.body);
    let books = as_mini(
        &app,
        "PROPFIND",
        "/dav/addressbooks/mini@example.de/contacts/",
        &[("depth", "1")],
        r#"<propfind xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav"><prop><getetag/><C:address-data/></prop></propfind>"#,
    )
    .await;
    assert!(books.body.contains("FN:Nyu Katze"), "{}", books.body);

    // Leni's things are not Mini's business.
    let foreign = as_mini(&app, "PROPFIND", "/dav/calendars/leni@example.de/", &[("depth", "1")], "").await;
    assert_eq!(foreign.status, StatusCode::FORBIDDEN);
}
