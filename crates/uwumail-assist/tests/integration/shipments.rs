//! The look back for parcels (docs/jmap-shipments.md): earlier mail becomes shipments, oldest
//! first, and a second look changes nothing. Names and numbers are made up.

use tokio::sync::watch;

use crate::common::rig;

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
