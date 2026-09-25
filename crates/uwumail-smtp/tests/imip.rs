//! Invitations by mail (iMIP, RFC 6047) between two in-process servers: an invitation from one
//! lands in the calendar of someone on the other, their answer comes back into the organizer's
//! copy, and a cancellation marks the event. Answers and cancellations that are not vouched for
//! change nothing.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use uwumail_smtp::scheduling::Delivery;
use uwumail_smtp::{DeliveryConfig, ListenerKind, Smtp, SmtpConfig, SmtpSettings, SpamConfig, ToneConfig};
use uwumail_store::itip::{self, Component};
use uwumail_store::{Account, CalendarEventWrite, DavKind, MailboxRole, NewAccount, NewDavCollection, Role, Store};

struct TestServer {
    smtp: Smtp,
    mx: SocketAddr,
    _shutdown: watch::Sender<bool>,
    _dir: tempfile::TempDir,
}

fn spam() -> SpamConfig {
    SpamConfig { enabled: false, ..SpamConfig::default() }
}

async fn start(domain: &str, user: &str) -> TestServer {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain(domain).await.unwrap();
    store
        .create_account(NewAccount {
            address: format!("{user}@{domain}"),
            display_name: user.to_string(),
            password: Some("katzenpfote-123".into()),
            role: Role::User,
            quota_bytes: 0,
            protocols: None,
        })
        .await
        .unwrap();
    let hostname = format!("mx.{domain}");
    let smtp = Smtp::new(
        store,
        SmtpSettings {
            hostname: hostname.clone(),
            smtp: SmtpConfig::default(),
            spam: spam(),
            delivery: DeliveryConfig::default(),
            tone: ToneConfig::default(),
            server_tls: None,
        },
    )
    .unwrap();
    for name in [domain.to_string(), hostname, format!("_dmarc.{domain}"), "localhost".into()] {
        smtp.dns_cache().pin_no_txt(&name);
    }
    let (shutdown, rx) = watch::channel(false);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mx = listener.local_addr().unwrap();
    tokio::spawn(uwumail_smtp::serve(smtp.clone(), listener, ListenerKind::Mx, rx.clone()));
    tokio::spawn(uwumail_smtp::run_queue(smtp.clone(), rx));
    TestServer { smtp, mx, _shutdown: shutdown, _dir: dir }
}

async fn reply_line(lines: &mut Lines<BufReader<OwnedReadHalf>>) -> String {
    loop {
        let line = lines.next_line().await.unwrap().unwrap();
        if line.as_bytes().get(3) != Some(&b'-') {
            return line;
        }
    }
}

impl TestServer {
    /// Sends mail for `domain` to `other`, and trusts its DKIM keys.
    async fn knows(&self, other: &TestServer, domain: &str) {
        let delivery = DeliveryConfig {
            routes: HashMap::from([(domain.to_owned(), other.mx.to_string())]),
            ..DeliveryConfig::default()
        };
        self.smtp.update_settings(SmtpConfig::default(), spam(), delivery, ToneConfig::default()).unwrap();
        for key in uwumail_smtp::dkim::ensure_domain_keys(other.smtp.store(), domain).await.unwrap() {
            let (name, value) = key.dns_record();
            self.smtp.dns_cache().pin_txt(&name, &value).unwrap();
        }
        for name in [domain.to_string(), format!("mx.{domain}"), format!("_dmarc.{domain}")] {
            self.smtp.dns_cache().pin_no_txt(&name);
        }
    }

    async fn account(&self, login: &str) -> Account {
        self.smtp.store().account(login).await.unwrap().unwrap()
    }

    async fn copy(&self, login: &str, uid: &str) -> Option<Component> {
        let account = self.account(login).await;
        let record = self.smtp.store().own_calendar_event_by_uid(account.id, uid).await.unwrap()?;
        Component::parse(&record.content)
    }

    /// Waits until someone's copy of an event satisfies `done`.
    async fn wait_for(&self, login: &str, uid: &str, done: impl Fn(&Component) -> bool) -> Component {
        let started = Instant::now();
        loop {
            if let Some(copy) = self.copy(login, uid).await
                && done(&copy)
            {
                return copy;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "{login}'s copy of {uid} never got there");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Sends a message straight to the MX, as a stranger could, and waits until it is taken.
    async fn inject(&self, from: &str, to: &str, message: &str) {
        let stream = TcpStream::connect(self.mx).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        reply_line(&mut lines).await;
        for command in ["EHLO stranger.test".to_owned(), format!("MAIL FROM:<{from}>"), format!("RCPT TO:<{to}>"), "DATA".into()] {
            write.write_all(format!("{command}\r\n").as_bytes()).await.unwrap();
            reply_line(&mut lines).await;
        }
        write.write_all(format!("{message}\r\n.\r\n").as_bytes()).await.unwrap();
        let reply = reply_line(&mut lines).await;
        assert!(reply.starts_with("250"), "{reply}");
        write.write_all(b"QUIT\r\n").await.unwrap();
    }
}

fn invitation(uid: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//DE\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260917T080000Z\r\n\
DTSTART:20261001T150000Z\r\nDTEND:20261001T160000Z\r\nSUMMARY:Kaffee\r\nSEQUENCE:0\r\n\
ORGANIZER;CN=Mini:mailto:mini@a.test\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:mini@a.test\r\n\
ATTENDEE;CN=Nyu;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:nyu@b.test\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn partstat(copy: &Component, address: &str) -> String {
    itip::partstats(copy, address).first().map(|(_, status)| status.clone()).unwrap_or_default()
}

fn itip_mail(from: &str, to: &str, method: &str, calendar: &Component) -> String {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {method}\r\nMIME-Version: 1.0\r\n\
Content-Type: text/calendar; method={method}; charset=utf-8\r\n\r\n{}",
        calendar.to_ics()
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn invitations_travel_by_mail_and_answers_come_back() {
    let a = start("a.test", "mini").await;
    let b = start("b.test", "nyu").await;
    a.knows(&b, "b.test").await;
    b.knows(&a, "a.test").await;

    // Mini keeps the event in her calendar, as her client would, and the invitation goes out.
    let mini = a.account("mini@a.test").await;
    let calendars =
        a.smtp.store().dav_collections(mini.id, DavKind::Calendar, NewDavCollection::default_calendar("K")).await.unwrap();
    let content = invitation("kaffee@a.test");
    let checked = uwumail_store::ical::check_calendar(&content, &[]).unwrap();
    let write = CalendarEventWrite {
        id: None,
        calendar_id: calendars[0].id,
        content: content.clone(),
        uid: checked.uid,
        starts_at: checked.starts_at,
        ends_at: checked.ends_at,
        if_etag: None,
        keep_schedule_tag: false,
    };
    a.smtp.store().put_calendar_event(mini.id, write).await.unwrap();
    let report = a.smtp.schedule_change(&mini, None, Some(&content)).await;
    assert_eq!(report.sent, [("nyu@b.test".to_owned(), "REQUEST".to_owned(), Delivery::Mail)]);

    // On b.test the invitation lands in Nyu's calendar, waiting for an answer, and the mail too.
    let copy = b.wait_for("nyu@b.test", "kaffee@a.test", |_| true).await;
    assert_eq!(partstat(&copy, "nyu@b.test"), "NEEDS-ACTION");
    let nyu = b.account("nyu@b.test").await;
    let inbox = b.smtp.store().mailboxes(nyu.id).await.unwrap();
    let inbox = inbox.iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap();
    let mails = b.smtp.store().emails_in_mailbox(inbox.id, 10).await.unwrap();
    assert_eq!(mails.len(), 1, "the invitation mail arrives as well");
    assert_eq!(mails[0].subject, "Einladung: Kaffee");

    // A forged answer straight to a.test's door, without a signature, changes nothing.
    let forged = itip::reply(&copy, "nyu@b.test", Some("DECLINED"), 0);
    a.inject("nyu@b.test", "mini@a.test", &itip_mail("nyu@b.test", "mini@a.test", "REPLY", &forged)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(partstat(&a.copy("mini@a.test", "kaffee@a.test").await.unwrap(), "nyu@b.test"), "NEEDS-ACTION");

    // Nyu accepts on b.test: the signed answer reaches Mini's copy on a.test.
    let mut accepted = copy.clone();
    for event in accepted.components.iter_mut().filter(|c| c.name == "VEVENT") {
        for attendee in event.properties.iter_mut().filter(|p| p.address().as_deref() == Some("nyu@b.test")) {
            attendee.set_param("PARTSTAT", "ACCEPTED");
        }
    }
    let report = b.smtp.schedule_change(&nyu, Some(&copy.to_ics()), Some(&accepted.to_ics())).await;
    assert_eq!(report.sent, [("mini@a.test".to_owned(), "REPLY".to_owned(), Delivery::Mail)]);
    let organizer = a.wait_for("mini@a.test", "kaffee@a.test", |c| partstat(c, "nyu@b.test") == "ACCEPTED").await;
    assert_eq!(partstat(&organizer, "nyu@b.test"), "ACCEPTED", "the forged decline did not count");

    // A cancellation from someone who is not the organizer changes nothing at b.test.
    let cancel = itip::cancel(&Component::parse(&content).unwrap(), &["nyu@b.test".to_owned()], true, 0);
    b.inject("fremd@a.test", "nyu@b.test", &itip_mail("fremd@a.test", "nyu@b.test", "CANCEL", &cancel)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let still = b.copy("nyu@b.test", "kaffee@a.test").await.unwrap();
    assert_ne!(still.main_event().unwrap().value("STATUS"), Some("CANCELLED"));

    // Mini cancels: Nyu's copy says so.
    a.smtp.schedule_change(&mini, Some(&content), None).await;
    b.wait_for("nyu@b.test", "kaffee@a.test", |c| c.main_event().unwrap().value("STATUS") == Some("CANCELLED")).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn invitations_from_outside_for_organizers_of_ours_are_not_believed() {
    let a = start("a.test", "mini").await;
    // Someone elsewhere claims Mini invites Mini's own server colleague: organizers of a.test
    // tell their attendees directly, so this is not taken into any calendar.
    let request = itip::request(&Component::parse(&invitation("falsch@a.test")).unwrap(), 0);
    let for_mini = itip_mail("mini@a.test", "mini@a.test", "REQUEST", &request).replace("nyu@b.test", "mini@a.test");
    a.inject("mini@a.test", "mini@a.test", &for_mini).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(a.copy("mini@a.test", "falsch@a.test").await.is_none());
}
