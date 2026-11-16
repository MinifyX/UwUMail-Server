//! Shipments found at delivery (docs/jmap-shipments.md): a shipping mail becomes a shipment,
//! unless the person switched them off. Names and numbers are made up.

use crate::flow::{RawSession, TestServer, start};

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

/// A fake shipping mail (unauthenticated, a fee, links to a third site) goes to Junk and is no
/// shipment.
#[tokio::test(flavor = "multi_thread")]
async fn a_fake_shipping_mail_is_junk_and_no_shipment() {
    let a = crate::flow::spam_test_server(uwumail_smtp::SpamConfig::default(), None).await;
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    let reply = crate::flow::relay_message_from_outside(
        &a,
        "From: DHL Paket <news@sender.test>\r\nTo: mini@a.test\r\nSubject: Ihr DHL Paket wartet\r\n\r\n\
         Ihre Sendung 00340433958441804804 konnte nicht zugestellt werden. Bitte zahlen Sie die \
         Zustellgebuehr: https://pay.elsewhere.example/dhl\r\nZustellgebühr 1,99 EUR.\r\n",
    )
    .await;
    assert!(reply.starts_with("250"), "{reply}");
    let started = std::time::Instant::now();
    while a.mailbox("mini@a.test", uwumail_store::MailboxRole::Junk).await.is_empty() {
        assert!(started.elapsed() < std::time::Duration::from_secs(20), "the fake never reached Junk");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(store.shipments(mini, None).await.unwrap().is_empty());

    // Not bad enough for Junk (the limit is high here), but doubted: greylisted, then in the Inbox
    // on the retry, and still no shipment though it links to the carrier's tracking page.
    let a =
        crate::flow::spam_test_server(uwumail_smtp::SpamConfig { junk_score: 50.0, ..Default::default() }, None).await;
    let store = a.smtp.store();
    let mini = store.account("mini@a.test").await.unwrap().unwrap().id;
    let doubted = "From: Versand <news@sender.test>\r\nTo: mini@a.test\r\nSubject: Ihr DHL Paket ist unterwegs\r\n\r\n\
         Ihre Sendung ist unterwegs: \
         https://www.dhl.example/de/privatkunden/pakete-empfangen/verfolgen.html?piececode=00340434161234567896\r\n";
    let reply = crate::flow::relay_message_from_outside(&a, doubted).await;
    assert!(reply.starts_with("451"), "{reply}");
    let reply = crate::flow::relay_message_from_outside(&a, doubted).await;
    assert!(reply.starts_with("250"), "{reply}");
    a.wait_for_inbox("mini@a.test", 1).await;
    assert!(store.shipments(mini, None).await.unwrap().is_empty());
}
