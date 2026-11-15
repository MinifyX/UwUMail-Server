//! Shipments by rules (docs/labels.md, "Shipments"): invented German mails per carrier and
//! status, English UPS and Amazon mails, three Shopify themes, and mails that are no shipment.
//! Names, addresses and numbers are made up; carriers' hosts are under `.example`.

use uwumail_labels::{Carrier, DateWindow, Day, Mail, PickupKind, ShipmentFacts, ShipmentStatus, shipment};

use crate::{html_message, message};

fn found(mail: &Mail) -> ShipmentFacts {
    shipment(mail).unwrap_or_else(|| panic!("no shipment in {:?}", mail.subject))
}

fn on(day: u32, month: u32, year: i32) -> Day {
    Day::On { year: Some(year), month, day }
}

#[test]
fn dhl_by_status() {
    let shipped = html_message(
        &["From: DHL Paket <noreply@dhl.example>", "Subject: Ihr DHL Paket kommt bald"],
        r#"<p>Hallo Leni Beispiel,</p><p>Ihre Sendung von Beispielshop wurde an DHL übergeben.
        Voraussichtliche Zustellung: 16.11.2026</p>
        <a href="https://www.dhl.example/de/privatkunden/pakete-empfangen/verfolgen.html?piececode=00340434161234567890&amp;lang=de">Sendung verfolgen</a>"#,
    );
    let facts = found(&shipped);
    assert_eq!(facts.carrier, Some(Carrier::Dhl));
    assert_eq!(facts.numbers, ["00340434161234567890"]);
    assert_eq!(facts.status, Some(ShipmentStatus::Shipped));
    assert_eq!(facts.expected, Some(DateWindow { day: on(16, 11, 2026), from: None, to: None }));
    assert_eq!(
        facts.tracking_url.as_deref(),
        Some("https://www.dhl.de/de/privatkunden/pakete-empfangen/verfolgen.html?piececode=00340434161234567890"),
        "the carrier's own page, not the mail's link"
    );
    assert_eq!(facts.merchant, None, "a carrier is no shop");
    assert!(facts.confidence >= 0.9);

    let today = message(
        &["From: DHL Paket <noreply@dhl.example>", "Subject: Ihr DHL Paket wird heute zugestellt"],
        "Hallo Leni Beispiel,\n\nIhr Paket JJD000390000000000001 wird heute zugestellt.\nZustellung voraussichtlich zwischen 10:15 und 12:15 Uhr.",
    );
    let facts = found(&today);
    assert_eq!(facts.numbers, ["JJD000390000000000001"]);
    assert_eq!(facts.status, Some(ShipmentStatus::OutForDelivery));
    assert_eq!(facts.expected, Some(DateWindow { day: Day::InDays(0), from: Some((10, 15)), to: Some((12, 15)) }));
    assert_eq!(
        facts.tracking_url.as_deref(),
        Some("https://www.dhl.de/de/privatkunden/pakete-empfangen/verfolgen.html?piececode=JJD000390000000000001"),
        "built from the template when the mail has no link"
    );

    let delivered = message(
        &["From: DHL Paket <noreply@dhl.example>", "Subject: Ihr DHL Paket wurde zugestellt"],
        "Ihre Sendung 00340434161234567890 wurde zugestellt.\nAblageort: Garage",
    );
    assert_eq!(found(&delivered).status, Some(ShipmentStatus::Delivered));

    let packstation = message(
        &["From: DHL Paket <noreply@dhl.example>", "Subject: Ihr Paket liegt zur Abholung bereit"],
        "Ihre Sendung 00340434161234567890 liegt in der Packstation 123 zur Abholung bereit.\nBitte holen Sie sie bis zum 22.11.2026 ab.",
    );
    let facts = found(&packstation);
    assert_eq!(facts.status, Some(ShipmentStatus::ReadyForPickup));
    let pickup = facts.pickup.expect("a pickup place");
    assert_eq!(pickup.kind, PickupKind::Packstation);
    assert_eq!(pickup.name.as_deref(), Some("Packstation 123"));
    assert_eq!(pickup.until, Some(on(22, 11, 2026)));

    let failed = message(
        &["From: DHL Paket <noreply@dhl.example>", "Subject: Wir haben Sie leider nicht angetroffen"],
        "Leider konnten wir Sie heute nicht antreffen. Ihre Sendung 00340434161234567890 bringen wir in die Filiale Hauptstraße 1, abholbereit ab morgen.",
    );
    let facts = found(&failed);
    assert_eq!(facts.status, Some(ShipmentStatus::DeliveryFailed));
    assert_eq!(facts.pickup.map(|p| p.kind), Some(PickupKind::PostOffice));
}

#[test]
fn deutsche_post_registered_mail() {
    let mail = message(
        &["From: Deutsche Post <einschreiben@deutschepost.example>", "Subject: Ihr Einschreiben wurde zugestellt"],
        "Ihr Einschreiben RR123456789DE wurde zugestellt.",
    );
    let facts = found(&mail);
    assert_eq!(facts.carrier, Some(Carrier::DeutschePost));
    assert_eq!(facts.numbers, ["RR123456789DE"]);
    assert_eq!(facts.status, Some(ShipmentStatus::Delivered));
}

#[test]
fn dpd_predict() {
    let mail = html_message(
        &["From: DPD <noreply@dpd.example>", "Subject: Ihr DPD Paket kommt heute"],
        r#"<p>Guten Tag,</p><p>Ihr Paket von Beispielshop kommt heute zwischen 13:00 und 14:00 Uhr.</p>
        <a href="https://tracking.dpd.example/status/de_DE/parcel/01234567890123">Paket verfolgen</a>"#,
    );
    let facts = found(&mail);
    assert_eq!(facts.carrier, Some(Carrier::Dpd));
    assert_eq!(facts.numbers, ["01234567890123"]);
    assert_eq!(facts.status, Some(ShipmentStatus::OutForDelivery));
    assert_eq!(facts.expected, Some(DateWindow { day: Day::InDays(0), from: Some((13, 0)), to: Some((14, 0)) }));
}

#[test]
fn hermes_and_gls() {
    let hermes = message(
        &["From: Hermes <info@myhermes.example>", "Subject: Deine Sendung ist unterwegs"],
        "Hallo Leni,\ndeine Sendung H1234567890123456789 ist unterwegs und wird voraussichtlich am 16.11.2026 zugestellt.\nZustellzeitfenster: 14:00 - 18:00 Uhr",
    );
    let facts = found(&hermes);
    assert_eq!(facts.carrier, Some(Carrier::Hermes));
    assert_eq!(facts.numbers, ["H1234567890123456789"]);
    assert_eq!(facts.status, Some(ShipmentStatus::Shipped));
    assert_eq!(facts.expected.map(|e| e.day), Some(on(16, 11, 2026)));

    let gls = html_message(
        &["From: GLS <noreply@gls-pakete.example>", "Subject: Ihr GLS Paket ist auf dem Weg"],
        r#"<p>Ihr Paket ist auf dem Weg zu Ihnen und kommt voraussichtlich morgen.</p>
        <a href="https://gls-pakete.example/sendungsverfolgung?match=ZB12CD34&amp;txtAction=71000">FlexDeliveryService</a>"#,
    );
    let facts = found(&gls);
    assert_eq!(facts.carrier, Some(Carrier::Gls));
    assert_eq!(facts.numbers, ["ZB12CD34"]);
    assert_eq!(facts.expected.map(|e| e.day), Some(Day::InDays(1)));
}

#[test]
fn ups_in_english_and_german() {
    // The link goes through a click tracker that names its target.
    let english = html_message(
        &["From: UPS <mcinfo@ups.example>", "Subject: UPS Update: Package Scheduled for Delivery Today"],
        r#"<p>Your package is out for delivery. Estimated delivery: today between 2:00 PM and 6:00 PM.</p>
        <a href="https://click.mail.example/ls/click?url=https%3A%2F%2Fwww.ups.example%2Ftrack%3Ftracknum%3D1Z999AA10123456784">Track</a>"#,
    );
    let facts = found(&english);
    assert_eq!(facts.carrier, Some(Carrier::Ups));
    assert_eq!(facts.numbers, ["1Z999AA10123456784"]);
    assert_eq!(facts.status, Some(ShipmentStatus::OutForDelivery));
    assert_eq!(facts.expected, Some(DateWindow { day: Day::InDays(0), from: Some((14, 0)), to: Some((18, 0)) }));
    assert_eq!(facts.tracking_url.as_deref(), Some("https://www.ups.com/track?tracknum=1Z999AA10123456784"));

    let access_point = message(
        &["From: UPS <mcinfo@ups.example>", "Subject: Ihr Paket ist abholbereit"],
        "Ihr UPS Paket 1Z999AA10123456784 kann abgeholt werden.\nUPS Access Point: Kiosk Beispiel, Musterweg 2",
    );
    let facts = found(&access_point);
    assert_eq!(facts.status, Some(ShipmentStatus::ReadyForPickup));
    assert_eq!(facts.pickup.map(|p| p.kind), Some(PickupKind::AccessPoint));
}

#[test]
fn amazon() {
    let ordered = message(
        &[
            "From: Amazon.de <bestellbestaetigung@amazon.example>",
            "Subject: Bestellt: „Wasserkocher“ und 1 weiterer Artikel",
        ],
        "Vielen Dank für Ihre Bestellung.\nBestellnummer 302-1234567-1234567\nZustellung: Montag, 16. November",
    );
    let facts = found(&ordered);
    assert_eq!(facts.status, Some(ShipmentStatus::Ordered));
    assert_eq!(facts.order_ref.as_deref(), Some("302-1234567-1234567"));
    assert_eq!(facts.merchant.as_deref(), Some("Amazon"));
    assert!(facts.numbers.is_empty());
    assert_eq!(facts.carrier, None, "who delivers is not known yet");

    let shipped = message(
        &["From: Amazon.de <versandbestaetigung@amazon.example>", "Subject: Versandt: „Wasserkocher“"],
        "Ihr Paket wurde versandt und wird mit DHL zugestellt.\nSendungsnummer: 00340434161234567891\nBestellnummer 302-1234567-1234567",
    );
    let facts = found(&shipped);
    assert_eq!(facts.carrier, Some(Carrier::Dhl));
    assert_eq!(facts.numbers, ["00340434161234567891"]);
    assert_eq!(facts.order_ref.as_deref(), Some("302-1234567-1234567"));
    assert_eq!(facts.status, Some(ShipmentStatus::Shipped));

    let delivered = html_message(
        &[
            "From: Amazon.com <shipment-tracking@amazon.example>",
            "Subject: Delivered: Your Amazon.com order #112-7654321-7654321",
        ],
        r#"<p>Your package was delivered. Tracking ID TBA123456789012.</p>
        <a href="https://www.amazon.example/progress-tracker/package/ref=pe_1?orderId=112-7654321-7654321">Track package</a>"#,
    );
    let facts = found(&delivered);
    assert_eq!(facts.carrier, Some(Carrier::AmazonLogistics));
    assert_eq!(facts.numbers, ["TBA123456789012"]);
    assert_eq!(facts.status, Some(ShipmentStatus::Delivered));
    assert_eq!(facts.order_ref.as_deref(), Some("112-7654321-7654321"));
}

#[test]
fn amazon_delivering_itself_keeps_its_tracker() {
    let mail = html_message(
        &["From: Amazon.de <versandbestaetigung@amazon.example>", "Subject: Versendet: „Wasserkocher“"],
        r#"<p>Dein Paket wurde versendet!</p><p>Ankunft heute 18h – 22h</p><p>Bestellnr. 302-1234567-1234567</p>
        <a href="https://www.amazon.example/progress-tracker/package?orderId=302-1234567-1234567&amp;shipmentId=Ab1">Lieferung verfolgen</a>"#,
    );
    let facts = found(&mail);
    assert_eq!(facts.status, Some(ShipmentStatus::Shipped));
    assert_eq!(facts.carrier, None);
    assert!(facts.numbers.is_empty());
    assert!(facts.tracking_url.as_deref().unwrap().starts_with("https://www.amazon.example/progress-tracker/package?"));
    let expected = facts.expected.unwrap();
    assert_eq!((expected.day, expected.from, expected.to), (Day::InDays(0), Some((18, 0)), Some((22, 0))));
}

#[test]
fn a_shop_writing_in_the_plural() {
    let mail = message(
        &["From: Beispiel Store <shipping@orders.shop.example>", "Subject: Versandbenachrichtigung W1234567890"],
        "Deine Artikel wurden versendet.\nBestellnummer W1234567890",
    );
    let facts = found(&mail);
    assert_eq!(facts.status, Some(ShipmentStatus::Shipped));
    assert_eq!(facts.order_ref.as_deref(), Some("W1234567890"));
}

#[test]
fn tickets_are_no_parcels() {
    // The text part only says it is the text part; the HTML part has the ticket.
    let raw = "From: Support Kartenshop <help@karten.example>\r\nSubject: Ihre Bestellung #1234567 - Beispielcon 2027\r\n\
        MIME-Version: 1.0\r\nContent-Type: multipart/alternative; boundary=b\r\n\r\n\
        --b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nalternative Text\r\n\
        --b\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
        <p>Vielen Dank für Ihre Bestellung #1234567.</p><p>Ihre print@home Tickets zeigen Sie beim Einlass vor.</p>\r\n\
        --b--\r\n";
    let mail = Mail::parse(raw.as_bytes());
    assert!(mail.text.contains("Einlass"), "the HTML part stands in for the placeholder");
    assert_eq!(uwumail_labels::shipment(&mail), None);
    let invoice = message(
        &["From: Support Kartenshop <help@karten.example>", "Subject: Invoice for ticket order #1234567"],
        "Order #1234567",
    );
    assert_eq!(uwumail_labels::shipment(&invoice), None);
}

#[test]
fn shopify_themes() {
    // Dawn: a button to the carrier.
    let dawn = html_message(
        &["From: Kaffee Beispiel <hallo@kaffee-beispiel.example>", "Subject: Deine Bestellung ist unterwegs"],
        r#"<h2>Deine Bestellung ist unterwegs</h2><p>Bestellung #1042</p>
        <a class="button" href="https://www.dhl.example/de/privatkunden/pakete-empfangen/verfolgen.html?piececode=00340434161234567892">Sendung verfolgen</a>
        <a href="https://kaffee-beispiel.myshopify.example/12345/orders/abcdef/authenticate?key=x">Bestellung ansehen</a>"#,
    );
    let facts = found(&dawn);
    assert_eq!(facts.carrier, Some(Carrier::Dhl));
    assert_eq!(facts.numbers, ["00340434161234567892"]);
    assert_eq!(facts.status, Some(ShipmentStatus::Shipped));
    assert_eq!(facts.merchant.as_deref(), Some("Kaffee Beispiel"));
    assert_eq!(facts.order_ref.as_deref(), Some("1042"));

    // A classic theme: a shipping confirmation with a UPS link in a table.
    let classic = html_message(
        &["From: Wolle & Garn <shop@wolle-garn.example>", "Subject: Versandbestätigung für Bestellung #2077"],
        r#"<table><tr><td>Deine Artikel wurden versandt.</td></tr>
        <tr><td>UPS-Sendungsnummer: <a href="https://www.ups.example/track?loc=de_DE&amp;tracknum=1Z999AA10123456785">1Z999AA10123456785</a></td></tr></table>"#,
    );
    let facts = found(&classic);
    assert_eq!(facts.carrier, Some(Carrier::Ups));
    assert_eq!(facts.numbers, ["1Z999AA10123456785"]);
    assert_eq!(facts.order_ref.as_deref(), Some("2077"));

    // An English theme: DPD by a query parameter.
    let english = html_message(
        &["From: Example Outdoor <orders@outdoor.example>", "Subject: A shipment from order #A1007 is on the way"],
        r#"<p>Your order is on its way.</p>
        <a href="https://www.dpd.example/de/de/empfangen/sendungsverfolgung-und-live-tracking/?parcelNumber=09876543210987">Track your order</a>"#,
    );
    let facts = found(&english);
    assert_eq!(facts.carrier, Some(Carrier::Dpd));
    assert_eq!(facts.numbers, ["09876543210987"]);
    assert_eq!(facts.status, Some(ShipmentStatus::Shipped));
    assert_eq!(facts.order_ref.as_deref(), Some("A1007"));
}

#[test]
fn cancelled_and_returned() {
    let cancelled = message(
        &["From: Kaffee Beispiel <hallo@kaffee-beispiel.example>", "Subject: Bestellung #1050 wurde storniert"],
        "Deine Bestellung #1050 wurde storniert. Der Betrag wird erstattet.",
    );
    let facts = found(&cancelled);
    assert_eq!(facts.status, Some(ShipmentStatus::Cancelled));
    assert_eq!(facts.order_ref.as_deref(), Some("1050"));

    let returned = message(
        &["From: Mode Beispiel <service@mode.example>", "Subject: Deine Rücksendung ist unterwegs"],
        "Deine Rücksendung ist unterwegs zu uns. DHL Sendungsnummer 00340434161234567893.",
    );
    assert_eq!(found(&returned).status, Some(ShipmentStatus::Returned));
}

#[test]
fn no_shipment() {
    let advertising = message(
        &[
            "From: Mode Beispiel <newsletter@mode.example>",
            "Subject: Nur heute: kostenloser Versand und 20 % Rabatt",
            "List-Unsubscribe: <https://mode.example/abmelden>",
        ],
        "Sale! Bis zu 50 % reduziert, kostenloser Versand mit DHL. Jetzt shoppen, Angebot gilt nur heute.",
    );
    assert_eq!(shipment(&advertising), None);

    let invoice = message(
        &["From: Technik Beispiel <rechnung@technik.example>", "Subject: Ihre Rechnung RE-2026-0042"],
        "Anbei Ihre Rechnung über 49,90 €.\n\nHinweis: Die Ware ging mit DHL raus, Sendungsnummer 00340434161234567894.",
    );
    assert_eq!(shipment(&invoice), None);

    let newsletter = message(
        &[
            "From: DHL <newsletter@dhl.example>",
            "Subject: Neu: Pakete in der App verfolgen",
            "List-Unsubscribe: <https://www.dhl.example/abmelden>",
        ],
        "Mit der App sehen Sie, wann Ihr Paket kommt. Jetzt herunterladen.",
    );
    assert_eq!(shipment(&newsletter), None);

    let label = message(
        &["From: Mode Beispiel <service@mode.example>", "Subject: Dein Rücksendeetikett"],
        "Hier ist dein Rücksendeetikett für die Bestellung #3001. Bitte drucke es aus und gib das Paket bei DHL ab.",
    );
    assert_eq!(shipment(&label), None);

    // A link that only looks like DHL's, and a host that merely starts with its name.
    let lookalike = html_message(
        &["From: Paketdienst <info@paket.example>", "Subject: Ihre Sendung"],
        r#"<a href="https://www.dhl.example@evil.example/?piececode=00340434161234567895">Sendung</a>
        <a href="https://dhl.evil.example/?piececode=00340434161234567896">Sendung</a>"#,
    );
    assert_eq!(shipment(&lookalike), None);
}

#[test]
fn carrier_hosts_are_exact() {
    for host in ["dhl.de", "nolp.dhl.de", "www.ups.com", "dpd.co.uk", "tracking.dpd.de", "www.myhermes.de", "DHL.DE."] {
        assert!(Carrier::of_host(host).is_some(), "{host}");
    }
    for host in [
        "dhl.evil.example",
        "dhl.co.evil.example",
        "evildhl.de",
        "dhl.de.evil.example",
        "hermes.com",
        "dhl.xyz.invalid",
        "amazon.evil.test",
        "dhl.de%2eevil.example",
        "dhl.de&",
        "",
    ] {
        assert_eq!(Carrier::of_host(host), None, "{host}");
    }

    // An entity-encoded `@` does not hide the real host; a number-less link must be the carrier's.
    let encoded = html_message(
        &["From: Paketdienst <info@paket.example>", "Subject: Ihre DHL Sendung wurde versandt"],
        r#"<a href="https://www.dhl.de&#64;evil.example/?piececode=00340434161234567897">Sendung</a>"#,
    );
    assert_eq!(shipment(&encoded), None);
    let mut links = Vec::new();
    uwumail_labels::links_in("<a href=\"https://a.example/?x=1&#34;onclick=y&amp;z=2\">", &mut links);
    assert_eq!(links, ["https://a.example/?x=1"]);
}

#[test]
fn status_moves_forward() {
    use ShipmentStatus::*;
    assert!(Ordered.moves_to(Shipped));
    assert!(Shipped.moves_to(OutForDelivery));
    assert!(!Delivered.moves_to(InTransit));
    assert!(!OutForDelivery.moves_to(Shipped));
    assert!(Delivered.moves_to(Returned));
    assert!(DeliveryFailed.moves_to(OutForDelivery), "tried again");
    assert!(DeliveryFailed.moves_to(Delivered), "delivered on the next try");
    assert!(ReadyForPickup.moves_to(Delivered), "picked up");
    assert!(!Cancelled.moves_to(Shipped));
    for status in ShipmentStatus::ALL {
        assert_eq!(ShipmentStatus::from_id(status.id()), Some(status));
        assert!(!status.moves_to(status));
    }
    for id in ["dhl", "deutschePost", "dpd", "hermes", "gls", "ups", "amazonLogistics", "fedex", "other"] {
        assert_eq!(Carrier::from_id(id).map(|c| c.id()), Some(id));
    }
}

#[test]
fn odd_input_does_not_panic() {
    // Links in links, broken escapes, multibyte text around every word looked at.
    let text = format!(
        "{} Bestellnummer ä ü zwischen 9 Uhr ö voraussichtlich ü 31.ä Packstation ü bis zum ß https://x.example/?url=%E2%8 https://%",
        "https://".repeat(10_000)
    );
    let mail = Mail::new("a@b.example", "Ihre Sendung äöü", &text, Vec::new(), false, Vec::new());
    let _ = shipment(&mail);
    let mail = Mail::new(
        "a@b.example",
        "Zugestellt: ü",
        "Sendung 1Z999AA10123456784 ü zwischen",
        Vec::new(),
        false,
        Vec::new(),
    );
    assert_eq!(found(&mail).status, Some(ShipmentStatus::Delivered));
}
