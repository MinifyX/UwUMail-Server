//! Shipments found at delivery (docs/jmap-shipments.md): a shipping mail becomes a shipment,
//! unless the person switched them off. Names and numbers are made up.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use uwumail_labels::ShipmentStatus;
use uwumail_smtp::SpamConfig;
use uwumail_smtp::parcels::{LookupConfig, ParcelRequest, ParcelTransport};
use uwumail_smtp::provider_oauth::BoxFuture;
use uwumail_store::MailboxRole;

use crate::flow::{RawSession, TestServer, relay_message_from_outside, spam_test_server, start};

fn dhl_mail(number: &str) -> String {
    format!(
        "From: DHL Paket <noreply@dhl.example>\r\nTo: mini@a.test\r\nSubject: Ihr DHL Paket kommt bald\r\n\
         Message-ID: <{number}@dhl.example>\r\n\r\n\
         Ihre Sendung {number} von Beispielshop wurde an DHL \u{fc}bergeben.\r\n"
    )
}

async fn hand_in(server: &TestServer, message: &str) {
    let mut session = RawSession::connect(server.mx).await;
    assert!(session.command("EHLO relay.local").await.starts_with("250"));
    assert!(session.command("MAIL FROM:<noreply@dhl.example>").await.starts_with("250"));
    assert!(session.command("RCPT TO:<mini@a.test>").await.starts_with("250"));
    assert!(session.command("DATA").await.starts_with("354"));
    assert!(session.command(&format!("{message}.")).await.starts_with("250"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shipping_mail_is_followed_unless_switched_off() {
    let a = start("a.test", &["mini"], &[]).await;
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    // security-audit-0.36.0 S-5: shipments trust authenticated mail, so the sender's SPF passes.
    a.smtp.dns_cache().pin_txt("dhl.example", "v=spf1 ip4:127.0.0.1 -all").unwrap();
    for name in ["_dmarc.dhl.example", "relay.local"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }

    hand_in(&a, &dhl_mail("00340434161234567890")).await;
    let inbox = a.wait_for_inbox("mini@a.test", 1).await;
    let shipments = store.shipments(mini, None).await.unwrap();
    assert_eq!(shipments.len(), 1);
    assert_eq!(shipments[0].number.as_deref(), Some("00340434161234567890"));
    assert_eq!(shipments[0].email_ids(), [inbox[0].id]);

    let off = serde_json::json!({ "shipments": "off" }).as_object().unwrap().clone();
    store.update_preferences(mini, off).await.unwrap();
    assert!(store.shipments(mini, None).await.unwrap().is_empty());
    hand_in(&a, &dhl_mail("00340434161234567891")).await;
    a.wait_for_inbox("mini@a.test", 2).await;
    assert!(store.shipments(mini, None).await.unwrap().is_empty(), "nothing is kept while off");
}

/// Carriers that answer from a list instead of the network: the first answer whose key is part of
/// the address asked, else "not found".
#[derive(Default)]
struct FakeCarriers {
    answers: Mutex<Vec<(&'static str, u16, String)>>,
    asked: Mutex<Vec<String>>,
}

impl FakeCarriers {
    fn answer(&self, key: &'static str, status: u16, body: serde_json::Value) {
        self.answers.lock().unwrap().insert(0, (key, status, body.to_string()));
    }
    fn asked(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

impl ParcelTransport for FakeCarriers {
    fn send(&self, request: ParcelRequest) -> BoxFuture<'_, Result<(u16, Vec<u8>), String>> {
        let answer = self.answers.lock().unwrap().iter().find(|(key, _, _)| request.url.contains(key)).cloned();
        self.asked.lock().unwrap().push(request.url);
        Box::pin(
            async move { Ok(answer.map_or((404, b"{}".to_vec()), |(_, status, body)| (status, body.into_bytes()))) },
        )
    }
}

fn with_carriers(server: &TestServer) -> Arc<FakeCarriers> {
    let fake = Arc::new(FakeCarriers::default());
    server.smtp.parcels().set_transport(fake.clone());
    server.smtp.parcels().configure(LookupConfig { enabled: true, dhl_key: "test-key".into(), ..Default::default() });
    fake
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

#[tokio::test(flavor = "multi_thread")]
async fn parcels_are_asked_of_their_carrier_on_a_schedule_until_they_arrive() {
    let a = start("a.test", &["mini"], &[]).await;
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    a.smtp.dns_cache().pin_txt("dhl.example", "v=spf1 ip4:127.0.0.1 -all").unwrap();
    for name in ["_dmarc.dhl.example", "relay.local"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    hand_in(&a, &dhl_mail("00340434169999000001")).await;
    a.wait_for_inbox("mini@a.test", 1).await;
    let id = store.shipments(mini, None).await.unwrap()[0].id;
    let fake = with_carriers(&a);
    let parcels = a.smtp.parcels();
    assert_eq!(parcels.carriers(), ["dhl", "deutschePost", "dpd", "hermes", "gls"]);

    let out_for_delivery = json!({ "shipments": [{ "events": [
        { "timestamp": "2027-01-12T07:10:00+01:00", "statusCode": "transit", "description": "Die Sendung wird zugestellt.",
          "status": "in Zustellung", "location": { "address": { "addressLocality": "Musterstadt" } } },
        { "timestamp": "2027-01-11T20:00:00+01:00", "statusCode": "transit", "description": "Im Paketzentrum bearbeitet" },
    ], "estimatedTimeOfDelivery": "2027-01-12" }] });
    fake.answer("trackingNumber=00340434169999000001", 200, out_for_delivery);
    let start = now();
    assert!(!parcels.run_due(store, start).await);
    let shipment = &store.shipments(mini, None).await.unwrap()[0];
    assert_eq!(shipment.status, ShipmentStatus::OutForDelivery);
    assert_eq!(shipment.looked_up_at, Some(start));
    assert_eq!(shipment.lookup_error, None);
    let carrier: Vec<_> = shipment.events.iter().filter(|event| event.source == "carrier").collect();
    assert_eq!(carrier.len(), 2, "{:?}", shipment.events);
    assert!(carrier.iter().any(|event| event.location.as_deref() == Some("Musterstadt")));
    assert_eq!(shipment.expected_day.as_deref(), Some("2027-01-12"));

    // Not due again for half an hour; then the same events are not added twice.
    parcels.run_due(store, start + 60).await;
    assert_eq!(fake.asked(), 1);
    parcels.run_due(store, start + 1801).await;
    assert_eq!(fake.asked(), 2);
    let events = store.shipments(mini, None).await.unwrap()[0].events.len();
    assert_eq!(events, shipment.events.len(), "nothing twice");

    // Delivered: never asked again.
    fake.answer(
        "trackingNumber=00340434169999000001",
        200,
        json!({ "shipments": [{ "events": [{ "timestamp": "2027-01-12T11:00:00+01:00", "statusCode": "delivered", "description": "Zugestellt" }] }] }),
    );
    parcels.run_due(store, start + 3700).await;
    assert_eq!(store.shipments(mini, None).await.unwrap()[0].status, ShipmentStatus::Delivered);
    parcels.run_due(store, start + 100_000).await;
    assert_eq!(fake.asked(), 3);
    assert_eq!(parcels.refresh(store, mini, id, start + 100_000).await.unwrap(), Err("notSupported"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refresh_waits_five_minutes_and_says_what_the_carrier_said() {
    let a = start("a.test", &["mini"], &[]).await;
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    a.smtp.dns_cache().pin_txt("dhl.example", "v=spf1 ip4:127.0.0.1 -all").unwrap();
    for name in ["_dmarc.dhl.example", "relay.local"] {
        a.smtp.dns_cache().pin_no_txt(name);
    }
    hand_in(&a, &dhl_mail("00340434169999000002")).await;
    a.wait_for_inbox("mini@a.test", 1).await;
    let id = store.shipments(mini, None).await.unwrap()[0].id;
    let parcels = a.smtp.parcels();
    // Without a key, nobody is asked about DHL parcels.
    assert_eq!(parcels.refresh(store, mini, id, now()).await.unwrap(), Err("notSupported"));
    let fake = with_carriers(&a);
    let start = now();
    assert_eq!(parcels.refresh(store, mini, id, start).await.unwrap(), Err("notFound"));
    assert_eq!(store.shipments(mini, None).await.unwrap()[0].lookup_error.as_deref(), Some("notFound"));
    assert_eq!(parcels.refresh(store, mini, id, start + 299).await.unwrap(), Err("tooSoon"));
    fake.answer("trackingNumber=00340434169999000002", 503, json!({}));
    assert_eq!(parcels.refresh(store, mini, id, start + 301).await.unwrap(), Err("unavailable"));
    assert_eq!(fake.asked(), 2);
    assert!(matches!(
        parcels.refresh(store, mini, id + 1000, start).await,
        Err(uwumail_store::StoreError::NotFound(_))
    ));
}

/// A mail dressed up as DHL from somewhere else, about a parcel DHL never heard of, goes to Junk;
/// one whose parcel DHL knows stays where it was.
#[tokio::test(flavor = "multi_thread")]
async fn a_doubtful_mail_about_an_unknown_parcel_goes_to_junk() {
    // Doubtful, not spam: the filter alone would deliver it.
    let a =
        spam_test_server(SpamConfig { junk_score: 20.0, greylist_score: 20.0, ..SpamConfig::default() }, None).await;
    let fake = with_carriers(&a);
    let fake_dhl = |number: &str| {
        format!(
            "From: DHL Paket <news@sender.test>\r\nSubject: Ihr DHL Paket kommt bald\r\n\r\n\
             Ihre Sendung {number} von Beispielshop wurde an DHL \u{fc}bergeben.\r\n"
        )
    };
    let reply = relay_message_from_outside(&a, &fake_dhl("00340434169999000003")).await;
    assert!(reply.starts_with("250"), "{reply}");
    let started = std::time::Instant::now();
    while a.mailbox("mini@a.test", MailboxRole::Junk).await.is_empty() {
        assert!(started.elapsed() < Duration::from_secs(20), "the mail never went to Junk, asked {}", fake.asked());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let log = a.smtp.store().spam_log(uwumail_store::SpamLogFilter { limit: 10, ..Default::default() }).await.unwrap();
    let entry = log
        .iter()
        .find(|entry| entry.hits.iter().any(|hit| hit.rule == "PARCEL_NUMBER_UNKNOWN"))
        .unwrap_or_else(|| panic!("{log:#?} asked {}", fake.asked()));
    assert_eq!(entry.action, "junk");
    assert_eq!(entry.recipients[0].address, "mini@a.test");

    // DHL is asked once in five seconds at most, so the next one goes to a server of its own.
    let b =
        spam_test_server(SpamConfig { junk_score: 20.0, greylist_score: 20.0, ..SpamConfig::default() }, None).await;
    let known = with_carriers(&b);
    known.answer(
        "trackingNumber=00340434169999000004",
        200,
        json!({ "shipments": [{ "events": [{ "timestamp": "2027-01-11T20:00:00+01:00", "statusCode": "transit", "description": "Unterwegs" }] }] }),
    );
    let reply = relay_message_from_outside(&b, &fake_dhl("00340434169999000004")).await;
    assert!(reply.starts_with("250"), "{reply}");
    b.wait_for_inbox("mini@a.test", 1).await;
    while known.asked() < 1 {
        assert!(started.elapsed() < Duration::from_secs(20), "DHL was never asked");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(b.mailbox("mini@a.test", MailboxRole::Junk).await.is_empty(), "a known parcel stays put");
}
