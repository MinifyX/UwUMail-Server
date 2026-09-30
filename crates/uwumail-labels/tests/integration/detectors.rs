use serde_json::json;
use uwumail_labels::{Detector, detect};

use crate::{message, with_attachment};

fn finds(detector: Detector, mail: &uwumail_labels::Mail) -> Option<serde_json::Value> {
    detect(detector, mail).map(|finding| finding.params)
}

#[test]
fn invoices() {
    let pdf = with_attachment(
        &["From: Stadtwerke <rechnung@stadtwerke.example>", "Subject: Ihre Unterlagen"],
        "Guten Tag,\nanbei erhalten Sie Ihre Unterlagen.",
        "application/pdf",
        "Rechnung_2026-4711.pdf",
    );
    assert_eq!(finds(Detector::Invoice, &pdf), Some(json!({ "attachment": "Rechnung_2026-4711.pdf" })));

    let telekom = message(
        &["From: Mobilfunk <service@telco.example>", "Subject: Ihre Mobilfunkrechnung für September 2026"],
        "Hallo Frau Beispiel,\n\nIhre Rechnung ist da. Rechnungsbetrag: 39,99 € (inkl. MwSt.)\nDer Betrag wird am 05.10.2026 abgebucht.",
    );
    assert_eq!(finds(Detector::Invoice, &telekom), Some(json!({ "word": "Mobilfunkrechnung", "amount": "39,99 €" })));

    let receipt = message(
        &["From: Shop <receipts@shop.example>", "Subject: Your receipt from Example Shop #1234-5678"],
        "Thanks for your purchase!\n\nSubtotal $45.00\nTax $3.60\nTotal: $48.60\n",
    );
    assert_eq!(finds(Detector::Invoice, &receipt), Some(json!({ "word": "receipt", "amount": "$45.00" })));

    // An order confirmation mentions the billing address and amounts, but is no invoice.
    let order = message(
        &["From: Shop <bestellung@shop.example>", "Subject: Ihre Bestellung 302-1234567"],
        "Vielen Dank für Ihre Bestellung!\nRechnungsadresse: Leni Beispiel\nGesamtsumme: 89,90 €",
    );
    assert_eq!(finds(Detector::Invoice, &order), None);

    // A newsletter with prices and the word "total" in the text, no invoice word in the subject.
    let sale = message(
        &["From: Shop <news@shop.example>", "Subject: Nur heute: 20 % auf alles"],
        "Sparen Sie total! Jacke statt 99,00 € nur 79,20 €.",
    );
    assert_eq!(finds(Detector::Invoice, &sale), None);

    // "Rechnung" in the subject without an amount or a PDF: not enough.
    let question = message(
        &["From: Leni <leni@example.org>", "Subject: Frage zur Rechnung"],
        "Hallo, ich habe eine Frage zu eurer letzten Rechnung. Ruft ihr mich an?",
    );
    assert_eq!(finds(Detector::Invoice, &question), None);
}

#[test]
fn appointments() {
    let invite = {
        let raw = "From: Leni <leni@example.org>\r\nSubject: Einladung: Planung\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nPlanung\r\n\
--b\r\nContent-Type: text/calendar; method=REQUEST\r\n\r\nBEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n--b--\r\n";
        uwumail_labels::Mail::parse(raw.as_bytes())
    };
    assert_eq!(finds(Detector::Appointment, &invite), Some(json!({ "calendar": true })));

    let dentist = message(
        &["From: Praxis Dr. Zahn <praxis@zahn.example>", "Subject: Terminbestätigung"],
        "Sehr geehrte Frau Beispiel,\nhiermit bestätigen wir Ihren Termin am Dienstag, 06.10.2026 um 9:30 Uhr.\nBitte bringen Sie Ihre Karte mit.",
    );
    assert_eq!(
        finds(Detector::Appointment, &dentist),
        Some(json!({ "word": "Terminbestätigung", "date": "06.10.2026", "time": "9:30" }))
    );

    let restaurant = message(
        &["From: Bistro <hello@bistro.example>", "Subject: Your reservation is confirmed"],
        "Hi Leni,\nwe look forward to seeing you on October 9 at 7pm, table for 2.",
    );
    assert_eq!(
        finds(Detector::Appointment, &restaurant),
        Some(json!({ "word": "reservation", "date": "october 9", "time": "7pm" }))
    );

    // A delivery date is no appointment.
    let delivery = message(
        &["From: Shop <versand@shop.example>", "Subject: Ihr Liefertermin steht fest"],
        "Ihre Sendung kommt am 07.10.2026 zwischen 10:00 und 14:00.",
    );
    assert_eq!(finds(Detector::Appointment, &delivery), None);

    // An invitation without a time.
    let party = message(
        &["From: Mia <mia@example.org>", "Subject: Einladung zu meinem Geburtstag"],
        "Ich feiere am 12.10.2026, kommt vorbei!",
    );
    assert_eq!(finds(Detector::Appointment, &party), None);
}

#[test]
fn newsletters() {
    let news = message(
        &[
            "From: Magazin <news@magazin.example>",
            "Subject: Die Woche im Überblick",
            "List-Unsubscribe: <https://magazin.example/unsub?u=1>, <mailto:unsub@magazin.example>",
            "List-Id: Magazin Newsletter <news.magazin.example>",
        ],
        "Die Themen dieser Woche …",
    );
    assert_eq!(finds(Detector::Newsletter, &news), Some(json!({ "header": "List-Id" })));

    let one_click = message(
        &[
            "From: Shop <news@shop.example>",
            "Subject: New arrivals",
            "List-Unsubscribe: <https://shop.example/u/abc>",
            "List-Unsubscribe-Post: List-Unsubscribe=One-Click",
        ],
        "See what's new this autumn.",
    );
    assert_eq!(finds(Detector::Newsletter, &one_click), Some(json!({ "header": "List-Unsubscribe-Post" })));

    // A discussion list has List-Post.
    let list = message(
        &[
            "From: Leni <leni@example.org>",
            "Subject: [users] Question about backups",
            "List-Unsubscribe: <mailto:users-leave@lists.example.org>",
            "List-Id: <users.lists.example.org>",
            "List-Post: <mailto:users@lists.example.org>",
        ],
        "Does anyone back up to tape?",
    );
    assert_eq!(finds(Detector::Newsletter, &list), None);

    // A shipping notice sent through a newsletter service is a shipment, not a newsletter.
    let shipped = message(
        &[
            "From: Shop <versand@shop.example>",
            "Subject: Ihr Paket ist unterwegs",
            "List-Unsubscribe: <https://shop.example/u/abc>",
            "List-Unsubscribe-Post: List-Unsubscribe=One-Click",
        ],
        "Ihre Bestellung wurde mit DHL versendet. Sendungsnummer: 00340434161234567890",
    );
    assert_eq!(finds(Detector::Newsletter, &shipped), None);

    // A personal mail with List-Unsubscribe only (some providers add it).
    let personal = message(
        &["From: Mia <mia@example.org>", "Subject: Hi", "List-Unsubscribe: <mailto:x@example.org>"],
        "Wie geht's?",
    );
    assert_eq!(finds(Detector::Newsletter, &personal), None);
}

#[test]
fn shipments() {
    let dhl = message(
        &["From: Shop <versand@shop.example>", "Subject: Ihr Paket ist unterwegs"],
        "Ihre Bestellung wurde mit DHL versendet. Sendungsnummer: 00340434161234567890",
    );
    assert_eq!(finds(Detector::Shipping, &dhl), Some(json!({ "carrier": "DHL", "tracking": "00340434161234567890" })));

    let ups = message(
        &["From: Store <orders@store.example>", "Subject: Your order has shipped"],
        "Good news! Your package is on its way.\nTracking number: 1Z999AA10123456784",
    );
    assert_eq!(finds(Detector::Shipping, &ups), Some(json!({ "carrier": "UPS", "tracking": "1Z999AA10123456784" })));

    let amazon = message(
        &["From: Amazon <shipment-tracking@amazon.example>", "Subject: Zugestellt: Ihr Paket"],
        "Ihr Paket wurde zugestellt. Sendungsnummer TBA123456789012.",
    );
    assert_eq!(finds(Detector::Shipping, &amazon), Some(json!({ "carrier": "Amazon", "tracking": "TBA123456789012" })));

    let dpd = message(
        &["From: DPD <noreply@dpd.example>", "Subject: Ihre DPD Paketzustellung heute"],
        "Ihr Paket 01234567890123 wird heute zwischen 10:30 und 11:30 zugestellt.",
    );
    assert_eq!(finds(Detector::Shipping, &dpd), Some(json!({ "carrier": "DPD", "tracking": "01234567890123" })));

    let hermes = message(
        &["From: Hermes <info@myhermes.example>", "Subject: Deine Sendung ist unterwegs"],
        "Wir bringen dein Paket bald vorbei.",
    );
    assert_eq!(finds(Detector::Shipping, &hermes), Some(json!({ "carrier": "Hermes", "tracking": null })));

    // "ups" in English words is no carrier.
    let backups = message(
        &["From: Admin <admin@example.org>", "Subject: Backups delivered"],
        "Oops, the backups for the groups were delivered late. Ticket 1234567890123.",
    );
    assert_eq!(finds(Detector::Shipping, &backups), None);

    // A shop's newsletter about free shipping.
    let promo = message(
        &[
            "From: Shop <news@shop.example>",
            "Subject: Kostenloser Versand mit DHL bis Sonntag",
            "List-Unsubscribe: <https://shop.example/u/abc>",
        ],
        "Nur bis Sonntag: versandkostenfrei bestellen!",
    );
    assert_eq!(finds(Detector::Shipping, &promo), None);
}
