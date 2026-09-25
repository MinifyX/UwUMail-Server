//! CalDAV scheduling (RFC 6638) and shared calendars the way Apple Calendar, Thunderbird and
//! DAVx5 use them: invitations between people of the server land in their calendars, answers go
//! back into the organizer's copy, people elsewhere get mail, and shared calendars show up in the
//! homes of those they are shared with.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;
use uwumail_dav::{Dav, DavSettings};
use uwumail_jmap::ClientInfo;
use uwumail_smtp::{DeliveryConfig, Smtp, SmtpConfig, SmtpSettings, ToneConfig};
use uwumail_store::itip::{self, Component};
use uwumail_store::{NewAccount, Role, ShareRights, Store};

const PASSWORD: &str = "katzenpfote-123";
const MINI: &str = "mini@example.org";
const LENI: &str = "leni@example.org";

struct Server {
    app: Router,
    store: Store,
    _dir: tempfile::TempDir,
}

struct Reply {
    status: StatusCode,
    etag: Option<String>,
    schedule_tag: Option<String>,
    body: String,
}

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    for login in ["mini", "leni"] {
        store
            .create_account(NewAccount {
                address: format!("{login}@example.org"),
                display_name: login.to_uppercase(),
                password: Some(PASSWORD.into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
    }
    let smtp = Smtp::new(
        store.clone(),
        SmtpSettings {
            hostname: "mail.example.org".into(),
            smtp: SmtpConfig::default(),
            spam: Default::default(),
            delivery: DeliveryConfig::default(),
            tone: ToneConfig::default(),
            server_tls: None,
        },
    )
    .unwrap();
    let dav =
        Dav::new(store.clone(), DavSettings { calendar_name: "Kalender".into(), addressbook_name: "Kontakte".into() })
            .with_scheduling(smtp);
    Server { app: dav.router(), store, _dir: dir }
}

impl Server {
    async fn send(&self, login: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Reply {
        use base64::Engine as _;
        let auth = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{login}:{PASSWORD}")));
        let mut request = Request::builder().method(method).uri(path).header(header::AUTHORIZATION, auth);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut request = request.body(Body::from(body.to_owned())).unwrap();
        request.extensions_mut().insert(ClientInfo { https: true, ..ClientInfo::default() });
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let text = |name: &str| response.headers().get(name).map(|v| v.to_str().unwrap().to_owned());
        let (etag, schedule_tag) = (text("etag"), text("schedule-tag"));
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 22).await.unwrap();
        Reply { status, etag, schedule_tag, body: String::from_utf8_lossy(&bytes).into_owned() }
    }

    /// Someone's own copy of an event, found by its UID.
    async fn copy(&self, login: &str, uid: &str) -> Option<Component> {
        let account = self.store.account(login).await.unwrap().unwrap();
        let record = self.store.own_calendar_event_by_uid(account.id, uid).await.unwrap()?;
        Component::parse(&record.content)
    }
}

fn invitation(summary: &str, start: &str, leni: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//DE\r\nBEGIN:VEVENT\r\nUID:kaffee@example.org\r\n\
DTSTAMP:20260917T080000Z\r\nDTSTART:{start}\r\nDTEND:20261001T160000Z\r\nSUMMARY:{summary}\r\nSEQUENCE:0\r\n\
ORGANIZER;CN=Mini:mailto:{MINI}\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:{MINI}\r\n\
ATTENDEE;CN=Leni;PARTSTAT={leni};RSVP=TRUE:mailto:{LENI}\r\n\
ATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:gast@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

const CALENDAR: &str = "/dav/calendars/mini@example.org/personal/kaffee.ics";

#[tokio::test]
async fn invitations_answers_and_cancellations_between_calendars() {
    let server = server().await;

    // What Apple Calendar asks the principal about before it offers invitations.
    let principal = server
        .send(
            MINI,
            "PROPFIND",
            "/dav/principals/mini@example.org/",
            &[("depth", "0")],
            r#"<propfind xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><prop><C:calendar-user-address-set/>
<C:schedule-inbox-URL/><C:schedule-outbox-URL/><C:calendar-user-type/></prop></propfind>"#,
        )
        .await;
    assert!(principal.body.contains("<d:href>mailto:mini@example.org</d:href>"), "{}", principal.body);
    assert!(principal.body.contains("<c:schedule-outbox-URL><d:href>/dav/calendars/mini@example.org/outbox/"));
    assert!(principal.body.contains("<c:calendar-user-type>INDIVIDUAL</c:calendar-user-type>"));
    let options = server.send(MINI, "OPTIONS", "/dav/", &[], "").await;
    assert_eq!(options.status, StatusCode::OK);
    let home = server.send(MINI, "PROPFIND", "/dav/calendars/mini@example.org/", &[("depth", "1")], "").await;
    assert!(home.body.contains("<c:schedule-inbox/>") && home.body.contains("<c:schedule-outbox/>"), "{}", home.body);
    let inbox = server
        .send(
            MINI,
            "PROPFIND",
            "/dav/calendars/mini@example.org/inbox/",
            &[("depth", "0")],
            r#"<propfind xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><prop><C:schedule-default-calendar-URL/></prop></propfind>"#,
        )
        .await;
    assert!(inbox.body.contains("/dav/calendars/mini@example.org/personal/"), "{}", inbox.body);
    let taken = server.send(MINI, "MKCALENDAR", "/dav/calendars/mini@example.org/inbox/", &[], "").await;
    assert_eq!(taken.status, StatusCode::METHOD_NOT_ALLOWED);

    // Mini invites Leni, who is on this server, and a guest from elsewhere.
    let created =
        server.send(MINI, "PUT", CALENDAR, &[], &invitation("Kaffee", "20261001T150000Z", "NEEDS-ACTION")).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let first_tag = created.schedule_tag.clone().expect("a Schedule-Tag");
    let leni_copy = server.copy(LENI, "kaffee@example.org").await.expect("the invitation is in Leni's calendar");
    assert_eq!(itip::partstats(&leni_copy, LENI), [(None, "NEEDS-ACTION".to_owned())]);
    assert_eq!(itip::method(&leni_copy), None);
    let queue = server.store.queue_entries().await.unwrap();
    assert_eq!(queue.len(), 1, "the guest gets mail");
    assert_eq!(queue[0].recipients[0].address, "gast@example.com");
    let raw = String::from_utf8(server.store.blob(&queue[0].message.blob).await.unwrap()).unwrap();
    assert!(raw.contains("method=\"REQUEST\"") && raw.contains("Einladung: Kaffee"), "{raw}");
    assert!(
        raw.contains("From: \"MINI\" <mini@example.org>") || raw.contains("From: MINI <mini@example.org>"),
        "{raw}"
    );

    // Leni accepts in her calendar app; the answer lands in Mini's copy, which keeps its
    // Schedule-Tag although its ETag changes.
    let leni_path = "/dav/calendars/leni@example.org/personal/kaffee@example.org.ics";
    let fetched = server.send(LENI, "GET", leni_path, &[], "").await;
    assert_eq!(fetched.status, StatusCode::OK, "{}", fetched.body);
    let accepted = fetched.body.replace("PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:leni", "PARTSTAT=ACCEPTED:mailto:leni");
    assert_ne!(accepted, fetched.body);
    let etag = fetched.etag.unwrap();
    let answered = server.send(LENI, "PUT", leni_path, &[("if-match", &etag)], &accepted).await;
    assert_eq!(answered.status, StatusCode::NO_CONTENT, "{}", answered.body);
    let mini_copy = server.copy(MINI, "kaffee@example.org").await.unwrap();
    assert_eq!(itip::partstats(&mini_copy, LENI), [(None, "ACCEPTED".to_owned())]);
    let mini_now = server.send(MINI, "GET", CALENDAR, &[], "").await;
    assert_eq!(mini_now.schedule_tag.as_deref(), Some(first_tag.as_str()), "the answer keeps the Schedule-Tag");
    assert_ne!(mini_now.etag, created.etag);

    // Mini's client still has the copy from before the answer and changes the title with
    // If-Schedule-Tag-Match: Leni's answer stays, and Leni's copy gets the new title with her answer.
    let renamed = invitation("Kaffee und Kuchen", "20261001T150000Z", "NEEDS-ACTION");
    let stale = server.send(MINI, "PUT", CALENDAR, &[("if-schedule-tag-match", "\"old\"")], &renamed).await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED);
    let stored = server.send(MINI, "PUT", CALENDAR, &[("if-schedule-tag-match", &first_tag)], &renamed).await;
    assert_eq!(stored.status, StatusCode::NO_CONTENT, "{}", stored.body);
    assert!(stored.etag.is_none(), "what was stored differs from what was sent");
    let mini_copy = server.copy(MINI, "kaffee@example.org").await.unwrap();
    assert_eq!(itip::partstats(&mini_copy, LENI), [(None, "ACCEPTED".to_owned())]);
    let leni_copy = server.copy(LENI, "kaffee@example.org").await.unwrap();
    assert_eq!(itip::summary(&leni_copy).title, "Kaffee und Kuchen");
    assert_eq!(itip::partstats(&leni_copy, LENI), [(None, "ACCEPTED".to_owned())], "same time, same answer");
    assert_eq!(server.store.queue_entries().await.unwrap().len(), 2, "the guest hears about the change");

    // Free and busy times, as Apple Calendar asks for them when attendees are added.
    let free_busy = server
        .send(
            MINI,
            "POST",
            "/dav/calendars/mini@example.org/outbox/",
            &[("content-type", "text/calendar")],
            &format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//DE\r\nMETHOD:REQUEST\r\nBEGIN:VFREEBUSY\r\n\
UID:fb-1\r\nDTSTAMP:20260917T080000Z\r\nDTSTART:20261001T000000Z\r\nDTEND:20261002T000000Z\r\n\
ORGANIZER:mailto:{MINI}\r\nATTENDEE:mailto:{LENI}\r\nATTENDEE:mailto:gast@example.com\r\nEND:VFREEBUSY\r\nEND:VCALENDAR\r\n"
            ),
        )
        .await;
    assert_eq!(free_busy.status, StatusCode::OK, "{}", free_busy.body);
    assert!(free_busy.body.contains("FREEBUSY:20261001T150000Z/20261001T160000Z"), "{}", free_busy.body);
    assert!(free_busy.body.contains("3.7;Invalid calendar user"), "nothing is known about the guest");

    // Mini cancels by deleting: Leni's copy says so, the guest gets mail.
    let deleted = server.send(MINI, "DELETE", CALENDAR, &[], "").await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    let leni_copy = server.copy(LENI, "kaffee@example.org").await.unwrap();
    assert_eq!(leni_copy.main_event().unwrap().value("STATUS"), Some("CANCELLED"));
    assert_eq!(server.store.queue_entries().await.unwrap().len(), 3);
}

#[tokio::test]
async fn attendees_decline_by_deleting() {
    let server = server().await;
    server.send(MINI, "PUT", CALENDAR, &[], &invitation("Kaffee", "20261001T150000Z", "NEEDS-ACTION")).await;
    let leni_path = "/dav/calendars/leni@example.org/personal/kaffee@example.org.ics";
    assert_eq!(server.send(LENI, "DELETE", leni_path, &[], "").await.status, StatusCode::NO_CONTENT);
    let mini_copy = server.copy(MINI, "kaffee@example.org").await.unwrap();
    assert_eq!(itip::partstats(&mini_copy, LENI), [(None, "DECLINED".to_owned())]);
}

#[tokio::test]
async fn shared_calendars_appear_in_the_home_of_those_they_are_shared_with() {
    let server = server().await;
    let event = invitation("Kaffee", "20261001T150000Z", "NEEDS-ACTION")
        .replace("ORGANIZER;CN=Mini:mailto:mini@example.org\r\n", "")
        .lines()
        .filter(|line| !line.starts_with("ATTENDEE"))
        .map(|line| format!("{line}\r\n"))
        .collect::<String>();
    assert_eq!(server.send(MINI, "PUT", CALENDAR, &[], &event).await.status, StatusCode::CREATED);
    let mini = server.store.account(MINI).await.unwrap().unwrap();
    let calendar =
        server.store.dav_collection(mini.id, uwumail_store::DavKind::Calendar, "personal").await.unwrap().unwrap();
    server.store.dav_share(mini.id, calendar.id, LENI, ShareRights::Read).await.unwrap();

    let shared_path = format!("/dav/calendars/leni@example.org/shared~{}/", calendar.id);
    let home = server
        .send(
            LENI,
            "PROPFIND",
            "/dav/calendars/leni@example.org/",
            &[("depth", "1")],
            r#"<propfind xmlns="DAV:" xmlns:CS="http://calendarserver.org/ns/"><prop><resourcetype/><displayname/>
<current-user-privilege-set/><owner/><CS:invite/></prop></propfind>"#,
        )
        .await;
    assert!(home.body.contains(&format!("<d:href>{shared_path}</d:href>")), "{}", home.body);
    assert!(home.body.contains("<c:calendar/><cs:shared/>"));
    assert!(home.body.contains("<d:owner><d:href>/dav/principals/mini@example.org/</d:href></d:owner>"));
    assert!(home.body.contains("<cs:read/>"));

    let listed = server.send(LENI, "PROPFIND", &shared_path, &[("depth", "1")], "").await;
    assert!(listed.body.contains(&format!("{shared_path}kaffee.ics")), "{}", listed.body);
    let fetched = server.send(LENI, "GET", &format!("{shared_path}kaffee.ics"), &[], "").await;
    assert!(fetched.body.contains("SUMMARY:Kaffee"));
    let refused =
        server.send(LENI, "PUT", &format!("{shared_path}neu.ics"), &[], &event.replace("kaffee@", "neu@")).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(refused.body.contains("need-privileges"));
    let not_theirs = server
        .send(
            LENI,
            "PROPPATCH",
            &shared_path,
            &[],
            r#"<propertyupdate xmlns="DAV:"><set><prop><displayname>X</displayname></prop></set></propertyupdate>"#,
        )
        .await;
    assert_eq!(not_theirs.status, StatusCode::FORBIDDEN);

    // With writing allowed, Leni's entries land in Mini's calendar.
    server.store.dav_share(mini.id, calendar.id, LENI, ShareRights::Write).await.unwrap();
    let written =
        server.send(LENI, "PUT", &format!("{shared_path}neu.ics"), &[], &event.replace("kaffee@", "neu@")).await;
    assert_eq!(written.status, StatusCode::CREATED, "{}", written.body);
    let mine = server.send(MINI, "GET", "/dav/calendars/mini@example.org/personal/neu.ics", &[], "").await;
    assert_eq!(mine.status, StatusCode::OK);

    // Deleting a shared calendar only leaves it.
    assert_eq!(server.send(LENI, "DELETE", &shared_path, &[], "").await.status, StatusCode::NO_CONTENT);
    assert_eq!(server.send(LENI, "PROPFIND", &shared_path, &[("depth", "0")], "").await.status, StatusCode::NOT_FOUND);
    assert_eq!(server.send(MINI, "GET", CALENDAR, &[], "").await.status, StatusCode::OK, "Mini still has it");
}
