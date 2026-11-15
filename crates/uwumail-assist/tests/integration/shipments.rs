//! The look back for parcels (docs/jmap-shipments.md): earlier mail becomes shipments, oldest
//! first, and a second look changes nothing. Names and numbers are made up.

use serde_json::json;
use tokio::sync::watch;
use uwumail_assist::SettingsPatch;

use crate::common::{Reply, chat, rig};

fn mail(n: u32, subject: &str, body: &str) -> String {
    format!(
        "From: DHL Paket <noreply@dhl.example>\nTo: Mia <mia@example.org>\nSubject: {subject}\n\
Message-ID: <parcel-{n}@dhl.example>\nContent-Type: text/plain; charset=utf-8\n\n{body}\n"
    )
}

#[tokio::test]
async fn looking_back_finds_each_parcel_once() {
    let rig = rig().await;
    let shipped = rig
        .deliver(
            &rig.mia,
            &mail(1, "Ihr DHL Paket kommt bald", "Ihre Sendung 00340434161234567890 wurde an DHL übergeben."),
        )
        .await;
    let delivered = rig
        .deliver(
            &rig.mia,
            &mail(2, "Ihr DHL Paket wurde zugestellt", "Ihre Sendung 00340434161234567890 wurde zugestellt."),
        )
        .await;
    rig.deliver(&rig.mia, &mail(3, "Neuigkeiten", "Unser Newsletter ohne Paket.")).await;

    let (_tx, shutdown) = watch::channel(false);
    let look = async || {
        rig.store.start_shipment_rescan(rig.mia.id, 14).await.unwrap();
        while rig.assist.shipment_rescan_round(&shutdown).await {}
        assert!(rig.store.shipment_rescan(rig.mia.id).await.unwrap().is_none(), "the look back is done");
        rig.store.shipments(rig.mia.id, None).await.unwrap()
    };
    let found = look().await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].status, uwumail_labels::ShipmentStatus::Delivered);
    assert_eq!(found[0].email_ids(), [shipped, delivered]);
    assert_eq!(look().await, found, "a second look changes nothing");
}

/// A shop's mail the rules cannot read a tracking number from.
const SHOP: &str = "From: Laden <versand@laden.example>\nTo: Mia <mia@example.org>\n\
Subject: Deine Bestellung 4711-0815 ist unterwegs\nMessage-ID: <shop-1@laden.example>\n\
Content-Type: text/plain; charset=utf-8\n\n\
Hallo Mia, dein Paket ist unterwegs. Hermes Paketschein: ZZ1234567890 (bitte aufheben).\n";

#[tokio::test]
async fn the_model_reads_a_parcel_but_only_numbers_the_mail_has() {
    let rig = rig().await;
    rig.server_provider("openaiCompatible", json!({})).await;
    rig.store.create_assist_label(rig.mia.id, "Shop".into(), "Mails von Läden".into(), None).await.unwrap();
    let email = rig.deliver(&rig.mia, SHOP).await;
    let mail = uwumail_labels::Mail::parse(SHOP.as_bytes());
    let ruled = uwumail_labels::shipment(&mail);
    assert!(ruled.as_ref().is_none_or(|facts| facts.numbers.is_empty()), "{ruled:?}");

    // AI labels off: nothing is asked.
    rig.store.enqueue_auto_label(rig.mia.id, email).await.unwrap();
    rig.assist.work_queue().await;
    assert!(rig.fake.seen().is_empty());

    let patch = SettingsPatch { auto_labels: Some(true), ..SettingsPatch::default() };
    rig.assist.set_settings(&rig.mia, patch).await.unwrap();
    rig.store.enqueue_auto_label(rig.mia.id, email).await.unwrap();
    let labels = json!({ "labels": [{ "name": "Shipping", "reason": "Ein Paket", "fits": "yes" }] });
    let parcel = json!({
        "carrier": "hermes", "numbers": ["ZZ1234567890", "QQ9999999999"], "status": "shipped",
        "expectedDate": null, "expectedFrom": null, "expectedTo": null, "merchant": "Laden",
        "orderRef": "4711-0815", "confidence": 0.9
    });
    rig.fake.push(Reply::Json(200, chat(&labels.to_string()), vec![]));
    rig.fake.push(Reply::Json(200, chat(&parcel.to_string()), vec![]));
    assert!(rig.assist.work_queue().await);
    let seen = rig.fake.seen();
    assert!(seen.last().unwrap().body.to_string().contains("shipment_facts"), "the parcel was asked about");

    let found = rig.store.shipments(rig.mia.id, None).await.unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].number.as_deref(), Some("ZZ1234567890"), "a made-up number is left out");
    assert_eq!(found[0].events[0].source, "ai");

    // The shipping label is on now: labeling the mail again asks nothing.
    let _ = rig.assist.label_email(&rig.mia, email).await;
    let asked = rig.fake.seen().iter().filter(|seen| seen.body.to_string().contains("shipment_facts")).count();
    assert_eq!(asked, 1);
}
