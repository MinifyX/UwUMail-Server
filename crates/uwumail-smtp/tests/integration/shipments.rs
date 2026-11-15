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
